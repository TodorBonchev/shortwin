//! `waterfall` (and `risk::{fair_value, fv_rate_bps, k_s, k_e}`) against the exits captured
//! from `shortwin_sim.py` v0.4.1 in `tests/vectors/risk/exits.json` (`gen_exits.py`: every
//! liquidation, expiry, close and settlement branch incl. underwater vaults, the keeper
//! floor and its truncation, truncated expiry keeper rewards, sandwiched liquidations,
//! S1/S2/S3 with and without the FV cap and with and without a vesting lump, FV and its
//! `p'` at every evaluated box corner, `K_s` / `K_e` on random amounts and terms — exact,
//! no tolerance), plus properties: legs sum to `V` (I-E4), no leg is negative, penalty
//! 50/20/30 and rent 85/15 splits are exact with rounding in favour of lenders, the keeper
//! floor (I-E10) and the lump bounds hold. `exit_cascade` holds the buyback exits of
//! `attacks/cascade.py`'s liquidation baskets (ARCH §8.1), checked with their fixed-delta
//! bound and `max_in` (I-E1).

mod common;

use common::Dec;
use proptest::prelude::*;
use serde::Deserialize;
use shortwin_math::num::WAD;
use shortwin_math::risk::{self, ExpiryKeeperTerms, SettleKeeperTerms, SettleKind};
use shortwin_math::waterfall::{self, ExitInputs, Payout, PenaltySplit};

#[derive(Deserialize)]
struct File {
    generator: String,
    exit: Vec<Exit>,
    exit_cascade: Vec<CascadeExit>,
    close: Vec<Close>,
    fv: Vec<Fv>,
    settle: Vec<Settle>,
    keepers: Vec<Keepers>,
    exit_fuzz: ExitFuzz,
}

/// `Market.settle_reward(v)` and `Market.expiry_keeper(b_ref)` at the given terms.
#[derive(Deserialize)]
struct Keepers {
    settle_terms: (Dec<u64>, Dec<u32>, Dec<u64>),
    expiry_terms: (Dec<u64>, Dec<u32>, Dec<u64>),
    v: Dec<u64>,
    k_s: Dec<u64>,
    b_ref: Dec<u64>,
    k_e: Dec<u64>,
}

/// Statistics of the sim's exit fuzz the rows were sampled from.
#[derive(Deserialize)]
struct ExitFuzz {
    runs: Dec<u64>,
    exits: Dec<u64>,
    penalty_truncated_by_sandwich: Dec<u64>,
}

#[derive(Deserialize)]
struct Exit {
    kind: String,
    v: Dec<u64>,
    b: Dec<u64>,
    r_due: Dec<u64>,
    b_live: Dec<u64>,
    b_ref: Dec<u64>,
    penalty_bps: Dec<u32>,
    expiry_fee_bps: Dec<u32>,
    rent_protocol_bps: Dec<u32>,
    k_s: Dec<u64>,
    split: [Dec<u32>; 3],
    expiry_keeper: (Dec<u64>, Dec<u32>, Dec<u64>),
    rent: Dec<u64>,
    penalty: Dec<u64>,
    expiry_fee: Dec<u64>,
    keeper: Dec<u64>,
    keeper_topup: Dec<u64>,
    protocol: Dec<u64>,
    lenders: Dec<u64>,
    lump: Dec<u64>,
    borrower: Dec<u64>,
    k_e: Dec<u64>,
}

/// A buyback exit of `attacks/cascade.py` with the bound it was checked against.
#[derive(Deserialize)]
struct CascadeExit {
    #[serde(flatten)]
    exit: Exit,
    delta_bps: Dec<u16>,
    ert: Dec<u64>,
    bound: Dec<u64>,
    max_in: Dec<u64>,
}

#[derive(Deserialize)]
struct Close {
    v: Dec<u64>,
    b: Dec<u64>,
    r_due: Dec<u64>,
    rent_protocol_bps: Dec<u32>,
    protocol: Dec<u64>,
    lenders: Dec<u64>,
    borrower: Dec<u64>,
}

