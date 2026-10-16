//! ECON §10.1's graduation scenarios on the fork (M6 row "ECON §10 graduation scenarios"; ARCH
//! §2.5 "On → PumpSwap", §5.4, §8.2, §9): ECON's `D@50%`, `D@80%`, `D@90%` and `D@97%` markets built
//! through real instructions (`Sim::d_market`: the fresh fixture curve at `at_progress(x)`), the
//! position at the per-position cap and 80 % utilisation, and `shortwin_sim.run(new_market(key),
//! 1.0, GRAD_SCENARIOS[i], keeper_latency=1)` replayed second by second through a **real
//! graduation** ([`Grad::run`], `graduation_support.rs`): another user's `buy_v2` of the whole `rT`
//! completes the curve at `TE` (1 h after the open), pump's own `migrate_v2` creates the canonical
//! pool 2 s later, the keeper's crank moves the market `→ PumpSwap`, pump's boost is spent with the
//! real `boost_buy_and_burn` on the sim's schedule (`⌈vault/40⌉` every 9 s), other users ramp the
//! pool `×2` / `×3` over 60 s from `TE + 30` with real PumpSwap buys (`amm_ramp_events`), and the
//! keeper's exits run on the pool. The invariant hooks run after every instruction.
//!
//! **To the lamport — against the normative sim.** Since ECON #105 (the owner's decisions of
//! 2026-09-30) `docs/design/sim/shortwin_sim.py` models the graduation as built, and ECON §10.1 is
//! its as-built table; every constant below is that sim's output, and the fork reproduces each one
//! — the exit second, the keeper's number of cranks, the pool's spot at the exit, `B`, `B̂_ref`,
//! `max_in`, the rent, the penalty and its split, the S1 take, the CRs — up to one modelled
//! difference:
//!
//! - **`R_min`** (M5's documented difference, `pump_curve_support.rs`): the sim prices a position's
//!   minimum rent at 80 % utilisation, the program at the pre-open 0 % (`r_min` 323,528 / 499,066 /
//!   609,255 / 720,630 instead of 1,294,111 / 1,996,263 / 2,437,017 / 2,882,519); where the rent due
//!   is `R_min` the legs below it move by the difference.
//!
//! The two differences this binary documented before #105 are gone from the sim: **the graduation
//! rule** (ARCH §2.5, `venues::pump_amm::graduate`; the sim's `Market._graduate`: the ring rewritten
//! to the pool's `lp_supply` — 4,193,388,282,654 on the D@x pools —, `L_slow = min(L_slow,
//! anchor)`, the chains cleared, the post-change restart `(8, 1)`, the transitioning crank
//! recording nothing, after the roll of the curve's last bucket: D@80 % liquidated at **+12 s**
//! instead of v0.4.1's +15 s, D@50 % settling S1 at **+52 s** instead of +50 s, D@90 %'s gap row at
//! **+303 s** instead of +302 s) and **the boost's rounding on the ELF**
//! ([`the_elf_prices_a_boost_chunk_at_one_lamport_less`]: `boost_buy_and_burn(b)` moves `b` lamports
//! into the WSOL vault but burns `⌊X·(b − 1)/(Y + b − 1)⌋` tokens, now the sim's `CPPool.boost_tick`
//! too). The M6R-1 rules (the pool's locked depth from its burned LP: nothing binds on these pools,
//! whose creation LP is all burned) and M6-SEC-1's fall bound (the seed rises here) change no row;
//! ★T-SEED-FALL's rows ([`t_seed_fall_a_recorded_low_in_the_warm_up_delays_the_exit_by_the_fall_bound`],
//! [`t_seed_fall_a_genuine_dump_after_the_graduation_forces_nothing`]) reproduce
//! `attacks/pumpswap_lp.py` part 1 (+13 / +22 / +28 s; +14 s for the 8th second before AIR-22).
//!
//! | ECON §10.1 row (v0.4.1; the as-built column since #105) | M6 on the fork (this binary) | Test |
//! |---|---|---|
//! | D@50% graduation gap — S1 +50 s, 6.39×, lenders 0 % + 2.8738, keeper 0.0391 | S1 **+52 s**, 6.39×, lenders 0 % + 2.8738, keeper 0.0391 | [`d50_graduation_gap_settles_s1`] |
//! | D@50% then +2x on AMM — S1 +50 s at 8.14× | S1 **+52 s** at **8.33×** (two more ramp seconds), same amounts | [`d50_graduation_then_2x_settles_s1`] |
//! | D@50% then +3x on AMM — S1 +50 s at 9.37× | S1 **+52 s** at **9.72×**, same amounts | [`d50_graduation_then_3x_settles_s1`] |
//! | D@80% graduation gap — liquidation +15 s, 2.58×, borrower −2.0910, lenders 100 % + 0.1449, keeper 0.0859 | liquidation **+12 s**, 2.58×, CR 1.57/1.59, borrower −2.0865, lenders 100 % + 0.1421, keeper 0.0850 | [`d80_graduation_gap_is_liquidated_on_the_pool`] |
//! | D@80% then +2x / +3x — the same (the exit precedes the ramp) | the same as the gap row | [`d80_graduation_then_2x_is_liquidated_on_the_pool`], [`d80_graduation_then_3x_is_liquidated_on_the_pool`] |
//! | D@90% graduation gap — liquidation +302 s by the boost push, 2.34×, lenders 100 % + 0.1598 | liquidation **+303 s**, 2.34×, lenders 100 % + 0.1591 | [`d90_graduation_gap_is_liquidated_by_the_boost_push`] |
//! | D@90% then +2x — liquidation +60 s, 2.60×, lenders 100 % + 0.1780 | liquidation +60 s, 2.60×, lenders 100 % + 0.1764 | [`d90_graduation_then_2x_is_liquidated_on_the_pool`] |
//! | D@90% then +3x — liquidation +52 s, 2.74×, lenders 100 % + 0.1878 | liquidation +52 s, 2.74×, lenders 100 % + 0.1863 | [`d90_graduation_then_3x_is_liquidated_on_the_pool`] |
//! | D@97% graduation gap — open +1800 s at 1.77×, CR 2.31 | open +1800 s at 1.77×, CR 2.31 | [`d97_graduation_gap_survives_the_graduation`] |
//! | D@97% then +2x — liquidation +87 s, 2.60×, lenders 100 % + 0.2104 | liquidation +87 s, 2.60×, lenders 100 % + 0.2085 | [`d97_graduation_then_2x_is_liquidated_on_the_pool`] |
//! | D@97% then +3x — liquidation +70 s, 2.77×, lenders 100 % + 0.2242 | liquidation +70 s, 2.77×, lenders 100 % + 0.2224 | [`d97_graduation_then_3x_is_liquidated_on_the_pool`] |
//!
//! (The lenders' SOL differs from ECON's by the rent-protocol share of the `R_min` difference plus,
//! on D@80 %, the penalty on the earlier exit's lower `B̂_ref`; the "migration stuck" rows are M5's,
//! `pump_curve_scenarios`.) ECON's structural claims hold on the ELFs: positions opened at 80–90 %
//! are liquidated on the completing buy or by the boost push that follows, the lenders made whole
//! with their tokens back plus SOL; at 50 % the completing buy settles S1 at `V − K_s`; at 97 % the
//! position survives the graduation and is liquidated only if the price keeps rising.
//!
//! Printed with `-- --nocapture`: each row in ECON's column format.

