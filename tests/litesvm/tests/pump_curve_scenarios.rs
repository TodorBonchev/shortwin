//! ECON §11.2's deterministic curve scenarios and ECON §10.1's stuck migrations, reproduced on
//! the fork with the release short.win and the dumped pump ELFs (M5 exit criterion "ECON §11.2
//! curve scenarios"; ARCH §8.2, §9): ECON's `D@50%`, `D@80%` and `D@90%` markets built through
//! real instructions ([`Sim::d_market`]), the position at the per-position cap and 80 %
//! utilisation, and `shortwin_sim.run` replayed second by second ([`Sim::run`]: the keeper's
//! cadence, other users' real `buy_v2` / `sell_v2` along the price path, the keeper's exit one
//! second after the position becomes actionable). The invariant hooks run after every
//! instruction.
//!
//! **To the lamport.** The constants are the sim's own numbers, printed by
//! `shortwin_sim.run(new_market(key), 1.0, SC[name], keeper_latency=1)` (and, for the stuck
//! migrations, `GRAD_SCENARIOS[3]` with `grad_extra`), from which ECON's tables are rounded.
//! Every row asserts the sim's second of the outcome, the number of keeper cranks, the spot at
//! the exit, the open's `S`, `M`, `V`, `N_open` and fee, the exit's buyback cost, reference,
//! bound, penalty / fee, keeper pay, settlement amounts and FV exactly. The one modelled
//! difference is `R_min` (`pump_curve_support.rs` module docs): where the sim's rent due is its
//! `R_min` (80 % utilisation) the program's is `max(accrued, r_min)` with `r_min` at the
//! pre-open 0 %, and the legs below the rent — the borrower's remainder, or an FV-capped
//! lenders' take — move by exactly the difference `ΔR`; where the sim's rent due is the accrued
//! rent (the expiries, the D@50 % stuck migration) the program's is identical. ECON's rounded
//! borrower P&L and "lender gets" therefore differ by `ΔR` (≤ 0.0015 SOL) on the rows whose
//! rent is `R_min`, and match to every printed digit otherwise.
//!
//! | ECON row | Test |
//! |---|---|
//! | D@50% gap 2x — open, CR 2.05/2.05 | [`d50_gap_2x_stays_open`] |
//! | D@50% gap 5x — S1 +51 s, lenders `V − K_s` 2.8738, keeper 0.0391 | [`d50_gap_5x_settles_s1`] |
//! | D@50% pump 10x/60s — liquidation +31 s at 3.29×, keeper 0.0705 | [`d50_pump_10x_60s_is_liquidated`] |
//! | D@50% pump 10x/10min — liquidation +230 s at 2.42× (+231 s at the program's `r_min`, see the test) | [`d50_pump_10x_10min_is_liquidated`] |
//! | D@50% pump 3x/1h — liquidation +2789 s at 2.34×, keeper 0.0501 | [`d50_pump_3x_1h_is_liquidated`] |
//! | D@50% dump −50 %, close — close +3600 s, borrower +0.3344 | [`d50_dump_then_close`] |
//! | D@50% flat to expiry — expiry +259201 s, lenders 0.0334, keeper 0.0017 | [`d50_flat_to_expiry`] |
//! | D@90% dump −50 %, close — close +3600 s, borrower +0.6298 | [`d90_dump_then_close`] |
//! | D@90% flat to expiry — expiry +259201 s, lenders 0.0628, keeper 0.0023 | [`d90_flat_to_expiry`] |
//! | ECON §10.1 D@50% graduation, migration stuck — S3 +4200 s, lenders 2.8738 | [`d50_stuck_migration_settles_s3_at_70_min`] |
//! | ECON §10.1 D@80% graduation, migration stuck — S3 +4200 s at FV, lenders 3.0872 | [`d80_stuck_migration_settles_s3_at_fair_value`] |
//! | ECON §10.1 D@90% graduation, migration stuck — open +10800 s, CR_EMA 2.40 | [`d90_stuck_migration_stays_open`] |
//!
//! **Not reproducible in M5** (recorded, not skipped silently):
//! - every `D@0%` row: the sim opens its short on a curve with no real SOL (`rQ = 0`); pump
//!   pays a sale out of `rQ`, so no short can open there on a real curve — asserted by
//!   [`d0_the_sims_position_cannot_open_on_a_real_curve`];
//! - `D@50%` gap 10x and spike 10x/5s, every `D@90%` gap / pump / spike row and ECON §10.1's
//!   graduation-gap and "+2x / +3x on AMM" rows: the path completes the curve (at 5.84× from
//!   50 %, 1.65× from 90 %) and the sim continues on the migrated PumpSwap pool. The program's
//!   `→ PumpSwap` transition is built (M6: `pamm_it::graduation`, `pump_curve_e2e.rs` with open
//!   positions, the pause, S3 at 70 min, `→ PumpSwap` and `→ None`). **ECON §10.1's twelve
//!   graduation rows run in `graduation_scenarios.rs`** (through a real `migrate_v2` and the
//!   `→ PumpSwap` transition, to the lamport against the sim with M6's graduation rule; its module
//!   docs list the three modelled differences from ECON's published table), so they are not
//!   duplicated here. Still not replayed on the fork: §11.2's `D@50%` gap 10x / spike 10x/5s and
//!   the `D@90%` gap / pump / spike rows (`graduation_support::Grad::run` can drive them).

