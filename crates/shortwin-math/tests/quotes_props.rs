//! Properties of the quotes, fees and pool selection (ARCHITECTURE §8.1: "`B̂` monotone
//! in q for every venue family", "round trips never create value"; ECON §2 rounding).
//!
//! Each quote is compared with an independent num-bigint implementation of the venue
//! formula (R-PUMP §4, R-VEN §2.4, R-RAY §3.3) over the full `u64` input range and wide
//! `u128` reserves: equal results, and `None` exactly when the reference is undefined.
//! Further properties: rounding never favours the trader (against the exact rational
//! value), monotonicity in `q`, a buy costs at least what a sale of the same size pays,
//! capped (risk) fees never quote above live ones (one combined fee at the cap on pump
//! venues, ECON §3.1), fee gates compare ppm exactly (ECON §2.1), `reprice` rounds so
//! that `Y/X ≥ P` (R3B-7 c), and the locked depth floors `λ` to bps first (ECON §2.1).
//! At least 2,000 cases each.

mod common;

use num_bigint::BigUint;
use proptest::prelude::*;
use shortwin_math::fees::{self, CpmmFees, FeeBps, FeeTier, PammFeeSchedule, RayV4Fee};
use shortwin_math::num::{Rounding, sqrt_u128};
use shortwin_math::pool_select;
use shortwin_math::quote::{
    self, BuyQuote, CurveOffsets, CurveState, MigrationParams, SellQuote, cpmm, ray_v4, sol_leg,
};

type B = BigUint;

fn b<T: Into<B>>(x: T) -> B {
    x.into()
}

fn zero() -> B {
    B::ZERO
}

fn floor(n: &B, d: &B) -> Option<B> {
    (d != &zero()).then(|| n / d)
}

fn ceil(n: &B, d: &B) -> Option<B> {
    (d != &zero()).then(|| (n + d - 1u8) / d)
}

fn u64_of(x: &B) -> Option<u64> {
    u64::try_from(x).ok()
}

fn u128_of(x: &B) -> Option<u128> {
    u128::try_from(x).ok()
}

// ------------------------------------------------------------------ strategies

fn amount() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => any::<u64>(),
        2 => 0u64..1_000,
        1 => Just(0u64),
        1 => Just(1u64),
        1 => Just(u64::MAX),
        3 => 1_000u64..1_000_000_000_000_000,
    ]
}

fn reserve() -> impl Strategy<Value = u128> {
    prop_oneof![
        4 => 1u128..10_000_000_000_000_000_000,
        2 => any::<u64>().prop_map(u128::from),
        1 => any::<u128>(),
        1 => Just(0u128),
        1 => 0u128..100,
    ]
}

fn bps() -> impl Strategy<Value = u64> {
    prop_oneof![6 => 0u64..=10_000, 1 => Just(0u64), 1 => any::<u64>()]
}

fn fee_bps() -> impl Strategy<Value = FeeBps> {
    (bps(), bps(), bps()).prop_map(|(a, b, c)| FeeBps::new(a, b, c))
}

fn sane_fee_bps() -> impl Strategy<Value = FeeBps> {
    (0u64..=300, 0u64..=300, 0u64..=300).prop_map(|(a, b, c)| FeeBps::new(a, b, c))
}

fn ppm() -> impl Strategy<Value = u64> {
    prop_oneof![6 => 0u64..60_000, 1 => 0u64..=1_000_000, 1 => any::<u64>()]
}

fn cpmm_fees() -> impl Strategy<Value = CpmmFees> {
    (
        ppm(),
        ppm(),
        any::<bool>(),
        0u64..=1_000_000,
        0u64..=1_000_000,
    )
        .prop_map(
            |(trade, creator, creator_on_input, protocol, fund)| CpmmFees {
                trade,
                creator,
                creator_on_input,
                protocol,
                fund,
            },
        )
}

fn sane_cpmm_fees() -> impl Strategy<Value = CpmmFees> {
    (0u64..50_000, 0u64..20_000, any::<bool>()).prop_map(|(trade, creator, creator_on_input)| {
        CpmmFees {
            trade,
            creator,
            creator_on_input,
            protocol: 120_000,
            fund: 40_000,
        }
    })
}

/// A realistic pool: reserves in the u64 range, both non-trivial.
fn pool() -> impl Strategy<Value = (u128, u128)> {
    (
        1_000u128..10_000_000_000_000_000_000,
        1_000u128..10_000_000_000_000_000_000,
    )
}

// ------------------------------------------------------------------ references

/// ECON §3.1 / the sims' `Venue._risk_fees`: above the cap (ppm vs `cap·100`), one combined
/// fee at the cap.
fn capped_ref(f: FeeBps, cap: u64) -> FeeBps {
    if (b(f.lp) + f.protocol + f.creator) * 100u32 > b(cap) * 100u32 {
        FeeBps::new(0, cap, 0)
    } else {
        f
    }
}

fn ref_fee(x: &B, rate: u64, den: u64) -> B {
    ceil(&(x * rate), &b(den)).unwrap_or_else(zero)
}

/// ECON §3.1 exact-out quote `n = ⌈q·y/(x − q)⌉ + ε₁` (R-VEN §2.4's charge for ε₁ = 0),
/// or with `charge` the pump curve's own `⌊q·y/(x − q)⌋ + 1` (R-PUMP §4).
fn ref_sol_buy_n(
    q: u64,
    x: u128,
    y: u128,
    eps1: bool,
    charge: bool,
    f: &FeeBps,
) -> Option<BuyQuote> {
    if q == 0 || u128::from(q) >= x || y == 0 {
        return None;
    }
    let rest = b(x) - q;
    let num = b(q) * y;
    let n = if charge {
        floor(&num, &rest)? + 1u8
    } else {
        ceil(&num, &rest)? + u8::from(eps1)
    };
    u64_of(&n)?;
    let lp = ref_fee(&n, f.lp, 10_000);
    let cost = &n + &lp + ref_fee(&n, f.protocol, 10_000) + ref_fee(&n, f.creator, 10_000);
    Some(BuyQuote {
        cost: u64_of(&cost)?,
        net: u64_of(&n)?,
        token_after: u128_of(&rest)?,
        sol_after: u128_of(&(b(y) + &n + &lp))?,
    })
}

fn ref_sol_buy(q: u64, x: u128, y: u128, eps1: bool, f: &FeeBps) -> Option<BuyQuote> {
    ref_sol_buy_n(q, x, y, eps1, false, f)
}

fn ref_sol_sell(q: u64, x: u128, y: u128, f: &FeeBps, cap: u64) -> Option<SellQuote> {
    if q == 0 || x == 0 || y == 0 {
        return None;
    }
    let tok = b(x) + q;
    let g = floor(&(b(q) * y), &tok)?;
    u64_of(&g)?;
    let lp = ref_fee(&g, f.lp, 10_000);
    if lp > g || b(cap) < &g - &lp {
        return None;
    }
    let fees = &lp + ref_fee(&g, f.protocol, 10_000) + ref_fee(&g, f.creator, 10_000);
    if fees > g {
        return None;
    }
    Some(SellQuote {
        proceeds: u64_of(&(&g - &fees))?,
        token_after: u128_of(&tok)?,
        sol_after: u128_of(&(b(y) - (&g - &lp)))?,
    })
}

/// R-RAY §3.3 AMM v4.
fn ref_v4_buy(q: u64, x: u128, y: u128, num: u64, den: u64) -> Option<BuyQuote> {
    if q == 0 || u128::from(q) >= y || num >= den {
        return None;
    }
    let pre = ceil(&(b(x) * q), &(b(y) - q))?;
    let cost = ceil(&(&pre * den), &b(den - num))?;
    if cost == zero() {
        return None;
    }
    Some(BuyQuote {
        cost: u64_of(&cost)?,
        net: u64_of(&pre)?,
        token_after: y - u128::from(q),
        sol_after: u128_of(&(b(x) + &cost))?,
    })
}

fn ref_v4_sell(a: u64, x: u128, y: u128, num: u64, den: u64) -> Option<SellQuote> {
    if a == 0 || num >= den {
        return None;
    }
    let net = b(a) - ref_fee(&b(a), num, den);
    let out = floor(&(b(y) * &net), &(b(x) + &net))?;
    if out == zero() || out >= b(y) {
        return None;
    }
    Some(SellQuote {
        proceeds: u64_of(&out)?,
        token_after: u128_of(&(b(x) + a))?,
        sol_after: u128_of(&(b(y) - &out))?,
    })
}