/// `Market.fv_parts` at one evaluated `(P, L)` (a box corner or the EMA point).
#[derive(Deserialize)]
struct Fv {
    expired: bool,
    v: Dec<u64>,
    r_due: Dec<u64>,
    l_bps: Dec<u32>,
    penalty_bps: Dec<u32>,
    expiry_fee_bps: Dec<u32>,
    b_fv: Option<Dec<u64>>,
    pprime: Option<Dec<u32>>,
    fv: Option<Dec<u64>>,
}

#[derive(Deserialize)]
struct Settle {
    kind: String,
    v: Dec<u64>,
    k_s: Dec<u64>,
    q: Dec<u64>,
    p_ema: Dec<u128>,
    r_due: Dec<u64>,
    /// The venue executes: the immediate part prices the tokens at `P_ema`, else at 0
    /// (`sol_settle`; AIR-09).
    executable: bool,
    expired: bool,
    l_bps: Dec<u32>,
    penalty_bps: Dec<u32>,
    expiry_fee_bps: Dec<u32>,
    b_fv: Option<Dec<u64>>,
    pprime: Option<Dec<u32>>,
    fv: Option<Dec<u64>>,
    keeper: Dec<u64>,
    lenders: Dec<u64>,
    lump: Dec<u64>,
    borrower: Dec<u64>,
}

fn load() -> File {
    let f: File = common::load_vectors("risk", "exits.json");
    assert_eq!(f.generator, "gen_exits.py");
    f
}

fn opt<T: Copy>(v: &Option<Dec<T>>) -> Option<T> {
    v.as_ref().map(|d| d.0)
}

/// ARCH §8.1: the rows come from ECON §13's v0.4.1 exit fuzz (1,000 runs, 697 exits — 709 as
/// ECON §13 published it, before AIR-22's lower-median growth — 22 rent/penalty/fee truncations
/// by a sandwich), not a shorter run (review F-S6).
#[test]
fn rows_come_from_the_econ_13_exit_fuzz() {
    let s = load().exit_fuzz;
    assert_eq!(
        (s.runs.0, s.exits.0, s.penalty_truncated_by_sandwich.0),
        (1_000, 697, 22)
    );
}

/// `K_s = min(fixed + ⌊V·bps/10⁴⌋, cap, V)` and `K_e = min(fixed + ⌊B̂_ref·bps/10⁴⌋, cap)`
/// (before the waterfall's truncation), default and random locked terms.
#[test]
fn keeper_rewards_match_sim() {
    let f = load();
    assert!(f.keepers.len() >= 200);
    for (i, c) in f.keepers.iter().enumerate() {
        let st = SettleKeeperTerms {
            fixed: c.settle_terms.0.0,
            bps: c.settle_terms.1.0,
            cap: c.settle_terms.2.0,
        };
        let et = ExpiryKeeperTerms {
            fixed: c.expiry_terms.0.0,
            bps: c.expiry_terms.1.0,
            cap: c.expiry_terms.2.0,
        };
        assert_eq!(risk::k_s(c.v.0, &st), Ok(c.k_s.0), "keepers case {i}: K_s");
        assert_eq!(
            risk::k_e(c.b_ref.0, &et),
            Ok(c.k_e.0),
            "keepers case {i}: K_e"
        );
    }
}

fn check_totals(
    p: &Payout,
    keeper: u64,
    protocol: u64,
    lenders: u64,
    borrower: u64,
    v: u64,
    ctx: &str,
) {
    assert_eq!(
        (p.keeper, p.protocol, p.lenders, p.borrower),
        (keeper, protocol, lenders, borrower),
        "{ctx}: keeper, protocol, lenders, borrower"
    );
    assert_eq!(p.total(), Ok(v), "{ctx}: legs sum to V");
}