#![allow(clippy::result_large_err)]

#[path = "pump_curve_support.rs"]
mod support;

use shortwin_math::rent;
use support::*;

/// A sim market and its position at the cap (the sim's `max_open_q()`, `Market.open`).
struct D {
    /// Tokens bought from the fresh curve (`int(PUMP_RT0·x)`).
    sold: u64,
    /// The curve then (`PumpCurve.at_progress(x)`).
    vt: u128,
    vq: u128,
    q: u64,
    s: u64,
    m: u64,
    v: u64,
    n_open: u64,
    fee_open: u64,
    /// The sim's `R_min` (80 % utilisation).
    r_min: u64,
    /// `P1`: the spot right after the open.
    p1: u128,
}

const D50: D = D {
    sold: SOLD_D50,
    vt: 676_450_000_000_000,
    vq: 47_586_665_682,
    q: 10_071_769_920_663,
    s: 689_404_100,
    m: 2_223_494_440,
    v: 2_912_898_540,
    n_open: 706_857_373,
    fee_open: 1_767_144,
    r_min: 1_294_111,
    p1: 68_298_686_219_984,
};

const D80: D = D {
    sold: 634_480_000_000_000,
    vt: 438_520_000_000_000,
    vq: 73_406_002_007,
    q: 6_529_192_912_424,
    s: 1_063_457_547,
    m: 3_429_906_865,
    v: 4_493_364_412,
    n_open: 0, // not printed by the stuck-migration run; checked through S, M and V
    fee_open: 2_725_952,
    r_min: 1_996_263,
    p1: 162_519_299_265_941,
};

const D90: D = D {
    sold: SOLD_D90,
    vt: 359_210_000_000_000,
    vq: 89_613_318_115,
    q: 5_348_333_909_677,
    s: 1_298_258_410,
    m: 4_187_196_266,
    v: 5_485_454_676,
    n_open: 1_331_125_716,
    fee_open: 3_327_815,
    r_min: 2_437_017,
    p1: 242_207_125_279_645,
};

/// ECON's market `d` on the fork and its position at the cap, the open checked against the sim:
/// the curve at `at_progress(x)`, `S`, `M`, `V`, `N_open`, the fee and `P1` exactly; `r_min` at
/// the pre-open utilisation (0 %) where the sim's is at 80 % (the documented difference).
fn open(d: &D) -> (Sim, Pos) {
    let mut s = Sim::d_market(d.sold, lend_for_80(d.q));
    let c = curve(&s.env, &s.pm);
    assert_eq!(
        (c.vt, c.vq),
        (d.vt, d.vq),
        "the curve is the sim's at_progress(x)"
    );
    let p = s.open(d.q);
    assert_eq!(p.open.tier, shortwin_math::tier::Tier::D as u8);
    assert_eq!(
        (p.open.q, p.open.s, p.open.m, p.open.v, p.open.fee_open),
        (d.q, d.s, d.m, d.v, d.fee_open),
        "Q, S, M, V and the open fee are the sim's"
    );
    if d.n_open > 0 {
        assert_eq!(p.open.n_open, d.n_open, "N_open = the sim's B_open");
    }
    assert_eq!(s.spot(), d.p1, "P1");
    let pos = s.position(&p);
    let m = market(&s.env, &s.pm);
    assert_eq!(
        rent::utilization_bps(m.tokens_free, p.open.q).unwrap(),
        8_000,
        "exactly 80 % utilisation once Q is borrowed"
    );
    // The EMA at the open is the listing price (nothing moved since the listing).
    let p0 = m.ema_price.get();
    let r = |u| {
        rent::min_rent(
            d.q,
            p0,
            &pos.rent_terms(),
            u,
            8_000,
            u32::try_from(4 * 3_600).unwrap(),
        )
        .unwrap()
    };
    assert_eq!(r(8_000), d.r_min, "the sim's R_min: 4 h at 80 %");
    assert_eq!(
        pos.r_min,
        r(0),
        "the program's r_min: 4 h at the pre-open 0 %"
    );
    (s, p)
}

/// `(V − R)·10⁴ // B̂` (ECON's CR columns at the sim's rent `r`).
fn cr_at(v: u64, r: u64, b: u64) -> u64 {
    (u128::from(v - r) * 10_000 / u128::from(b)) as u64
}