#![allow(clippy::result_large_err)]

#[path = "graduation_support.rs"]
mod support;

use shortwin_math::num::sqrt_u128;
use support::*;

/// A sim market and its position at the cap (`Market.open`), with the program's `r_min`.
struct Mkt {
    name: &'static str,
    sold: u64,
    q: u64,
    s: u64,
    m: u64,
    v: u64,
    fee_open: u64,
    n_open: u64,
    /// The program's `r_min` (4 h at the pre-open 0 %).
    r_min: u64,
    p1: u128,
}

const D50: Mkt = Mkt {
    name: "D@50%",
    sold: SOLD_D50,
    q: 10_071_769_920_663,
    s: 689_404_100,
    m: 2_223_494_440,
    v: 2_912_898_540,
    fee_open: 1_767_144,
    n_open: 706_857_373,
    r_min: 323_528,
    p1: 68_298_686_219_984,
};

const D80: Mkt = Mkt {
    name: "D@80%",
    sold: 634_480_000_000_000,
    q: 6_529_192_912_424,
    s: 1_063_457_547,
    m: 3_429_906_865,
    v: 4_493_364_412,
    fee_open: 2_725_952,
    n_open: 1_090_380_527,
    r_min: 499_066,
    p1: 162_519_299_265_941,
};