/// R-RAY §3.3 CPMM, from `curve/calculator.rs` and `curve/fees.rs`.
fn ref_pre(a: &B, r: &B) -> Option<B> {
    let d = b(1_000_000u32);
    if r == &zero() {
        return Some(a.clone());
    }
    if r >= &d {
        return None;
    }
    ceil(&(a * &d), &(&d - r))
}

fn ref_carved(trade: &B, f: &CpmmFees) -> B {
    trade * f.protocol / 1_000_000u32 + trade * f.fund / 1_000_000u32
}

fn ref_shares_ok(f: &CpmmFees) -> bool {
    b(f.protocol) + f.fund <= b(1_000_000u32)
}

fn ref_cp_buy(q: u64, x: u128, y: u128, f: &CpmmFees) -> Option<BuyQuote> {
    if q == 0 || x == 0 || y == 0 || !ref_shares_ok(f) {
        return None;
    }
    let (t, c) = (b(f.trade), b(f.creator));
    let actual = if f.creator_on_input {
        b(q)
    } else {
        ref_pre(&b(q), &c)?
    };
    if actual >= b(y) {
        return None;
    }
    let sw = ceil(&(b(x) * &actual), &(b(y) - &actual))?;
    let (cost, trade, creator_in) = if f.creator_on_input {
        let cost = ref_pre(&sw, &(&t + &c))?;
        let total = &cost - &sw;
        let creator = floor(&(&total * &c), &(&t + &c))?;
        (cost, &total - &creator, creator)
    } else {
        let cost = ref_pre(&sw, &t)?;
        let trade = &cost - &sw;
        (cost, trade, zero())
    };
    u64_of(&cost)?;
    // ECON §3.1's fee-free n(q) (σ's input): sw unless the creator fee grossed q up.
    let n = ceil(&(b(x) * q), &(b(y) - q))?;
    Some(BuyQuote {
        cost: u64_of(&cost)?,
        net: u64_of(&n)?,
        token_after: u128_of(&(b(y) - &actual))?,
        sol_after: u128_of(&(b(x) + &cost - ref_carved(&trade, f) - creator_in))?,
    })
}

fn ref_cp_sell(a: u64, x: u128, y: u128, f: &CpmmFees) -> Option<SellQuote> {
    if a == 0 || x == 0 || y == 0 || !ref_shares_ok(f) {
        return None;
    }
    let (t, c) = (b(f.trade), b(f.creator));
    let d = b(1_000_000u32);
    let (net, trade, creator_in) = if f.creator_on_input {
        let total = ceil(&(b(a) * (&t + &c)), &d)?;
        let creator = floor(&(&total * &c), &(&t + &c))?;
        if total > b(a) {
            return None;
        }
        (b(a) - &total, &total - &creator, creator)
    } else {
        let trade = ceil(&(b(a) * &t), &d)?;
        if trade > b(a) {
            return None;
        }
        (b(a) - &trade, trade, zero())
    };
    let sw = floor(&(b(y) * &net), &(b(x) + &net))?;
    let out = if f.creator_on_input {
        sw.clone()
    } else {
        let cr = ceil(&(&sw * &c), &d)?;
        if cr > sw {
            return None;
        }
        &sw - cr
    };
    if out == zero() {
        return None;
    }
    Some(SellQuote {
        proceeds: u64_of(&out)?,
        token_after: u128_of(&(b(x) + a - ref_carved(&trade, f) - creator_in))?,
        sol_after: u128_of(&(b(y) - &sw))?,
    })
}

/// ECON §3.1's post-migration pool (pump-final §5.1): `Y_p = rQ + (⌈rT·vQ/(vT − rT)⌉ + 1) −
/// fee + pcQ` (`n` 0 when `rT = 0`); `None` exactly where `quote::migrated_pool` is.
fn ref_migrated_pool(s: &CurveState, m: &MigrationParams) -> Option<(u128, u128)> {
    let fee = m.migration_fee;
    let y = if s.rt == 0 {
        (b(s.rq) >= b(fee)).then(|| b(s.rq) - fee)
    } else if s.rt >= s.vt {
        None
    } else {
        let n = ceil(&(b(s.rt) * s.vq), &(b(s.vt) - s.rt)).expect("vt > rt") + 1u8;
        (u64_of(&n).is_some() && b(s.rq) + &n >= b(fee)).then(|| b(s.rq) + n - fee)
    }?;
    u128_of(&y)?;
    Some((m.pool_tokens, u128_of(&(y + m.post_complete_quote_in))?))
}

/// pump-final §5.2's `B̂_v3`: up to `rT` the curve at `f`; above it `rT` on the curve plus
/// `q − rT` on [`ref_migrated_pool`]'s would-be pool, both at `fs`, `ε₁ = 1` on both legs.
fn ref_synthetic(
    q: u64,
    s: &CurveState,
    f: &FeeBps,
    fs: &FeeBps,
    m: &MigrationParams,
) -> Option<BuyQuote> {
    if q == 0 || s.complete {
        return None;
    }
    if u128::from(q) <= s.rt {
        return ref_sol_buy(q, s.vt, s.vq, true, f);
    }
    let (c1, n1) = if s.rt == 0 {
        (0, 0)
    } else {
        let l = ref_sol_buy(u64::try_from(s.rt).ok()?, s.vt, s.vq, true, fs)?;
        (l.cost, l.net)
    };
    let (xs, ys) = ref_migrated_pool(s, m)?;
    let l2 = ref_sol_buy(u64::try_from(u128::from(q) - s.rt).ok()?, xs, ys, true, fs)?;
    Some(BuyQuote {
        cost: u64_of(&(b(l2.cost) + c1))?,
        net: u64_of(&(b(l2.net) + n1))?,
        ..l2
    })
}

/// pump's charge for `buy_v3(rT + q₂)` by synthetic migration (pump-final §5.2; T-PUMP-MATH-V3
/// (b)): the curve part `n₁ᵖ = ⌈rT·vQ/(vT − rT)⌉` (`v2`: `⌊·⌋ + 1`, what pump charged on the
/// exact divisions G0 measured), the pool part `n₂ᵖ = ⌈Y_sᵖ·q₂/(X_s − q₂)⌉` on
/// `Y_sᵖ = rQ + n₁ᵖ − fee`, each fee component `⌈n·b/10⁴⌉` per part. `None` without such a trade.
fn ref_pump_charge(
    s: &CurveState,
    q2: u64,
    x_s: u128,
    fee_m: u64,
    f: &FeeBps,
    v2: bool,
) -> Option<B> {
    if s.rt == 0 || s.rt >= s.vt || q2 == 0 || u128::from(q2) >= x_s {
        return None;
    }
    let rest = b(s.vt) - s.rt;
    let num = b(s.rt) * s.vq;
    let n1 = if v2 {
        floor(&num, &rest)? + 1u8
    } else {
        ceil(&num, &rest)?
    };
    let ysp = b(s.rq) + &n1;
    if ysp < b(fee_m) {
        return None;
    }
    let ysp = ysp - fee_m;
    let n2 = ceil(&(&ysp * q2), &(b(x_s) - q2))?;
    let part = |n: &B| {
        n + ref_fee(n, f.lp, 10_000)
            + ref_fee(n, f.protocol, 10_000)
            + ref_fee(n, f.creator, 10_000)
    };
    Some(part(&n1) + part(&n2))
}

/// pump-final §5.2's general bound on `B̂_v3 − charge`: `(1 + Σᵢ⌈bᵢ/10⁴⌉) + (Δ₂ +
/// Σᵢ⌈Δ₂·bᵢ/10⁴⌉)` with `Δ₂ = 1 + ⌈q₂/(X_s − q₂)⌉` over the non-zero fee components.
fn v3_bound(q2: u64, x_s: u128, f: &FeeBps) -> B {
    let d2 = ceil(&b(q2), &(b(x_s) - q2)).expect("q₂ < X_s") + 1u8;
    let mut out = b(1u8) + &d2;
    for c in [f.lp, f.protocol, f.creator].into_iter().filter(|&c| c > 0) {
        out += ref_fee(&b(1u8), c, 10_000) + ref_fee(&d2, c, 10_000);
    }
    out
}