#[test]
fn buyback_exits_match_sim() {
    let f = load();
    let (mut liq, mut exp, mut truncated, mut topped, mut topup_cut) = (0, 0, 0, 0, 0);
    for (i, c) in f.exit.iter().enumerate() {
        let x = ExitInputs {
            v: c.v.0,
            b: c.b.0,
            r_due: c.r_due.0,
            b_live: c.b_live.0,
            b_ref: c.b_ref.0,
            rent_protocol_bps: c.rent_protocol_bps.0,
        };
        let ctx = format!("exit case {i} ({})", c.kind);
        assert_eq!(
            risk::k_s(c.v.0, &SettleKeeperTerms::DEFAULT),
            Ok(c.k_s.0),
            "{ctx}: K_s"
        );
        let p = match c.kind.as_str() {
            "liquidation" => {
                liq += 1;
                let split = PenaltySplit {
                    lender_bps: c.split[0].0,
                    protocol_bps: c.split[1].0,
                    keeper_bps: c.split[2].0,
                };
                let p = waterfall::liquidate(&x, c.penalty_bps.0, &split, c.k_s.0).unwrap();
                assert_eq!(p.penalty, c.penalty.0, "{ctx}: penalty");
                assert_eq!(p.keeper_topup, c.keeper_topup.0, "{ctx}: keeper top-up");
                assert_eq!(p.keeper, p.penalty_keeper + p.keeper_topup, "{ctx}");
                if p.keeper_topup > 0 {
                    topped += 1;
                    if p.keeper < c.k_s.0 {
                        topup_cut += 1;
                        assert_eq!(
                            p.borrower, 0,
                            "{ctx}: the top-up takes all the borrower has"
                        );
                    }
                }
                p
            }
            "expiry" => {
                exp += 1;
                let terms = ExpiryKeeperTerms {
                    fixed: c.expiry_keeper.0.0,
                    bps: c.expiry_keeper.1.0,
                    cap: c.expiry_keeper.2.0,
                };
                assert_eq!(risk::k_e(c.b_ref.0, &terms), Ok(c.k_e.0), "{ctx}: K_e");
                let p = waterfall::expire(&x, c.expiry_fee_bps.0, &terms).unwrap();
                assert_eq!(p.expiry_fee, c.expiry_fee.0, "{ctx}: expiry fee");
                p
            }
            k => panic!("unexpected kind {k}"),
        };
        if p.rent < c.r_due.0 {
            truncated += 1;
        }
        assert_eq!(p.buyback, c.b.0, "{ctx}");
        assert_eq!(p.rent, c.rent.0, "{ctx}: rent");
        assert_eq!(p.lump, c.lump.0, "{ctx}: vesting lump");
        check_totals(
            &p,
            c.keeper.0,
            c.protocol.0,
            c.lenders.0,
            c.borrower.0,
            c.v.0,
            &ctx,
        );
    }
    assert!(liq > 0 && exp > 0 && truncated > 0 && topped > 0 && topup_cut > 0);
}

