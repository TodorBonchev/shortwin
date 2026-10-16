//! Quotes against the normative sim `docs/design/sim/shortwin_sim.py` v0.4.1, default
//! switches (ARCHITECTURE §8.1, §9 M0b). Vectors: `tests/vectors/quotes/sim.json`
//! (`gen_sim.py`). Every comparison with the sim is exact (no tolerance):
//!
//! - live execution quotes and sales on PumpSwap, the curve (ECON §3.1's
//!   `⌈q·vQ/(vT − q)⌉ + 1`; past `rT` pump's synthetic migration `B̂_v3`, pump-final §5.2,
//!   and the straddle `B̂_str` of a `PumpMigrating` market's states and the retired bundle)
//!   and Raydium;
//! - risk quotes at a fee hard cap: one combined fee at the cap on pump venues
//!   (`FeeBps::capped`, the sims' `Venue._risk_fees`), `min(rate, cap)` on Raydium;
//! - quotes on hypothetical states (`Market.quote_repriced`): `reprice` with
//!   `sp = isqrt⁺(P)`, X floor, Y ceil, at the live fee tier (R3B-7 b);
//! - a complete curve's pool and FV (pump-final §5.4, P7): `migrated_pool` with `pcQ` and
//!   the live `curve_base_ata` after a pool part, `migrating_fv_price`'s ρ, `migrating_fv`
//!   (the sim's `Market.fv_parts`), and `B̂_str` on its reprojected ring states;
//! - `reprice`, `liquidity` (`sqrt_k`), `pool_select::{lambda_bps, locked_depth}`.
//!
//! The sim's own execution of a curve trade (`exact_*`: the program's `⌊·⌋ + 1` charge;
//! pump's synthetic migration past `rT`; for the retired straddle, the pool that migration
//! actually creates) equals `sol_leg::curve_charge_exact_out` on the curve and sits at most
//! `tolerance*` lamports below the quote (a quote-vs-execution bound, ECON §3.2, pump-final
//! §5.2).

mod common;

use common::Dec;
use serde::Deserialize;
use shortwin_math::fees::{self, CpmmFees, FeeBps, FeeTier, PammFeeSchedule, RayV4Fee};
use shortwin_math::pool_select;
use shortwin_math::quote::{
    self, CurveOffsets, CurvePricing, CurveState, MigrationParams, Pricing, cpmm, ray_v4, sol_leg,
};

#[derive(Deserialize)]
struct File {
    generator: String,
    pumpswap_tiers: Vec<Tier>,
    pump_migration_fee: Dec<u64>,
    pump_pool_tokens: Dec<u128>,
    pump_supply: Dec<u64>,
    pumpswap: Vec<PammCase>,
    curve: Vec<CurveCase>,
    migrating: Vec<MigratingCase>,
    migrating_ring: Vec<RingCase>,
    raydium: Vec<RayCase>,
    repriced: Vec<RepricedCase>,
    reprice: Vec<RepriceCase>,
    liquidity: Vec<LiquidityCase>,
    locked_depth: Vec<DepthCase>,
}

#[derive(Deserialize)]
struct Tier {
    threshold: Dec<u128>,
    lp: Dec<u64>,
    protocol: Dec<u64>,
    creator: Dec<u64>,
}

#[derive(Deserialize)]
struct PammCase {
    q: Dec<u64>,
    x: Dec<u128>,
    y: Dec<u128>,
    y_real: Dec<u64>,
    supply: Dec<u64>,
    fees: [Dec<u64>; 3],
    oneway_ppm: Dec<u32>,
    cap_bps: Dec<u64>,
    buy: Option<[Dec<u64>; 2]>,
    buy_risk: Option<[Dec<u64>; 2]>,
    sell: Option<Dec<u64>>,
}

