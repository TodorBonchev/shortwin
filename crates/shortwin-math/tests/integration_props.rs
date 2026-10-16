//! Cross-module properties added at integration (ARCHITECTURE §8.1), at least 2,000 cases
//! each:
//!
//! - `D = λ·√(X·Y)` under any swap sequence on AMM v4 and CPMM changes only by the fee
//!   the pool retains (V2-7): `X·Y` never falls, and grows by at most the retained fee
//!   times the other reserve plus one unit of rounding per swap;
//! - `quote::Pricing` dispatches to the venue modules with the (token, SOL) order;
//! - the liquidation price is the exact crossing of the CR predicate, which is monotone
//!   in the price on `reprice` for every constant-product family; on the curve
//!   (`CurvePricing`) it is the first crossing although `B̂_str` dips where `rT(P)`
//!   crosses `Q` (review F-A2).
//!
//! The v0.3 property "the FV gate on the ring equals the gate on the ring's lower medians"
//! (V2-9) is gone with the gate: the settlement box replaces it (`settle_props.rs`,
//! `settle_vectors.rs`).

mod common;

use num_bigint::BigUint;
use proptest::prelude::*;
use shortwin_math::fees::{
    self, CpmmConfig, CpmmFees, CpmmPoolFees, Direction, FeeBps, FeeTier, PammFeeSchedule,
};
use shortwin_math::num::{PPM, Rounding, WAD, mul_div, sqrt_u128};
use shortwin_math::pool_select::locked_depth;
use shortwin_math::quote::{
    CurveOffsets, CurvePricing, CurveState, MigrationParams, Pricing, cpmm, curve_reprice, ray_v4,
    reprice, sol_leg,
};
use shortwin_math::risk;

fn b(x: u128) -> BigUint {
    BigUint::from(x)
}

fn cpmm_fees() -> impl Strategy<Value = (CpmmFees, CpmmFees)> {
    (
        0u64..=20_000,
        0u64..=300_000,
        0u64..=300_000,
        0u64..=10_000,
        any::<bool>(),
        0u8..=2,
        any::<bool>(),
    )
        .prop_map(|(t, p, f, c, enable, on, sol0)| {
            let cfg = CpmmConfig {
                trade_fee_rate: t,
                protocol_fee_rate: p,
                fund_fee_rate: f,
                creator_fee_rate: c,
            };
            let pool = CpmmPoolFees {
                enable_creator_fee: enable,
                creator_fee_on: on,
                sol_is_token0: sol0,
            };
            (
                fees::cpmm(&cfg, &pool, Direction::Buy).expect("valid config"),
                fees::cpmm(&cfg, &pool, Direction::Sell).expect("valid config"),
            )
        })
}

/// A Raydium family (the venues whose depth `D` selects pools, ARCH §2.6).
fn raydium() -> impl Strategy<Value = Pricing> {
    prop_oneof![
        (0u64..=300, 1u64..=4).prop_map(|(num, k)| {
            let den = 10_000 * k;
            Pricing::RayV4(fees::ray_v4(num * k, den).expect("num < den"))
        }),
        cpmm_fees().prop_map(|(buy, sell)| Pricing::Cpmm { buy, sell }),
    ]
}

fn any_family() -> impl Strategy<Value = Pricing> {
    prop_oneof![
        (0u64..=100, 0u64..=100, 0u64..=100)
            .prop_map(|(lp, p, c)| Pricing::PumpSwap(FeeBps::new(lp, p, c))),
        raydium(),
    ]
}

/// Pre-fee input the curve priced with, for a sale of `a` (`ray_v4` / `cpmm` sources).
fn sale_net_input(v: &Pricing, a: u64) -> u128 {
    let a = u128::from(a);
    let fee = match v {
        Pricing::RayV4(f) => mul_div(a, f.num.into(), f.den.into(), Rounding::Ceil),
        Pricing::Cpmm { sell, .. } => {
            let rate = if sell.creator_on_input {
                sell.trade + sell.creator
            } else {
                sell.trade
            };
            mul_div(a, rate.into(), PPM.into(), Rounding::Ceil)
        }
        Pricing::PumpSwap(_) => unreachable!("not a Raydium family"),
    };
    a - fee.expect("fee fits")
}