/// ARCH §8.1 `cascade.py`: each basket liquidation's waterfall, its fixed-δ bound
/// `⌊B̂_ref·(10⁴ + δ)/10⁴⌋` and `max_in_liq` (no reserve), and `B ≤ max_in` (I-E1).
#[test]
fn cascade_exits_match_sim() {
    use shortwin_math::bounds;
    let f = load();
    assert!(f.exit_cascade.len() >= 20);
    for (i, c) in f.exit_cascade.iter().enumerate() {
        let (e, ctx) = (&c.exit, format!("cascade exit {i}"));
        let bound = u128::from(e.b_ref.0) * (10_000 + u128::from(c.delta_bps.0)) / 10_000;
        assert_eq!(u128::from(c.bound.0), bound, "{ctx}: bound");
        assert_eq!(e.kind, "liquidation", "{ctx}: the baskets liquidate");
        let max_in = bounds::max_in_liq(e.b_ref.0, c.delta_bps.0, e.v.0, e.k_s.0, c.ert.0);
        assert_eq!(max_in, Some(c.max_in.0), "{ctx}: max_in");
        assert!(e.b.0 <= c.max_in.0, "{ctx}: I-E1");
        let x = ExitInputs {
            v: e.v.0,
            b: e.b.0,
            r_due: e.r_due.0,
            b_live: e.b_live.0,
            b_ref: e.b_ref.0,
            rent_protocol_bps: e.rent_protocol_bps.0,
        };
        let split = PenaltySplit {
            lender_bps: e.split[0].0,
            protocol_bps: e.split[1].0,
            keeper_bps: e.split[2].0,
        };
        let p = waterfall::liquidate(&x, e.penalty_bps.0, &split, e.k_s.0).unwrap();
        assert_eq!(
            (p.buyback, p.rent, p.penalty, p.keeper_topup, p.lump),
            (e.b.0, e.rent.0, e.penalty.0, e.keeper_topup.0, e.lump.0),
            "{ctx}"
        );
        check_totals(
            &p,
            e.keeper.0,
            e.protocol.0,
            e.lenders.0,
            e.borrower.0,
            e.v.0,
            &ctx,
        );
    }
}

#[test]
fn closes_match_sim() {
    let f = load();
    assert!(f.close.len() > 50);
    for (i, c) in f.close.iter().enumerate() {
        let p = waterfall::close(c.v.0, c.b.0, c.r_due.0, c.rent_protocol_bps.0).unwrap();
        assert_eq!(p.lump, 0);
        check_totals(
            &p,
            0,
            c.protocol.0,
            c.lenders.0,
            c.borrower.0,
            c.v.0,
            &format!("close case {i}"),
        );
    }
}

/// FV's rate `p'` on the same `(P, L)` it is evaluated at (v0.4.1, R4A-3; O15): the
/// expiry fee iff expired and `CR(V_eff, B̂_FV) ≥ L` there, else the penalty.
fn pprime(expired: bool, v: u64, r_due: u64, b_fv: u64, l: u32, pen: u32, fee: u32) -> u32 {
    let ve = risk::v_eff(v, r_due);
    let penalty_applies = !expired || risk::cr_below(ve, Some(b_fv), l).unwrap();
    risk::fv_rate_bps(penalty_applies, pen, fee)
}

#[test]
fn fair_value_matches_sim() {
    let f = load();
    let (mut exp_fee, mut pen) = (0, 0);
    for (i, c) in f.fv.iter().enumerate() {
        let ctx = format!("fv case {i}");
        match opt(&c.b_fv) {
            None => {
                assert_eq!(c.fv.as_ref().map(|d| d.0), None, "{ctx}");
                assert_eq!(risk::fair_value(None, c.penalty_bps.0, c.r_due.0), None);
            }
            Some(b) => {
                let rate = pprime(
                    c.expired,
                    c.v.0,
                    c.r_due.0,
                    b,
                    c.l_bps.0,
                    c.penalty_bps.0,
                    c.expiry_fee_bps.0,
                );
                assert_eq!(Some(rate), opt(&c.pprime), "{ctx}: p'");
                if rate == c.penalty_bps.0 {
                    pen += 1;
                } else {
                    exp_fee += 1;
                }
                assert_eq!(
                    risk::fair_value(Some(b), rate, c.r_due.0),
                    opt(&c.fv),
                    "{ctx}: FV"
                );
            }
        }
    }
    assert!(exp_fee > 0 && pen > 0);
}