const D90: Mkt = Mkt {
    name: "D@90%",
    sold: SOLD_D90,
    q: 5_348_333_909_677,
    s: 1_298_258_410,
    m: 4_187_196_266,
    v: 5_485_454_676,
    fee_open: 3_327_815,
    n_open: 1_331_125_716,
    r_min: 609_255,
    p1: 242_207_125_279_645,
};

const D97: Mkt = Mkt {
    name: "D@97%",
    sold: 769_307_000_000_000,
    q: 4_521_732_607_755,
    s: 1_535_588_254,
    m: 4_952_641_686,
    v: 6_488_229_940,
    fee_open: 3_936_160,
    n_open: 1_574_463_910,
    r_min: 720_630,
    p1: 338_855_226_469_379,
};

/// What the sim's exit was (lamports).
enum Want {
    S1 {
        keeper: u64,
        lenders: u64,
    },
    Liq {
        b: u64,
        b_ref: u64,
        max_in: u64,
        rent: u64,
        pen: u64,
        keeper: u64,
        protocol: u64,
        lender_sol: u64,
        borrower: u64,
    },
    Open,
}

/// One row: the sim's second of the outcome, its number of keeper cranks (`step_to` calls), the
/// venue's spot at the exit, `CR` spot / EMA at the exit and the exit's amounts.
struct Row {
    mkt: &'static Mkt,
    sc: GradScenario,
    name: &'static str,
    t: i64,
    steps: usize,
    spot: u128,
    cr: (u64, u64),
    want: Want,
}

/// The sim's graduation second (completion at `TE`, `migrate_v2` 2 s later).
const T_GRAD: i64 = TE + MIGRATION_DELAY_S;

/// ECON's `D@x` market on the fork and its capped position, the open checked against the sim.
fn open(mkt: &Mkt) -> (Grad, Pos) {
    let mut g = Grad::d_market(mkt.sold, lend_for_80(mkt.q));
    let p = g.s.open(mkt.q);
    assert_eq!(
        (
            p.open.q,
            p.open.s,
            p.open.m,
            p.open.v,
            p.open.fee_open,
            p.open.n_open
        ),
        (mkt.q, mkt.s, mkt.m, mkt.v, mkt.fee_open, mkt.n_open),
        "{}: Q, S, M, V, the open fee and N_open are the sim's",
        mkt.name
    );
    assert_eq!(g.s.position(&p).r_min, mkt.r_min, "the program's r_min");
    assert_eq!(g.s.spot(), mkt.p1, "P1");
    (g, p)
}