proptest! {
    #![proptest_config(common::proptest_config())]

    /// V2-7: swaps cannot shrink the locked depth, and grow `X·Y` only by the retained fee.
    #[test]
    fn locked_depth_moves_only_by_retained_fees(
        venue in raydium(),
        x0 in 1_000_000u64..10u64.pow(18),
        y0 in 1_000_000u64..10u64.pow(15),
        counter in 1u64..10u64.pow(15),
        supply_frac in 0u64..=10_000,
        swaps in prop::collection::vec((any::<bool>(), 1u64..=1_000_000), 1..20),
    ) {
        let supply = counter / 10_000 * supply_frac;
        let (mut x, mut y) = (u128::from(x0), u128::from(y0));
        let mut d = locked_depth(counter, supply, x0, y0).expect("depth");
        for (buy, frac) in swaps {
            let k0 = b(x) * b(y);
            let (x1, y1, bound) = if buy {
                let q = u64::try_from(x.saturating_sub(1) * u128::from(frac) / 1_000_000)
                    .unwrap_or(u64::MAX)
                    .max(1);
                let Some(bq) = venue.buy(q, x, y) else { continue };
                // Retained fee in SOL: what the SOL reserve gained beyond the curve's input.
                let fee = bq.sol_after.checked_sub(y).and_then(|g| g.checked_sub(u128::from(bq.net)));
                prop_assert!(fee.is_some(), "buy lost SOL beyond its input: {bq:?}");
                (bq.token_after, bq.sol_after, &k0 + (b(fee.unwrap()) + 1u32) * b(bq.token_after))
            } else {
                let a = u64::try_from(x * u128::from(frac) / 1_000_000).unwrap_or(u64::MAX).max(1);
                let Some(sq) = venue.sell(a, x, y, 0) else { continue };
                let fee = sq.token_after.checked_sub(x).and_then(|g| g.checked_sub(sale_net_input(&venue, a)));
                prop_assert!(fee.is_some(), "sale lost tokens beyond its input: {sq:?}");
                (sq.token_after, sq.sol_after, &k0 + b(fee.unwrap()) * b(y) + b(sq.token_after))
            };
            let k1 = b(x1) * b(y1);
            prop_assert!(k1 >= k0, "X·Y fell: {x}·{y} -> {x1}·{y1}");
            prop_assert!(k1 <= bound, "X·Y grew beyond the retained fee: {x}·{y} -> {x1}·{y1}");
            (x, y) = (x1, y1);
            let (Ok(xs), Ok(ys)) = (u64::try_from(x), u64::try_from(y)) else { break };
            let d1 = locked_depth(counter, supply, xs, ys).expect("depth");
            prop_assert!(d1 >= d, "D fell: {d} -> {d1}");
            d = d1;
        }
    }

    /// `Pricing` is the venue modules with (token, SOL) reserves.
    #[test]
    fn pricing_dispatch(venue in any_family(), q in 1u64..10u64.pow(15),
                        x in 1u128..10u128.pow(20), y in 1u128..10u128.pow(16), cap in any::<u64>()) {
        let (want_buy, want_sell) = match &venue {
            Pricing::PumpSwap(fees) => (
                sol_leg::buy_exact_out(q, x, y, false, fees),
                sol_leg::sell_exact_in(q, x, y, fees, cap),
            ),
            Pricing::RayV4(f) => (
                ray_v4::buy_exact_out(q, y, x, f.num, f.den),
                ray_v4::sell_exact_in(q, x, y, f.num, f.den),
            ),
            Pricing::Cpmm { buy, sell } => (
                cpmm::buy_exact_out(q, y, x, buy),
                cpmm::sell_exact_in(q, x, y, sell),
            ),
        };
        prop_assert_eq!(venue.buy(q, x, y), want_buy);
        prop_assert_eq!(venue.sell(q, x, y, cap), want_sell);
        let capped = venue.capped(cap % 200);
        prop_assert!(capped.oneway_ppm() <= venue.oneway_ppm());
        if let (Some(live), Some(risk)) = (venue.buy(q, x, y), capped.buy(q, x, y)) {
            prop_assert!(risk.cost <= live.cost);
        }
    }

    /// The liquidation price is where `CR(V_eff, B̂(Q; reprice(L, P))) < L_liq` starts to
    /// hold, and the predicate is monotone in `P` around it.
    #[test]
    fn liquidation_price_is_the_crossing(
        venue in any_family(),
        x in 10u128.pow(12)..10u128.pow(18),
        y in 10u128.pow(9)..10u128.pow(14),
        q_bps in 1u64..=500,
        cr_liq in 10_500u32..=20_000,
        head in 1u64..=40_000,
        probes in prop::collection::vec(any::<u64>(), 1..8),
    ) {
        let l = sqrt_u128(x * y, Rounding::Floor);
        let p0 = y * WAD / x;
        let q = u64::try_from(x * u128::from(q_bps) / 10_000).expect("fits").max(1);
        let quote = |p: u128| venue.buy_repriced(q, l, p).map(|bq| bq.cost);
        let Some(b0) = quote(p0) else { return Ok(()) };
        let v_eff = u64::try_from(u128::from(b0) * u128::from(cr_liq + u32::try_from(head).unwrap()) / 10_000)
            .unwrap_or(u64::MAX);
        let below = |p: u128| risk::cr_below(v_eff, quote(p), cr_liq).expect("quote > 0");
        let p_liq = risk::liquidation_price(p0, v_eff, cr_liq, quote)
            .expect("no error")
            .expect("an undefined quote at u128::MAX is below");
        prop_assert!(below(p_liq));
        if p_liq > p0 {
            prop_assert!(!below(p_liq - 1));
        }
        for r in probes {
            let r = u128::from(r);
            // A point below the crossing and one above it. `(p_liq − p0)·r` exceeds u128 once
            // the crossing lies ≳ 1.8·10¹⁹ (18 SOL per raw unit, WAD) above `p0`, so the offset
            // is taken with the 256-bit fallback (it is ≤ `p_liq − p0`, as r ≤ u64::MAX).
            let lo = p0 + mul_div(p_liq - p0, r, u128::from(u64::MAX), Rounding::Floor)
                .expect("≤ p_liq − p0");
            prop_assert!(lo == p_liq || !below(lo), "predicate holds at {lo} < {p_liq}");
            let hi = p_liq.saturating_add((p_liq / 1_000_000).saturating_mul(r % 1_000_000_000));
            prop_assert!(below(hi), "predicate fails at {hi} > {p_liq}");
        }
    }

    /// Curve positions (review F-A2): `CurvePricing::liquidation_price` returns a crossing
    /// with no crossing anywhere in the curve regime below it, also when `V_eff` is set so
    /// that CR first falls below `L` just before `rT(P)` reaches `Q`, where the plain
    /// bisection can land past the dip.
    #[test]
    fn curve_liquidation_price_is_the_first_crossing(
        sold in 0u64..=900_000,
        q_frac in 1u64..=1_000_000,
        cr_liq in 10_500u32..=20_000,
        at_split: bool,
        v_frac in 1u64..=1_000_000,
        probes in prop::collection::vec(any::<u64>(), 1..6),
    ) {
        let (vt0, vs0, rt0) = (1_073_000_000_000_000u128, 30_000_000_000u128, 793_100_000_000_000u128);
        let sold = rt0 * u128::from(sold) / 1_000_000;
        let vt = vt0 - sold;
        let vq = (vt0 * vs0).div_ceil(vt);
        let live = CurveState { vt, vq, rt: rt0 - sold, rq: vq - vs0, complete: false };
        let tiers = [
            FeeTier { threshold: 0, fees: FeeBps::new(2, 93, 30) },
            FeeTier { threshold: 420_000_000_000, fees: FeeBps::new(20, 5, 95) },
        ];
        let curve = CurvePricing {
            fees: FeeBps::new(0, 95, 30),
            offsets: CurveOffsets::of(&live).expect("offsets"),
            migration: MigrationParams { migration_fee: 15_000_001, pool_tokens: 206_900_000_000_000,
                                         post_complete_quote_in: 0 },
            pool: PammFeeSchedule { tiers: &tiers, base_supply: 1_000_000_000_000_000,
                                    creator_fee_configurable: true, pool_creator_fee_bps: 0,
                                    coin_creator_is_default: false, cap_bps: None },
            synthetic: None,
        };
        let l = sqrt_u128(vt * vq, Rounding::Floor);
        let p0 = vq * WAD / vt;
        let q = u64::try_from(live.rt * u128::from(q_frac) / 1_000_000).expect("fits").max(1);
        let quote = |p: u128| curve.buy_repriced(q, l, p).map(|bq| bq.cost);
        let in_pool = |p: u128| curve_reprice(l, p, curve.offsets).is_some_and(|st| st.rt < u128::from(q));
        // The price where the straddle takes over (rT(P) < q), by bisection.
        let (mut lo, mut hi) = (p0, p0 * 1_000);
        prop_assume!(!in_pool(lo) && in_pool(hi));
        while lo + 1 < hi {
            let mid = lo.midpoint(hi);
            if in_pool(mid) { hi = mid } else { lo = mid }
        }
        let split = hi;
        // V_eff: CR crosses L just before the split, or at a random price up to it.
        let target = if at_split { split - 1 } else { p0 + (split - 1 - p0) * u128::from(v_frac) / 1_000_000 };
        let Some(bt) = quote(target) else { return Ok(()) };
        let v_eff = u64::try_from(u128::from(bt) * u128::from(cr_liq) / 10_000).expect("fits").saturating_sub(1);
        let below = |p: u128| risk::cr_below(v_eff, quote(p), cr_liq).expect("quote > 0");
        let got = curve.liquidation_price(q, v_eff, cr_liq, l, p0).expect("no error").expect("some");
        prop_assert!(below(got));
        if got > p0 {
            prop_assert!(!below(got - 1));
        }
        // The curve regime crossed at `target` at the latest.
        prop_assert!(got <= target, "{got} > {target} (split {split})");
        for r in probes {
            let probe = p0 + (got - p0) * u128::from(r) / u128::from(u64::MAX);
            prop_assert!(probe == got || !below(probe), "holds at {probe} < {got}");
        }
    }
}