#[derive(Deserialize)]
struct CurveCase {
    q: Dec<u64>,
    vt: Dec<u128>,
    vq: Dec<u128>,
    rt: Dec<u128>,
    rq: Dec<u128>,
    off_t: Dec<u128>,
    off_q: Dec<u128>,
    ema: Option<[Dec<u128>; 2]>,
    fees: [Dec<u64>; 3],
    cap_bps: Dec<u64>,
    /// `B̂_v3`'s would-be pool: the live `curve_base_ata − rT`, or 206.9e12 on a reprojection.
    pool_tokens: Dec<u128>,
    sim_v3: Option<Dec<u64>>,
    sim_risk: Option<Dec<u64>>,
    sim_str: Option<Dec<u64>>,
    sim_risk_str: Option<Dec<u64>>,
    sim_two_leg: Option<Dec<u64>>,
    exact_v3: Option<Dec<u64>>,
    exact_str: Option<Dec<u64>>,
    exact_two_leg: Option<Dec<u64>>,
    sell_checked: bool,
    sell: Option<Dec<u64>>,
    tolerance_v3: Dec<u64>,
    tolerance: Dec<u64>,
    tolerance_two_leg: Dec<u64>,
}

/// A complete curve's FV quote (`Market.fv_parts`, pump-final §5.4).
#[derive(Deserialize)]
struct MigratingCase {
    vt: Dec<u128>,
    vq: Dec<u128>,
    rq: Dec<u128>,
    pcb: Dec<u64>,
    pcq: Dec<u64>,
    pool_tokens: Dec<u128>,
    x1: Dec<u128>,
    y1: Dec<u128>,
    p: Dec<u128>,
    p1: Option<Dec<u128>>,
    cap_bps: Dec<u64>,
    q: Dec<u64>,
    fv: Option<Dec<u64>>,
}

/// `B̂_str` on a complete curve's reprojected ring state (`PumpCurve.quote_at`).
#[derive(Deserialize)]
struct RingCase {
    vt: Dec<u128>,
    vq: Dec<u128>,
    rq: Dec<u128>,
    pcq: Dec<u64>,
    pool_tokens: Dec<u128>,
    off_t: Dec<u128>,
    off_q: Dec<u128>,
    l: Dec<u128>,
    p: Dec<u128>,
    fees: [Dec<u64>; 3],
    cap_bps: Dec<u64>,
    q: Dec<u64>,
    sim_str: Option<Dec<u64>>,
}

#[derive(Deserialize)]
struct RayCase {
    q: Dec<u64>,
    x: Dec<u128>,
    y: Dec<u128>,
    fee_ppm: Dec<u64>,
    creator_ppm: Dec<u64>,
    oneway_ppm: Dec<u32>,
    cap_bps: Dec<u64>,
    buy: Option<[Dec<u64>; 2]>,
    buy_risk: Option<[Dec<u64>; 2]>,
    sell: Option<Dec<u64>>,
}

#[derive(Deserialize)]
struct RepricedCase {
    kind: String,
    x: Dec<u128>,
    y: Dec<u128>,
    supply: Dec<u64>,
    fee_ppm: Dec<u64>,
    creator_ppm: Dec<u64>,
    live_fees: Option<[Dec<u64>; 3]>,
    l: Dec<u128>,
    p: Dec<u128>,
    cap_bps: Dec<u64>,
    q: Dec<u64>,
    xy: Option<[Dec<u128>; 2]>,
    risk: Option<Dec<u64>>,
}

#[derive(Deserialize)]
struct RepriceCase {
    l: Dec<u128>,
    p: Dec<u128>,
    xy: Option<[Dec<u128>; 2]>,
}

#[derive(Deserialize)]
struct LiquidityCase {
    x: Dec<u128>,
    y: Dec<u128>,
    l: Option<Dec<u128>>,
}

#[derive(Deserialize)]
struct DepthCase {
    lp_counter: Dec<u64>,
    lp_supply: Dec<u64>,
    x: Dec<u64>,
    y: Dec<u64>,
    lambda_bps: Dec<u32>,
    locked_depth: Dec<u128>,
}

fn load() -> File {
    let f: File = common::load_vectors("quotes", "sim.json");
    assert_eq!(f.generator, "gen_sim.py");
    f
}

fn tiers(f: &File) -> Vec<FeeTier> {
    f.pumpswap_tiers
        .iter()
        .map(|t| FeeTier {
            threshold: t.threshold.0,
            fees: FeeBps::new(t.lp.0, t.protocol.0, t.creator.0),
        })
        .collect()
}