/// Replays `row` on the fork and checks it against the sim (module docs).
fn replay(row: &Row) {
    let mkt = row.mkt;
    let (mut g, p) = open(mkt);
    let lend0 = market(&g.s.env, &g.s.pm).tokens_free;
    let pos = g.s.position(&p);
    let run = g.run(&p, &row.sc);
    let r = &run.r;
    let name = format!("{} {}", mkt.name, row.name);
    assert_eq!(run.t_complete, Some(TE), "{name}: the completion at TE");
    assert_eq!(run.t_grad, Some(T_GRAD), "{name}: → PumpSwap 2 s later");
    assert_eq!(r.t, row.t, "{name}: the sim's second of the outcome");
    let cranks = r.steps.len() + usize::from(matches!(r.outcome, Outcome::Open));
    assert_eq!(cranks, row.steps, "{name}: the sim's keeper cadence");
    assert_eq!(
        r.status.spot,
        Some(row.spot),
        "{name}: the pool's spot at the exit"
    );
    assert_eq!(
        (r.status.cr_s, r.status.cr_e),
        row.cr,
        "{name}: CR spot / EMA at the exit"
    );
    let m = market(&g.s.env, &g.s.pm);
    assert_eq!(m.venue_kind(), Some(VenueKind::PumpSwap));
    assert_eq!(m.graduated_at, g.s.chain(T_GRAD));
    let p_rent = u64::from(g.s.env.config().params.rent_protocol_bps);
    let term = row.spot;
    let value = u128::from(mkt.q) * term / W;
    let (kind, borrower, lender_sol, tokens, keeper) = match (&r.outcome, &row.want) {
        (Outcome::Settled(ev), Want::S1 { keeper, lenders }) => {
            assert_eq!(ev.condition, 1, "{name}: S1");
            assert_eq!(ev.k_s, *keeper, "{name}: K_s");
            assert_eq!(ev.lender_amount, *lenders, "{name}: lenders V − K_s");
            assert_eq!(ev.lender_amount, mkt.v - ev.k_s);
            assert_eq!(ev.borrower_amount, 0);
            ("S1", 0u64, ev.lender_amount, 0u64, ev.k_s)
        }
        (
            Outcome::Liquidated(ev),
            Want::Liq {
                b,
                b_ref,
                max_in,
                rent,
                pen,
                keeper,
                protocol,
                lender_sol,
                borrower,
            },
        ) => {
            assert_eq!(
                (ev.buyback_cost, ev.b_ref, ev.max_in),
                (*b, *b_ref, *max_in),
                "{name}: B, B̂_ref, max_in"
            );
            assert_eq!(
                (ev.rent, ev.penalty, ev.penalty_keeper + ev.keeper_topup),
                (*rent, *pen, *keeper),
                "{name}: rent, penalty, keeper"
            );
            let rent_protocol = ev.rent * p_rent / 10_000;
            assert_eq!(
                ev.penalty_protocol + rent_protocol,
                *protocol,
                "{name}: protocol"
            );
            let lsol = ev.rent - rent_protocol + ev.penalty_lender;
            assert_eq!(lsol, *lender_sol, "{name}: lenders' SOL");
            assert_eq!(ev.borrower_payout, *borrower, "{name}: borrower");
            let back = market(&g.s.env, &g.s.pm).tokens_free - lend0;
            check_pool_liquidation(ev, &pos, ev.b_live, back);
            (
                "liquidation",
                ev.borrower_payout,
                lsol,
                mkt.q,
                ev.penalty_keeper + ev.keeper_topup,
            )
        }
        (Outcome::Open, Want::Open) => {
            assert_eq!(m.open_positions, 1, "{name}: still open");
            ("open", 0, 0, mkt.q, 0)
        }
        (o, _) => panic!("{name}: got {} (at {})", o.kind(), r.t),
    };
    let bp = if kind == "open" {
        "-".to_string()
    } else {
        let pnl = i128::from(borrower) - i128::from(mkt.m) - i128::from(mkt.fee_open);
        format!(
            "{} ({:+.0}% of M)",
            sol4(pnl),
            100.0 * pnl as f64 / mkt.m as f64
        )
    };
    let lender_pnl =
        i128::from(lender_sol) - (i128::from(mkt.q - tokens) * term as i128 / W as i128);
    eprintln!(
        "| {} | {} | {kind} | {:+}s | {:.2}x / {:.2}x | {:.2}/{:.2} | {:.3} | {bp} | {:.0}% + {:.4} SOL | {} ({:+.1}%) | {:.4} |",
        mkt.name,
        row.name,
        r.t - TE,
        row.spot as f64 / mkt.p1 as f64,
        term as f64 / mkt.p1 as f64,
        r.status.cr_s as f64 / 1e4,
        r.status.cr_e as f64 / 1e4,
        value as f64 / 1e9,
        100.0 * tokens as f64 / mkt.q as f64,
        lender_sol as f64 / 1e9,
        sol4(lender_pnl),
        100.0 * lender_pnl as f64 / value as f64,
        keeper as f64 / 1e9,
    );
}

