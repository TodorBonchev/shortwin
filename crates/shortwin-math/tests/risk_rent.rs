//! `rent` against `tests/vectors/risk/rent.json` (`gen_rent.py`: `shortwin_sim.py` v0.4.1's
//! basis indices with the `P_ACC_MAX` clamp and per-position rent, ECON §7's lender
//! credit (its remainder carried from accrual to accrual since AIR-03) and true-up with
//! per-position `rent_protocol_bps`, and `pool_model.py` v0.4.1's `receivable` under
//! governance changes — all exact, no tolerance), plus properties: kinked-rate continuity,
//! basis-index rent equals the direct integral, continuous credits never exceed the true-ups
//! (and, carried, fall short of them by at most one lamport) and `receivable` returns to 0
//! (ECON I-P7).

mod common;

use std::collections::HashMap;

use common::Dec;
use num_bigint::BigUint;
use proptest::prelude::*;
use serde::Deserialize;
use shortwin_math::MathError;
use shortwin_math::num::WAD;
use shortwin_math::rent::{self, Aggr, Basis, P_ACC_MAX, PosRent, RentTerms, YEAR_S};
use shortwin_math::waterfall;

#[derive(Deserialize)]
struct File {
    generator: String,
    paths: Vec<Path>,
    pool_trace: Vec<Trace>,
    min_rent: Vec<MinRent>,
    rate: Vec<Rate>,
    utilization: Vec<Util>,
}

#[derive(Deserialize)]
struct Path {
    kink_bps: Dec<u32>,
    steps: Vec<Step>,
    positions: Vec<Pos>,
}

#[derive(Deserialize)]
struct Step {
    dt: Dec<u64>,
    p_ema: Dec<u128>,
    util_bps: Dec<u32>,
    delta: [Dec<u128>; 3],
    j: [Dec<u128>; 3],
    aggr: [Dec<u128>; 3],
    lender_credit: Option<Dec<u64>>,
    lender_credit_rem: Dec<u128>,
    receivable: Option<Dec<u64>>,
}

#[derive(Deserialize)]
struct Pos {
    q: Dec<u64>,
    terms: [Dec<u32>; 3],
    rent_protocol_bps: Dec<u32>,
    open_step: Dec<usize>,
    close_step: Dec<usize>,
    r_min: Dec<u64>,
    j_open: [Dec<u128>; 3],
    accrued: Option<Dec<u64>>,
    due: Option<Dec<u64>>,
    credited_for: Option<Dec<u64>>,
}

#[derive(Deserialize)]
struct Trace {
    coef: Dec<u32>,
    kink_bps: Dec<u32>,
    ops: Vec<Op>,
}

#[derive(Deserialize)]
struct Op {
    op: String,
    id: Option<Dec<u64>>,
    q: Option<Dec<u64>>,
    rent_protocol_bps: Option<Dec<u32>>,
    dt: Option<Dec<u64>>,
    p_ema: Option<Dec<u128>>,
    util_bps: Option<Dec<u32>>,
    credit: Option<Dec<u64>>,
    credit_rem: Option<Dec<u128>>,
    credited_for: Option<Dec<u64>>,
    receivable: Dec<u64>,
}

#[derive(Deserialize)]
struct MinRent {
    q: Dec<u64>,
    p_ema: Dec<u128>,
    terms: [Dec<u32>; 3],
    util_bps: Dec<u32>,
    kink_bps: Dec<u32>,
    min_rent_s: Dec<u32>,
    expected: Option<Dec<u64>>,
}

#[derive(Deserialize)]
struct Rate {
    terms: [Dec<u32>; 3],
    util_bps: Dec<u32>,
    kink_bps: Dec<u32>,
    rate_bps: Dec<u64>,
}

#[derive(Deserialize)]
struct Util {
    t_free: Dec<u64>,
    borrowed: Dec<u64>,
    util_bps: Dec<u32>,
}