fn sched(tiers: &[FeeTier], supply: u64) -> PammFeeSchedule<'_> {
    PammFeeSchedule {
        tiers,
        base_supply: supply,
        creator_fee_configurable: false,
        pool_creator_fee_bps: 0,
        coin_creator_is_default: false,
        cap_bps: None,
    }
}

fn opt(x: &Option<Dec<u64>>) -> Option<u64> {
    x.as_ref().map(|d| d.0)
}

fn pair(x: &Option<[Dec<u64>; 2]>) -> Option<(u64, u64)> {
    x.as_ref().map(|[n, t]| (n.0, t.0))
}

/// The sim's Raydium pool as a CPMM with the creator fee on the input (its only model).
fn cpmm_fees(trade: u64, creator: u64) -> CpmmFees {
    CpmmFees {
        trade,
        creator,
        creator_on_input: true,
        protocol: 120_000,
        fund: 40_000,
    }
}

#[test]
fn pumpswap_matches_sim() {
    let f = load();
    let tiers = tiers(&f);
    assert!(f.pumpswap.len() >= 200);
    let mut combined = 0;
    for (i, c) in f.pumpswap.iter().enumerate() {
        let fee = fees::pump_amm(c.y.0, c.x.0, &sched(&tiers, c.supply.0)).unwrap();
        assert_eq!(
            fee,
            FeeBps::new(c.fees[0].0, c.fees[1].0, c.fees[2].0),
            "case {i}"
        );
        assert_eq!(fee.oneway_ppm(), c.oneway_ppm.0, "case {i}");
        let b = sol_leg::buy_exact_out(c.q.0, c.x.0, c.y.0, false, &fee);
        assert_eq!(b.map(|b| (b.net, b.cost)), pair(&c.buy), "case {i}");
        // Risk view: one combined fee at the cap when φ is above it.
        let cap = c.cap_bps.0;
        let risk_fee = fee.capped(cap);
        if fees::exceeds_cap(fee.oneway_ppm(), cap) {
            combined += 1;
            assert_eq!(risk_fee, FeeBps::new(0, cap, 0), "case {i}");
        } else {
            assert_eq!(risk_fee, fee, "case {i}");
        }
        let r = Pricing::PumpSwap(fee)
            .capped(cap)
            .buy(c.q.0, c.x.0, c.y.0)
            .map(|b| (b.net, b.cost));
        assert_eq!(r, pair(&c.buy_risk), "case {i} risk");
        // The schedule's own risk cap resolves to the same fee.
        let capped_sched = PammFeeSchedule {
            cap_bps: Some(cap),
            ..sched(&tiers, c.supply.0)
        };
        assert_eq!(
            fees::pump_amm(c.y.0, c.x.0, &capped_sched),
            Ok(risk_fee),
            "case {i}"
        );
        let s = sol_leg::sell_exact_in(c.q.0, c.x.0, c.y.0, &fee, c.y_real.0);
        assert_eq!(s.map(|s| s.proceeds), opt(&c.sell), "case {i}");
    }
    assert!(combined >= 50, "{combined}");
}