const GAP: &str = "graduation gap (1 buy completes curve)";
const X2: &str = "graduation, then +2x on AMM over 60s";
const X3: &str = "graduation, then +3x on AMM over 60s";

// ---- D@50% --------------------------------------------------------------------------------------

const D50_S1: Want = Want::S1 {
    keeper: 39_128_985,
    lenders: 2_873_769_555,
};

/// ECON: S1 +50 s at 6.39×, lenders 0 % + 2.8738 SOL (`V − K_s`), keeper 0.0391. M6: S1 **+52 s**:
/// the completion puts the position far underwater (5.84× from 50 %), the keeper's liquidation
/// returns `UseSolSettle`, and `sol_settle` evaluates nothing until the warm-up after the
/// graduation has run; the arming clock's persistence then ends two seconds later than ECON's.
#[test]
fn d50_graduation_gap_settles_s1() {
    replay(&Row {
        mkt: &D50,
        sc: GradScenario::gap(),
        name: GAP,
        t: 3_652,
        steps: 181,
        spot: 436_672_648_222_652,
        cr: (6_215, 6_254),
        want: D50_S1,
    });
}

/// ECON: S1 +50 s at 8.14×. M6: S1 +52 s at 8.33× (the ramp's two more seconds), same amounts.
#[test]
fn d50_graduation_then_2x_settles_s1() {
    replay(&Row {
        mkt: &D50,
        sc: GradScenario::amm_ramp(2.0),
        name: X2,
        t: 3_652,
        steps: 181,
        spot: 568_776_456_777_376,
        cr: (4_736, 5_099),
        want: D50_S1,
    });
}

/// ECON: S1 +50 s at 9.37×. M6: S1 +52 s at 9.72×, same amounts.
#[test]
fn d50_graduation_then_3x_settles_s1() {
    replay(&Row {
        mkt: &D50,
        sc: GradScenario::amm_ramp(3.0),
        name: X3,
        t: 3_652,
        steps: 181,
        spot: 663_918_986_063_896,
        cr: (4_037, 4_517),
        want: D50_S1,
    });
}

// ---- D@80% --------------------------------------------------------------------------------------

const D80_LIQ: Want = Want::Liq {
    b: 2_863_357_448,
    b_ref: 2_833_636_795,
    max_in: 2_890_309_530,
    rent: 499_066,
    pen: 283_363_680,
    keeper: 85_009_104,
    protocol: 56_747_595,
    lender_sol: 142_106_047,
    borrower: 1_346_144_218,
};

/// ECON: liquidation +15 s at 2.58×, lenders 100 % + 0.1449 SOL. M6: liquidation **+12 s** on the
/// pool — the warm-up gate opens with the eighth integration at +11 s (the keeper's view waits for
/// it, then acts one second later) — at 2.58×, the lenders' tokens back plus 0.1421 SOL.
#[test]
fn d80_graduation_gap_is_liquidated_on_the_pool() {
    replay(&Row {
        mkt: &D80,
        sc: GradScenario::gap(),
        name: GAP,
        t: 3_612,
        steps: 141,
        spot: 419_322_886_289_833,
        cr: (15_690, 15_855),
        want: D80_LIQ,
    });
}

/// ECON: as the gap row (the exit precedes the ramp at +30 s). M6: the same.
#[test]
fn d80_graduation_then_2x_is_liquidated_on_the_pool() {
    replay(&Row {
        mkt: &D80,
        sc: GradScenario::amm_ramp(2.0),
        name: X2,
        t: 3_612,
        steps: 141,
        spot: 419_322_886_289_833,
        cr: (15_690, 15_855),
        want: D80_LIQ,
    });
}

/// ECON: as the gap row. M6: the same.
#[test]
fn d80_graduation_then_3x_is_liquidated_on_the_pool() {
    replay(&Row {
        mkt: &D80,
        sc: GradScenario::amm_ramp(3.0),
        name: X3,
        t: 3_612,
        steps: 141,
        spot: 419_322_886_289_833,
        cr: (15_690, 15_855),
        want: D80_LIQ,
    });
}