fn terms(t: &[Dec<u32>; 3]) -> RentTerms {
    RentTerms {
        base_bps: t[0].0,
        slope1_bps: t[1].0,
        slope2_bps: t[2].0,
    }
}

fn arr<T: Copy>(a: &[Dec<T>; 3]) -> [T; 3] {
    [a[0].0, a[1].0, a[2].0]
}

fn or_overflow(v: &Option<Dec<u64>>) -> Result<u64, MathError> {
    v.as_ref().map(|d| d.0).ok_or(MathError::Overflow)
}

fn get<T: Copy>(v: &Option<Dec<T>>, ctx: &str) -> T {
    v.as_ref()
        .unwrap_or_else(|| panic!("{ctx}: missing field"))
        .0
}

fn load() -> File {
    let f: File = common::load_vectors("risk", "rent.json");
    assert_eq!(f.generator, "gen_rent.py");
    f
}

/// The program's rent bookkeeping on one market (ARCH §3.0 step 4, §3.4 step 13, §3.5,
/// §4.5): the basis, `rent_aggr`, the carried credit remainder (`Market.lender_credit_rem`,
/// AIR-03) and `receivable` with the true-up clamp. `receivable` is `None` once the program
/// would have refused an instruction (`Overflow`); a refused credit leaves the remainder.
struct Books {
    basis: Basis,
    aggr: Aggr,
    rem: u128,
    receivable: Option<u64>,
}

impl Books {
    fn new() -> Self {
        Self {
            basis: Basis::default(),
            aggr: Aggr::default(),
            rem: 0,
            receivable: Some(0),
        }
    }

    /// `open_short`: the position snapshots the basis and joins `rent_aggr`.
    fn open(&mut self, q: u64, terms: RentTerms, rent_protocol_bps: u32, r_min: u64) -> PosRent {
        self.aggr
            .add(q, &terms, rent_protocol_bps)
            .expect("aggr add");
        PosRent {
            q,
            j_open: self.basis.j,
            terms,
            rent_protocol_bps,
            r_min,
        }
    }

    /// The prologue's accrual; returns `δJ` and the lenders' credit.
    fn accrue(
        &mut self,
        p: u128,
        u: u32,
        kink: u32,
        dt: u64,
    ) -> ([u128; 3], Result<u64, MathError>) {
        let d = rent::accrue(&mut self.basis, p, u, kink, dt).expect("accrue");
        let credit = rent::lender_credit_carried(&self.aggr, &d, self.rem).map(|(c, rem)| {
            self.rem = rem;
            c
        });
        self.receivable = match (self.receivable, credit) {
            (Some(r), Ok(c)) => r.checked_add(c),
            _ => None,
        };
        (d, credit)
    }

    /// An exit: `rent_aggr −= …`, `receivable −= min(credited_for, receivable)`.
    fn close(&mut self, pos: &PosRent) -> Result<u64, MathError> {
        self.aggr
            .sub(pos.q, &pos.terms, pos.rent_protocol_bps)
            .expect("aggr sub");
        let credited = rent::credited_for(pos, &self.basis);
        self.receivable = match (self.receivable, credited) {
            (Some(r), Ok(c)) => Some(r - c.min(r)),
            _ => None,
        };
        credited
    }
}

/// Time `k` of a path (before step `k`): opens, then closes, as `gen_rent.py`.
fn path_time(
    books: &mut Books,
    path: &Path,
    pi: usize,
    k: usize,
    open: &mut [Option<PosRent>],
) -> usize {
    for (i, p) in path.positions.iter().enumerate() {
        if p.open_step.0 == k {
            let pos = books.open(p.q.0, terms(&p.terms), p.rent_protocol_bps.0, p.r_min.0);
            assert_eq!(pos.j_open, arr(&p.j_open), "path {pi} pos {i}: j_open");
            open[i] = Some(pos);
        }
    }
    let mut closed = 0;
    for (i, p) in path.positions.iter().enumerate() {
        if p.close_step.0 == k {
            let pos = open[i].take().expect("opened before closing");
            let ctx = format!("path {pi} pos {i}");
            let at = &books.basis;
            assert_eq!(
                rent::accrued(&pos, at),
                or_overflow(&p.accrued),
                "{ctx}: accrued"
            );
            assert_eq!(rent::due(&pos, at), or_overflow(&p.due), "{ctx}: due");
            assert_eq!(
                books.close(&pos),
                or_overflow(&p.credited_for),
                "{ctx}: credited_for"
            );
            closed += 1;
        }
    }
    closed
}