/// The curve on the sim's default switches (P7): up to `rT` ECON §3.1's quote, past it
/// `B̂_v3` — `CurvePricing` with `synthetic` (the curve's fee on both legs, `ε₁ = 1`) on the
/// case's would-be pool, live (`curve_base_ata − rT`, donations included) or the standard
/// 206.9e12 on a reprojection — and the sim's own execution of pump's synthetic buy within
/// pump-final §5.2's bound below it; `B̂_str` (a migrating market's state, the retired bundle)
/// on `CurvePricing` without `synthetic`; the two-leg `B̂₂`; sales.
#[test]
fn curve_and_straddle_match_sim() {
    let f = load();
    let tiers = tiers(&f);
    let mig = MigrationParams {
        migration_fee: f.pump_migration_fee.0,
        pool_tokens: f.pump_pool_tokens.0,
        post_complete_quote_in: 0,
    };
    let pool = sched(&tiers, f.pump_supply.0);
    let (mut straddles, mut emas, mut capped, mut donated, mut synthetic_exact) = (0, 0, 0, 0, 0);
    for (i, c) in f.curve.iter().enumerate() {
        let off = CurveOffsets {
            token: c.off_t.0,
            quote: c.off_q.0,
        };
        let state = match &c.ema {
            None => CurveState {
                vt: c.vt.0,
                vq: c.vq.0,
                rt: c.rt.0,
                rq: c.rq.0,
                complete: false,
            },
            Some([l, p]) => {
                emas += 1;
                let s = quote::curve_reprice(l.0, p.0, off).unwrap();
                assert_eq!(
                    (s.vt, s.vq, s.rt, s.rq),
                    (c.vt.0, c.vq.0, c.rt.0, c.rq.0),
                    "case {i}"
                );
                s
            }
        };
        let fee = FeeBps::new(c.fees[0].0, c.fees[1].0, c.fees[2].0);
        let q = c.q.0;
        if u128::from(q) > state.rt {
            straddles += 1;
        }
        if c.pool_tokens.0 != mig.pool_tokens {
            donated += 1;
        }
        let live = MigrationParams {
            pool_tokens: c.pool_tokens.0,
            ..mig
        };
        // The live curve's pricing (pump-final §6.2): `B̂_v3` past `rT` at the curve's fee.
        let v3 = CurvePricing {
            fees: fee,
            offsets: off,
            migration: live,
            pool,
            synthetic: Some(fee),
        };
        // A migrating market's state and the retired bundle: `B̂_str`.
        let straddle = CurvePricing {
            migration: mig,
            synthetic: None,
            ..v3
        };

        let b = quote::curve_synthetic(q, &state, &fee, &fee, &live).map(|b| b.cost);
        assert_eq!(b, opt(&c.sim_v3), "case {i} B̂_v3");
        assert_eq!(v3.buy(q, &state).map(|b| b.cost), b, "case {i}");
        if let Some([l, p]) = &c.ema {
            assert_eq!(v3.buy_repriced(q, l.0, p.0).map(|b| b.cost), b, "case {i}");
        }
        // pump trades exactly where `B̂_v3` is defined (NotEnoughTokensToBuy past the would-be
        // pool), except on a reprojected state at or past completion (`rT = 0`), which pump does
        // not trade while `B̂_v3` still prices it on the pool.
        if state.rt > 0 {
            assert_eq!(b.is_some(), c.exact_v3.is_some(), "case {i} B̂_v3 defined");
        } else {
            assert!(c.exact_v3.is_none(), "case {i}: an execution at rT = 0");
        }
        if let (Some(b), Some(e)) = (b, opt(&c.exact_v3)) {
            // pump's charge (the sim's `PumpCurve.buy`, its synthetic migration past `rT`) at
            // most pump-final §5.2's bound below the quote.
            assert!(
                b >= e && b - e <= c.tolerance_v3.0,
                "case {i}: exact {e} rust {b} (bound {})",
                c.tolerance_v3.0
            );
            synthetic_exact += usize::from(u128::from(q) > state.rt);
        }
        if u128::from(q) <= state.rt {
            let charge = sol_leg::curve_charge_exact_out(q, state.vt, state.vq, &fee);
            assert_eq!(charge.map(|b| b.cost), opt(&c.exact_v3), "case {i} charge");
            assert_eq!(opt(&c.exact_v3), opt(&c.exact_str), "case {i}");
        }

        let s = quote::curve_straddle(q, &state, &fee, &mig, &pool).map(|b| b.cost);
        assert_eq!(s, opt(&c.sim_str), "case {i} B̂_str");
        assert_eq!(straddle.buy(q, &state).map(|b| b.cost), s, "case {i}");
        if let Some([l, p]) = &c.ema {
            assert_eq!(
                straddle.buy_repriced(q, l.0, p.0).map(|b| b.cost),
                s,
                "case {i}"
            );
        }
        assert_eq!(s.is_some(), c.exact_str.is_some(), "case {i}");
        if let (Some(s), Some(e)) = (s, opt(&c.exact_str)) {
            assert!(
                s >= e && s - e <= c.tolerance.0,
                "case {i}: exact {e} rust {s}"
            );
        }

        // Risk quotes: the curve's fee (both legs of `B̂_v3`) and the migrated pool's, each
        // capped (V2-1).
        if fees::exceeds_cap(fee.oneway_ppm(), c.cap_bps.0) {
            capped += 1;
        }
        for (what, pricing, want) in [
            ("risk", v3, &c.sim_risk),
            ("risk B̂_str", straddle, &c.sim_risk_str),
        ] {
            let risk = pricing.capped(c.cap_bps.0);
            let r = risk.buy(q, &state).map(|b| b.cost);
            assert_eq!(r, opt(want), "case {i} {what}");
            if let Some([l, p]) = &c.ema {
                assert_eq!(
                    risk.buy_repriced(q, l.0, p.0).map(|b| b.cost),
                    r,
                    "case {i}"
                );
            }
        }

        let t = quote::curve_two_leg(q, &state, &fee, &mig, &pool).map(|b| b.cost);
        assert_eq!(t, opt(&c.sim_two_leg), "case {i} two-leg");
        assert_eq!(t.is_some(), c.exact_two_leg.is_some(), "case {i}");
        if let (Some(t), Some(e)) = (t, opt(&c.exact_two_leg)) {
            assert!(
                t >= e && t - e <= c.tolerance_two_leg.0,
                "case {i}: exact {e} rust {t}"
            );
        }
        if let (Some(t), Some(s)) = (t, s) {
            // V1-9: the two-leg form never prices above the executed bundle.
            assert!(t <= s, "case {i}");
        }

        if c.sell_checked {
            let s = u64::try_from(state.rq).unwrap();
            let got = sol_leg::sell_exact_in(q, state.vt, state.vq, &fee, s).map(|s| s.proceeds);
            assert_eq!(got, opt(&c.sell), "case {i} sell");
        }
    }
    assert!(
        straddles >= 50 && emas >= 50 && capped >= 50 && donated >= 20 && synthetic_exact >= 20,
        "{straddles} {emas} {capped} {donated} {synthetic_exact}"
    );
}