/// A live curve for the synthetic-exit properties: the standard offsets (`vT − rT` 279.9e12,
/// `vQ − rQ` 30e9, on the standard curve `vT·vQ ≈ 1.073e15·30e9`) or random ones; with `exact`,
/// `rT·vQ` is made a multiple of `vT − rT` (pump-final T-MATH-3: exact divisions on purpose).
fn synthetic_curve(
    standard: bool,
    off_t: u128,
    off_q: u128,
    rt: u128,
    rq: u128,
    exact: bool,
) -> CurveState {
    let (off_t, off_q) = if standard {
        (279_900_000_000_000, 30_000_000_000)
    } else {
        (off_t, off_q)
    };
    let vt = off_t + rt;
    let mut vq = if standard {
        (1_073_000_000_000_000u128 * 30_000_000_000).div_ceil(vt)
    } else {
        off_q + rq
    };
    let mut rt = rt;
    if exact {
        // rT·vQ/(vT − rT) exact: rT = k·d/g and vQ = j·g with g | d, or (any d) vQ a multiple of
        // d / gcd(rT, d).
        let d = off_t;
        if d % 2_048 == 0 {
            let unit = d / 2_048;
            rt = (rt / unit).max(1) * unit;
            vq = vq.div_ceil(2_048) * 2_048;
        } else {
            let mut g = (rt, d);
            while g.1 != 0 {
                g = (g.1, g.0 % g.1);
            }
            let step = d / g.0;
            vq = vq.div_ceil(step) * step;
        }
    }
    let vt = off_t + rt;
    let vq = vq.max(off_q + 1);
    CurveState {
        vt,
        vq,
        rt,
        rq: vq - off_q,
        complete: false,
    }
}

// ------------------------------------------------------------------ differential