#[test]
fn basis_paths_match_sim() {
    let f = load();
    assert!(f.paths.len() >= 40);
    let (mut clamped, mut closes, mut ip7) = (0, 0, 0);
    for (pi, path) in f.paths.iter().enumerate() {
        let kink = path.kink_bps.0;
        let mut books = Books::new();
        let mut open = vec![None; path.positions.len()];
        closes += path_time(&mut books, path, pi, 0, &mut open);
        for (i, st) in path.steps.iter().enumerate() {
            let ctx = format!("path {pi} step {i}");
            assert_eq!(books.aggr.a, arr(&st.aggr), "{ctx}: A");
            let (d, credit) = books.accrue(st.p_ema.0, st.util_bps.0, kink, st.dt.0);
            assert_eq!(d, arr(&st.delta), "{ctx}: δJ");
            assert_eq!(books.basis.j, arr(&st.j), "{ctx}: J");
            assert_eq!(
                credit,
                or_overflow(&st.lender_credit),
                "{ctx}: lender credit"
            );
            assert_eq!(
                books.rem, st.lender_credit_rem.0,
                "{ctx}: carried remainder"
            );
            if st.p_ema.0 > P_ACC_MAX && st.dt.0 > 0 {
                clamped += 1;
                assert_eq!(d[0], P_ACC_MAX * u128::from(st.dt.0), "{ctx}: clamp");
            }
            closes += path_time(&mut books, path, pi, i + 1, &mut open);
            assert_eq!(
                books.receivable,
                st.receivable.as_ref().map(|d| d.0),
                "{ctx}: receivable"
            );
        }
        // ECON I-P7: every position closed, so A_j = 0 and (if tracked) receivable = 0.
        assert!(open.iter().all(Option::is_none), "path {pi}");
        assert_eq!(books.aggr, Aggr::default(), "path {pi}");
        if let Some(r) = books.receivable {
            assert_eq!(r, 0, "path {pi}: I-P7");
            ip7 += 1;
        }
    }
    assert!(clamped > 0 && closes > 100 && ip7 >= 30);
}

/// `pool_model.py` v0.4.1's continuous crediting (`Pool.advance`, `borrow`, `repay`,
/// `settle`, `_credited_for`) on base-only curves: its `receivable` after every operation,
/// under governance changes of `rent_protocol_bps` that reach new positions only.
#[test]
fn pool_model_receivable_matches() {
    let f = load();
    assert!(f.pool_trace.len() >= 10);
    let mut gov_changes = 0;
    for (ti, tr) in f.pool_trace.iter().enumerate() {
        let curve = RentTerms {
            base_bps: tr.coef.0,
            slope1_bps: 0,
            slope2_bps: 0,
        };
        let mut books = Books::new();
        let mut open = HashMap::new();
        let mut last_p = None;
        for (i, op) in tr.ops.iter().enumerate() {
            let ctx = format!("trace {ti} op {i} ({})", op.op);
            match op.op.as_str() {
                "open" => {
                    let p = get(&op.rent_protocol_bps, &ctx);
                    gov_changes += usize::from(last_p.is_some_and(|l| l != p));
                    last_p = Some(p);
                    let pos = books.open(get(&op.q, &ctx), curve, p, 0);
                    open.insert(get(&op.id, &ctx), pos);
                }
                "accrue" => {
                    let (_, credit) = books.accrue(
                        get(&op.p_ema, &ctx),
                        get(&op.util_bps, &ctx),
                        tr.kink_bps.0,
                        get(&op.dt, &ctx),
                    );
                    assert_eq!(credit, Ok(get(&op.credit, &ctx)), "{ctx}: credit");
                    assert_eq!(books.rem, get(&op.credit_rem, &ctx), "{ctx}: remainder");
                }
                "close" => {
                    let pos = open.remove(&get(&op.id, &ctx)).expect("open");
                    assert_eq!(
                        books.close(&pos),
                        Ok(get(&op.credited_for, &ctx)),
                        "{ctx}: credited_for"
                    );
                }
                k => panic!("unknown op {k}"),
            }
            assert_eq!(books.receivable, Some(op.receivable.0), "{ctx}: receivable");
        }
        assert!(open.is_empty());
        assert_eq!((books.aggr, books.receivable), (Aggr::default(), Some(0)));
    }
    assert!(gov_changes > 5);
}