#[test]
fn repriced_state_has_liquidity_l() {
    // The spot state reprojects onto itself within isqrt rounding (ECON §2).
    let (x, y) = (206_900_000_000_000u128, 84_990_359_345u128);
    let l = sqrt_u128(x * y, Rounding::Floor);
    let (x2, y2) = reprice(l, y * WAD / x).expect("defined");
    assert!(x2.abs_diff(x) * 100_000 < x && y2.abs_diff(y) * 100_000 < y);
}

/// Review F-A2 on a live standard curve with 14,325,368,750 raw tokens left: `B̂_str` is
/// not monotone at `q = rT` (the post-migration pool starts ≈ 0.023 % below the curve's
/// completion price), but the dip stays within 3 bps (ARCH §4.3 as built; owner item).
#[test]
fn straddle_dip_at_rt_is_bounded() {
    let rt = 14_325_368_750u128;
    let (vt0, vs0) = (1_073_000_000_000_000u128, 30_000_000_000u128);
    let vt = 279_900_000_000_000 + rt;
    let vq = (vt0 * vs0).div_ceil(vt);
    let s = CurveState {
        vt,
        vq,
        rt,
        rq: vq - vs0,
        complete: false,
    };
    let tiers = [FeeTier {
        threshold: 0,
        fees: FeeBps::new(2, 93, 30),
    }];
    let curve = CurvePricing {
        fees: FeeBps::new(0, 95, 30),
        offsets: CurveOffsets::of(&s).expect("offsets"),
        migration: MigrationParams {
            migration_fee: 15_000_001,
            pool_tokens: 206_900_000_000_000,
            post_complete_quote_in: 0,
        },
        pool: PammFeeSchedule {
            tiers: &tiers,
            base_supply: 1_000_000_000_000_000,
            creator_fee_configurable: true,
            pool_creator_fee_bps: 0,
            coin_creator_is_default: false,
            cap_bps: None,
        },
        synthetic: None,
    };
    let rt64 = u64::try_from(rt).expect("fits");
    let a = curve.buy(rt64, &s).expect("curve").cost;
    let c = curve.buy(rt64 + 1, &s).expect("pool").cost;
    assert!(c < a, "the dip exists: {c} vs {a}");
    assert!(
        u128::from(c) * 10_000 >= u128::from(a) * 9_997,
        "{c} vs {a}"
    );
}