fn curve_tiers(f: &File) -> (Vec<FeeTier>, MigrationParams) {
    (
        tiers(f),
        MigrationParams {
            migration_fee: f.pump_migration_fee.0,
            pool_tokens: f.pump_pool_tokens.0,
            post_complete_quote_in: 0,
        },
    )
}

/// pump-final §5.4 against the sim's `Market.fv_parts` (P7): on curves completed by an exact
/// buy of `rT` or by pump's synthetic migration, with token donations before or after, the pool
/// `migrate_v2` will create is `migrated_pool` on the live `curve_base_ata` after a pool part
/// (the standard 206.9e12 without one) plus `pcQ`; FV's price is ρ-scaled after a pool part
/// (`migrating_fv_price`; `p` itself without one) and `migrating_fv` is the sim's `B̂_FV` at
/// that pool's PumpSwap tier capped, to the lamport.
#[test]
fn migrating_fv_matches_sim() {
    let f = load();
    let (tiers, standard) = curve_tiers(&f);
    let (mut rho, mut plain, mut defined) = (0, 0, 0);
    for (i, c) in f.migrating.iter().enumerate() {
        let live = CurveState {
            vt: c.vt.0,
            vq: c.vq.0,
            rt: 0,
            rq: c.rq.0,
            complete: true,
        };
        if c.pcb.0 == 0 && c.pcq.0 == 0 {
            // Without a pool part a donation is ignored (ARCH §4.3).
            assert_eq!(c.pool_tokens.0, standard.pool_tokens, "case {i}");
        }
        let m = MigrationParams {
            pool_tokens: c.pool_tokens.0,
            post_complete_quote_in: c.pcq.0,
            ..standard
        };
        assert_eq!(
            quote::migrated_pool(&live, &m),
            Some((c.x1.0, c.y1.0)),
            "case {i} (X₁, Y₁)"
        );
        let p1 = if c.pcq.0 == 0 {
            plain += 1;
            Some(c.p.0)
        } else {
            rho += 1;
            let (x0, y0) = quote::migrated_pool(&live, &standard).unwrap();
            quote::migrating_fv_price(c.p.0, x0, y0, c.x1.0, c.y1.0)
        };
        assert_eq!(p1, c.p1.as_ref().map(|d| d.0), "case {i} p'");
        let s = PammFeeSchedule {
            cap_bps: Some(c.cap_bps.0),
            ..sched(&tiers, f.pump_supply.0)
        };
        let fv = quote::migrating_fv(c.q.0, c.p.0, &live, &m, &s).map(|b| b.cost);
        assert_eq!(fv, opt(&c.fv), "case {i} B̂_FV");
        defined += usize::from(fv.is_some());
    }
    assert!(
        rho >= 100 && plain >= 40 && defined >= 150,
        "{rho} {plain} {defined}"
    );
}