#[test]
fn min_rent_rate_and_utilization_match_sim() {
    let f = load();
    for (i, c) in f.min_rent.iter().enumerate() {
        let got = rent::min_rent(
            c.q.0,
            c.p_ema.0,
            &terms(&c.terms),
            c.util_bps.0,
            c.kink_bps.0,
            c.min_rent_s.0,
        );
        assert_eq!(got, or_overflow(&c.expected), "min_rent case {i}");
    }
    for (i, c) in f.rate.iter().enumerate() {
        let r = rent::rate_bps(&terms(&c.terms), c.util_bps.0, c.kink_bps.0);
        assert_eq!(r, Ok(c.rate_bps.0), "rate case {i}");
    }
    for c in &f.utilization {
        assert_eq!(
            rent::utilization_bps(c.t_free.0, c.borrowed.0),
            Ok(c.util_bps.0)
        );
    }
}

fn big(x: u128) -> BigUint {
    BigUint::from(x)
}

fn tier_terms() -> impl Strategy<Value = RentTerms> {
    prop_oneof![
        Just([1_000, 5_000, 100_000]),
        Just([2_500, 9_375, 150_000]),
        Just([5_000, 18_750, 300_000]),
        Just([10_000, 37_500, 550_000]),
        (0u32..=50_000, 0u32..=1_000_000, 0u32..=1_000_000).prop_map(|(a, b, c)| [a, b, c]),
    ]
    .prop_map(|[b, s1, s2]| RentTerms {
        base_bps: b,
        slope1_bps: s1,
        slope2_bps: s2,
    })
}

/// Notional `q·P/WAD` at most 10¹⁵ lamports (10⁶ SOL): rent then always fits `u64`.
const MAX_QP: u128 = 10u128.pow(33);

fn step(q: u64) -> impl Strategy<Value = (u64, u128, u32)> {
    let max_p = MAX_QP / u128::from(q);
    (
        prop_oneof![0u64..=3_600, 0u64..=2_592_000],
        prop_oneof![1u128..=max_p.min(1_000_000_000_000), 1u128..=max_p],
        0u32..=10_000,
    )
}

/// A position size and a path of `(Δt, P_ema, U)` accruals.
fn sized_path() -> impl Strategy<Value = (u64, Vec<(u64, u128, u32)>)> {
    (1u64..=1_000_000_000_000_000)
        .prop_flat_map(|q| (Just(q), proptest::collection::vec(step(q), 1..40)))
}

/// `(q, P)` with a notional of at most 10¹⁵ lamports.
fn sized_price() -> impl Strategy<Value = (u64, u128)> {
    (1u64..=1_000_000_000_000_000).prop_flat_map(|q| (Just(q), 1u128..=MAX_QP / u128::from(q)))
}