#[test]
fn settlements_match_sim() {
    let f = load();
    let (mut lumps, mut capped) = (0, 0);
    for (i, c) in f.settle.iter().enumerate() {
        let ctx = format!("settle case {i} ({})", c.kind);
        let kind = match c.kind.as_str() {
            "S1" => SettleKind::S1,
            "S2" => SettleKind::S2,
            "S3" => SettleKind::S3,
            k => panic!("unexpected kind {k}"),
        };
        assert_eq!(
            risk::k_s(c.v.0, &SettleKeeperTerms::DEFAULT),
            Ok(c.k_s.0),
            "{ctx}: K_s"
        );
        let fv = match (kind, opt(&c.b_fv)) {
            (SettleKind::S1, _) => None,
            (_, None) => None,
            (_, Some(b)) => {
                let rate = pprime(
                    c.expired,
                    c.v.0,
                    c.r_due.0,
                    b,
                    c.l_bps.0,
                    c.penalty_bps.0,
                    c.expiry_fee_bps.0,
                );
                assert_eq!(Some(rate), opt(&c.pprime), "{ctx}: p'");
                risk::fair_value(Some(b), rate, c.r_due.0)
            }
        };
        assert_eq!(fv, opt(&c.fv), "{ctx}: FV at (P_ema, L_ema)");
        let p_imm = if c.executable { c.p_ema.0 } else { 0 };
        let p = waterfall::settle(c.v.0, kind, c.k_s.0, fv, c.q.0, p_imm, c.r_due.0).unwrap();
        check_totals(&p, c.keeper.0, 0, c.lenders.0, c.borrower.0, c.v.0, &ctx);
        assert_eq!(p.lump, c.lump.0, "{ctx}: vesting lump");
        // I-E5: S1 takes V − K_s; S2/S3 take min(V − K_s, FV).
        let take = c.v.0 - c.k_s.0;
        match (kind, fv) {
            (SettleKind::S2 | SettleKind::S3, Some(fv)) => {
                assert_eq!(p.lenders, take.min(fv), "{ctx}");
                capped += usize::from(fv < take);
            }
            _ => assert_eq!(p.lenders, take, "{ctx}"),
        }
        lumps += usize::from(p.lump > 0);
        assert_eq!(p.buyback + p.protocol, 0, "{ctx}");
    }
    assert!(lumps > 0 && capped > 0);
}

fn exit_inputs() -> impl Strategy<Value = ExitInputs> {
    (
        0u64..=u64::MAX / 4,
        0u64..=10_000,
        0u64..=u64::MAX / 4,
        1u64..=u64::MAX / 4,
        1u64..=u64::MAX / 4,
        0u32..=3_000,
    )
        .prop_flat_map(|(v, b_frac, r, bl, br, rp)| {
            let b = u64::try_from(u128::from(v) * u128::from(b_frac) / 10_000).unwrap();
            (
                Just(v),
                Just(b),
                prop_oneof![Just(r), 0..=v],
                Just(bl),
                Just(br),
                Just(rp),
            )
        })
        .prop_map(
            |(v, b, r_due, b_live, b_ref, rent_protocol_bps)| ExitInputs {
                v,
                b,
                r_due,
                b_live,
                b_ref,
                rent_protocol_bps,
            },
        )
}

fn split() -> impl Strategy<Value = PenaltySplit> {
    prop_oneof![
        Just(PenaltySplit::DEFAULT),
        (0u32..=5_000, 0u32..=5_000).prop_map(|(k, p)| PenaltySplit {
            lender_bps: 10_000 - k - p,
            protocol_bps: p,
            keeper_bps: k,
        }),
    ]
}

/// Checks the invariants shared by every waterfall.
fn conserved(p: &Payout, v: u64) -> Result<(), TestCaseError> {
    prop_assert_eq!(p.total(), Ok(v));
    prop_assert_eq!(p.rent, p.rent_protocol + p.rent_lenders);
    prop_assert_eq!(
        p.penalty,
        p.penalty_keeper + p.penalty_protocol + p.penalty_lenders
    );
    prop_assert_eq!(p.protocol, p.rent_protocol + p.penalty_protocol);
    prop_assert!(p.lump <= p.lenders);
    Ok(())
}