/// `B̂_str` on a complete curve's reprojected ring states (P7; pump-final §5.3, G-2): the
/// pricing a `PumpMigrating` market's EMA leg reads — no synthetic route, all of `q` past the
/// state's `rT` on the pool `migrate_v2` will create from that state (`curve_base_ata` after a
/// pool part, `+ pcQ`) at its tier, capped — equals the sim's `PumpCurve.quote_at` on the
/// complete curve.
#[test]
fn migrating_ring_matches_sim() {
    let f = load();
    let (tiers, standard) = curve_tiers(&f);
    let pool = sched(&tiers, f.pump_supply.0);
    let mut past = 0;
    for (i, c) in f.migrating_ring.iter().enumerate() {
        let off = CurveOffsets {
            token: c.off_t.0,
            quote: c.off_q.0,
        };
        let state = quote::curve_reprice(c.l.0, c.p.0, off).unwrap();
        let pricing = CurvePricing {
            fees: FeeBps::new(c.fees[0].0, c.fees[1].0, c.fees[2].0),
            offsets: off,
            migration: MigrationParams {
                pool_tokens: c.pool_tokens.0,
                post_complete_quote_in: c.pcq.0,
                ..standard
            },
            pool,
            synthetic: None,
        };
        let got = pricing
            .capped(c.cap_bps.0)
            .buy(c.q.0, &state)
            .map(|b| b.cost);
        assert_eq!(got, opt(&c.sim_str), "case {i}");
        assert_eq!(
            pricing
                .capped(c.cap_bps.0)
                .buy_repriced(c.q.0, c.l.0, c.p.0)
                .map(|b| b.cost),
            got,
            "case {i}"
        );
        past += usize::from(u128::from(c.q.0) > state.rt);
        // The complete curve's own fields (the vectors' provenance).
        assert!(c.vt.0 > 0 && c.vq.0 > c.rq.0, "case {i}");
    }
    assert!(past >= 60, "{past}");
}

#[test]
fn raydium_matches_sim() {
    let f = load();
    assert!(f.raydium.len() >= 150);
    let mut capped = 0;
    for (i, c) in f.raydium.iter().enumerate() {
        let (t, cr) = (c.fee_ppm.0, c.creator_ppm.0);
        let fee = cpmm_fees(t, cr);
        assert_eq!(fee.oneway_ppm(), c.oneway_ppm.0, "case {i}");
        // The sim's pool is (X token, Y SOL); a buy pays SOL in.
        let b = cpmm::buy_exact_out(c.q.0, c.y.0, c.x.0, &fee).map(|b| (b.net, b.cost));
        assert_eq!(b, pair(&c.buy), "case {i} cpmm buy");
        let s = cpmm::sell_exact_in(c.q.0, c.x.0, c.y.0, &fee).map(|s| s.proceeds);
        assert_eq!(s, opt(&c.sell), "case {i} cpmm sell");
        let cap = c.cap_bps.0;
        capped += usize::from(fees::exceeds_cap(fee.oneway_ppm(), cap));
        let rc = Pricing::Cpmm {
            buy: fee,
            sell: fee,
        }
        .capped(cap);
        let r = rc.buy(c.q.0, c.x.0, c.y.0).map(|b| (b.net, b.cost));
        assert_eq!(r, pair(&c.buy_risk), "case {i} cpmm risk");
        if cr == 0 {
            let v4 = fees::ray_v4(t, 1_000_000).unwrap();
            assert_eq!(u64::from(v4.oneway_ppm()), t, "case {i}");
            let b =
                ray_v4::buy_exact_out(c.q.0, c.y.0, c.x.0, v4.num, v4.den).map(|b| (b.net, b.cost));
            assert_eq!(b, pair(&c.buy), "case {i} v4 buy");
            let s = ray_v4::sell_exact_in(c.q.0, c.x.0, c.y.0, v4.num, v4.den).map(|s| s.proceeds);
            assert_eq!(s, opt(&c.sell), "case {i} v4 sell");
            let r = Pricing::RayV4(v4)
                .capped(cap)
                .buy(c.q.0, c.x.0, c.y.0)
                .map(|b| (b.net, b.cost));
            assert_eq!(r, pair(&c.buy_risk), "case {i} v4 risk");
        }
    }
    assert!(capped >= 30, "{capped}");
}