/// The run's cranks equal the sim's `step_to` calls (`n_sim`; an open row's final duplicate
/// crank excluded).
fn assert_steps(r: &RunResult, n_sim: usize, t: i64) {
    assert_eq!(r.t, t, "the sim's second of the outcome");
    assert_eq!(r.steps.len(), n_sim, "the sim's keeper cadence");
    assert!(
        r.dust_skipped.is_empty(),
        "no dust sale: {:?}",
        r.dust_skipped
    );
}

/// A liquidation as the sim computed it (lamports; `rent` is the sim's `R_min`).
struct SimLiq {
    b: u64,
    rent: u64,
    pen: u64,
    keeper: u64,
    protocol: u64,
    lender: u64,
    lump: u64,
    borrower: u64,
    b_ref: u64,
    max_in: u64,
}

/// Compares a liquidation with the sim's: `B`, `B̂_ref`, `max_in`, the penalty and its split
/// exactly; the rent is the program's `max(accrued, r_min)` (≤ the sim's `R_min`) and the
/// borrower's remainder moves by exactly `ΔR`; the waterfall conserves `V`.
fn assert_liq(ev: &Liquidate, sim: &SimLiq, pos: &ShortPosition) -> u64 {
    assert!(!ev.expired);
    assert_eq!(ev.buyback_cost, sim.b, "B");
    assert_eq!(ev.b_ref, sim.b_ref, "B̂_ref");
    assert_eq!(ev.max_in, sim.max_in, "max_in");
    assert_eq!(ev.penalty, sim.pen, "penalty");
    assert_eq!(ev.penalty_keeper + ev.keeper_topup, sim.keeper, "keeper");
    assert_eq!(ev.keeper_topup, 0);
    let rp = |r: u64| r * u64::from(pos.rent_protocol_bps) / 10_000;
    assert_eq!(
        ev.penalty_protocol,
        sim.protocol - rp(sim.rent),
        "penalty protocol"
    );
    assert_eq!(ev.penalty_lender, sim.lump, "penalty lenders (vests)");
    assert_eq!(
        sim.lender,
        (sim.rent - rp(sim.rent)) + sim.lump,
        "the sim's lender take = rent share + penalty share"
    );
    assert!(ev.rent <= sim.rent, "rent ≤ the sim's R_min");
    assert!(ev.rent >= pos.r_min);
    let d = sim.rent - ev.rent;
    assert_eq!(
        ev.borrower_payout,
        sim.borrower + d,
        "borrower (rent adjusted)"
    );
    assert_eq!(
        ev.buyback_cost + ev.rent + ev.penalty + ev.keeper_topup + ev.borrower_payout,
        pos.v
    );
    assert!(u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * 10_200);
    d
}

/// The rent-adjusted CR at the exit equals ECON's (the sim's `cr_s` / `cr_e` in bps).
fn assert_cr(st: &Status, v: u64, sim_rent: u64, cr: (u64, u64)) {
    let (b, e) = (st.b_live.expect("B̂_live"), st.b_ema.expect("B̂_ema"));
    assert_eq!(
        (cr_at(v, sim_rent, b), cr_at(v, sim_rent, e)),
        cr,
        "CR spot / EMA at the exit (at the sim's rent)"
    );
}

fn liquidation(r: &RunResult) -> &Liquidate {
    match &r.outcome {
        Outcome::Liquidated(l) => l,
        o => panic!("expected a liquidation, got {}", o.kind()),
    }
}

// ---- D@50% ------------------------------------------------------------------------------------

/// ECON §11.2 `D@50% | gap 2x | open | +3600s | 2.00x / 2.00x | 2.05/2.05`: a step to 2× at
/// `TE` held for an hour never makes the position liquidatable (CR 2.05 against 1.75 on both
/// legs); the curve, `P_ema` and `L_ema` end exactly where the sim's do.
#[test]
fn d50_gap_2x_stays_open() {
    let (mut s, p) = open(&D50);
    let r = s.run(&p, &Scenario::gap(2.0));
    assert!(matches!(r.outcome, Outcome::Open));
    assert_steps(&r, 3_729, 7_200);
    let c = curve(&s.env, &s.pm);
    assert_eq!((c.vt, c.vq), (485_444_198_944_178, 66_310_402_042));
    let m = market(&s.env, &s.pm);
    assert_eq!(
        (m.ema_price.get(), m.ema_liq.get()),
        (136_597_372_439_963, 5_673_623_181_080),
        "P_ema and L_ema are the sim's"
    );
    assert_eq!(
        (r.status.b_live, r.status.b_ema),
        (Some(1_422_487_636), Some(1_422_487_671))
    );
    assert_cr(&r.status, D50.v, D50.r_min, (20_468, 20_468));
    assert!(!r.status.cr_liq() && !r.status.expired);
}