/// Review F-A2: the same dip in `P` on the EMA leg. With `V_eff` set so that CR < 125 %
/// first holds at or below 410,837,000,000,000 (`rT(P)` crosses `Q` at
/// 410,837,995,801,322 under the v0.4.1 reprice), the plain bisection lands past the dip
/// (410,953,497,472,145); `CurvePricing::liquidation_price` returns the first crossing.
#[test]
fn curve_liquidation_price_finds_the_first_crossing() {
    let (vt0, vs0) = (1_073_000_000_000_000u128, 30_000_000_000u128);
    let vt = vt0 - 400_000_000_000_000;
    let vq = (vt0 * vs0).div_ceil(vt);
    let live = CurveState {
        vt,
        vq,
        rt: 793_100_000_000_000 - 400_000_000_000_000,
        rq: vq - vs0,
        complete: false,
    };
    let tiers = [FeeTier {
        threshold: 0,
        fees: FeeBps::new(2, 93, 30),
    }];
    let curve = CurvePricing {
        fees: FeeBps::new(0, 95, 30),
        offsets: CurveOffsets::of(&live).expect("offsets"),
        migration: MigrationParams {
            migration_fee: 15_000_001,
            pool_tokens: 206_900_000_000_000,
            post_complete_quote_in: 0,
        },
        pool: PammFeeSchedule {
            tiers: &tiers,
            base_supply: 1_000_000_000_000_000,
            creator_fee_configurable: true,
            pool_creator_fee_bps: 0,
            coin_creator_is_default: false,
            cap_bps: None,
        },
        synthetic: None,
    };
    let l = sqrt_u128(live.vt * live.vq, Rounding::Floor);
    let q = 14_365_431_703u64;
    let quote = |p: u128| curve.buy_repriced(q, l, p).map(|b| b.cost);
    let p_dip = 410_837_000_000_000u128;
    let v_eff = quote(p_dip).expect("quote") * 12_500 / 10_000 - 1;
    let below = |p: u128| risk::cr_below(v_eff, quote(p), 12_500).expect("quote > 0");
    assert!(below(p_dip));
    let plain = risk::liquidation_price(400_000_000_000_000, v_eff, 12_500, quote)
        .expect("ok")
        .expect("some");
    assert!(
        plain > p_dip,
        "the plain bisection misses the first crossing"
    );
    let got = curve
        .liquidation_price(q, v_eff, 12_500, l, 400_000_000_000_000)
        .expect("ok")
        .expect("some");
    assert!(got <= p_dip && below(got) && !below(got - 1), "{got}");
}