/// A path of up to 24 accruals and up to 5 positions `(q, terms, rent_protocol_bps, open,
/// close)` on it (each position at most 10¹² raw units, so all notionals stay in range).
#[allow(clippy::type_complexity)]
fn market_book() -> impl Strategy<
    Value = (
        Vec<(u64, u128, u32)>,
        Vec<(u64, RentTerms, u32, usize, usize)>,
    ),
> {
    proptest::collection::vec(step(1_000_000_000_000), 1..25).prop_flat_map(|steps| {
        let n = steps.len();
        let pos = (
            1u64..=1_000_000_000_000,
            tier_terms(),
            prop_oneof![Just(1_500u32), 0u32..=3_000, Just(10_000)],
            0..=n,
            0..=n,
        )
            .prop_map(|(q, t, p, a, b)| (q, t, p, a.min(b), a.max(b)));
        (Just(steps), proptest::collection::vec(pos, 1..6))
    })
}

proptest! {
    #![proptest_config(common::proptest_config())]

    /// SPEC §6.1: the kinked curve is continuous at the kink and non-decreasing in U
    /// (the display form: each slope term floored, the sims' `rate_bps`).
    #[test]
    fn kinked_rate_is_continuous_and_monotone(t in tier_terms(), kink in 0u32..=9_500, u in 0u32..10_000) {
        let r = |u| rent::rate_bps(&t, u, kink).unwrap();
        prop_assert!(r(u) <= r(u + 1));
        // At the kink both branches agree: base + ⌊slope1·k/10⁴⌋.
        let at_kink = u128::from(t.base_bps) + u128::from(t.slope1_bps) * u128::from(kink) / 10_000;
        prop_assert_eq!(u128::from(r(kink)), at_kink);
        // One more bp of utilisation adds at most one bp-slope step (+1 for rounding).
        let step = if u < kink { t.slope1_bps } else { t.slope2_bps };
        prop_assert!(r(u + 1) - r(u) <= u64::from(step).div_ceil(10_000) + 1);
    }

    /// ARCH §8.1: basis-index rent equals the direct integral
    /// `Q·Σ P·r(U)·Δt/(10⁴·YEAR·WAD)` up to the rounding up of each index increment.
    #[test]
    fn basis_rent_equals_direct_integral(
        t in tier_terms(),
        kink in 0u32..=9_500,
        (q, steps) in sized_path(),
        rp in 0u32..=3_000,
    ) {
        let mut basis = Basis::default();
        let mut aggr = Aggr::default();
        aggr.add(q, &t, rp).unwrap();
        let mut exact = BigUint::ZERO;
        let (mut credits, mut rem) = (0u128, 0u128);
        for &(dt, p, u) in &steps {
            let d = rent::accrue(&mut basis, p, u, kink, dt).unwrap();
            let rate = u128::from(t.base_bps) * 10_000
                + u128::from(t.slope1_bps) * u128::from(u.min(kink))
                + u128::from(t.slope2_bps) * u128::from(u.saturating_sub(kink));
            exact += big(p) * big(u128::from(dt)) * big(rate);
            let (c, r) = rent::lender_credit_carried(&aggr, &d, rem).unwrap();
            credits += u128::from(c);
            rem = r;
        }
        let pos = PosRent { q, j_open: [0; 3], terms: t, rent_protocol_bps: rp, r_min: 0 };
        let den = big(10_000) * big(10_000) * big(u128::from(YEAR_S)) * big(WAD);
        let integral = exact * big(u128::from(q));
        let accrued = rent::accrued(&pos, &basis).unwrap();
        let floor = &integral / &den;
        let ceil = if &floor * &den == integral { floor.clone() } else { &floor + 1u8 };
        prop_assert!(big(u128::from(accrued)) >= ceil);
        prop_assert!(big(u128::from(accrued)) <= ceil + 1u8);
        // Continuous credits (the floor of their exact total, the remainder carried from
        // accrual to accrual: AIR-03) never exceed the true-up (rounded up) and fall short of
        // it by at most one lamport, however many accruals; the true-up never exceeds the
        // lenders' share of the rent paid in full.
        let credited = rent::credited_for(&pos, &basis).unwrap();
        prop_assert!(credits <= u128::from(credited));
        prop_assert!(u128::from(credited) <= credits + 1);
        let paid = waterfall::close(u64::MAX, 0, accrued, rp).unwrap();
        prop_assert!(paid.rent_lenders >= credited);
    }

    /// ECON I-P7 (ARCH §4.5): positions with different locked shares open and close over a
    /// path; the floored continuous credits never outrun the ceiled true-ups, so
    /// `receivable` (with the clamp) and `A_j` are 0 once all have closed.
    #[test]
    fn receivable_returns_to_zero(kink in 0u32..=9_500, (steps, positions) in market_book()) {
        let mut books = Books::new();
        let mut open: Vec<Option<PosRent>> = vec![None; positions.len()];
        let (mut credits, mut trueups) = (0u128, 0u128);
        for k in 0..=steps.len() {
            for (i, &(q, t, p, o, _)) in positions.iter().enumerate() {
                if o == k {
                    open[i] = Some(books.open(q, t, p, 0));
                }
            }
            for (i, &(.., c)) in positions.iter().enumerate() {
                if c == k {
                    let pos = open[i].take().unwrap();
                    trueups += u128::from(books.close(&pos).unwrap());
                }
            }
            if let Some(&(dt, p, u)) = steps.get(k) {
                credits += u128::from(books.accrue(p, u, kink, dt).1.unwrap());
            }
        }
        prop_assert!(credits <= trueups);
        prop_assert_eq!(books.aggr, Aggr::default());
        prop_assert_eq!(books.receivable, Some(0));
    }

    /// Splitting an accrual never lowers the indices (each increment rounds up); a price
    /// above `P_ACC_MAX` accrues exactly as `P_ACC_MAX`.
    #[test]
    fn split_accrual_rounds_up(p in 1u128..10u128.pow(24), u in 0u32..=10_000, kink in 0u32..=10_000,
                               dt1 in 0u64..1_000_000, dt2 in 0u64..1_000_000,
                               above in P_ACC_MAX..=u128::MAX) {
        let mut one = Basis::default();
        rent::accrue(&mut one, p, u, kink, dt1 + dt2).unwrap();
        let mut two = Basis::default();
        rent::accrue(&mut two, p, u, kink, dt1).unwrap();
        rent::accrue(&mut two, p, u, kink, dt2).unwrap();
        prop_assert_eq!(one.j[0], two.j[0]);
        for j in 1..3 {
            prop_assert!(two.j[j] >= one.j[j] && two.j[j] <= one.j[j] + 1);
        }
        let (mut a, mut b) = (Basis::default(), Basis::default());
        prop_assert_eq!(
            rent::accrue(&mut a, above, u, kink, dt1),
            rent::accrue(&mut b, P_ACC_MAX, u, kink, dt1)
        );
    }

    /// Minimum rent is `min_rent_s` seconds of rent at the open's rate: equal to accruing
    /// that long on a fresh basis, up to the rounding of the indices.
    #[test]
    fn min_rent_is_rent_for_min_rent_s(t in tier_terms(), (q, p) in sized_price(), u in 0u32..=10_000,
                                       kink in 0u32..=9_500, s in 0u32..=86_400) {
        let r_min = rent::min_rent(q, p, &t, u, kink, s).unwrap();
        let mut b = Basis::default();
        rent::accrue(&mut b, p, u, kink, u64::from(s)).unwrap();
        let pos = PosRent { q, j_open: [0; 3], terms: t, rent_protocol_bps: 1_500, r_min: 0 };
        let accrued = rent::accrued(&pos, &b).unwrap();
        prop_assert!(accrued >= r_min && accrued <= r_min + 1);
        prop_assert_eq!(rent::due(&PosRent { r_min, ..pos }, &Basis::default()), Ok(r_min));
    }
}