// ---- D@90% --------------------------------------------------------------------------------------

/// ECON: the position survives the completion (1.65× from 90 %) and is liquidated by the boost
/// push at +302 s, 2.34×, lenders 100 % + 0.1598 SOL. M6: +303 s (the boost is the real
/// `boost_buy_and_burn`, 40 chunks every 9 s), 2.34×, lenders 100 % + 0.1591 SOL.
#[test]
fn d90_graduation_gap_is_liquidated_by_the_boost_push() {
    replay(&Row {
        mkt: &D90,
        sc: GradScenario::gap(),
        name: GAP,
        t: 3_903,
        steps: 432,
        spot: 567_968_304_675_166,
        cr: (17_299, 17_410),
        want: Want::Liq {
            b: 3_170_506_871,
            b_ref: 3_175_337_266,
            max_in: 3_238_844_011,
            rent: 690_973,
            pen: 317_050_688,
            keeper: 95_115_206,
            protocol: 63_513_782,
            lender_sol: 159_112_673,
            borrower: 1_997_206_144,
        },
    });
}

/// ECON: liquidation +60 s, 2.60×. M6: the same second and price.
#[test]
fn d90_graduation_then_2x_is_liquidated_on_the_pool() {
    replay(&Row {
        mkt: &D90,
        sc: GradScenario::amm_ramp(2.0),
        name: X2,
        t: 3_660,
        steps: 189,
        spot: 629_245_013_292_515,
        cr: (15_589, 16_791),
        want: Want::Liq {
            b: 3_518_341_773,
            b_ref: 3_523_535_042,
            max_in: 3_594_005_742,
            rent: 609_853,
            pen: 351_834_178,
            keeper: 105_550_253,
            protocol: 70_458_312,
            lender_sol: 176_435_466,
            borrower: 1_614_668_872,
        },
    });
}

/// ECON: liquidation +52 s, 2.74×. M6: the same second and price.
#[test]
fn d90_graduation_then_3x_is_liquidated_on_the_pool() {
    replay(&Row {
        mkt: &D90,
        sc: GradScenario::amm_ramp(3.0),
        name: X3,
        t: 3_652,
        steps: 181,
        spot: 663_918_986_057_969,
        cr: (14_761, 16_487),
        want: Want::Liq {
            b: 3_715_550_355,
            b_ref: 3_718_899_017,
            max_in: 3_793_276_997,
            rent: 609_255,
            pen: 371_555_036,
            keeper: 111_466_510,
            protocol: 74_402_395,
            lender_sol: 186_295_386,
            borrower: 1_397_740_030,
        },
    });
}

// ---- D@97% --------------------------------------------------------------------------------------

/// ECON: the position survives the graduation and the whole boost (1.18× from 97 %, ×1.4566):
/// open at +1800 s, 1.77×, CR 2.31/2.31. M6: the same, the keeper cranking the pool every second of
/// the window.
#[test]
fn d97_graduation_gap_survives_the_graduation() {
    replay(&Row {
        mkt: &D97,
        sc: GradScenario::gap(),
        name: GAP,
        t: 5_400,
        steps: 1_930,
        spot: 598_345_112_264_025,
        cr: (23_067, 23_067),
        want: Want::Open,
    });
}

/// ECON: liquidation +87 s, 2.60×. M6: the same second and price.
#[test]
fn d97_graduation_then_2x_is_liquidated_on_the_pool() {
    replay(&Row {
        mkt: &D97,
        sc: GradScenario::amm_ramp(2.0),
        name: X2,
        t: 3_687,
        steps: 216,
        spot: 879_737_456_342_455,
        cr: (15_600, 16_790),
        want: Want::Liq {
            b: 4_158_633_058,
            b_ref: 4_163_935_706,
            max_in: 4_247_214_420,
            rent: 726_150,
            pen: 415_863_306,
            keeper: 124_758_991,
            protocol: 83_281_583,
            lender_sol: 208_548_882,
            borrower: 1_913_007_426,
        },
    });
}