proptest! {
    #![proptest_config(common::proptest_config())]

    /// ECON §8.3, §8.5, I-E4, I-E10: liquidation legs sum to V; rent is senior to the
    /// penalty; the penalty is ⌈p·min(B̂_live, B̂_ref)⌉ unless the vault runs out; splits
    /// are exact with lenders taking the rounding remainder; the keeper floor tops the
    /// keeper up to K_s from the borrower's remainder, and no further.
    #[test]
    fn liquidation_conserves(x in exit_inputs(), pen in 0u32..=2_000, sp in split(),
                             ks in prop_oneof![Just(0u64), 0u64..=250_000_000, any::<u64>()]) {
        let p = waterfall::liquidate(&x, pen, &sp, ks).unwrap();
        conserved(&p, x.v)?;
        let w = x.v - x.b;
        prop_assert_eq!(p.rent, x.r_due.min(w));
        let full = (u128::from(x.b_live.min(x.b_ref)) * u128::from(pen)).div_ceil(10_000);
        prop_assert_eq!(u128::from(p.penalty), full.min(u128::from(w - p.rent)));
        if p.penalty > 0 {
            prop_assert_eq!(p.rent, x.r_due);
        }
        if p.borrower > 0 {
            prop_assert_eq!(u128::from(p.penalty), full);
            prop_assert!(p.keeper >= ks);                                  // I-E10
        }
        prop_assert_eq!(p.rent_protocol, u64::try_from(u128::from(p.rent) * u128::from(x.rent_protocol_bps) / 10_000).unwrap());
        let cut = |bps: u32| u64::try_from(u128::from(p.penalty) * u128::from(bps) / 10_000).unwrap();
        prop_assert_eq!(p.penalty_keeper, cut(sp.keeper_bps));
        prop_assert_eq!(p.penalty_protocol, cut(sp.protocol_bps));
        prop_assert!(p.penalty_lenders >= cut(sp.lender_bps));
        prop_assert!(p.penalty_lenders <= cut(sp.lender_bps) + 2);
        // Keeper floor: kp + min(max(0, K_s − kp), W); the borrower pays it.
        let rest = w - p.rent - p.penalty;
        prop_assert_eq!(p.keeper_topup, ks.saturating_sub(p.penalty_keeper).min(rest));
        prop_assert_eq!(p.keeper, p.penalty_keeper + p.keeper_topup);
        prop_assert!(p.keeper <= ks.max(p.penalty_keeper));
        prop_assert_eq!(p.borrower, rest - p.keeper_topup);
        prop_assert_eq!(p.lenders, p.rent_lenders + p.penalty_lenders);
        prop_assert_eq!(p.lump, p.penalty_lenders);
        // The floor moves lamports from the borrower to the keeper only.
        let plain = waterfall::liquidate(&x, pen, &sp, 0).unwrap();
        prop_assert_eq!((plain.protocol, plain.lenders, plain.lump), (p.protocol, p.lenders, p.lump));
        prop_assert_eq!(plain.borrower - p.borrower, p.keeper_topup);
    }

    /// SPEC §6.3 defaults: penalty 50/20/30 and rent 85/15, exactly, lenders rounding up.
    #[test]
    fn default_splits_exact(x in exit_inputs(), pen in 0u32..=2_000) {
        let p = waterfall::liquidate(&x, pen, &PenaltySplit::DEFAULT, 0).unwrap();
        prop_assert_eq!(p.penalty_keeper, p.penalty * 3 / 10);
        prop_assert_eq!(p.penalty_protocol, p.penalty / 5);
        prop_assert_eq!(p.penalty_lenders, p.penalty - p.penalty * 3 / 10 - p.penalty / 5);
        prop_assert!(p.penalty_lenders >= p.penalty.div_ceil(2));
        let c = waterfall::close(u64::MAX, 0, x.r_due, 1_500).unwrap();
        prop_assert_eq!(c.rent_protocol, u64::try_from(u128::from(x.r_due) * 1_500 / 10_000).unwrap());
        prop_assert_eq!(u128::from(c.rent_lenders), (u128::from(x.r_due) * 8_500).div_ceil(10_000));
    }

    /// ECON §8.4: expiry pays rent, then the fee (100 % lenders, vests), then the keeper
    /// from the borrower's rest; legs sum to V.
    #[test]
    fn expiry_conserves(x in exit_inputs(), fee in 0u32..=500, fixed in 0u64..=10_000_000,
                        kb in 0u32..=100, cap in 0u64..=250_000_000) {
        let terms = ExpiryKeeperTerms { fixed, bps: kb, cap };
        let p = waterfall::expire(&x, fee, &terms).unwrap();
        conserved(&p, x.v)?;
        let w = x.v - x.b - p.rent;
        prop_assert_eq!(p.rent, x.r_due.min(x.v - x.b));
        let full = (u128::from(x.b_live.min(x.b_ref)) * u128::from(fee)).div_ceil(10_000);
        prop_assert_eq!(u128::from(p.expiry_fee), full.min(u128::from(w)));
        prop_assert_eq!(p.keeper, risk::k_e(x.b_ref, &terms).unwrap().min(w - p.expiry_fee));
        prop_assert_eq!(p.lenders, p.rent_lenders + p.expiry_fee);
        prop_assert_eq!(p.lump, p.expiry_fee);
        prop_assert_eq!((p.penalty, p.keeper_topup), (0, 0));
        if p.keeper > 0 {
            prop_assert_eq!(u128::from(p.expiry_fee), full);
        }
    }

    /// ECON §8.1: close pays B, then all of R, then the borrower; it refuses a vault that
    /// cannot cover both. Nothing vests.
    #[test]
    fn close_conserves(v in any::<u64>(), b in any::<u64>(), r in any::<u64>(), rp in 0u32..=3_000) {
        match waterfall::close(v, b, r, rp) {
            Ok(p) => {
                conserved(&p, v)?;
                prop_assert_eq!(p.rent, r);
                prop_assert_eq!((p.keeper, p.lump), (0, 0));
            }
            Err(e) => {
                prop_assert_eq!(e, shortwin_math::MathError::Underflow);
                prop_assert!(u128::from(b) + u128::from(r) > u128::from(v));
            }
        }
    }

    /// ECON §8.6, §9.4, I-E5: settlement legs sum to V; S1 is never capped; S2/S3 take
    /// x = min(V − K_s, FV); on every kind x − min(x, ⌊Q·P_ema/WAD⌋ + R_due) vests (S1 since
    /// AIR-21); the protocol gets nothing.
    #[test]
    fn settlement_conserves(v in any::<u64>(), fv in prop::option::of(any::<u64>()), k in 0u8..3,
                            fixed in 0u64..=50_000_000, bps in 0u32..=500, cap in 0u64..=1_000_000_000,
                            q in any::<u64>(), p_ema in prop_oneof![0u128..=10u128.pow(24), any::<u128>()],
                            r_due in any::<u64>()) {
        let ks = risk::k_s(v, &SettleKeeperTerms { fixed, bps, cap }).unwrap();
        prop_assert!(ks <= v && ks <= cap);
        let kind = [SettleKind::S1, SettleKind::S2, SettleKind::S3][usize::from(k)];
        let p = waterfall::settle(v, kind, ks, fv, q, p_ema, r_due).unwrap();
        conserved(&p, v)?;
        prop_assert_eq!(p.keeper, ks);
        prop_assert_eq!(p.protocol, 0);
        let take = v - ks;
        match (kind, fv) {
            (SettleKind::S1, _) | (_, None) => prop_assert_eq!(p.lenders, take),
            (_, Some(fv)) => prop_assert_eq!(p.lenders, take.min(fv)),
        }
        let value = num_bigint::BigUint::from(q) * p_ema / WAD + r_due;
        let x = num_bigint::BigUint::from(p.lenders);
        let immediate = if value < x { value } else { x.clone() };
        prop_assert_eq!(num_bigint::BigUint::from(p.lump), x - immediate);
    }
}