/// ECON §11.2 `D@50% | gap 5x | S1 | +51s | 5.00x / 5.00x | 0.81/0.81 | 3.439 | -2.2253 (-100%
/// of M) | 0% + 2.8738 SOL | -0.5657 (-16.4%) | 0.0391`: the buyback no longer fits `V − K_s`
/// (liquidate refuses `UseSolSettle`), the keeper's `sol_settle` arms the clock and settles S1
/// after `settle_persist_n` EMA updates: lenders `V − K_s`, never capped (D3), the borrower
/// nothing — exactly the sim's amounts (no rent enters S1).
#[test]
fn d50_gap_5x_settles_s1() {
    let (mut s, p) = open(&D50);
    let r = s.run(&p, &Scenario::gap(5.0));
    assert_steps(&r, 180, 3_651);
    let Outcome::Settled(ev) = &r.outcome else {
        panic!("expected S1, got {}", r.outcome.kind())
    };
    assert_eq!(ev.condition, 1, "S1");
    assert_eq!(ev.k_s, 39_128_985);
    assert_eq!(
        ev.lender_amount, 2_873_769_555,
        "lenders: V − K_s (0.0391 to the keeper)"
    );
    assert_eq!(ev.borrower_amount, 0);
    assert_eq!(ev.lump, 0, "S1: nothing vests");
    assert_eq!(
        r.status.spot,
        Some(341_493_431_099_921),
        "5.00× at the exit"
    );
    assert_eq!(
        (r.status.b_live, r.status.b_ema),
        (Some(3_600_551_433), Some(3_600_055_346))
    );
    assert_cr(&r.status, D50.v, D50.r_min, (8_086, 8_087));
    // ECON: lender P&L vs holding at the terminal 5× price −0.5657 SOL (−16.4 %).
    let term = (D50.p1 as f64 * 5.0) as u128;
    let value = (u128::from(D50.q) * term / W) as i128;
    assert_eq!(sol4(i128::from(ev.lender_amount) - value), "-0.5657");
    assert!(
        read::account(
            &s.env.svm,
            &pda::position(&s.pm.market(), &p.key(), p.nonce).0
        )
        .is_none()
    );
}

/// ECON §11.2 `D@50% | pump 10x/60s | liquidation | +31s | 3.29x / 10.00x | 1.24/1.53 | 6.879 |
/// -1.8999 (-85% of M) | 100% + 0.1186 SOL | +0.1186 (+1.7%) | 0.0705`: liquidated by normal
/// buyback on the curve (pump's own `buy_v2` through the trader), `q` back in the lending vault.
#[test]
fn d50_pump_10x_60s_is_liquidated() {
    let (mut s, p) = open(&D50);
    let pos = s.position(&p);
    let vault0 = tokens(&s.env, &s.pm.keys.pool_vault);
    let r = s.run(&p, &Scenario::pump(10.0, 60, 3_600));
    assert_steps(&r, 160, 3_631);
    assert_eq!(
        r.status.spot,
        Some(224_429_021_739_140),
        "3.29× at the exit"
    );
    let d = assert_liq(
        liquidation(&r),
        &SimLiq {
            b: 2_351_179_884,
            rent: 1_294_111,
            pen: 235_036_088,
            keeper: 70_510_826,
            protocol: 47_201_333,
            lender: 118_618_040,
            lump: 117_518_045,
            borrower: 325_388_457,
            b_ref: 2_350_360_875,
            max_in: 2_397_368_092,
        },
        &pos,
    );
    assert_cr(&r.status, D50.v, D50.r_min, (12_383, 15_328));
    assert_eq!(
        tokens(&s.env, &s.pm.keys.pool_vault) - vault0,
        D50.q,
        "q back"
    );
    // ECON's borrower P&L −1.8999 (−85 % of M) at the sim's R_min; the program's is ΔR higher.
    let ev = liquidation(&r);
    let pnl = i128::from(ev.borrower_payout) - i128::from(D50.m + D50.fee_open);
    assert_eq!(sol4(pnl - i128::from(d)), "-1.8999");
}