/// ECON: liquidation +70 s, 2.77×. M6: the same second and price.
#[test]
fn d97_graduation_then_3x_is_liquidated_on_the_pool() {
    replay(&Row {
        mkt: &D97,
        sc: GradScenario::amm_ramp(3.0),
        name: X3,
        t: 3_670,
        steps: 199,
        spot: 937_242_757_880_119,
        cr: (14_627, 16_342),
        want: Want::Liq {
            b: 4_435_173_921,
            b_ref: 4_439_614_323,
            max_in: 4_528_406_609,
            rent: 720_630,
            pen: 443_517_393,
            keeper: 133_055_217,
            protocol: 88_811_572,
            lender_sol: 222_371_234,
            borrower: 1_608_817_996,
        },
    });
}

// ---- the boost on the ELF -------------------------------------------------------------------

/// The boost's rounding on the ELF (module docs; a modelled difference until ECON #105, the
/// normative sim's `CPPool.boost_tick` since), on the pool a real `migrate_v2` created from ECON's
/// D@80 % curve: the pool is the sim's `CPPool.graduate(rQ − pool_migration_fee)` to the lamport
/// (206.9·10¹² tokens, the WSOL vault `migrated − VQR`, VQR = boost vault = `⌊migrated·2069/10⁴⌋`,
/// `lp_supply` = the sim's `√(X·Y)`), and each `boost_buy_and_burn(b)` adds `b` to the vault but
/// burns `⌊X·(b − 1)/(Y + b − 1)⌋` tokens (v0.4.1's sim: `⌊X·b/(Y + b)⌋`), for the sim's chunk
/// `⌈vault/40⌉`, an odd size, and dust down to 2 lamports; VQR is constant (R-VEN H9).
#[test]
fn the_elf_prices_a_boost_chunk_at_one_lamport_less() {
    let (mut g, _p) = open(&D80);
    g.run_boost = false;
    g.s.env.warp(1);
    g.crank();
    g.complete_curve();
    let rq = u64::try_from(curve(&g.s.env, &g.s.pm).rq).unwrap();
    g.crank();
    g.migrate();
    g.crank();
    assert!(g.graduated());
    let pool = g.pam().pool;
    let st = pool.state(&g.s.env.svm);
    let r = pool.reserves(&g.s.env.svm).unwrap();
    let migrated = rq - 15_000_001;
    let vqr = migrated * 2_069 / 10_000;
    assert_eq!(
        (r.base, r.real_quote, st.virtual_quote_reserves),
        (206_900_000_000_000, migrated - vqr, i128::from(vqr)),
        "CPPool.graduate(rQ − pool_migration_fee)"
    );
    assert_eq!(pool.boost_amount(&g.s.env.svm), vqr, "the boost vault");
    assert_eq!(
        u128::from(st.lp_supply),
        sqrt_u128(
            u128::from(r.base) * r.effective_quote,
            shortwin_math::num::Rounding::Floor
        ),
        "lp_supply = √(X·Y) at creation (the graduation's anchor)"
    );
    let chunk = vqr.div_ceil(BOOST_CALLS);
    assert_eq!(g.boost.expect("schedule").chunk, chunk);
    for b in [chunk, chunk, 1_000_000_007, 12_345, 2] {
        g.s.env.warp(1);
        let r0 = pool.reserves(&g.s.env.svm).unwrap();
        ok(boost(&mut g.s.env, &pool, b));
        let r1 = pool.reserves(&g.s.env.svm).unwrap();
        let (x, y) = (u128::from(r0.base), r0.effective_quote);
        let burned = u128::from(r0.base - r1.base);
        let bb = u128::from(b);
        assert_eq!(burned, x * (bb - 1) / (y + bb - 1), "the ELF: b − 1 priced");
        assert!(
            burned <= x * bb / (y + bb),
            "v0.4.1's sim formula burns at least as much"
        );
        assert_eq!(r1.real_quote - r0.real_quote, b, "b into the vault");
        assert_eq!(
            pool.state(&g.s.env.svm).virtual_quote_reserves,
            i128::from(vqr),
            "VQR constant"
        );
    }
}