/// Risk quotes on hypothetical states (`Market.quote_repriced`): `reprice(L, P)` quoted at
/// the fees resolved on the live state (R3B-7 b), capped at the hard cap (V2-1).
#[test]
fn repriced_quotes_match_sim() {
    let f = load();
    let tiers = tiers(&f);
    let (mut defined, mut tier_differs) = (0, 0);
    for (i, c) in f.repriced.iter().enumerate() {
        let (l, p, q, cap) = (c.l.0, c.p.0, c.q.0, c.cap_bps.0);
        assert_eq!(
            quote::reprice(l, p),
            c.xy.as_ref().map(|[x, y]| (x.0, y.0)),
            "case {i}"
        );
        let live = match c.kind.as_str() {
            "pumpswap" => {
                let s = sched(&tiers, c.supply.0);
                let fee = fees::pump_amm(c.y.0, c.x.0, &s).unwrap();
                let lf = c.live_fees.as_ref().unwrap();
                assert_eq!(fee, FeeBps::new(lf[0].0, lf[1].0, lf[2].0), "case {i}");
                // Where the tier at the repriced market cap differs, the live one is
                // normative.
                if let Some((x, y)) = quote::reprice(l, p)
                    && fees::pump_amm(y, x, &s).is_ok_and(|h| h != fee)
                {
                    tier_differs += 1;
                }
                Pricing::PumpSwap(fee)
            }
            "raydium" => {
                let fee = cpmm_fees(c.fee_ppm.0, c.creator_ppm.0);
                Pricing::Cpmm {
                    buy: fee,
                    sell: fee,
                }
            }
            k => panic!("kind {k}"),
        };
        let got = live.capped(cap).buy_repriced(q, l, p).map(|b| b.cost);
        assert_eq!(got, opt(&c.risk), "case {i}");
        defined += usize::from(got.is_some());
    }
    assert!(
        defined >= 300 && tier_differs >= 10,
        "{defined} {tier_differs}"
    );
}

#[test]
fn reprice_matches_sim() {
    let f = load();
    for c in &f.reprice {
        let want = c.xy.as_ref().map(|[x, y]| (x.0, y.0));
        assert_eq!(
            quote::reprice(c.l.0, c.p.0),
            want,
            "L {} P {}",
            c.l.0,
            c.p.0
        );
        if let Some((x, _)) = want {
            assert_eq!(quote::reprice_token(c.l.0, c.p.0), Ok(x));
        }
    }
}

#[test]
fn liquidity_matches_sim() {
    let f = load();
    for c in &f.liquidity {
        assert_eq!(
            quote::liquidity(c.x.0, c.y.0).ok(),
            c.l.as_ref().map(|l| l.0),
            "X {} Y {}",
            c.x.0,
            c.y.0
        );
    }
}

#[test]
fn locked_depth_matches_sim() {
    let f = load();
    assert!(f.locked_depth.len() >= 100);
    for (i, c) in f.locked_depth.iter().enumerate() {
        let (counter, supply) = (c.lp_counter.0, c.lp_supply.0);
        assert_eq!(
            pool_select::lambda_bps(counter, supply),
            c.lambda_bps.0,
            "case {i}"
        );
        assert_eq!(
            pool_select::locked_depth(counter, supply, c.x.0, c.y.0),
            Ok(c.locked_depth.0),
            "case {i}"
        );
    }
}

/// AMM v4's `num/den` rate in ppm is `⌈num·10⁶/den⌉` (ARCH §4.2), which the sim's
/// `fee_ppm` is for the 25/10,000 pools of every sampled market.
#[test]
fn v4_rate_in_ppm() {
    let v4: RayV4Fee = fees::ray_v4(25, 10_000).unwrap();
    assert_eq!(v4.oneway_ppm(), 2_500);
    assert_eq!(fees::ray_v4(1, 3).unwrap().oneway_ppm(), 333_334);
}