/// ECON §11.2 `D@50% | pump 10x/10min | liquidation | +230s | 2.42x / 10.00x | 1.69/1.73 | 6.879
/// | -1.2088 (-54% of M) | 100% + 0.0872 SOL | +0.0872 (+1.3%) | 0.0517`.
///
/// The one row the `R_min` difference moves by a second: at `TE + 229` the EMA leg's third ring
/// entry is below `L` at the sim's rent (`R_min` 1,294,111) but not at the program's (the
/// accrued 345,516, `V_eff` 0.03 % higher), so the keeper's exit lands at `+231 s` instead of
/// `+230 s`. The sim run with the program's `r_min` (its `Market.open` pricing `R_min` at 0 %
/// utilisation, everything else unchanged) gives exactly this fork run: liquidated at `+231 s`
/// at 2.43×, `B` 1.7296, lenders 0.0868, keeper 0.0519, borrower −1.2153 — the ECON outcome
/// (a normal liquidation, lenders whole, the same 1.69/1.73 CRs to two digits).
#[test]
fn d50_pump_10x_10min_is_liquidated() {
    let (mut s, p) = open(&D50);
    let pos = s.position(&p);
    let r = s.run(&p, &Scenario::pump(10.0, 600, 3_600));
    assert_steps(&r, 360, 3_831);
    assert_eq!(
        r.status.spot,
        Some(165_734_281_464_346),
        "2.43× at the exit"
    );
    // The program-r_min sim's figures: the rent is the accrued one (above r_min), so exact.
    let d = assert_liq(
        liquidation(&r),
        &SimLiq {
            b: 1_729_631_031,
            rent: 345_928,
            pen: 172_916_928,
            keeper: 51_875_078,
            protocol: 34_635_274,
            lender: 86_752_504,
            lump: 86_458_465,
            borrower: 1_010_004_653,
            b_ref: 1_729_169_271,
            max_in: 1_763_752_656,
        },
        &pos,
    );
    assert_eq!(d, 0, "the rent is the accrued one: no adjustment");
    assert_eq!(r.status.r_due, 345_928);
    assert_eq!(
        (r.status.cr_s, r.status.cr_e),
        (16_839, 17_232),
        "CR 1.68/1.72 (ECON 1.69/1.73 at +230 s)"
    );
    let ev = liquidation(&r);
    let pnl = i128::from(ev.borrower_payout) - i128::from(D50.m + D50.fee_open);
    assert_eq!(sol4(pnl), "-1.2153");
    let rp = ev.rent * 1_500 / 10_000;
    assert_eq!(
        sol4(i128::from(ev.rent - rp + ev.penalty_lender)),
        "+0.0868"
    );
    assert_eq!(sol4(i128::from(ev.penalty_keeper)), "+0.0519");
}

/// ECON §11.2 `D@50% | pump 3x/1h | liquidation | +2789s | 2.34x / 3.00x | 1.74/1.75 | 2.064 |
/// -1.1494 (-52% of M) | 100% + 0.0845 SOL | +0.0845 (+4.1%) | 0.0501`: a slow pump the EMA
/// tracks; the liquidation lands 1 s after both legs hold, within the fixed 2 % bound.
#[test]
fn d50_pump_3x_1h_is_liquidated() {
    let (mut s, p) = open(&D50);
    let pos = s.position(&p);
    let r = s.run(&p, &Scenario::pump(3.0, 3_600, 4_000));
    assert_steps(&r, 2_918, 6_389);
    assert_eq!(
        r.status.spot,
        Some(159_973_773_248_057),
        "2.34× at the exit"
    );
    let d = assert_liq(
        liquidation(&r),
        &SimLiq {
            b: 1_668_829_093,
            rent: 1_294_111,
            pen: 166_877_654,
            keeper: 50_063_296,
            protocol: 33_569_646,
            lender: 84_538_823,
            lump: 83_438_828,
            borrower: 1_075_897_682,
            b_ref: 1_668_776_540,
            max_in: 1_702_152_070,
        },
        &pos,
    );
    assert_cr(&r.status, D50.v, D50.r_min, (17_446, 17_479));
    let ev = liquidation(&r);
    let pnl = i128::from(ev.borrower_payout) - i128::from(D50.m + D50.fee_open);
    assert_eq!(sol4(pnl - i128::from(d)), "-1.1494");
}

/// ECON §11.2 `D@50% | dump -50%, close | close | +3600s | 0.50x / 0.50x | 8.27/8.26 | 0.344 |
/// +0.3344 (+15% of M) | 100% + 0.0011 SOL`: other users sell the curve down 50 % over an hour
/// (real `sell_v2`, paid out of `rQ`), the borrower closes at `2·TE`: pump's charge, the rent,
/// the rest to the borrower.
#[test]
fn d50_dump_then_close() {
    let (mut s, p) = open(&D50);
    let pos = s.position(&p);
    let r = s.run(&p, &Scenario::dump_close());
    assert_steps(&r, 3_728, 7_200);
    let Outcome::Closed(ev) = &r.outcome else {
        panic!("expected a close, got {}", r.outcome.kind())
    };
    assert_eq!(
        r.status.spot,
        Some(34_149_343_110_664),
        "0.50× at the close"
    );
    assert_eq!(ev.buyback_cost, 351_894_100, "B: the sim's");
    let sim_rent = 1_294_111;
    assert!(ev.rent < sim_rent && ev.rent >= pos.r_min);
    let d = sim_rent - ev.rent;
    assert_eq!(
        ev.borrower_payout,
        2_559_710_329 + d,
        "borrower (rent adjusted)"
    );
    assert_eq!(ev.rent_protocol, ev.rent * 1_500 / 10_000);
    assert_cr(&r.status, D50.v, sim_rent, (82_740, 82_644));
    let pnl = i128::from(ev.borrower_payout) - i128::from(D50.m + D50.fee_open);
    assert_eq!(sol4(pnl - i128::from(d)), "+0.3344");
}