proptest! {
    #![proptest_config(common::proptest_config())]

    #[test]
    fn sol_leg_equals_reference(q in amount(), x in reserve(), y in reserve(), eps1: bool,
                                f in fee_bps(), cap in amount()) {
        prop_assert_eq!(sol_leg::buy_exact_out(q, x, y, eps1, &f), ref_sol_buy(q, x, y, eps1, &f));
        let charge = sol_leg::curve_charge_exact_out(q, x, y, &f);
        prop_assert_eq!(charge, ref_sol_buy_n(q, x, y, true, true, &f));
        prop_assert_eq!(sol_leg::sell_exact_in(q, x, y, &f, cap), ref_sol_sell(q, x, y, &f, cap));
        // ECON §3.1: the curve quote is never below the charge, and at most 1 + #fees above
        // (one more lamport of n moves a fee of b bps by at most ⌈b/10⁴⌉).
        if let (Some(c), Some(qt)) = (charge, sol_leg::buy_exact_out(q, x, y, true, &f)) {
            let k: u64 = [f.lp, f.protocol, f.creator].iter().map(|r| r.div_ceil(10_000)).sum();
            prop_assert!(c.cost <= qt.cost && qt.cost - c.cost <= 1 + k);
            prop_assert!(qt.net - c.net <= 1);
        }
    }

    #[test]
    fn ray_v4_equals_reference(q in amount(), x in reserve(), y in reserve(),
                               num in prop_oneof![0u64..100, any::<u64>()],
                               den in prop_oneof![Just(10_000u64), 1u64..1_000_000, any::<u64>()]) {
        prop_assert_eq!(ray_v4::buy_exact_out(q, x, y, num, den), ref_v4_buy(q, x, y, num, den));
        prop_assert_eq!(ray_v4::sell_exact_in(q, x, y, num, den), ref_v4_sell(q, x, y, num, den));
    }

    #[test]
    fn cpmm_equals_reference(q in amount(), x in reserve(), y in reserve(), f in cpmm_fees()) {
        prop_assert_eq!(cpmm::buy_exact_out(q, x, y, &f), ref_cp_buy(q, x, y, &f));
        prop_assert_eq!(cpmm::sell_exact_in(q, x, y, &f), ref_cp_sell(q, x, y, &f));
    }

    // -------------------------------------------------------------- rounding direction

    /// A buy never costs less than the exact rational price plus exact fees, and a sale
    /// never pays more (ECON §2: quotes round against the trader).
    #[test]
    fn sol_leg_never_favours_the_trader((x, y) in pool(), q in amount(), eps1: bool,
                                        f in sane_fee_bps()) {
        let phi = f.oneway_bps();
        if let Some(bq) = sol_leg::buy_exact_out(q, x, y, eps1, &f) {
            // cost ≥ q·y/(x − q)·(1 + φ/10⁴)
            prop_assert!(b(bq.cost) * (b(x) - q) * 10_000u32 >= b(q) * y * (10_000 + phi));
        }
        if let Some(s) = sol_leg::sell_exact_in(q, x, y, &f, u64::MAX) {
            prop_assert!(b(s.proceeds) * (b(x) + q) * 10_000u32 <= b(q) * y * (10_000 - phi));
        }
    }

    #[test]
    fn raydium_never_favours_the_trader((x, y) in pool(), q in amount(), num in 0u64..500,
                                        f in sane_cpmm_fees()) {
        let den = 10_000u64;
        if let Some(bq) = ray_v4::buy_exact_out(q, x, y, num, den) {
            prop_assert!(b(bq.cost) * (b(y) - q) * (den - num) >= b(x) * q * den);
        }
        if let Some(s) = ray_v4::sell_exact_in(q, x, y, num, den) {
            // S ≤ y·n/(x + n) at the exact net n = a·(den − num)/den.
            let n_den = b(q) * (den - num);
            prop_assert!(b(s.proceeds) * (b(x) * den + &n_den) <= b(y) * &n_den);
        }
        let tc = f.trade + f.creator;
        let d = 1_000_000u64;
        if let Some(bq) = cpmm::buy_exact_out(q, x, y, &f) {
            if f.creator_on_input {
                // cost ≥ x·q/(y − q) / (1 − (t + c)/D)
                prop_assert!(b(bq.cost) * (b(y) - q) * (d - tc) >= b(x) * q * d);
            } else {
                // The pool pays out q' = q·D/(D − c): cost ≥ x·q'/(y − q') / (1 − t/D).
                let room = b(y) * (d - f.creator) - b(q) * d;
                prop_assert!(b(bq.cost) * room * (d - f.trade) >= b(x) * q * d * d);
            }
        }
        if let Some(s) = cpmm::sell_exact_in(q, x, y, &f) {
            let n_den = b(q) * (d - if f.creator_on_input { tc } else { f.trade });
            let keep = if f.creator_on_input { d } else { d - f.creator };
            prop_assert!(b(s.proceeds) * (b(x) * d + &n_den) * d <= b(y) * &n_den * keep);
        }
    }

    // -------------------------------------------------------------- monotonicity, spread

    /// `B̂` is monotone in q for every family (ARCH §8.1); a sale's proceeds are monotone
    /// too, except on pump venues where each of the k non-zero fee components is rounded
    /// up separately: then S may dip by at most k − 1 lamports when q grows.
    #[test]
    fn monotone_in_q((x, y) in pool(), q1 in amount(), q2 in amount(), eps1: bool,
                     f in sane_fee_bps(), num in 0u64..500, cf in sane_cpmm_fees()) {
        let (lo, hi) = (q1.min(q2), q1.max(q2));
        let k = [f.lp, f.protocol, f.creator].iter().filter(|&&r| r > 0).count() as u64;
        let pairs_b = [
            (sol_leg::buy_exact_out(lo, x, y, eps1, &f), sol_leg::buy_exact_out(hi, x, y, eps1, &f)),
            (ray_v4::buy_exact_out(lo, x, y, num, 10_000), ray_v4::buy_exact_out(hi, x, y, num, 10_000)),
            (cpmm::buy_exact_out(lo, x, y, &cf), cpmm::buy_exact_out(hi, x, y, &cf)),
        ];
        for (a, c) in pairs_b {
            if let Some(c) = c {
                // Defined at hi implies defined at lo (> 0), and no cheaper.
                if lo > 0 {
                    let a = a.expect("defined at the larger size");
                    prop_assert!(a.cost <= c.cost && a.net <= c.net);
                }
            }
        }
        if let (Some(a), Some(c)) = (sol_leg::sell_exact_in(lo, x, y, &f, u64::MAX),
                                     sol_leg::sell_exact_in(hi, x, y, &f, u64::MAX)) {
            prop_assert!(a.proceeds <= c.proceeds + k.saturating_sub(1));
        }
        for (a, c) in [
            (ray_v4::sell_exact_in(lo, x, y, num, 10_000), ray_v4::sell_exact_in(hi, x, y, num, 10_000)),
            (cpmm::sell_exact_in(lo, x, y, &cf), cpmm::sell_exact_in(hi, x, y, &cf)),
        ] {
            if let (Some(a), Some(c)) = (a, c) {
                prop_assert!(a.proceeds <= c.proceeds);
            }
        }
    }

    /// Buying q costs at least what selling q pays on the same state, and a sell followed
    /// by the buyback of the same q never returns more SOL than it cost (round trips
    /// never create value, ARCH §8.1).
    #[test]
    fn buy_covers_sell_and_round_trips((x, y) in pool(), q in amount(), eps1: bool,
                                       f in sane_fee_bps(), num in 0u64..500,
                                       cf in sane_cpmm_fees()) {
        let s = sol_leg::sell_exact_in(q, x, y, &f, u64::MAX);
        if let (Some(bq), Some(s)) = (sol_leg::buy_exact_out(q, x, y, eps1, &f), s) {
            prop_assert!(bq.cost >= s.proceeds);
        }
        if let Some(s) = s
            && let Some(back) = sol_leg::buy_exact_out(q, s.token_after, s.sol_after, eps1, &f)
        {
            prop_assert!(back.cost >= s.proceeds);
            prop_assert!(back.sol_after >= y);
        }
        // Raydium: the buy is SOL in (x_sol = y), the sale token in (x_tok = x).
        if let (Some(bq), Some(s)) = (ray_v4::buy_exact_out(q, y, x, num, 10_000),
                                      ray_v4::sell_exact_in(q, x, y, num, 10_000)) {
            prop_assert!(bq.cost >= s.proceeds);
        }
        if let Some(s) = ray_v4::sell_exact_in(q, x, y, num, 10_000)
            && let Some(back) = ray_v4::buy_exact_out(q, s.sol_after, s.token_after, num, 10_000)
        {
            prop_assert!(back.cost >= s.proceeds);
        }
        if let (Some(bq), Some(s)) = (cpmm::buy_exact_out(q, y, x, &cf), cpmm::sell_exact_in(q, x, y, &cf)) {
            prop_assert!(bq.cost >= s.proceeds);
        }
    }

    // -------------------------------------------------------------- fees

    /// The risk view never quotes above the live fee, and never exceeds the cap. Pump
    /// venues charge one combined fee at the cap above it (ECON §3.1; the sims'
    /// `Venue._risk_fees`: `n + ⌈n·cap/10⁴⌉`), Raydium grosses up at `min(rate, cap)`; every
    /// gate compares the ppm rate with `cap·100` exactly (ECON §2.1).
    #[test]
    fn capped_fees_never_quote_higher((x, y) in pool(), q in amount(), f in fee_bps(),
                                      cap in 0u64..3_000, num in 0u64..10_000,
                                      den in 1u64..20_000, cf in cpmm_fees()) {
        let ppm = (b(f.lp) + f.protocol + f.creator) * 100u32;
        let above = ppm > b(cap) * 100u32;
        // The saturated u32 view gives the same answer for every cap below 42,949,672 bps.
        prop_assert_eq!(fees::exceeds_cap(f.oneway_ppm(), cap), above);
        let c = f.capped(cap);
        if above {
            prop_assert_eq!(c, FeeBps::new(0, cap, 0));
        } else {
            prop_assert_eq!(c, f);
        }
        prop_assert_eq!(c.oneway_bps(), f.oneway_bps().min(cap));
        if let (Some(live), Some(risk)) = (sol_leg::buy_exact_out(q, x, y, false, &f),
                                           sol_leg::buy_exact_out(q, x, y, false, &c)) {
            prop_assert!(risk.cost <= live.cost);
            prop_assert_eq!(risk.net, live.net);
            if above {
                // One ceiling: n + ⌈n·cap/10⁴⌉.
                prop_assert_eq!(b(risk.cost), b(risk.net) + ref_fee(&b(risk.net), cap, 10_000));
            }
        }
        if let Ok(v4) = fees::ray_v4(num, den) {
            // ⌈num·10⁶/den⌉ ppm, ⌈num·10⁴/den⌉ bps for display.
            prop_assert_eq!(b(v4.oneway_ppm()), ceil(&(b(num) * 1_000_000u32), &b(den)).unwrap());
            let r: RayV4Fee = v4.capped(cap);
            prop_assert_eq!(r.oneway_bps(), v4.oneway_bps().min(cap));
            // Capped iff num/den > cap/10⁴, exactly.
            prop_assert_eq!(r != v4, b(num) * 10_000u32 > b(cap) * den);
            if let (Some(live), Some(risk)) = (ray_v4::buy_exact_out(q, y, x, v4.num, v4.den),
                                               ray_v4::buy_exact_out(q, y, x, r.num, r.den)) {
                prop_assert!(risk.cost <= live.cost);
            }
        }
        let rc = cf.capped(cap);
        let tc = b(cf.trade) + cf.creator;
        prop_assert!(rc.trade <= cf.trade && rc.creator <= cf.creator);
        prop_assert!(rc.oneway_bps() <= cf.oneway_bps());
        if tc > b(cap) * 100u32 {
            // t + c = cap·100 exactly (no rounding, R3A-3 iii).
            prop_assert_eq!(b(rc.trade) + rc.creator, b(cap) * 100u32);
        } else {
            prop_assert_eq!(rc, cf);
        }
        if let (Some(live), Some(risk)) = (cpmm::buy_exact_out(q, y, x, &cf), cpmm::buy_exact_out(q, y, x, &rc)) {
            prop_assert!(risk.cost <= live.cost);
        }
    }

    /// Fee resolution against a direct implementation of pump-fees' rules, and no panic
    /// on any input.
    #[test]
    fn fee_resolution(thresholds in proptest::collection::vec(any::<u64>(), 0..6), mcap_q in reserve(),
                      base in reserve(), supply: u64, creator_cfg: bool, override_bps in bps(),
                      default_creator: bool, cap in proptest::option::of(0u64..1_000),
                      vt in reserve(), vq in reserve()) {
        let mut th: Vec<u128> = thresholds.iter().map(|&t| u128::from(t) * 1_000).collect();
        th.sort_unstable();
        let tiers: Vec<FeeTier> = th.iter().enumerate()
            .map(|(i, &t)| FeeTier { threshold: t, fees: FeeBps::new(i as u64, 90 + i as u64, 30 + i as u64) })
            .collect();
        let s = PammFeeSchedule { tiers: &tiers, base_supply: supply, creator_fee_configurable: creator_cfg,
                                  pool_creator_fee_bps: override_bps, coin_creator_is_default: default_creator,
                                  cap_bps: cap };
        let got = fees::pump_amm(mcap_q, base, &s);
        if base == 0 {
            prop_assert_eq!(got, Err(shortwin_math::MathError::DivByZero));
        } else if tiers.is_empty() {
            prop_assert!(got.is_err());
        } else {
            let mcap = b(mcap_q) * supply / base;
            let mut want = tiers[0].fees;
            for t in &tiers {
                if mcap >= b(t.threshold) {
                    want = t.fees;
                }
            }
            if default_creator {
                want.creator = 0;
            } else if creator_cfg && override_bps > 0 {
                want.creator = override_bps;
            }
            let want = cap.map_or(want, |c| capped_ref(want, c));
            match u128_of(&mcap) {
                Some(_) => prop_assert_eq!(got, Ok(want)),
                None => prop_assert!(got.is_err()),
            }
        }
        let cs = fees::CurveFeeSchedule { tiers: &tiers, creator_fee_configurable: creator_cfg,
                                          curve_creator_fee_bps: override_bps,
                                          creator_is_default: default_creator, cap_bps: cap };
        let got = fees::pump_curve(vt, vq, &cs);
        if vt == 0 || tiers.is_empty() {
            prop_assert!(got.is_err());
        } else {
            let mcap = b(vq) * 1_000_000_000_000_000u64 / vt;
            let mut want = tiers[0].fees;
            for t in &tiers {
                if mcap >= b(t.threshold) {
                    want = t.fees;
                }
            }
            // The curve charges no LP fee.
            want.lp = 0;
            if default_creator {
                want.creator = 0;
            } else if creator_cfg && override_bps != 0 {
                want.creator = override_bps;
            }
            let want = cap.map_or(want, |c| capped_ref(want, c));
            match u128_of(&mcap) {
                Some(_) => prop_assert_eq!(got, Ok(want)),
                None => prop_assert!(got.is_err()),
            }
        }
    }

    // -------------------------------------------------------------- straddle

    /// On standard curves (ECON §1.1 constants) the executed-path quote is monotone in q on
    /// each side of `rT`; across it (review F-A2) the post-migration pool, priced ≈ 0.023 %
    /// below the curve's completion price, may quote `B̂_str(q > rT)` below `B̂_curve(q ≤ rT)`
    /// by at most 3 bps. The v1.1 two-leg form never prices above it (V1-9).
    #[test]
    fn straddle_monotone(sold_frac in 0u64..=1_000_000, q1 in 1u64..400_000_000_000_000,
                         q2 in 1u64..400_000_000_000_000, ema_k in 1u64..40) {
        let (vt0, vs0, rt0) = (1_073_000_000_000_000u128, 30_000_000_000u128, 793_100_000_000_000u128);
        let sold = rt0 * u128::from(sold_frac) / 1_000_000;
        let vt = vt0 - sold;
        let vq = (vt0 * vs0).div_ceil(vt);
        let live = CurveState { vt, vq, rt: rt0 - sold, rq: vq - vs0, complete: sold == rt0 };
        let tiers = [
            FeeTier { threshold: 0, fees: FeeBps::new(2, 93, 30) },
            FeeTier { threshold: 420_000_000_000, fees: FeeBps::new(20, 5, 95) },
        ];
        let sched = PammFeeSchedule { tiers: &tiers, base_supply: 1_000_000_000_000_000,
                                      creator_fee_configurable: true, pool_creator_fee_bps: 0,
                                      coin_creator_is_default: false, cap_bps: None };
        let mig = MigrationParams { migration_fee: 15_000_001, pool_tokens: 206_900_000_000_000,
                                    post_complete_quote_in: 0 };
        let fee = FeeBps::new(0, 95, 30);
        // Live leg and an EMA leg at price·k/8.
        let l = sqrt_u128(vt * vq, Rounding::Floor);
        let p = vq * 1_000_000_000_000_000_000 / vt * u128::from(ema_k) / 8;
        let off = CurveOffsets::of(&live).unwrap();
        let ema = quote::curve_reprice(l, p, off).unwrap();
        let (lo, hi) = (q1.min(q2), q1.max(q2));
        for s in [live, ema] {
            let a = quote::curve_straddle(lo, &s, &fee, &mig, &sched);
            let c = quote::curve_straddle(hi, &s, &fee, &mig, &sched);
            if let (Some(a), Some(c)) = (a, c) {
                if (u128::from(lo) <= s.rt) == (u128::from(hi) <= s.rt) {
                    prop_assert!(a.cost <= c.cost);
                } else {
                    prop_assert!(u128::from(c.cost) * 10_000 >= u128::from(a.cost) * 9_997,
                                 "dip beyond 3 bps: {} -> {}", a.cost, c.cost);
                }
            }
            if let Some(c) = c {
                prop_assert!(a.is_some() || s.complete);
                if let Some(t) = quote::curve_two_leg(hi, &s, &fee, &mig, &sched) {
                    prop_assert!(t.cost <= c.cost);
                }
            }
        }
    }

    /// Any curve state and size: the executed-path quote is the curve quote up to `rT`
    /// and the pool quote on the migrated pool (with a synthetic buy's `pcQ`, pump-final §5.1)
    /// above it, with no panic anywhere.
    #[test]
    fn straddle_equals_composition(q in amount(), vt in reserve(), vq in reserve(), rt in reserve(),
                                   rq in reserve(), complete: bool, fee in any::<u64>(),
                                   pool_tokens in reserve(), f in sane_fee_bps(), supply: u64,
                                   pcq in prop_oneof![Just(0u64), any::<u64>(), 0u64..100_000_000_000]) {
        let s = CurveState { vt, vq, rt, rq, complete };
        let mig = MigrationParams { migration_fee: fee, pool_tokens, post_complete_quote_in: pcq };
        let tiers = [FeeTier { threshold: 0, fees: FeeBps::new(2, 93, 30) },
                     FeeTier { threshold: 420_000_000_000, fees: FeeBps::new(20, 5, 95) }];
        let sched = PammFeeSchedule { tiers: &tiers, base_supply: supply, creator_fee_configurable: true,
                                      pool_creator_fee_bps: 0, coin_creator_is_default: false, cap_bps: None };
        // Reference migrated pool: Y_p = rQ + (⌈rT·vQ/(vT − rT)⌉ + 1) − fee + pcQ (ECON §3.1 n).
        let pool = ref_migrated_pool(&s, &mig);
        prop_assert_eq!(quote::migrated_pool(&s, &mig), pool);
        let want = if q == 0 || complete {
            None
        } else if u128::from(q) <= rt {
            ref_sol_buy(q, vt, vq, true, &f)
        } else {
            pool.and_then(|(_, yp)| {
                let pf = fees::pump_amm(yp, pool_tokens, &sched).ok()?;
                ref_sol_buy(q, pool_tokens, yp, false, &pf)
            })
        };
        prop_assert_eq!(quote::curve_straddle(q, &s, &f, &mig, &sched), want);
        let _ = quote::curve_two_leg(q, &s, &f, &mig, &sched);
        if let Some(off) = CurveOffsets::of(&s) {
            let _ = quote::curve_reprice(vq, vt, off);
        }
    }

    // -------------------------------------------------------------- synthetic (B̂_v3)

    /// T-MATH-3 (pump-final §5.2): on any state, `curve_synthetic` is its composition — the
    /// curve quote up to `rT` (equal to `curve_straddle` there), above it the curve leg for `rT`
    /// plus the would-be pool's leg for `q − rT`, both at the synthetic fee with `ε₁ = 1` —
    /// `None` exactly when that is, in particular iff `q − rT ≥ X_s` once the curve leg and the
    /// pool are defined, and no panic anywhere. `CurvePricing` routes `q > rT` through it when
    /// `synthetic` is set.
    #[test]
    fn synthetic_equals_composition(q in amount(), vt in reserve(), vq in reserve(), rt in reserve(),
                                    rq in reserve(), complete: bool, fee in any::<u64>(),
                                    pool_tokens in reserve(), f in sane_fee_bps(), fs in sane_fee_bps(),
                                    pcq in prop_oneof![3 => Just(0u64), 1 => any::<u64>()]) {
        let s = CurveState { vt, vq, rt, rq, complete };
        let mig = MigrationParams { migration_fee: fee, pool_tokens, post_complete_quote_in: pcq };
        let got = quote::curve_synthetic(q, &s, &f, &fs, &mig);
        prop_assert_eq!(got, ref_synthetic(q, &s, &f, &fs, &mig));
        let tiers = [FeeTier { threshold: 0, fees: FeeBps::new(2, 93, 30) }];
        let sched = PammFeeSchedule { tiers: &tiers, base_supply: 1_000_000_000_000_000,
                                      creator_fee_configurable: true, pool_creator_fee_bps: 0,
                                      coin_creator_is_default: false, cap_bps: None };
        if u128::from(q) <= rt {
            prop_assert_eq!(got, quote::curve_straddle(q, &s, &f, &mig, &sched));
        } else if q > 0 && !complete {
            let leg1 = rt == 0 || u64::try_from(rt).ok()
                .and_then(|r| sol_leg::buy_exact_out(r, vt, vq, true, &fs)).is_some();
            if let (true, Some((xs, _))) = (leg1, quote::migrated_pool(&s, &mig))
                && u128::from(q) - rt >= xs
            {
                prop_assert_eq!(got, None, "q − rT ≥ X_s: pump's NotEnoughTokensToBuy");
            }
        }
        if let Some(offsets) = CurveOffsets::of(&s) {
            let c = quote::CurvePricing { fees: f, offsets, migration: mig, pool: sched,
                                          synthetic: Some(fs) };
            prop_assert_eq!(c.buy(q, &s), got);
            let st = quote::CurvePricing { synthetic: None, ..c };
            prop_assert_eq!(st.buy(q, &s), quote::curve_straddle(q, &s, &f, &mig, &sched));
        }
    }

    /// T-MATH-3 (pump-final §5.2): `B̂_v3` covers pump's charge for `buy_v3(rT + q₂)` — the v3
    /// model (`n₁ᵖ = ⌈⌉`) and the v2 one (`⌊⌋ + 1`, what pump charged on G0's exact divisions) —
    /// by at most §5.2's general bound `(1 + Σ⌈bᵢ/10⁴⌉) + (Δ₂ + Σ⌈Δ₂·bᵢ/10⁴⌉)` over the full
    /// domain `q₂ < X_s`, and by at most 7 lamports for `q₂ ≤ X_s/2` (fee components up to
    /// 5,000 bps; the per-market cap keeps every position there, C8). Standard and random
    /// curves, any would-be pool (`X_s` with or without a donation), exact divisions of both
    /// parts generated on purpose.
    #[test]
    fn synthetic_covers_pumps_charge(standard in prop_oneof![2 => Just(true), 1 => Just(false)],
                                     off_t in 1_000_000_000_000u128..1_100_000_000_000_000,
                                     off_q in 1_000_000_000u128..200_000_000_000,
                                     rt in 1u128..800_000_000_000_000,
                                     rq in 0u128..500_000_000_000,
                                     exact: bool,
                                     x_s in prop_oneof![2 => Just(206_900_000_000_000u128),
                                                        1 => 206_900_000_000_000u128..420_000_000_000_000,
                                                        1 => 1_000_000_000_000u128..1_000_000_000_000_000],
                                     q2_mode in 0u8..4, q2_raw: u64,
                                     protocol in prop_oneof![3 => 0u64..=300, 1 => 0u64..=10_000],
                                     creator in prop_oneof![3 => 0u64..=300, 1 => 0u64..=10_000],
                                     fee_m in prop_oneof![3 => Just(15_000_001u64), 1 => 0u64..1_000_000_000]) {
        let s = synthetic_curve(standard, off_t, off_q, rt, rq, exact);
        if s.rq < u128::from(fee_m) {
            return Ok(());
        }
        let q2: u64 = match q2_mode {
            0 => u64::try_from((u128::from(q2_raw) % (x_s - 1)) + 1).unwrap(),
            1 => u64::try_from((u128::from(q2_raw) % (x_s / 2)) + 1).unwrap(),
            2 => u64::try_from(x_s / 2).unwrap(),
            _ => u64::try_from(x_s - 1 - u128::from(q2_raw % 1_000)).unwrap(),
        };
        let Ok(rt64) = u64::try_from(s.rt) else { return Ok(()); };
        let Some(q) = rt64.checked_add(q2) else { return Ok(()); };
        let f = FeeBps::new(0, protocol, creator);
        let mig = MigrationParams { migration_fee: fee_m, pool_tokens: x_s, post_complete_quote_in: 0 };
        let got = quote::curve_synthetic(q, &s, &f, &f, &mig);
        let bound = v3_bound(q2, x_s, &f);
        for v2 in [false, true] {
            let Some(charge) = ref_pump_charge(&s, q2, x_s, fee_m, &f, v2) else { continue };
            match got {
                Some(bq) => {
                    let quote = b(bq.cost);
                    prop_assert!(quote >= charge, "B̂_v3 {} below pump's charge {} (v2 {})", bq.cost, charge, v2);
                    prop_assert!(&quote - &charge <= bound, "B̂_v3 {} − charge {} above the bound {}", bq.cost, charge, bound);
                    if 2 * u128::from(q2) <= x_s && protocol <= 5_000 && creator <= 5_000 {
                        prop_assert!(&quote - &charge <= b(7u8), "B̂_v3 {} − charge {} > 7", bq.cost, charge);
                    }
                }
                // Undefined only when an amount leaves u64: then so does the charge plus the bound.
                None => prop_assert!(charge + &bound > b(u64::MAX), "no quote for a charge that fits"),
            }
        }
    }

    /// T-MATH-3 (pump-final §5.2): with one fee schedule `B̂_v3` is non-decreasing in `q`, and
    /// continuous at `rT`: `B̂_v3(rT + 1) − B̂_v3(rT)` is the would-be pool's quote for one
    /// token (≤ 4 lamports on a standard curve at today's fees). The risk view (capped) never
    /// quotes above the live one.
    #[test]
    fn synthetic_monotone_and_continuous(standard in prop_oneof![3 => Just(true), 1 => Just(false)],
                                         off_t in 1_000_000_000_000u128..1_100_000_000_000_000,
                                         off_q in 1_000_000_000u128..200_000_000_000,
                                         rt in 1u128..800_000_000_000_000,
                                         rq in 15_000_001u128..500_000_000_000,
                                         x_s in prop_oneof![2 => Just(206_900_000_000_000u128),
                                                            1 => 1_000_000_000_000u128..1_000_000_000_000_000],
                                         q1 in 1u64..1_200_000_000_000_000, q2 in 1u64..1_200_000_000_000_000,
                                         f in sane_fee_bps(), cap in 0u64..400) {
        let s = synthetic_curve(standard, off_t, off_q, rt, rq, false);
        let mig = MigrationParams { migration_fee: 15_000_001, pool_tokens: x_s, post_complete_quote_in: 0 };
        let f = FeeBps::new(0, f.protocol, f.creator);
        let (lo, hi) = (q1.min(q2), q1.max(q2));
        let a = quote::curve_synthetic(lo, &s, &f, &f, &mig);
        let c = quote::curve_synthetic(hi, &s, &f, &f, &mig);
        if let (Some(a), Some(c)) = (a, c) {
            prop_assert!(a.cost <= c.cost, "{} -> {}: {} > {}", lo, hi, a.cost, c.cost);
        }
        if c.is_some() {
            prop_assert!(a.is_some(), "defined at {} but not at {}", hi, lo);
        }
        let rt64 = u64::try_from(s.rt).unwrap();
        if let (Some(at), Some(past), Some((xs, ys))) = (
            quote::curve_synthetic(rt64, &s, &f, &f, &mig),
            quote::curve_synthetic(rt64 + 1, &s, &f, &f, &mig),
            quote::migrated_pool(&s, &mig),
        ) {
            let one = sol_leg::buy_exact_out(1, xs, ys, true, &f).unwrap();
            prop_assert_eq!(past.cost - at.cost, one.cost);
            if standard && x_s == 206_900_000_000_000 && f.protocol <= 100 && f.creator <= 100 {
                prop_assert!(one.cost <= 4, "{}", one.cost);
            }
        }
        // Capped ≤ live, on both sides of rT.
        let tiers = [FeeTier { threshold: 0, fees: FeeBps::new(2, 93, 30) }];
        let offsets = CurveOffsets::of(&s).unwrap();
        let c = quote::CurvePricing { fees: f, offsets, migration: mig,
                                      pool: PammFeeSchedule { tiers: &tiers, base_supply: 1_000_000_000_000_000,
                                                              creator_fee_configurable: true, pool_creator_fee_bps: 0,
                                                              coin_creator_is_default: false, cap_bps: None },
                                      synthetic: Some(f) };
        for q in [lo, hi] {
            if let (Some(live), Some(risk)) = (c.buy(q, &s), c.capped(cap).buy(q, &s)) {
                prop_assert!(risk.cost <= live.cost, "capped {} > live {}", risk.cost, live.cost);
            }
        }
    }

    /// T-MATH-3 (pump-final §5.2, D3): on standard curves at today's fees, `B̂_v3` is never more
    /// than ≈ 2.4 bps above `B̂_str` (the dip region just above `rT`: the curve's final price over
    /// the would-be pool's, 115,005,359,190/279.9e12 against 84,990,359,189/206.9e12 = 1.000244,
    /// plus a few lamports of rounding), and below it once `q − rT` is a few percent of `X_s`.
    #[test]
    fn synthetic_sits_at_most_2_4_bps_above_the_straddle(sold_frac in 1u64..1_000_000,
                                                         extra in prop_oneof![1u64..1_000_000_000,
                                                                              1u64..10_000_000_000_000,
                                                                              1u64..150_000_000_000_000]) {
        let (vt0, vs0, rt0) = (1_073_000_000_000_000u128, 30_000_000_000u128, 793_100_000_000_000u128);
        let sold = rt0 * u128::from(sold_frac) / 1_000_000;
        let vt = vt0 - sold;
        let vq = (vt0 * vs0).div_ceil(vt);
        let s = CurveState { vt, vq, rt: rt0 - sold, rq: vq - vs0, complete: false };
        let tiers = [
            FeeTier { threshold: 0, fees: FeeBps::new(2, 93, 30) },
            FeeTier { threshold: 420_000_000_000, fees: FeeBps::new(20, 5, 95) },
        ];
        let sched = PammFeeSchedule { tiers: &tiers, base_supply: 1_000_000_000_000_000,
                                      creator_fee_configurable: true, pool_creator_fee_bps: 0,
                                      coin_creator_is_default: false, cap_bps: None };
        let mig = MigrationParams { migration_fee: 15_000_001, pool_tokens: 206_900_000_000_000,
                                    post_complete_quote_in: 0 };
        let fee = FeeBps::new(0, 95, 30);
        let q = u64::try_from(s.rt).unwrap() + extra;
        let (Some(v3), Some(st)) = (quote::curve_synthetic(q, &s, &fee, &fee, &mig),
                                    quote::curve_straddle(q, &s, &fee, &mig, &sched)) else {
            return Ok(());
        };
        prop_assert!(u128::from(v3.cost) * 1_000_000 <= u128::from(st.cost) * 1_000_245 + 8_000_000,
                     "B̂_v3 {} vs B̂_str {}", v3.cost, st.cost);
        if 50 * u128::from(extra) >= 206_900_000_000_000 {
            prop_assert!(v3.cost < st.cost, "B̂_v3 {} vs B̂_str {}", v3.cost, st.cost);
        }
    }

    // -------------------------------------------------------------- migrating FV (ρ)

    /// T-MATH-4 (pump-final §5.4): without a pool part (`pcQ = 0`) `migrating_fv` is the FV of
    /// before pump's v3 — the pool of `pool_tokens` at `P`, at its own tier, ε₁ = 0 — whatever
    /// `pool_tokens` is (the standard 206.9e12, or a donation-inflated live balance the caller
    /// would not pass without a pool part), and ρ = 1 returns `P` itself.
    #[test]
    fn migrating_fv_price_is_identity_without_pc(q in amount(), p in reprice_arg(), rq in reserve(),
                                                 fee in prop_oneof![Just(15_000_001u64), any::<u64>()],
                                                 pool_tokens in prop_oneof![Just(206_900_000_000_000u128),
                                                                            206_900_000_000_000u128..1_000_000_000_000_000,
                                                                            reserve()],
                                                 x in 1u128..u128::MAX, y in 1u128..u128::MAX) {
        let done = CurveState { vt: 279_900_000_000_000, vq: rq.saturating_add(30_000_000_000), rt: 0, rq,
                                complete: true };
        let tiers = [FeeTier { threshold: 0, fees: FeeBps::new(2, 93, 30) },
                     FeeTier { threshold: 420_000_000_000, fees: FeeBps::new(20, 5, 95) }];
        let sched = PammFeeSchedule { tiers: &tiers, base_supply: 1_000_000_000_000_000,
                                      creator_fee_configurable: true, pool_creator_fee_bps: 0,
                                      coin_creator_is_default: false, cap_bps: None };
        let m = MigrationParams { migration_fee: fee, pool_tokens, post_complete_quote_in: 0 };
        let old = quote::migrated_pool(&done, &m).and_then(|(xp, yp)| {
            let f = fees::pump_amm(yp, xp, &sched).ok()?;
            let (x, y) = quote::reprice(quote::liquidity(xp, yp).ok()?, p)?;
            sol_leg::buy_exact_out(q, x, y, false, &f)
        });
        prop_assert_eq!(quote::migrating_fv(q, p, &done, &m, &sched), old);
        prop_assert_eq!(quote::migrating_fv_price(p, x, y, x, y), Some(p));
    }

    /// T-MATH-4 (pump-final §5.4): after a synthetic pool part `pcB` priced on the would-be pool
    /// `(206.9e12 + D, Y₀)` (a donation `D` before the buy, `D′` after it), FV at the frozen
    /// pool's price `P(X₀, Y₀)` prices the pool `migrate_v2` creates, `(X₁, Y₁)` = (the live
    /// `curve_base_ata`, `Y₀ + pcQ`): `p'` is within `⌈ρ⌉ + 2` of `P(X₁, Y₁)` (in WAD units), so
    /// `reprice(L₁, p')` is `(X₁, Y₁)` up to `reprice`'s own `√P` granularity. A donation after
    /// the pool part lowers `p'`.
    #[test]
    fn migrating_fv_price_reprices_to_the_post_buy_pool(rq in 20_000_000_000u128..200_000_000_000,
                                                        d_before in prop_oneof![Just(0u128), 1u128..300_000_000_000_000],
                                                        d_after in prop_oneof![Just(0u128), 1u128..300_000_000_000_000],
                                                        pcb_frac in 1u64..999_000) {
        let fee = 15_000_001u64;
        let x0 = 206_900_000_000_000u128;
        let y0 = rq - u128::from(fee);
        let xs = x0 + d_before;
        let pcb = xs * u128::from(pcb_frac) / 1_000_000;
        // pump's pool part: n₂ᵖ = ⌈Y₀·pcB/(X_s − pcB)⌉ on the donation-inclusive X_s.
        let pcq = u64::try_from((y0 * pcb).div_ceil(xs - pcb)).unwrap();
        let a = xs - pcb + d_after;
        let done = CurveState { vt: 279_900_000_000_000, vq: rq + 30_000_000_000, rt: 0, rq, complete: true };
        let m = MigrationParams { migration_fee: fee, pool_tokens: a, post_complete_quote_in: pcq };
        let (x1, y1) = quote::migrated_pool(&done, &m).unwrap();
        prop_assert_eq!((x1, y1), (a, y0 + u128::from(pcq)));
        let wad = 1_000_000_000_000_000_000u128;
        let p0 = y0 * wad / x0;
        let p1 = y1 * wad / x1;
        let p = quote::migrating_fv_price(p0, x0, y0, x1, y1).unwrap();
        let rho_ceil = (y1 * x0).div_ceil(x1 * y0);
        prop_assert!(p.abs_diff(p1) <= rho_ceil + 2, "p' {} vs P(X₁, Y₁) {} (ρ ≤ {})", p, p1, rho_ceil);
        let (x, y) = quote::reprice(quote::liquidity(x1, y1).unwrap(), p).unwrap();
        // reprice's own granularity: `isqrt⁺(P)` moves X and Y by up to 1/√P relatively, the
        // floored L by up to 10⁹/√P raw units.
        let sp = u128::try_from(b(p).sqrt()).unwrap();
        prop_assert!(x.abs_diff(x1) <= 2 * x1 / sp + 64, "X {} vs X₁ {}", x, x1);
        prop_assert!(y.abs_diff(y1) <= 2 * y1 / sp + 64, "Y {} vs Y₁ {}", y, y1);
        if d_after > 0 {
            let without = MigrationParams { pool_tokens: xs - pcb, ..m };
            let (xw, yw) = quote::migrated_pool(&done, &without).unwrap();
            prop_assert!(quote::migrating_fv_price(p0, x0, y0, xw, yw).unwrap() >= p);
        }
    }

    // -------------------------------------------------------------- reprice

    /// ECON §2.1 (R3B-7 c): `sp = isqrt⁺(P)`, `X = ⌊L·10⁹/sp⌋`, `Y = ⌈L·sp/10⁹⌉`, `None`
    /// when `X = 0` (the sim); the reprojected price never understates `P`.
    #[test]
    fn reprice_is_exact(l in reprice_arg(), p in reprice_arg()) {
        let got = quote::reprice(l, p);
        if l == 0 || p == 0 {
            prop_assert_eq!(got, None);
        } else {
            let r = b(p).sqrt();
            let sp = if &r * &r == b(p) { r } else { r + 1u8 };
            let x = b(l) * 1_000_000_000u32 / &sp;
            let y = ceil(&(b(l) * &sp), &b(1_000_000_000u32)).unwrap();
            let want = if x == zero() { None } else { u128_of(&x).zip(u128_of(&y)) };
            prop_assert_eq!(got, want);
            if let Some((x, y)) = got {
                prop_assert!(b(y) * 1_000_000_000_000_000_000u64 >= b(p) * x);
                prop_assert_eq!(quote::reprice_token(l, p), Ok(x));
            }
        }
    }

    /// `L = isqrt(X·Y)` on `u128` only; `Overflow` outside ECON §2.2's bounds.
    #[test]
    fn liquidity_is_the_u128_root(x in reserve(), y in reserve()) {
        let k = b(x) * y;
        match u128_of(&k) {
            Some(k) => prop_assert_eq!(quote::liquidity(x, y), Ok(k.isqrt())),
            None => prop_assert_eq!(quote::liquidity(x, y), Err(shortwin_math::MathError::Overflow)),
        }
    }

    // -------------------------------------------------------------- pool selection

    /// ECON §2.1: `λ_bps = ⌊(counter − supply)·10⁴/counter⌋`, `D = ⌊λ_bps·isqrt(X·Y)/10⁴⌋`.
    #[test]
    fn locked_depth_properties(counter: u64, supply: u64, x: u64, y: u64, d1: u128, d2: u128,
                               margin in 0u64..20_000) {
        let d = pool_select::locked_depth(counter, supply, x, y).unwrap();
        let root = sqrt_u128(u128::from(x) * u128::from(y), Rounding::Floor);
        let lam = pool_select::lambda_bps(counter, supply);
        prop_assert!(d <= root && lam <= 10_000);
        if counter > supply {
            prop_assert_eq!(b(lam), b(counter - supply) * 10_000u32 / counter);
            prop_assert_eq!(b(d), b(lam) * root / 10_000u32);
            // Never above v0.3's single floor on the raw ratio.
            prop_assert!(b(d) <= b(counter - supply) * root / counter);
        } else {
            prop_assert_eq!((d, lam), (0, 0));
        }
        // Unlocked LP added or removed pro rata leaves D (almost) unchanged: doubling the
        // counter, the circulating supply and both reserves doubles √(X·Y) and keeps λ.
        if let (Some(c2), Some(x2), Some(y2)) = (counter.checked_mul(2), x.checked_mul(2), y.checked_mul(2)) {
            let d2x = pool_select::locked_depth(c2, supply.saturating_mul(2), x2, y2).unwrap();
            if counter > supply {
                prop_assert!(d2x + 2 >= 2 * d && d2x <= 2 * d + 2);
            }
        }
        prop_assert_eq!(pool_select::dominates(d1, d2, margin),
                        b(d1) * 10_000u32 >= b(d2) * (10_000 + margin));
        prop_assert_eq!(pool_select::fast_path_qualifies(d1, d2, margin),
                        b(d1) * 10_000u32 >= b(d2) * margin);
        prop_assert_eq!(pool_select::lock_share_ok(counter, supply, margin),
                        counter > 0 && counter >= supply
                            && b(counter - supply.min(counter)) * 10_000u32 >= b(margin) * counter);
    }

    /// ARCH §2.6 eligibility as amended by M4-SEC-1 / M4-SEC-3: executable, `φ_ppm ≤
    /// venue_fee_cap_bps·100` (no rounding), and the locked-depth floor
    /// `⌊⌊D·isqrt(P_ref)/10⁹⌋·10⁴/λ_min⌋ ≥ pool_min_real_sol` (false for λ_min = 0).
    #[test]
    fn eligibility(executable: bool, fee_ppm in prop_oneof![0u32..40_000, any::<u32>()],
                   d in prop_oneof![any::<u128>(), 0u128..10u128.pow(20)],
                   p_ref in prop_oneof![any::<u128>(), 0u128..10u128.pow(24)],
                   cap in 0u64..400, min_sol: u64, lock_min in 0u64..=10_000) {
        let facts = pool_select::PoolFacts { executable, fee_ppm, locked_depth: d, p_ref };
        let params = pool_select::EligibilityParams {
            venue_fee_cap_bps: cap, pool_min_real_sol: min_sol, pool_lock_min_bps: lock_min,
        };
        let locked = (b(d) * b(p_ref).sqrt() / 1_000_000_000u32).min(b(u128::MAX));
        let floor_ok = lock_min > 0 && locked * 10_000u32 / lock_min >= b(min_sol);
        let want = executable && b(fee_ppm) <= b(cap) * 100u32 && floor_ok;
        prop_assert_eq!(pool_select::eligible(&facts, &params), want);
    }

    /// M4-SEC-1 / M4-SEC-3: at the pool's own spot price the floor admits every pool the
    /// v0.4.1 pair `R ≥ pool_min_real_sol ∧ λ ≥ λ_min` admitted (the floor is `≈ λ·R/λ_min ≥ R`),
    /// up to the floors of `D`, `isqrt` and `P` (a few lamports); an unlocked deposit of any size
    /// moves `D` only by `λ`'s floor to whole bps; a window's adoption liquidity is
    /// the smaller of its record and the executing instruction's sample, capped at `D/λ_min`
    /// (M4V-2), and an honest record of a pool with `λ ≥ λ_min` is its live depth (to one unit of
    /// rounding); the floor of an adoption carrying positions is the locked SOL itself (M4V-1).
    #[test]
    fn locked_floor_keeps_the_pair_and_ignores_unlocked_lp(
        x in 1_000_000u64..10u64.pow(18), y in 1_000_000u64..10u64.pow(15),
        counter in 1_000u64..10u64.pow(15), lock in 0u64..=10_000, lock_min in 2_000u64..=10_000,
        k in 0u64..20) {
        let supply = counter - (counter / 10_000 * lock + (counter % 10_000) * lock / 10_000);
        let d = pool_select::locked_depth(counter, supply, x, y).unwrap();
        let wad = 1_000_000_000_000_000_000u128;
        let p = u128::from(y) * wad / u128::from(x);
        let lam = u64::from(pool_select::lambda_bps(counter, supply));
        let l_live = (u128::from(x) * u128::from(y)).isqrt();
        // The pair at its floor: R = y, λ = lam ≥ λ_min.
        let s0 = 2 * l_live / 1_000_000_000 + 2 * (p.isqrt() / 1_000_000_000) + 4;
        let slack = 5 * s0 + 1;
        let boundary = u128::from(y) * u128::from(lam) / u128::from(lock_min);
        if lam >= lock_min && boundary > slack {
            // The pair admits the pool for every floor up to R = y; the locked-depth floor
            // admits it up to λ·y/λ_min ≥ y.
            let pair = u64::try_from(u128::from(y).saturating_sub(slack)).unwrap();
            prop_assert!(pool_select::locked_floor_ok(d, p, lock_min, pair), "pair admitted, floor refused");
            let floor = u64::try_from(boundary - slack).unwrap_or(u64::MAX);
            prop_assert!(pool_select::locked_floor_ok(d, p, lock_min, floor), "below its boundary");
            prop_assert!(!pool_select::locked_floor_ok(d, p, lock_min,
                u64::try_from(u128::from(y) * u128::from(lam) / u128::from(lock_min) + 1).unwrap_or(u64::MAX)));
        }
        // An unlocked deposit of k times the pool, pro rata: λ divided by k + 1, D kept but for
        // λ's floor to whole bps.
        if let (Some(c2), Some(s2), Some(x2), Some(y2)) = (counter.checked_mul(k + 1),
            supply.checked_add(counter * k), x.checked_mul(k + 1), y.checked_mul(k + 1)) {
            let d2 = pool_select::locked_depth(c2, s2, x2, y2).unwrap();
            prop_assert!(d2.abs_diff(d) <= u128::from(k + 1) * (l_live / 10_000 + 2) + 2, "D {} -> {}", d, d2);
        }
        // M4R: the adoption anchor ⌊D·10⁴/λ_min⌋ is at least the pool's live depth whenever
        // λ ≥ λ_min (up to D's floor), so the warm-up's seed — not the anchor — is what binds.
        let cap = pool_select::depth_at_lambda(d, lock_min);
        prop_assert_eq!(cap, d * 10_000 / u128::from(lock_min));
        if lam >= lock_min && l_live > 0 {
            prop_assert!(cap + 10_000 / u128::from(lock_min) + 1 >= l_live);
        }
        // M4V-1: the floor of an adoption carrying positions is the locked SOL itself — never
        // looser than the listing floor, and exactly it at λ_min = 100 %.
        let min_sol = u64::try_from(u128::from(y) / 3).unwrap_or(u64::MAX);
        if pool_select::position_floor_ok(d, p, min_sol) {
            prop_assert!(pool_select::locked_floor_ok(d, p, lock_min, min_sol));
        }
        prop_assert_eq!(pool_select::position_floor_ok(d, p, min_sol),
            pool_select::locked_floor_ok(d, p, 10_000, min_sol));
    }
}

fn reprice_arg() -> impl Strategy<Value = u128> {
    prop_oneof![
        3 => any::<u128>(),
        2 => 0u128..1_000,
        2 => 1u128..10_000_000_000_000_000_000,
        2 => 1u128..10_000_000_000_000_000_000_000_000_000_000_000_000,
        1 => Just(0u128),
        1 => Just(u128::MAX),
    ]
}