// ---- ★T-SEED-FALL (M6-SEC-1, owner decision of 2026-09-30) --------------------------------------

/// ★T-SEED-FALL (ARCH §8.2; ECON §12.12 (12) (a); `attacks/pumpswap_lp.py` part 1): on ECON's D@80 %
/// gap row an attacker records a low of 0.25× the pool's price in one warm-up second — `[sell,
/// crank_price, buy back]` next to the keeper's crank — and the warm-up price seed falls by at most
/// one v0.4.1 step per integration (M6-SEC-1; as built it took the low at once and kept it until the
/// gate). The carried position is still liquidated on the pool with the lenders whole (their tokens
/// back plus SOL): a low in the 8th post-change second delays the exit from +12 s to +13 s (+14 s
/// before AIR-22's lower-median growth) and in
/// the 4th to +22 s (as built +28 s either way); in the 1st second the carried seed already lies at
/// 0.55× the new pool, so the low is reached anyway (+28 s: the disclosed residual, ECON §12.12
/// residual 17). The attacker's round trip is printed.
#[test]
fn t_seed_fall_a_recorded_low_in_the_warm_up_delays_the_exit_by_the_fall_bound() {
    for (k, want) in SEED_FALL_ROWS {
        let (mut g, p) = open(&D80);
        let run = g.run(
            &p,
            &GradScenario {
                low: Some((k, 2_500)),
                ..GradScenario::gap()
            },
        );
        let Outcome::Liquidated(ev) = &run.r.outcome else {
            panic!("low in second {k}: {}", run.r.outcome.kind());
        };
        eprintln!(
            "T-SEED-FALL: D@80 % low 0.25x in post-change second {k}: liquidation +{} s @ spot {:?} \
             (honest +12 s), lenders' SOL {}",
            run.r.t - TE,
            run.r.status.spot,
            ev.penalty_lender + ev.rent
        );
        assert_eq!(run.t_grad, Some(T_GRAD));
        assert_eq!(run.r.t - TE, want, "low in second {k}");
        assert!(ev.borrower_payout > 0 && ev.penalty_lender > 0);
    }
}

/// `(post-change second of the low, the liquidation's second after the completion)` on the fork
/// (`attacks/pumpswap_lp.py` part 1, #105: +13 / +22 / +28 s; +14 s in the 8th second under the
/// minimum growth rule, before AIR-22's lower median).
const SEED_FALL_ROWS: [(i64, i64); 3] = [(8, 13), (4, 22), (1, 28)];

/// ★T-SEED-FALL, the other side (`attacks/pumpswap_lp.py` part 1, "the genuine dump"): other users
/// dump the pool to 0.25× in the first post-change second and hold it there. The price seed falls
/// by one v0.4.1 step per integration, so it lags the dump — and forces nothing: the spot leg reads
/// the live price, so the carried D@80 % and D@97 % positions (short, and now deep in profit) stay
/// open, and the steady EMA meets the pool once the boost (360 s) has stopped pushing it.
#[test]
fn t_seed_fall_a_genuine_dump_after_the_graduation_forces_nothing() {
    for mkt in [&D80, &D97] {
        let (mut g, p) = open(mkt);
        let run = g.run(
            &p,
            &GradScenario {
                dump: Some(2_500),
                window: 600,
                ..GradScenario::gap()
            },
        );
        assert!(
            matches!(run.r.outcome, Outcome::Open),
            "{}: {} after a genuine dump",
            mkt.name,
            run.r.outcome.kind()
        );
        let m = market(&g.s.env, &g.s.pm);
        let spot = pool_spot(&g.s.env, &g.pam().pool);
        let ratio = m.ema_price.get() as f64 / spot as f64;
        eprintln!(
            "T-SEED-FALL: {} genuine dump to 0.25x: open; P_ema/P at +{} s {ratio:.4}",
            mkt.name,
            run.r.t - TE
        );
        assert!((0.99..=1.01).contains(&ratio), "{ratio}");
    }
}