/// ECON §11.2 `D@50% | flat to expiry | expiry | +259201s | 1.00x / 1.00x | 4.09/4.09 | 0.688 |
/// -0.0577 (-3% of M) | 100% + 0.0334 SOL | +0.0334 (+4.8%) | 0.0017`: 72 h of cranks every 30 s,
/// then every second from the last minute; the keeper's `expire` one second after the
/// deadline: every leg exactly the sim's (the rent is the accrued one, above both `R_min`s).
#[test]
fn d50_flat_to_expiry() {
    let (mut s, p) = open(&D50);
    let r = s.run(&p, &Scenario::flat_to_expiry());
    assert_steps(&r, 8_703, 259_201);
    let Outcome::Expired(ev) = &r.outcome else {
        panic!("expected an expiry, got {}", r.outcome.kind())
    };
    assert_eq!(
        (
            ev.buyback_cost,
            ev.rent,
            ev.expiry_fee,
            ev.keeper_reward,
            ev.borrower_payout
        ),
        (
            706_857_371,
            22_615_773,
            14_137_148,
            1_706_857,
            2_167_581_391
        ),
        "B, rent, expiry fee, K_e, borrower: the sim's"
    );
    assert_eq!((ev.b_ref, ev.max_in), (706_857_386, 713_925_959));
    assert_eq!(
        ev.b_live_risk, 706_857_373,
        "the fee base min(B̂_live,risk, B̂_ref)"
    );
    assert_eq!(r.status.spot, Some(D50.p1));
    let rp = ev.rent * 1_500 / 10_000;
    assert_eq!(ev.rent - rp + ev.expiry_fee, 33_360_556, "lenders 0.0334");
    assert_eq!(rp, 3_392_365, "protocol");
    let pnl = i128::from(ev.borrower_payout) - i128::from(D50.m + D50.fee_open);
    assert_eq!(sol4(pnl), "-0.0577");
}

// ---- D@90% ------------------------------------------------------------------------------------

/// ECON §11.2 `D@90% | dump -50%, close | close | +3600s | 0.50x / 0.50x | 8.27/8.26 | 0.648 |
/// +0.6298 (+15% of M) | 100% + 0.0021 SOL`.
#[test]
fn d90_dump_then_close() {
    let (mut s, p) = open(&D90);
    let pos = s.position(&p);
    let r = s.run(&p, &Scenario::dump_close());
    assert_steps(&r, 3_728, 7_200);
    let Outcome::Closed(ev) = &r.outcome else {
        panic!("expected a close, got {}", r.outcome.kind())
    };
    assert_eq!(
        r.status.spot,
        Some(121_103_562_640_699),
        "0.50× at the close"
    );
    assert_eq!(ev.buyback_cost, 662_672_986);
    let sim_rent = D90.r_min;
    assert!(ev.rent < sim_rent && ev.rent >= pos.r_min);
    let d = sim_rent - ev.rent;
    assert_eq!(
        ev.borrower_payout,
        4_820_344_673 + d,
        "borrower (rent adjusted)"
    );
    assert_cr(&r.status, D90.v, sim_rent, (82_740, 82_644));
    let pnl = i128::from(ev.borrower_payout) - i128::from(D90.m + D90.fee_open);
    assert_eq!(sol4(pnl - i128::from(d)), "+0.6298");
}

/// ECON §11.2 `D@90% | flat to expiry | expiry | +259201s | 1.00x / 1.00x | 4.09/4.09 | 1.295 |
/// -0.1077 (-3% of M) | 100% + 0.0628 SOL | +0.0628 (+4.8%) | 0.0023`.
#[test]
fn d90_flat_to_expiry() {
    let (mut s, p) = open(&D90);
    let r = s.run(&p, &Scenario::flat_to_expiry());
    assert_steps(&r, 8_703, 259_201);
    let Outcome::Expired(ev) = &r.outcome else {
        panic!("expected an expiry, got {}", r.outcome.kind())
    };
    assert_eq!(
        (
            ev.buyback_cost,
            ev.rent,
            ev.expiry_fee,
            ev.keeper_reward,
            ev.borrower_payout
        ),
        (
            1_331_125_715,
            42_589_125,
            26_622_515,
            2_331_125,
            4_082_786_196
        )
    );
    assert_eq!((ev.b_ref, ev.max_in), (1_331_125_885, 1_344_437_143));
    let rp = ev.rent * 1_500 / 10_000;
    assert_eq!(ev.rent - rp + ev.expiry_fee, 62_823_272, "lenders 0.0628");
    assert_eq!(rp, 6_388_368);
    let pnl = i128::from(ev.borrower_payout) - i128::from(D90.m + D90.fee_open);
    assert_eq!(sol4(pnl), "-0.1077");
}

// ---- ECON §10.1: graduation, migration stuck ---------------------------------------------------

/// The migrating curve every stuck row ends on: all of `rT` bought, `migrate_v2` never called
/// (the sim's final `PumpCurve` after `complete_curve`).
fn assert_completed(s: &Sim, vq: u128) {
    let c = curve(&s.env, &s.pm);
    assert!(c.complete);
    assert_eq!((c.vt, c.vq, c.rt), (279_900_000_000_000, vq, 0));
    assert_eq!(venue(&s.env, &s.pm), VenueKind::PumpMigrating);
    let m = market(&s.env, &s.pm);
    assert_eq!(
        m.migration_seen_at,
        s.chain(TE),
        "migration seen at the completing second"
    );
}

/// ECON §10.1 `D@50% | graduation, migration stuck | S3 | +4200s | 6.02x / 6.02x | 0.00/0.66 |
/// 4.138 | -2.2253 (-100% of M) | 0% + 2.8738 SOL | -1.2645 (-30.6%) | 0.0391`: another user's
/// buy of all `rT` completes the curve at `TE` (the market moves to `PumpMigrating` at the
/// keeper's crank that second); nobody migrates. Exits pause; the keeper's `sol_settle` every
/// second does nothing until `migration_seen_at + max_migration_pause_s + s3_wait_s` (70 min),
/// then settles S3: FV on the deterministic post-migration pool at `P_ema` with the 10 %
/// penalty is above `V − K_s`, so the lenders take `V − K_s` (the sim's rent here is the accrued
/// one: every amount is exact).
#[test]
fn d50_stuck_migration_settles_s3_at_70_min() {
    let (mut s, p) = open(&D50);
    let pos = s.position(&p);
    let r = s.run(&p, &Scenario::stuck_migration());
    assert_steps(&r, 4_329, 7_800);
    assert_eq!(
        r.t,
        TE + i64::from(pos.max_migration_pause_s) + i64::from(pos.s3_wait_s)
    );
    assert_completed(&s, 115_005_359_060);
    let Outcome::Settled(ev) = &r.outcome else {
        panic!("expected S3, got {}", r.outcome.kind())
    };
    assert_eq!(ev.condition, 3, "S3");
    assert_eq!(
        ev.fv, 4_847_411_474,
        "FV = B̂_FV 4,404,451,737 + 10 % + R 2,514,563"
    );
    assert_eq!(ev.k_s, 39_128_985);
    assert_eq!(
        ev.lender_amount, 2_873_769_555,
        "min(V − K_s, FV) = V − K_s"
    );
    // A migrating curve cannot execute: the tokens count for nothing immediate, so all of the
    // take above `R_due` vests (AIR-09).
    assert_eq!(
        (ev.borrower_amount, ev.lump),
        (0, 2_873_769_555 - 2_514_563)
    );
    assert_eq!(
        (ev.pprime_lo, ev.pprime_hi),
        (1_000, 1_000),
        "the penalty: CR-liquidatable"
    );
    let m = market(&s.env, &s.pm);
    assert_eq!(
        (m.ema_price.get(), m.ema_liq.get()),
        (410_880_168_131_470, 5_673_623_181_075),
        "the EMA integrated the completed curve's price"
    );
    assert_eq!(r.status.r_due, 2_514_563, "the sim's rent due (accrued)");
    assert_eq!(
        r.status.b_ema,
        Some(4_403_349_180),
        "B̂_str on the newest ring entry"
    );
    assert_eq!(r.status.cr_e, 6_609, "CR_EMA 0.66");
    assert_eq!(
        (r.status.b_live, r.status.cr_s),
        (None, 0),
        "no route: CR_spot 0"
    );
}

/// ECON §10.1 `D@80% | graduation, migration stuck | S3 | +4200s | 2.53x / 2.53x | 0.00/1.60 |
/// 2.683 | -2.0814 (-61% of M) | 0% + 3.0872 SOL | +0.4045 (+15.1%) | 0.0549`: here FV binds —
/// the lenders take FV (the post-migration pool's cost at `P_ema`, the penalty and the rent),
/// the borrower the rest of `V − K_s`, and all of FV above the rent vests (a migrating curve
/// cannot execute, AIR-09). FV contains the rent, the sim's `R_min`, so the lenders' take and the
/// borrower's remainder move by `ΔR`; the vesting lump does not.
#[test]
fn d80_stuck_migration_settles_s3_at_fair_value() {
    let (mut s, p) = open(&D80);
    let r = s.run(&p, &Scenario::stuck_migration());
    assert_steps(&r, 4_329, 7_800);
    assert_completed(&s, 115_005_359_059);
    let Outcome::Settled(ev) = &r.outcome else {
        panic!("expected S3, got {}", r.outcome.kind())
    };
    assert_eq!(ev.condition, 3, "S3");
    assert_eq!(ev.k_s, 54_933_644);
    let sim_rent = D80.r_min;
    let d = sim_rent - r.status.r_due;
    assert!(r.status.r_due < sim_rent);
    assert_eq!(
        ev.fv + d,
        3_087_245_115,
        "FV = B̂_FV 2,804,771,683 + 10 % + R (rent adjusted)"
    );
    assert_eq!(ev.lender_amount, ev.fv, "FV binds");
    assert_eq!(
        ev.borrower_amount,
        1_351_185_653 + d,
        "borrower (rent adjusted)"
    );
    assert_eq!(
        ev.lump,
        3_087_245_115 - sim_rent,
        "all of FV above R_due vests (AIR-09; rent-independent)"
    );
    assert_eq!(market(&s.env, &s.pm).ema_price.get(), 410_880_168_127_897);
    assert_eq!(
        r.status.cr_e,
        cr_at(D80.v, r.status.r_due, r.status.b_ema.unwrap())
    );
    assert_eq!(
        cr_at(D80.v, sim_rent, r.status.b_ema.unwrap()),
        16_017,
        "CR_EMA 1.60"
    );
    let pnl = i128::from(ev.borrower_amount) - i128::from(D80.m + D80.fee_open);
    assert_eq!(sol4(pnl - i128::from(d)), "-2.0814");
}

/// ECON §10.1 `D@90% | graduation, migration stuck | open | +10800s | 1.70x / 1.70x | 0.00/2.40`:
/// the completion is only 1.70× above the open, the position is not liquidatable on the frozen
/// EMA (CR_EMA 2.40) and not expired, so no S3 is due: it stays frozen through the 3 h window
/// (the keeper cranking every second, `sol_settle` never called).
#[test]
fn d90_stuck_migration_stays_open() {
    let (mut s, p) = open(&D90);
    let r = s.run(&p, &Scenario::stuck_migration());
    assert!(matches!(r.outcome, Outcome::Open));
    assert_steps(&r, 10_929, 14_400);
    assert_completed(&s, 115_005_359_059);
    assert_eq!(r.status.r_due, 3_601_447, "the sim's rent due (accrued)");
    assert_eq!(r.status.b_ema, Some(2_283_478_033));
    assert_eq!(r.status.cr_e, 24_006, "CR_EMA 2.40");
    assert!(!r.status.cr_liq() && !r.status.expired);
    // The exits still refuse: the migration pause.
    let kp = s.keeper.insecure_clone();
    let ix = liquidate_ix(&s.pm, &kp.pubkey(), &p.key(), p.nonce);
    assert_err(&s.env.send(&[ix], &[&kp]), E::VenueMigrating);
    assert_err(&s.env.close(&s.pm, &p.b, p.nonce), E::VenueMigrating);
}

// ---- D@0% -------------------------------------------------------------------------------------

/// ECON's `D@0%` rows open their short at `PumpCurve.at_progress(0)`, a curve with no real SOL
/// (`rQ = 0`: nobody has bought yet). The sim's `PumpCurve.sell` does not check `rQ`; pump pays a
/// sale out of it, and short.win's `S_exp` is capped by it (ARCH §4.3), so the sim's position
/// (`Q` = 15,976,064,934,395, `S` = 0.4346 SOL) cannot open on a real curve: the open is refused
/// and nothing moves. (ECON's D@0% rows scale exactly to D@50%'s — the same CRs, exit seconds
/// and multiples — which the tests above reproduce.)
#[test]
fn d0_the_sims_position_cannot_open_on_a_real_curve() {
    let mut s = Sim::d_market(0, lend_for_80(15_976_064_934_395));
    let c = curve(&s.env, &s.pm);
    assert_eq!(
        (c.vt, c.vq, c.rq),
        (u128::from(PUMP_VT0), u128::from(PUMP_VS0), 0)
    );
    assert!(
        s.pm.curve
            .quote_sell(&s.env.svm, 15_976_064_934_395)
            .is_none()
    );
    let mut b = s.env.borrower();
    let (res, _) = s.env.open(&s.pm, &mut b, 15_976_064_934_395);
    assert!(res.is_err(), "no sale on a curve without real SOL");
    eprintln!("D@0% open refused: {:?}", custom_code(&res));
    assert_eq!(market(&s.env, &s.pm).open_positions, 0);
}
