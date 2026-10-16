//! M4F-1, M4G-1, M4G-3 after "switch only if all fit" (owner decision of 2026-09-29; ARCH §7.1
//! duties 2 and 5; ECON §5.8, §12.12): the keeper against the capital deposit freeze after a pool
//! switch, on the mainnet fork (the release short.win, the dumped AMM v4 and CPMM programs,
//! planted pools). The M4X-2 shape: a 2,500-SOL λ 50 % AMM v4 incumbent (Tier A) halts with a
//! position open and a 320-SOL λ 99.78 % CPMM sibling is the candidate; the attacker's liquidity
//! moves are the dumped CPMM program's own `deposit` and `withdraw`
//! ([`shortwin_litesvm_tests::lp`]).
//!
//! As found, the position was at the incumbent's size cap, ≈ 3.1 % of the candidate's `X`, and
//! the fast path carried it: a freeze of `L_ema` (`[deposit 7·LP counter, crank_price, withdraw]`
//! every second), isolated keeper misses under it (M4G-1) or a real deposit held through the flat
//! phase and pulled at the pump (M4G-3) turned the genuine 4× pump into S1 (lenders −10 SOL).
//! Now:
//!
//! - **An oversized carried position is not adopted.** Its candidates are refused
//!   `CarriedPositionsTooLarge` (the keeper proposes nothing: its plan reports the refusal; a
//!   direct proposal is refused), an LP's held deposit in the candidate does not change that (the
//!   locked depth `D` that bounds `X₀` does not move), and the position exits on the failed pool
//!   through the S3 chain at fair value at its deadline.
//! - **A carried position that fits the candidate** (95 % of its per-position cap) is adopted
//!   and liquidated in the genuine pump whatever the freeze: the keeper stays per second while the
//!   post-change reference guard is set (M4F-1's guard coverage, kept); a freeze started after the
//!   honest guard clear starts **no** per-slot burst — the per-slot liquidity bursts M4F-1 added
//!   stay reverted (M4G-2: a one-fee trigger of a 60-s per-slot burst) — but its foreign
//!   liquidity keeps the keeper **per second** (FITS-2 (b): 10 s from each LP-wrapped crank on a
//!   market with positions), and, like isolated keeper misses under it (M4G-1) and a held deposit
//!   pulled at the pump (M4G-3), it only delays the exit (printed: the seconds after the
//!   control's liquidation).
//! - **FIT-2** (the verification of #99): at the fit limit of a **1 %** fee candidate taken at its
//!   own fee (its impact cap, 2.47 % of `X₀`, binds; the margin was posted on the 25-bps
//!   incumbent) a 4×/30 s pump ended in S1 under those pushes (`L_slow` kept crediting a pulled
//!   deposit). The carried fit's fee-scaled caps now take `min(min_open_fee_ppm, φ_cand)` — the
//!   25-bps class the positions were margined at, 0.75 % of `X₀` — so that position is **not
//!   adopted** and settles in S3 at fair value, and a position at the new fit limit is adopted and
//!   **liquidated** under every push (`fit2_*`). The exit reference keeps V2-14's `L_slow` term
//!   (no lows bound): **FITV-2**'s withdraw contest cannot force the liquidation of a position the
//!   honest keeper leaves open (`fitv2_*`).
//! - **FIT-1** (a documented residual): a position carried through a dump-wrapped proposal, up to
//!   `λ·√(P_cand/P_ema)` × a native open's cap, is liquidated under the same pushes (`fit1_*`).
//! - **FITS-1** (the verification of #100): `min(φ_inc, φ_cand)` left a **1 % → 1 %** switch
//!   carrying positions up to the candidate's impact cap (2.47 % of `X₀`), and at 60–100 % of it
//!   FIT-2's pushes turned the genuine 4×/30 s pump into S1. The carried fee is now also capped at
//!   `CARRIED_FEE_CAP_PPM` = 25 bps: such a position is **not adopted** and settles in S3 at fair
//!   value, and a position at the new carried cap (0.75 % of `X₀`) is liquidated under every push
//!   (`fits1_*`).
//! - **FITS-2** (the verification of #100): an unlocked LP forced liquidations the honest keeper
//!   never makes — (a) a withdrawal inside the liquidation drained `L_live`, the spot leg's
//!   depth, and (b) withdraw-wrapped cranks in the keeper's idle seconds dragged `L_ema` /
//!   `L_slow`. Now the spot leg is judged at `max(L_live, L_ema')` (a withdrawal inside the
//!   transaction cannot make it true) and a foreign bucket liquidity keeps the keeper per second
//!   for 10 s on a market with positions (no attacker-only second while the attacker keeps
//!   wrapping, at one keeper crank per attacker transaction). The program-level sweep, the
//!   verifier's small-position and 25-minute-drag contests, a genuine withdrawal before a pump
//!   (liquidated, the delay measured), the keeper's cost of a wrap, and the pinned residual — the
//!   first wrap's second is the attacker's in the adversarial timing, a band of ≈ 0.1 % of price
//!   under the honest threshold (`fits2_*`).
//! - **FITX-1** (the verification of #101; a pinned residual, owner item O18): the per-second
//!   coverage holds `L_ema` but cannot lift it. An LP that first withdraws **for real** for 20 s
//!   (nothing foreign: the keeper reads the same drained depth), re-deposits and then
//!   withdraw-wraps a crank every second freezes the genuinely reached low (≈ 0.51 × `√(X·Y)`),
//!   and its wrapped liquidation forces both FITS-2 reproducers and the 25-bps open-cap position
//!   again — each exactly the liquidation the honest keeper makes when the same LP withdraws for
//!   good (the same second, price and buyback cost; `fitx1_*`).
//! - **M4G-2** after the revert and FITS-2 (b): an LP-wrapped crank starts no per-slot burst and
//!   buys at most 10 per-second keeper observations on a market with positions (none on one
//!   without), leaving the example O17 budget unspent (`m4g2_*`).

use anchor_lang::prelude::Pubkey;
use shortwin_litesvm_tests::engine::{Borrower, VenueMarket, crank_price_ix};
use shortwin_litesvm_tests::errors::custom_code;
use shortwin_litesvm_tests::fork::{self, AnyPool, CpmmFixture, CpmmPool, Swap, V4Fixture};
use shortwin_litesvm_tests::lp;
use shortwin_litesvm_tests::pool_select::{self as ps, PlantSpec, plant_cpmm, plant_v4, spec_like};
use shortwin_litesvm_tests::raydium::{RayMarket, RaySpec, VENUE_HINT_RAYDIUM_V4};
use shortwin_litesvm_tests::{pda, read};
use shortwin_math::fees::Direction;
use shortwin_math::num::WAD;
use shortwin_math::observe::ring_liq_lower_median;

use crate::common::*;
use crate::raydium::{FART, q_of};

/// Seconds of flat price after the adoption before the genuine ramp.
const W: i64 = 90;

/// The sizes tried for a position on the incumbent, bps of its token reserve (the first that
/// opens).
const OPEN_BPS: [u64; 11] = [100, 90, 80, 74, 72, 70, 65, 60, 50, 40, 25];

/// A pool for `base`'s token at its price: AMM v4 (`v4`) or CPMM, `sol` lamports of real SOL,
/// locked share `lam`.
fn sized(env: &mut Env, v4: bool, base: &AnyPool, lam: u64, sol: u64) -> AnyPool {
    let s = spec_like(&env.svm, base, true, lam);
    let spec = PlantSpec {
        sol,
        tokens: u64::try_from(u128::from(s.tokens) * u128::from(sol) / u128::from(s.sol))
            .expect("u64"),
        ..s
    };
    if v4 {
        AnyPool::V4(plant_v4(env, V4Fixture::Fartcoin, spec))
    } else {
        AnyPool::Cpmm(plant_cpmm(env, CpmmFixture::Useless, spec))
    }
}

/// Halts `pool` (AMM v4 status 2; CPMM swap bit).
fn halt(env: &mut Env, pool: &AnyPool) {
    match pool {
        AnyPool::V4(p) => fork::set_v4_status(&mut env.svm, p, 2),
        AnyPool::Cpmm(p) => fork::set_cpmm_status(&mut env.svm, p, 4),
    }
}

fn spot(env: &Env, pool: &AnyPool) -> u128 {
    let (x, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    u128::from(y) * WAD / u128::from(x)
}

fn root_xy(env: &Env, pool: &AnyPool) -> u128 {
    let (x, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    (u128::from(x) * u128::from(y)).isqrt()
}

/// Real buys on `pool` until its spot is `mult_bps/10⁴` of `p0` (a genuine move).
fn pump_to(env: &mut Env, pool: &AnyPool, p0: u128, mult_bps: u128) {
    let now_p = spot(env, pool) as f64;
    let target = p0 as f64 * mult_bps as f64 / 10_000.0;
    if target <= now_p {
        return;
    }
    let (mint, program) = pool.token();
    let (_, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let a = (y as f64 * ((target / now_p).sqrt() - 1.0) / 0.99) as u64;
    let whale = fork::trader(env, &mint, program, a + 1, 0);
    ok(fork::swap(
        env,
        pool,
        &whale,
        Direction::Buy,
        Swap::ExactIn(a, 0),
    ));
}

/// The adopted market with its carried position, and the keeper (not ticked yet).
struct Switched {
    h: Harness,
    rm: RayMarket,
    cand: AnyPool,
    cp: CpmmPool,
    borrower: Borrower,
    nonce: u64,
    q: u64,
}

/// AIR-13: a Raydium listing starts `L_slow` at the pool's locked depth `D` — half a λ 50 % pool's
/// `√(X·Y)` — and sizing and the tier read it, so an open at the old sizes waits for it: four
/// hours of the keeper's 60-s observations bring `L_slow` within 2 % of the pool (τ_slow 1 h).
fn slow_catch_up(env: &mut Env, rm: &RayMarket) {
    for _ in 0..240 {
        env.warp(60);
        env.crank(rm);
    }
    env.fresh_oracle();
}

/// The M4X-2 incumbent, its halt and the 320-SOL candidate, with a position of `q`: the first
/// size of [`OPEN_BPS`] that opens, capped at `cap(env, candidate)` (the candidate is planted
/// before the open). The real Fartcoin pool, deeper than both planted pools, is halted so no
/// other candidate exists.
fn halted_with_a_position(
    cap: impl FnOnce(&Env, &AnyPool) -> u64,
) -> (Env, RayMarket, AnyPool, AnyPool, AnyPool, Borrower, u64) {
    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let inc = sized(&mut env, true, &base, 5_000, 2_500_000_000_000);
    ok(env.list_ray(&inc, VENUE_HINT_RAYDIUM_V4));
    let q0 = q_of(&env, &inc);
    let rm = env.ray_market_listed(RaySpec::new(FART, q0.saturating_mul(60)), inc);
    env.crank(&rm);
    for _ in 0..5 {
        env.warp(1);
        env.crank(&rm);
    }
    slow_catch_up(&mut env, &rm);
    let cand = sized(&mut env, false, &base, 9_978, 320_000_000_000);
    let cap = cap(&env, &cand);
    let (x_inc, _) = inc
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let mut borrower = env.borrower();
    let nonce = OPEN_BPS
        .into_iter()
        .find_map(|bps| {
            let q = u64::try_from(u128::from(x_inc) * u128::from(bps) / 10_000).expect("u64");
            let (res, nonce) = env.open(&rm, &mut borrower, q.min(cap));
            res.is_ok().then_some(nonce)
        })
        .expect("an open");
    for _ in 0..3 {
        env.warp(1);
        env.crank(&rm);
    }
    halt(&mut env, &base);
    (env, rm, base, inc, cand, borrower, nonce)
}

/// The M4X-2 setup with a position that **fits** the 320-SOL candidate (95 % of its per-position
/// cap, switch only if all fit); the window and the execution sent by the test (the keeper's fast
/// path is `pool_select::a_halted_incumbent_moves_open_positions_to_the_deepest_sibling`).
fn switched_with_a_fitting_position() -> Switched {
    let (mut env, rm, _, inc, cand, borrower, nonce) =
        halted_with_a_position(|env, cand| ps::carried_caps(env, cand).0 / 100 * 95);
    halt(&mut env, &inc);
    let AnyPool::Cpmm(cp) = cand else {
        unreachable!()
    };
    let q = env.position(&rm, &borrower.key(), nonce).q;
    for _ in 0..5 {
        env.warp(30);
        env.crank(&rm);
    }
    ok(env.propose(Some(&inc), &cand));
    for _ in 0..2 {
        env.warp(30);
        env.crank(&rm);
    }
    ok(env.execute_switch(&inc, &cand));
    let m = env.market(&rm.market());
    assert_eq!(
        (m.ema_warm, m.ema_guard),
        (8, 1),
        "adopted and marked (M4R)"
    );
    let rm = rm.on_pool(&env.svm, cand);
    Switched {
        h: Harness::new(env, KeeperConfig::default()),
        rm,
        cand,
        cp,
        borrower,
        nonce,
        q,
    }
}

/// What the attacker does after the adoption.
#[derive(Clone, Debug, Default)]
enum Attack {
    /// Nothing (the control).
    #[default]
    None,
    /// `[deposit 7·LP counter, crank_price, withdraw]` in every second from `from`, after the
    /// keeper's tick; the keeper's tick does not land in the seconds of `misses` (M4G-1).
    Freeze { from: i64, misses: Vec<i64> },
    /// A real deposit of 7× the LP counter at `from`, held and withdrawn at the start of the
    /// ramp's first second (M4G-3).
    Held { from: i64 },
}

/// What a run saw, second by second after the adoption.
#[derive(Debug, Default)]
struct Run {
    /// Seconds the keeper observed the market.
    observed: Vec<i64>,
    /// Seconds the keeper needed per-second coverage because the guard was set.
    guarded: Vec<i64>,
    /// Foreign-deviation seconds the keeper detected (duty 2: prices, per-slot bursts).
    bursts: Vec<i64>,
    /// Foreign-liquidity seconds the keeper detected (FITS-2 (b): per-second coverage).
    liq_covers: Vec<i64>,
    /// At the ramp's start: `(L_ema, L_mlo, ema_guard)` and the pool's depth `√(X·Y)`.
    at_ramp: Option<(u128, u128, u8)>,
    root_at_ramp: u128,
    /// The second the keeper's liquidation of the position landed.
    liquidated: Option<i64>,
}

/// Ticks the keeper once a second from the adoption with the attacker's `attack`; flat until
/// `w`, then a genuine 4×/120 s ramp; until the keeper liquidates the position (at most 300 s of
/// ramp), which it must.
fn run(sw: &mut Switched, w: i64, attack: &Attack) -> Run {
    let key = sw.rm.market();
    let position = pda::position(&key, &sw.borrower.key(), sw.nonce).0;
    let (who, p0, adopted_at) = sw.h.with_env(|env| {
        let (sol, tok) = lp::lp_needed(env, &sw.cp, 7 * sw.cp.state(&env.svm).lp_supply);
        let who = lp::provider(env, &sw.cp, 6 * sol, 6 * tok, 0);
        (who, spot(env, &sw.cand), env.now())
    });
    let mut held = 0u64;
    let mut out = Run::default();
    for s in 1..=(w + 300) {
        if let Attack::Held { .. } = attack
            && s == w + 1
            && held > 0
        {
            let cp = sw.cp;
            sw.h.with_env(|env| ok(env.send(&[lp::withdraw_ix(&cp, &who, held)], &[&who.owner])));
            held = 0;
        }
        if s > w {
            let g = 10_000 + 30_000 * u128::try_from((s - w).min(120)).expect("u128") / 120;
            sw.h.with_env(|env| pump_to(env, &sw.cand, p0, g));
        }
        let missed = matches!(attack, Attack::Freeze { misses, .. } if misses.contains(&s));
        if missed {
            sw.h.idle(1);
        } else {
            let r = sw.h.second().clone();
            assert!(r.errors.is_empty(), "+{s} s: {:?}", r.errors);
            if r.observed(&key) {
                out.observed.push(s);
            }
            if r.needs.get(&key).is_some_and(|n| n.guarded) {
                out.guarded.push(s);
            }
            out.bursts.extend(
                r.bursts
                    .iter()
                    .filter(|(m, _)| *m == key)
                    .map(|(_, t)| t - adopted_at),
            );
            out.liq_covers.extend(
                r.liq_covers
                    .iter()
                    .filter(|(m, _)| *m == key)
                    .map(|(_, t)| t - adopted_at),
            );
            if r.exits.iter().any(|e| {
                e.position == position
                    && e.kind == ExitKind::Liquidate
                    && e.result.landed().is_some()
            }) {
                out.liquidated = Some(s);
                break;
            }
        }
        match attack {
            Attack::Freeze { from, .. } if s >= *from => {
                let rm = &sw.rm;
                let cp = &sw.cp;
                sw.h.with_env(|env| {
                    let add = 7 * cp.state(&env.svm).lp_supply;
                    ok(env.send(
                        &[
                            lp::deposit_ix(cp, &who, add),
                            crank_price_ix(rm),
                            lp::withdraw_ix(cp, &who, add),
                        ],
                        &[&who.owner],
                    ));
                });
            }
            Attack::Held { from } if s == *from => {
                let cp = sw.cp;
                held = sw.h.with_env(|env| {
                    let add = 7 * cp.state(&env.svm).lp_supply;
                    ok(env.send(&[lp::deposit_ix(&cp, &who, add)], &[&who.owner]));
                    add
                });
            }
            _ => {}
        }
        if s == w {
            let m = sw.h.with_env(|env| env.market(&key));
            let o = m.observation();
            let l_mlo = ring_liq_lower_median(&o.ring).expect("ring");
            out.at_ramp = Some((o.ema.liq, l_mlo, o.ema.guard));
            out.root_at_ramp = sw.h.with_env(|env| root_xy(env, &sw.cand));
        }
    }
    let alive =
        sw.h.with_env(|env| read::account(&env.svm, &position).is_some());
    assert!(
        out.liquidated.is_some() && !alive,
        "the keeper did not liquidate the position (q {}, {attack:?}): {out:?}",
        sw.q
    );
    out
}

/// The freeze from the adoption on a position that fits the candidate: the guard never clears
/// (every second has the keeper's honest low), and the keeper stays per second for as long as it
/// is set — through the whole flat phase, not only the 8-integration warm-up — with `L_b` at the
/// lows; the pump is liquidated.
#[test]
fn a_deposit_freeze_from_the_adoption_keeps_the_keeper_per_second() {
    let mut sw = switched_with_a_fitting_position();
    let r = run(
        &mut sw,
        W,
        &Attack::Freeze {
            from: 1,
            misses: Vec::new(),
        },
    );
    println!(
        "M4F-1 keeper, freeze from +1 s: observed {} of the first {W} s, guarded {} s, bursts {:?}, \
         liquidated at +{:?} s",
        r.observed.iter().filter(|&&s| s <= W).count(),
        r.guarded.len(),
        r.bursts,
        r.liquidated
    );
    for s in 1..=W {
        assert!(r.observed.contains(&s), "+{s} s not observed: {r:?}");
    }
    assert!(
        (10..=W).all(|s| r.guarded.contains(&s)),
        "per second because the guard is set, after the warm-up too: {:?}",
        r.guarded
    );
    assert!(
        r.bursts.is_empty(),
        "a liquidity-only freeze starts no per-slot burst"
    );
    let (l_ema, l_mlo, guard) = r.at_ramp.expect("the ramp was reached");
    assert_eq!(guard, 1, "the freeze never clears the guard");
    assert!(
        l_mlo.abs_diff(r.root_at_ramp) * 100 <= r.root_at_ramp,
        "the lows are the pool's depth: L_mlo {l_mlo}, √(X·Y) {}",
        r.root_at_ramp
    );
    assert!(
        l_ema > 7 * r.root_at_ramp,
        "L_ema frozen at the deposit's depth"
    );
}

/// On a position that fits the candidate, the pushes after the honest guard clear only delay
/// the exit, with the real keeper against a control without attacker:
///
/// - M4F-1: a freeze from +40 s — no per-slot burst (the liquidity bursts stay reverted, M4G-2),
///   but the foreign liquidity keeps the keeper per second from the first wrap (FITS-2 (b)), so
///   only that first second is the attacker's: `L_ema` stays where it left it, and the depth cap
///   bounds the reference at `λ/λ_min` in any case;
/// - M4G-1: the same with the keeper's observation missing at +60, +80 and +100 s;
/// - M4G-3: a real 7× deposit held from +20 s and pulled at the start of the pump (after 600 s).
#[test]
fn freezes_after_the_guard_cleared_start_no_burst_and_only_delay_the_exit() {
    let control = run(&mut switched_with_a_fitting_position(), W, &Attack::None);
    let c_at = control.liquidated.expect("control");
    println!("M4F-1 keeper control: liquidated at +{c_at} s");
    assert!(control.bursts.is_empty());
    let freeze = Attack::Freeze {
        from: 40,
        misses: Vec::new(),
    };
    let r = run(&mut switched_with_a_fitting_position(), W, &freeze);
    let (l_ema, _, guard) = r.at_ramp.expect("the ramp was reached");
    println!(
        "M4F-1 keeper, freeze from +40 s: bursts {:?}, L_ema {:.3}× √(X·Y) at the ramp (guard \
         {guard}), liquidated at +{:?} s (+{} s after the control)",
        r.bursts,
        l_ema as f64 / r.root_at_ramp as f64,
        r.liquidated,
        r.liquidated.expect("liquidated") - c_at
    );
    assert!(r.bursts.is_empty(), "no per-slot burst: {r:?}");
    assert_eq!(guard, 0, "the honest warm-up cleared the guard");
    // FITS-2 (b): the first wrap (after the keeper's tick at +40 s) is seen at +40 / +41 s and
    // keeps the keeper per second for as long as the freeze recurs.
    let first = *r.liq_covers.first().expect("liquidity coverage");
    assert!((40..=41).contains(&first), "{:?}", r.liq_covers);
    assert!(
        (first + 1..=W).all(|s| r.observed.contains(&s)),
        "per second under the freeze: {:?}",
        r.observed
    );
    assert!(control.liq_covers.is_empty(), "honest depths start nothing");
    let freeze_at = r.liquidated.expect("liquidated");
    assert!((c_at..=c_at + 30).contains(&freeze_at), "a delay only");

    let misses = Attack::Freeze {
        from: 40,
        misses: vec![60, 80, 100],
    };
    let r = run(&mut switched_with_a_fitting_position(), W + 30, &misses);
    let at = r.liquidated.expect("liquidated");
    println!(
        "M4G-1 keeper, freeze from +40 s, misses at +60/+80/+100 s: liquidated at +{at} s (ramp \
         +30 s later than the control's: +{} s after its exit)",
        at - 30 - c_at
    );
    assert!(r.bursts.is_empty());
    assert!((c_at + 30..=c_at + 60).contains(&at), "a delay only");

    let held = Attack::Held { from: 20 };
    let r = run(&mut switched_with_a_fitting_position(), 600, &held);
    let at = r.liquidated.expect("liquidated");
    let (l_ema, _, _) = r.at_ramp.expect("the ramp was reached");
    println!(
        "M4G-3 keeper, a 7× deposit held +20..+600 s and pulled at the pump: bursts {:?}, L_ema \
         {:.3}× the held depth at the ramp, liquidated at +{at} s (+{} s after the control's \
         ramp-relative exit)",
        r.bursts,
        l_ema as f64 / r.root_at_ramp as f64,
        at - 600 - (c_at - W)
    );
    assert!(
        r.bursts.is_empty(),
        "a held deposit is honest depth: no burst"
    );
    assert!(
        (600 + c_at - W..=600 + c_at - W + 30).contains(&at),
        "a delay only"
    );
}

/// M4F-1 / M4G-3's own market: a position at the incumbent's size cap (≈ 3.1 % of the 320-SOL
/// candidate's `X`, above its per-position cap) is **not** carried. The incumbent halts 900 s
/// before the position's deadline; the keeper keeps the chains (duty 5), its pool monitor finds
/// the CPMM candidate and a 400-SOL AMM v4 sibling, both eligible and deep enough for the fast
/// path, and proposes neither (its plan: `CarriedPositionsTooLarge`); a direct proposal is
/// refused, before and after an LP's real 7× deposit held in the candidate (M4G-3: `D`, which
/// bounds `X₀`, does not move). No window opens. At the deadline the keeper settles the position
/// in S3 at fair value on the failed pool: the lenders get `FV(P_ema, L_ema)` in full.
#[test]
fn an_oversized_carried_position_is_not_adopted_and_settles_in_s3() {
    use shortwin::errors::ShortwinError;
    use shortwin::events::SolSettle;
    use shortwin_keeper::events;

    let (mut env, rm, base, inc, cand, borrower, nonce) = halted_with_a_position(|_, _| u64::MAX);
    let AnyPool::Cpmm(cp) = cand else {
        unreachable!()
    };
    let key = rm.market();
    let pos = env.position(&rm, &borrower.key(), nonce);
    let (cap, _) = ps::carried_caps(&env, &cand);
    assert!(pos.q > cap, "q {} over the candidate's cap {cap}", pos.q);
    let v4 = sized(&mut env, true, &base, 9_978, 400_000_000_000);
    let (cap_v4, _) = ps::carried_caps(&env, &v4);
    assert!(
        pos.q > cap_v4,
        "neither candidate fits: {} > {cap_v4}",
        pos.q
    );
    let position = pda::position(&key, &borrower.key(), nonce).0;
    let mut h = Harness::new(env, KeeperConfig::default());
    let wait = pos.deadline - 900 - h.now();
    h.idle(wait);
    h.seconds(3);
    h.with_env(|env| halt(env, &inc));
    let halted_at = h.now();
    let refused = |h: &Harness| {
        h.with_env(|env| custom_code(&env.propose(Some(&inc), &cand)))
            == Some(u32::from(ShortwinError::CarriedPositionsTooLarge))
    };
    let too_large = Some(u32::from(ShortwinError::CarriedPositionsTooLarge));
    let (mut blocked, mut settled) = (0u32, None);
    for s in 1..=1_200i64 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "+{s} s: {:?}", r.errors);
        let s3 = r
            .exits
            .iter()
            .filter(|e| e.position == position && e.kind == ExitKind::SolSettle)
            .find_map(|e| e.result.landed())
            .and_then(|out| events::of::<SolSettle>(out).into_iter().next());
        if let Some(e) = s3 {
            // Once the position is settled the market is empty and the keeper's fast path may
            // move it (a window at zero positions fits trivially).
            settled = Some((s, e));
            break;
        }
        assert!(r.pool_actions.is_empty(), "+{s} s: {:?}", r.pool_actions);
        if r.pool_plans
            .get(&key)
            .is_some_and(|p| p.blocked == too_large)
        {
            blocked += 1;
        }
        if s == 150 {
            assert!(refused(&h), "the candidate is refused");
            let add = h.with_env(|env| {
                let add = 7 * cp.state(&env.svm).lp_supply;
                let (sol, tok) = lp::lp_needed(env, &cp, add);
                let who = lp::provider(env, &cp, sol, tok, 0);
                ok(env.send(&[lp::deposit_ix(&cp, &who, add)], &[&who.owner]));
                add
            });
            assert!(add > 0);
            assert!(refused(&h), "a held deposit does not make it fit (M4G-3)");
        }
        let m = h.with_env(|env| env.market(&key));
        assert_eq!(m.fp_t0, 0, "+{s} s: no window");
    }
    let (s, e) = settled.expect("an S3 settlement at the deadline");
    let p_ema = h.with_env(|env| env.market(&key).ema_price.get());
    let owed = u128::from(pos.q) * p_ema / WAD;
    println!(
        "M4F-1 / M4G-3 keeper, oversized (q {} > cap {cap}): {blocked} plans blocked \
         CarriedPositionsTooLarge, no proposal; S{} at +{s} s after the halt (deadline +{} s): \
         lenders {:.3} SOL (FV {:.3}; q at P_ema {:.3}), borrower {:.3} SOL",
        pos.q,
        e.condition,
        pos.deadline - halted_at,
        e.lender_amount as f64 / 1e9,
        e.fv as f64 / 1e9,
        owed as f64 / 1e9,
        e.borrower_amount as f64 / 1e9
    );
    assert!(blocked > 0, "the keeper's plan reports the refusal");
    assert_eq!(e.condition, 3, "S3");
    assert!(
        e.fv > 0 && e.lender_amount == e.fv,
        "the lenders get FV in full"
    );
    assert!(u128::from(e.lender_amount) >= owed, "at least q at P_ema");
    assert!(e.borrower_amount > 0);
}

// ---- FIT-1, FIT-2 (the verification of #99) and M4G-2 after the revert ---------------------

/// A knob: the environment variable `k` parsed, else `d` (exploration only; the tests' defaults
/// are the regressions).
fn envv<T: std::str::FromStr>(k: &str, d: T) -> T {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

/// Sets the CPMM pool `cp`'s config trade fee to `fee_ppm` (its protocol, fund and creator rates
/// kept).
fn set_cpmm_fee(env: &mut Env, cp: &CpmmPool, fee_ppm: u64) {
    let config = shortwin_litesvm_tests::raydium::cpmm_config_key(&env.svm, cp);
    let acc = env.svm.get_account(&config).expect("config");
    let c = shortwin_venue_state::raydium_cpmm::read_config(&acc.owner.to_bytes(), &acc.data)
        .expect("config");
    fork::set_cpmm_config_rates(
        &mut env.svm,
        &config,
        fee_ppm,
        c.protocol_fee_rate,
        c.fund_fee_rate,
        c.creator_fee_rate,
    );
}

/// The M4X-2 incumbent (2,500 SOL, λ 50 %, AMM v4, 25 bps) and the 320-SOL λ 99.78 % CPMM
/// candidate with its trade fee set to `fee_ppm`, and a position of `q(env, market, candidate)`
/// (which may also move the candidate; capped by the first size of [`OPEN_BPS`] that opens), 3 s
/// of cranks, the real Fartcoin pool halted: the market before the incumbent's halt.
fn fee_market(
    fee_ppm: u64,
    q: impl FnOnce(&mut Env, &RayMarket, &AnyPool) -> u64,
) -> (Env, RayMarket, AnyPool, AnyPool, AnyPool, Borrower, u64) {
    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let inc = sized(&mut env, true, &base, 5_000, 2_500_000_000_000);
    ok(env.list_ray(&inc, VENUE_HINT_RAYDIUM_V4));
    let q0 = q_of(&env, &inc);
    let rm = env.ray_market_listed(RaySpec::new(FART, q0.saturating_mul(60)), inc);
    env.crank(&rm);
    for _ in 0..5 {
        env.warp(1);
        env.crank(&rm);
    }
    slow_catch_up(&mut env, &rm);
    let cand = sized(&mut env, false, &base, 9_978, 320_000_000_000);
    let AnyPool::Cpmm(cp) = cand else {
        unreachable!()
    };
    set_cpmm_fee(&mut env, &cp, fee_ppm);
    let cap = q(&mut env, &rm, &cand);
    let (x_inc, _) = inc
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let mut borrower = env.borrower();
    let nonce = OPEN_BPS
        .into_iter()
        .find_map(|bps| {
            let q = u64::try_from(u128::from(x_inc) * u128::from(bps) / 10_000).expect("u64");
            let (res, nonce) = env.open(&rm, &mut borrower, q.min(cap));
            res.is_ok().then_some(nonce)
        })
        .expect("an open");
    for _ in 0..3 {
        env.warp(1);
        env.crank(&rm);
    }
    halt(&mut env, &base);
    (env, rm, base, inc, cand, borrower, nonce)
}

/// Halts `inc`, runs the chain 150 s, proposes `cand` (which must open the window), waits the
/// window and executes: the position carried onto `cand`; the keeper harness built.
fn switch_now(
    mut env: Env,
    rm: RayMarket,
    inc: &AnyPool,
    cand: AnyPool,
    borrower: Borrower,
    nonce: u64,
) -> Switched {
    halt(&mut env, inc);
    let AnyPool::Cpmm(cp) = cand else {
        unreachable!()
    };
    let q = env.position(&rm, &borrower.key(), nonce).q;
    for _ in 0..5 {
        env.warp(30);
        env.crank(&rm);
    }
    ok(env.propose(Some(inc), &cand));
    for _ in 0..2 {
        env.warp(30);
        env.crank(&rm);
    }
    ok(env.execute_switch(inc, &cand));
    let rm = rm.on_pool(&env.svm, cand);
    Switched {
        h: Harness::new(env, KeeperConfig::default()),
        rm,
        cand,
        cp,
        borrower,
        nonce,
        q,
    }
}

/// How a run ended: `Ok(second)` of the keeper's liquidation, or `Err((second, condition,
/// lenders, FV, q at spot))` of a `SolSettle`.
type Outcome = Result<i64, (i64, u8, u64, u64, u128)>;

/// Ticks the keeper once a second with the attacker's `attack` (as [`run`]); flat until `w`,
/// then a genuine linear pump to 4× over `ramp` s; until the position exits (a liquidation or a
/// `SolSettle`; at most 900 s after the ramp starts). The position's `V/B` on the new pool is
/// printed.
fn run_to_exit(sw: &mut Switched, w: i64, ramp: i64, attack: &Attack) -> Outcome {
    use shortwin::events::SolSettle;
    use shortwin_keeper::events;
    let key = sw.rm.market();
    let position = pda::position(&key, &sw.borrower.key(), sw.nonce).0;
    let (who, p0) = sw.h.with_env(|env| {
        let (sol, tok) = lp::lp_needed(env, &sw.cp, 7 * sw.cp.state(&env.svm).lp_supply);
        let who = lp::provider(env, &sw.cp, 6 * sol, 6 * tok, 0);
        (who, spot(env, &sw.cand))
    });
    sw.h.with_env(|env| {
        let pos = env.position(&sw.rm, &sw.borrower.key(), sw.nonce);
        let (x, _) = sw
            .cand
            .io_reserves(&env.svm, Direction::Sell)
            .expect("reserves");
        let b = sw
            .cand
            .quote_exact_out(&env.svm, Direction::Buy, pos.q)
            .map_or(0, |q| q.cost);
        println!(
            "  position: q {} ({:.3} % of X), v {}, live buyback {b} (V/B {:.3})",
            pos.q,
            pos.q as f64 * 100.0 / x as f64,
            pos.v,
            pos.v as f64 / b.max(1) as f64
        );
    });
    let mut held = 0u64;
    for s in 1..=(w + 900) {
        if let Attack::Held { .. } = attack
            && s == w + 1
            && held > 0
        {
            let cp = sw.cp;
            sw.h.with_env(|env| ok(env.send(&[lp::withdraw_ix(&cp, &who, held)], &[&who.owner])));
            held = 0;
        }
        if s > w {
            let g = 10_000
                + 30_000 * u128::try_from((s - w).min(ramp)).expect("u128")
                    / u128::try_from(ramp).expect("u128");
            sw.h.with_env(|env| pump_to(env, &sw.cand, p0, g));
        }
        let missed = matches!(attack, Attack::Freeze { misses, .. } if misses.contains(&s));
        if missed {
            sw.h.idle(1);
        } else {
            let r = sw.h.second().clone();
            assert!(r.errors.is_empty(), "+{s} s: {:?}", r.errors);
            for e in r.exits.iter().filter(|e| e.position == position) {
                if let Some(out) = e.result.landed() {
                    match e.kind {
                        ExitKind::Liquidate => return Ok(s),
                        ExitKind::SolSettle => {
                            if let Some(ev) = events::of::<SolSettle>(out).into_iter().next() {
                                let owed = sw
                                    .h
                                    .with_env(|env| u128::from(sw.q) * spot(env, &sw.cand) / WAD);
                                return Err((s, ev.condition, ev.lender_amount, ev.fv, owed));
                            }
                        }
                        ExitKind::Expire => panic!("+{s} s: expired"),
                    }
                }
            }
        }
        match attack {
            Attack::Freeze { from, .. } if s >= *from => {
                let rm = &sw.rm;
                let cp = &sw.cp;
                sw.h.with_env(|env| {
                    let add = 7 * cp.state(&env.svm).lp_supply;
                    ok(env.send(
                        &[
                            lp::deposit_ix(cp, &who, add),
                            crank_price_ix(rm),
                            lp::withdraw_ix(cp, &who, add),
                        ],
                        &[&who.owner],
                    ));
                });
            }
            Attack::Held { from } if s == *from => {
                let cp = sw.cp;
                let k: u64 = envv("FIT_K", 7);
                held = sw.h.with_env(|env| {
                    let add = k * cp.state(&env.svm).lp_supply;
                    ok(env.send(&[lp::deposit_ix(&cp, &who, add)], &[&who.owner]));
                    add
                });
            }
            _ => {}
        }
    }
    panic!("no exit");
}

/// The attacks of FIT-2's grid: (name, the ramp's start W, the attack).
fn fit_attacks(w_held: i64) -> [(&'static str, i64, Attack); 4] {
    [
        ("control", W, Attack::None),
        (
            "held 7x +20 s, pulled at the pump (M4G-3)",
            w_held,
            Attack::Held { from: 20 },
        ),
        (
            "freeze from +40 s (M4F-1)",
            W,
            Attack::Freeze {
                from: 40,
                misses: Vec::new(),
            },
        ),
        (
            "freeze from +40 s, misses at +60/80/100 s (M4G-1)",
            W + 30,
            Attack::Freeze {
                from: 40,
                misses: vec![60, 80, 100],
            },
        ),
    ]
}

/// Runs every attack of [`fit_attacks`] on a fresh market from `setup`, printing each outcome
/// and its delay after the control (ramp-relative); returns them.
fn fit_grid(label: &str, ramp: i64, setup: impl Fn() -> Switched) -> Vec<(&'static str, Outcome)> {
    let w_held: i64 = envv("FIT_W", 600);
    let which: String = envv("FIT_ATTACK", "all".to_string());
    let mut out = Vec::new();
    let mut control = None;
    for (name, w, attack) in fit_attacks(w_held) {
        if which != "all" && !name.starts_with(&which) {
            continue;
        }
        println!("{label}, 4x/{ramp} s pump, {name}:");
        let mut sw = setup();
        let r = run_to_exit(&mut sw, w, ramp, &attack);
        let rel = r.as_ref().map(|s| s - w);
        if name == "control" {
            control = rel.ok();
        }
        match (&r, control) {
            (Ok(_), Some(c)) => println!(
                "  liquidated at +{} s into the ramp ({:+} s after the control)",
                rel.expect("ok"),
                rel.expect("ok") - c
            ),
            (Ok(_), None) => println!("  liquidated at +{} s into the ramp", rel.expect("ok")),
            (Err((s, cond, lenders, _, owed)), _) => println!(
                "  S{cond} at +{} s into the ramp: lenders {:.3} SOL for q worth {:.3} SOL at spot",
                s - w,
                *lenders as f64 / 1e9,
                *owed as f64 / 1e9
            ),
        }
        out.push((name, r.map(|s| s - w)));
    }
    out
}

/// The one-way fee of the M4X-2 incumbent (AMM v4, 25 bps): the fee the positions of
/// [`fee_market`] post their margin at, hence their `min_open_fee_ppm`.
const INC_FEE_PPM: u32 = 2_500;

/// The per-position cap of a position carried onto `cand` if the fee-scaled cap took `phi_ppm`
/// (the fit as a proposal judges it: `X₀` with the live `X`, the smaller at `min(P_ema, P_cand)`
/// and `P_ema`, tiers A–C's 5 % impact cap).
fn carried_cap_at(env: &Env, rm: &RayMarket, cand: &AnyPool, phi_ppm: u32) -> u64 {
    let params = env.config().params;
    let d = shortwin_litesvm_tests::raydium::locked_depth(&env.svm, cand);
    let m = env.market(&rm.market());
    let (x, _) = cand
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let p_ema = m.ema_price.get();
    [spot(env, cand).min(p_ema), p_ema]
        .into_iter()
        .map(|p| {
            let x0 = shortwin_math::caps::conservative_x0(
                x,
                m.liq_slow.get().min(d),
                p,
                Some((d, params.pool_lock_min_bps)),
            )
            .expect("x0");
            shortwin_math::caps::position_cap(x0, 500, phi_ppm, params.fee_size_cap_x100)
                .expect("cap")
        })
        .min()
        .expect("a price")
}

/// **FIT-2, not adopted** (the documented fallback, ECON #100). Before, the fit took the
/// candidate's own fee: a position at 76 % of a 1 % candidate's fit limit at that fee (its impact
/// cap, 2.47 % of `X₀`; the position 1.87 % of its `X`), margined on the deep 25-bps incumbent
/// (V/B 2.93 on the new pool against 3.12 for the same size opened there), was adopted, and a
/// 7× deposit held and pulled at a genuine 4×/30 s pump (M4G-3), the freeze after the guard's clear
/// (M4F-1) and the freeze with keeper misses (M4G-1) all ended in S1 (lenders 17.876 SOL for `q`
/// worth 23.973 SOL at spot). Now the fee-scaled caps take `min(min_open_fee_ppm, φ_cand)` =
/// 25 bps (0.75 % of `X₀`): the same position does not fit, the keeper proposes nothing (its
/// plan: `CarriedPositionsTooLarge`), a direct proposal is refused, no window opens — no push
/// can reach it on the new pool — and the keeper settles it in S3 at fair value at its deadline
/// on the failed pool. Knobs: `FIT_FEE` (ppm), `FIT_PM` (permille of the cap at the candidate's
/// own fee).
#[test]
fn fit2_a_position_at_the_one_percent_fit_limit_is_not_adopted_and_settles_in_s3() {
    let fee: u64 = envv("FIT_FEE", 10_000);
    let pm: u64 = envv("FIT_PM", 760);
    let phi = u32::try_from(fee).expect("u32");
    let (env, rm, _, inc, cand, borrower, nonce) = fee_market(fee, |env, rm, cand| {
        carried_cap_at(env, rm, cand, phi) / 1_000 * pm
    });
    let key = rm.market();
    let pos = env.position(&rm, &borrower.key(), nonce);
    let m = env.market(&key);
    assert_eq!(
        m.min_open_fee_ppm, INC_FEE_PPM,
        "margined at the incumbent's fee"
    );
    let (x_c, _) = cand
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let own = carried_cap_at(&env, &rm, &cand, phi);
    let (cap, _) = ps::carried_caps(&env, &cand);
    println!(
        "FIT-2, fee {fee} ppm: q {} ({:.3} % of the candidate's X) = {:.3} of the cap at its own \
         fee ({own}), {:.2}× the carried cap at min(25 bps, fee) ({cap})",
        pos.q,
        pos.q as f64 * 100.0 / x_c as f64,
        pos.q as f64 / own as f64,
        pos.q as f64 / cap as f64
    );
    assert!(pos.q <= own, "it fitted the candidate's own fee");
    assert!(pos.q > cap, "it does not fit the fee it was margined at");
    not_adopted_then_s3(
        &format!("FIT-2, fee {fee} ppm"),
        env,
        &rm,
        &inc,
        &cand,
        &borrower,
        nonce,
    );
}

/// The keeper on `rm` (an incumbent `inc` still trading, a candidate `cand` the position does
/// not fit) from 900 s before the position's deadline: the incumbent halts; for 1,200 s the
/// keeper's plan reports `CarriedPositionsTooLarge` and it proposes nothing, a direct proposal at
/// +150 s is refused, no window opens; the keeper settles the position in S3 at its due second
/// `max(deadline, no_route_since + s3_wait_s, graduated_at + grad_grace_s)` with the lenders paid
/// `FV` in full (at least `q` at `P_ema`) and the borrower the rest.
fn not_adopted_then_s3(
    label: &str,
    env: Env,
    rm: &RayMarket,
    inc: &AnyPool,
    cand: &AnyPool,
    borrower: &Borrower,
    nonce: u64,
) {
    use shortwin::errors::ShortwinError;
    use shortwin::events::SolSettle;
    use shortwin_keeper::events;
    let key = rm.market();
    let pos = env.position(rm, &borrower.key(), nonce);
    let position = pda::position(&key, &borrower.key(), nonce).0;
    let mut h = Harness::new(env, KeeperConfig::default());
    let wait = pos.deadline - 900 - h.now();
    h.idle(wait);
    h.seconds(3);
    h.with_env(|env| halt(env, inc));
    let halted_at = h.now();
    let too_large = Some(u32::from(ShortwinError::CarriedPositionsTooLarge));
    let (mut blocked, mut settled) = (0u32, None);
    for s in 1..=1_200i64 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "+{s} s: {:?}", r.errors);
        let s3 = r
            .exits
            .iter()
            .filter(|e| e.position == position && e.kind == ExitKind::SolSettle)
            .find_map(|e| e.result.landed())
            .and_then(|out| events::of::<SolSettle>(out).into_iter().next());
        if let Some(e) = s3 {
            settled = Some((s, h.now(), e));
            break;
        }
        assert!(r.pool_actions.is_empty(), "+{s} s: {:?}", r.pool_actions);
        if r.pool_plans
            .get(&key)
            .is_some_and(|p| p.blocked == too_large)
        {
            blocked += 1;
        }
        if s == 150 {
            let res = h.with_env(|env| custom_code(&env.propose(Some(inc), cand)));
            assert_eq!(res, too_large, "a direct proposal is refused");
        }
        let m = h.with_env(|env| env.market(&key));
        assert_eq!(m.fp_t0, 0, "+{s} s: no window");
    }
    let (s, at, e) = settled.expect("an S3 settlement at the deadline");
    let m = h.with_env(|env| env.market(&key));
    let due = pos
        .deadline
        .max(m.no_route_since + i64::from(pos.s3_wait_s))
        .max(m.graduated_at + i64::from(pos.grad_grace_s));
    let owed = u128::from(pos.q) * m.ema_price.get() / WAD;
    println!(
        "{label}: {blocked} plans blocked CarriedPositionsTooLarge, no proposal; S{} \
         at +{s} s after the halt ({} s after the due second; deadline +{} s): lenders {:.3} SOL \
         (FV {:.3}; q at P_ema {:.3}), borrower {:.3} SOL",
        e.condition,
        at - due,
        pos.deadline - halted_at,
        e.lender_amount as f64 / 1e9,
        e.fv as f64 / 1e9,
        owed as f64 / 1e9,
        e.borrower_amount as f64 / 1e9
    );
    assert!(blocked > 0, "the keeper's plan reports the refusal");
    assert_eq!(e.condition, 3, "S3");
    assert!(at >= due && at - due <= 2, "S3 at its due second");
    assert!(
        e.fv > 0 && e.lender_amount == e.fv,
        "the lenders get FV in full"
    );
    assert!(u128::from(e.lender_amount) >= owed, "at least q at P_ema");
    assert!(e.borrower_amount > 0);
}

/// **FIT-2 at the new fit limit.** On the same 1 % candidate a position at `FIT_PM` (default
/// 950 ‰) of the carried per-position cap — the fee-scaled cap at `min(min_open_fee_ppm, φ_cand)`
/// = 25 bps, 0.75 % of `X₀`, the class it was margined at — is adopted; the control and the three
/// pushes of FIT-2 (M4G-3's held deposit, M4F-1's freeze, M4G-1's misses) with the genuine
/// 4×/30 s pump all end in a normal liquidation (no S1), within `FIT_SLACK` (default 3) s of the
/// control (measured: +0 s at 30 s, 45 s and 60 s ramps, +0–1 s at 120 s, +0–2 s at 300 s; at
/// 1,000 ‰ +0 s). A 4×/20 s pump ends in S1 in the control too, pushes or not: the fast-pump class
/// native positions share (ECON residual 2), not a carried-fit effect. Knobs: `FIT_FEE`, `FIT_PM`,
/// `FIT_RAMP`, `FIT_W`, `FIT_K`, `FIT_ATTACK`, `FIT_SLACK`.
#[test]
fn fit2_a_carried_position_at_the_new_fit_limit_is_liquidated_under_the_pushes() {
    let fee: u64 = envv("FIT_FEE", 10_000);
    let pm: u64 = envv("FIT_PM", 950);
    let ramp: i64 = envv("FIT_RAMP", 30);
    let slack: i64 = envv("FIT_SLACK", 3);
    let setup = || {
        let (env, rm, _, inc, cand, borrower, nonce) = fee_market(fee, |env, rm, cand| {
            let phi =
                shortwin_math::caps::carried_phi(INC_FEE_PPM, u32::try_from(fee).expect("u32"));
            carried_cap_at(env, rm, cand, phi) / 1_000 * pm
        });
        assert_eq!(env.market(&rm.market()).min_open_fee_ppm, INC_FEE_PPM);
        switch_now(env, rm, &inc, cand, borrower, nonce)
    };
    let res = fit_grid(
        &format!("FIT-2, fee {fee} ppm, {pm}‰ of the carried fit limit"),
        ramp,
        setup,
    );
    let control = res
        .iter()
        .find(|(n, _)| *n == "control")
        .and_then(|(_, r)| r.ok());
    for (name, r) in &res {
        let at = r.unwrap_or_else(|e| panic!("{name}: not liquidated: {e:?}"));
        if let Some(c) = control {
            assert!(at - c <= slack, "{name}: +{} s after the control", at - c);
        }
    }
}

/// FIT-2's steady-state counterpart: the same 320-SOL 1 % CPMM listed directly (no switch), a
/// position at its own open cap (its margin posted on this pool's cost: V/B 3.12), under the
/// same pushes from its listing: liquidated as the control (a native position's margin covers
/// what a carried one at the candidate's own fee did not; the carried fit now keeps carried
/// positions in the size class of the fee they were margined at).
#[test]
fn fit2_a_native_position_at_the_open_cap_is_liquidated_under_the_same_pushes() {
    use shortwin_litesvm_tests::raydium::VENUE_HINT_RAYDIUM_CPMM;
    let fee: u64 = envv("FIT_FEE", 10_000);
    let ramp: i64 = envv("FIT_RAMP", 30);
    let setup = || {
        let mut env = Env::ray_fork();
        let base = env.load_ray_pool(FART);
        let pool = sized(&mut env, false, &base, 9_978, 320_000_000_000);
        let AnyPool::Cpmm(cp) = pool else {
            unreachable!()
        };
        set_cpmm_fee(&mut env, &cp, fee);
        ok(env.list_ray(&pool, VENUE_HINT_RAYDIUM_CPMM));
        let q0 = q_of(&env, &pool);
        let rm = env.ray_market_listed(RaySpec::new(FART, q0.saturating_mul(60)), pool);
        env.crank(&rm);
        for _ in 0..12 {
            env.warp(1);
            env.crank(&rm);
        }
        let params = env.config().params;
        let m = env.market(&rm.market());
        let (x, _) = pool
            .io_reserves(&env.svm, Direction::Sell)
            .expect("reserves");
        let d = shortwin_litesvm_tests::raydium::locked_depth(&env.svm, &pool);
        let x0 = shortwin_math::caps::conservative_x0(
            x,
            m.liq_slow.get(),
            m.ema_price.get(),
            Some((d, params.pool_lock_min_bps)),
        )
        .expect("x0");
        let cap = shortwin_math::caps::position_cap(
            x0,
            500,
            u32::try_from(fee).expect("u32"),
            params.fee_size_cap_x100,
        )
        .expect("cap");
        let mut borrower = env.borrower();
        let mut q = cap;
        let nonce = loop {
            let (res, nonce) = env.open(&rm, &mut borrower, q);
            if res.is_ok() {
                break nonce;
            }
            q = q / 1_000 * 995;
        };
        for _ in 0..3 {
            env.warp(1);
            env.crank(&rm);
        }
        Switched {
            h: Harness::new(env, KeeperConfig::default()),
            rm,
            cand: pool,
            cp,
            borrower,
            nonce,
            q,
        }
    };
    let res = fit_grid(&format!("FIT-2 native, fee {fee} ppm"), ramp, setup);
    for (name, r) in &res {
        assert!(r.is_ok(), "{name}: not liquidated: {r:?}");
    }
}

/// **FIT-1, under the pushes** (a documented residual). The 320-SOL candidate genuinely trades
/// at `r`× the incumbent's price before the halt, so after it at `r`× the frozen `P_ema`; the
/// position is sized at 99 % of the cap the execution judges (`X₀` without the live `X_cand`; the
/// fee-scaled cap at the carried fee `min(min_open_fee_ppm, φ_cand)` = 25 bps, FIT-2), above the
/// literal cap (with it): the honest proposal is refused, a dump-wrapped one opens the window and
/// the execution adopts — `q` up to `λ·√r`× a native open's cap on the new pool (r 1.5: 0.91 % of
/// its `X`, 1.21× the literal cap). Under the M4F-1 / M4G-1 / M4G-3 pushes it is still
/// liquidated, within a second of the control. Knobs: `FIT_FEE`, `FIT_R` (bps), `FIT_PM`,
/// `FIT_RAMP`, `FIT_W`, `FIT_K`, `FIT_ATTACK`.
#[test]
fn fit1_a_dump_wrapped_carried_position_is_liquidated_under_the_pushes() {
    use shortwin_litesvm_tests::pool_select::propose_pool_ix;
    use shortwin_litesvm_tests::raydium::candidate_read_set;
    use solana_signer::Signer as _;
    let fee: u64 = envv("FIT_FEE", 10_000);
    let r_bps: u64 = envv("FIT_R", 15_000);
    let pm: u64 = envv("FIT_PM", 990);
    let ramp: i64 = envv("FIT_RAMP", 120);
    let setup = || {
        let (mut env, rm, _, inc, cand, borrower, nonce) = fee_market(fee, |env, rm, cand| {
            let p_c0 = spot(env, cand);
            pump_to(env, cand, p_c0, u128::from(r_bps));
            let params = env.config().params;
            let d = shortwin_litesvm_tests::raydium::locked_depth(&env.svm, cand);
            let m = env.market(&rm.market());
            let x0 = shortwin_math::caps::conservative_x0(
                u64::MAX,
                m.liq_slow.get().min(d),
                m.ema_price.get(),
                Some((d, params.pool_lock_min_bps)),
            )
            .expect("x0");
            // The fee-scaled cap at the carried fee, min(25 bps margined at, the candidate's).
            let phi =
                shortwin_math::caps::carried_phi(INC_FEE_PPM, u32::try_from(fee).expect("u32"));
            shortwin_math::caps::position_cap(x0, 500, phi, params.fee_size_cap_x100).expect("cap")
                / 1_000
                * pm
        });
        assert_eq!(env.market(&rm.market()).min_open_fee_ppm, INC_FEE_PPM);
        let q = env.position(&rm, &borrower.key(), nonce).q;
        let lit = ps::carried_caps(&env, &cand).0;
        let (x_c, _) = cand
            .io_reserves(&env.svm, Direction::Sell)
            .expect("reserves");
        println!(
            "  FIT-1 setup: P_cand/P_ema {:.3}, q {q} = {:.3} % of the candidate's X, {:.3}× the \
             literal cap {lit}",
            spot(&env, &cand) as f64 / env.market(&rm.market()).ema_price.get() as f64,
            q as f64 * 100.0 / x_c as f64,
            q as f64 / lit as f64,
        );
        assert!(q > lit, "q above the literal cap");
        halt(&mut env, &inc);
        for _ in 0..5 {
            env.warp(30);
            env.crank(&rm);
        }
        assert!(
            env.propose(Some(&inc), &cand).is_err(),
            "the honest proposal is refused"
        );
        // [sell X tokens into the candidate, propose_pool, buy them back].
        let (x, _) = cand
            .io_reserves(&env.svm, Direction::Sell)
            .expect("reserves");
        let (mint, program) = cand.token();
        let t = fork::trader(&mut env, &mint, program, 0, x + 1);
        let owner = t.owner.insecure_clone();
        let out = cand
            .quote_exact_in(&env.svm, Direction::Sell, x)
            .expect("quote")
            .proceeds;
        let sell = cand.swap_ix(
            &env.svm,
            &owner.pubkey(),
            &t,
            Direction::Sell,
            Swap::ExactIn(x, 0),
        );
        let buy = cand.swap_ix(
            &env.svm,
            &owner.pubkey(),
            &t,
            Direction::Buy,
            Swap::ExactIn(out, 0),
        );
        let inc_rs = candidate_read_set(&env.svm, &inc, false);
        let prop = propose_pool_ix(&env.svm, &mint, &inc_rs, &cand);
        ok(env.send(&[sell, prop, buy], &[&owner]));
        for _ in 0..2 {
            env.warp(30);
            env.crank(&rm);
        }
        ok(env.execute_switch(&inc, &cand));
        assert_eq!(
            env.market(&rm.market()).venue_kind(),
            Some(shortwin::state::VenueKind::RaydiumCpmm),
            "adopted"
        );
        let AnyPool::Cpmm(cp) = cand else {
            unreachable!()
        };
        let rm = rm.on_pool(&env.svm, cand);
        Switched {
            h: Harness::new(env, KeeperConfig::default()),
            rm,
            cand,
            cp,
            borrower,
            nonce,
            q,
        }
    };
    let res = fit_grid(
        &format!("FIT-1, fee {fee} ppm, r {r_bps} bps, {pm}‰ of the execution's cap"),
        ramp,
        setup,
    );
    for (name, r) in &res {
        assert!(r.is_ok(), "{name}: not liquidated: {r:?}");
    }
}

// ---- FITS-1: the carried fee capped at 25 bps --------------------------------------------------

/// FITS-1's market (the verifier's reproducer): a **1 %** CPMM incumbent (2,500 SOL, λ 50 %) and
/// the 320-SOL λ 99.78 % CPMM candidate, both on the planted template's AmmConfig set to `fee_ppm`
/// (so the positions are margined at the candidate's own fee: `min_open_fee_ppm = φ_cand`); a
/// position of `q(env, market, candidate)` (capped by the first size of [`OPEN_BPS`] that opens),
/// 3 s of cranks, the real Fartcoin pool halted: the market before the incumbent's halt.
fn equal_fee_market(
    fee_ppm: u64,
    q: impl FnOnce(&mut Env, &RayMarket, &AnyPool) -> u64,
) -> (Env, RayMarket, AnyPool, AnyPool, AnyPool, Borrower, u64) {
    use shortwin_litesvm_tests::raydium::{VENUE_HINT_RAYDIUM_CPMM, cpmm_config_key};
    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let inc = sized(&mut env, false, &base, 5_000, 2_500_000_000_000);
    let AnyPool::Cpmm(icp) = inc else {
        unreachable!()
    };
    set_cpmm_fee(&mut env, &icp, fee_ppm);
    ok(env.list_ray(&inc, VENUE_HINT_RAYDIUM_CPMM));
    let q0 = q_of(&env, &inc);
    let rm = env.ray_market_listed(RaySpec::new(FART, q0.saturating_mul(60)), inc);
    env.crank(&rm);
    for _ in 0..5 {
        env.warp(1);
        env.crank(&rm);
    }
    let cand = sized(&mut env, false, &base, 9_978, 320_000_000_000);
    let AnyPool::Cpmm(ccp) = cand else {
        unreachable!()
    };
    assert_eq!(
        cpmm_config_key(&env.svm, &ccp),
        cpmm_config_key(&env.svm, &icp),
        "one config, one fee"
    );
    let cap = q(&mut env, &rm, &cand);
    let (x_inc, _) = inc
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let mut borrower = env.borrower();
    let nonce = OPEN_BPS
        .into_iter()
        .find_map(|bps| {
            let q = u64::try_from(u128::from(x_inc) * u128::from(bps) / 10_000).expect("u64");
            let (res, nonce) = env.open(&rm, &mut borrower, q.min(cap));
            res.is_ok().then_some(nonce)
        })
        .expect("an open");
    for _ in 0..3 {
        env.warp(1);
        env.crank(&rm);
    }
    halt(&mut env, &base);
    assert_eq!(
        env.market(&rm.market()).min_open_fee_ppm,
        u32::try_from(fee_ppm).expect("u32"),
        "margined at the candidate's own fee"
    );
    (env, rm, base, inc, cand, borrower, nonce)
}

/// **FITS-1, not adopted** (a regression; owner decision of 2026-09-29). With the carried fee at
/// `min(φ_inc, φ_cand)` alone, a 1 % → 1 % switch carried a position up to the candidate's impact
/// cap (2.47 % of `X₀`), margined on the deeper incumbent's cost (V/B 2.94–2.98 on the new pool
/// against 3.12 native), and FIT-2's pushes with a genuine 4×/30 s pump turned it into S1 at
/// 60–100 % of that limit (1.48–2.46 % of `X`; at 60 % the freeze and the misses, lenders −4.716
/// SOL). The carried fee is now `min(φ_inc, φ_cand, CARRIED_FEE_CAP_PPM)` = 25 bps: the same
/// position (`FITS_PM`, default 600 ‰ of the old limit, the band's lower edge) does not fit, the
/// keeper proposes nothing, a direct proposal is refused, no window opens, and the keeper settles
/// it in S3 at fair value at its due second on the failed pool. Knobs: `FIT_FEE`, `FITS_PM`.
#[test]
fn fits1_an_equal_fee_position_above_the_carried_cap_is_not_adopted_and_settles_in_s3() {
    let fee: u64 = envv("FIT_FEE", 10_000);
    let pm: u64 = envv("FITS_PM", 600);
    let phi = u32::try_from(fee).expect("u32");
    let (env, rm, _, inc, cand, borrower, nonce) = equal_fee_market(fee, |env, rm, cand| {
        carried_cap_at(env, rm, cand, phi) / 1_000 * pm
    });
    let pos = env.position(&rm, &borrower.key(), nonce);
    let (x_c, _) = cand
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let old = carried_cap_at(&env, &rm, &cand, phi);
    let (cap, _) = ps::carried_caps(&env, &cand);
    println!(
        "FITS-1, 1 % → 1 %: q {} ({:.3} % of the candidate's X) = {:.3} of the old fit limit at          min(φ_inc, φ_cand) ({old}), {:.2}× the carried cap at 25 bps ({cap})",
        pos.q,
        pos.q as f64 * 100.0 / x_c as f64,
        pos.q as f64 / old as f64,
        pos.q as f64 / cap as f64
    );
    assert!(pos.q <= old, "it fitted the old rule");
    assert!(pos.q > cap, "it does not fit the 25-bps class");
    assert_eq!(
        cap,
        carried_cap_at(&env, &rm, &cand, shortwin_math::consts::CARRIED_FEE_CAP_PPM),
        "the carried cap is the 25-bps one"
    );
    not_adopted_then_s3(
        &format!("FITS-1, 1 % → 1 %, {pm}‰ of the old fit limit"),
        env,
        &rm,
        &inc,
        &cand,
        &borrower,
        nonce,
    );
}

/// **FITS-1, within the new cap** (a regression). On the same 1 % → 1 % switch a position at
/// `FITS_PM` (default 950 ‰) of the carried per-position cap at 25 bps (0.75 % of `X₀`) is
/// adopted, and the control and FIT-2's three pushes (M4G-3's held deposit, M4F-1's freeze, M4G-1's
/// misses) with the genuine 4×/30 s pump all end in a normal liquidation within `FIT_SLACK`
/// (default 3) s of the control. Knobs: `FIT_FEE`, `FITS_PM`, `FIT_RAMP`, `FIT_W`, `FIT_K`,
/// `FIT_ATTACK`, `FIT_SLACK`.
#[test]
fn fits1_an_equal_fee_position_within_the_carried_cap_is_liquidated_under_the_pushes() {
    let fee: u64 = envv("FIT_FEE", 10_000);
    let pm: u64 = envv("FITS_PM", 950);
    let ramp: i64 = envv("FIT_RAMP", 30);
    let slack: i64 = envv("FIT_SLACK", 3);
    let setup = || {
        let (env, rm, _, inc, cand, borrower, nonce) = equal_fee_market(fee, |env, rm, cand| {
            let phi = shortwin_math::caps::carried_phi(
                u32::try_from(fee).expect("u32"),
                u32::try_from(fee).expect("u32"),
            );
            assert_eq!(phi, shortwin_math::consts::CARRIED_FEE_CAP_PPM.min(phi));
            carried_cap_at(env, rm, cand, phi) / 1_000 * pm
        });
        switch_now(env, rm, &inc, cand, borrower, nonce)
    };
    let res = fit_grid(
        &format!("FITS-1, 1 % → 1 %, {pm}‰ of the carried cap at 25 bps"),
        ramp,
        setup,
    );
    let control = res
        .iter()
        .find(|(n, _)| *n == "control")
        .and_then(|(_, r)| r.ok());
    for (name, r) in &res {
        let at = r.unwrap_or_else(|e| panic!("{name}: not liquidated: {e:?}"));
        if let Some(c) = control {
            assert!(at - c <= slack, "{name}: +{} s after the control", at - c);
        }
    }
}

/// What a keeper-cost run measured.
#[derive(Debug, Default)]
struct CostRun {
    /// The keeper's observation lamports and cranks on the market (its metrics).
    lamports: u64,
    cranks: u64,
    /// Per-slot bursts (foreign prices) and liquidity-coverage triggers (FITS-2 (b)) reported.
    bursts: usize,
    liq: usize,
    /// The second the O17 (global) budget refused an observation, if any.
    exhausted: Option<i64>,
    /// The attacker's transactions.
    att_txs: u64,
    /// The seconds the keeper observed the market.
    observed: Vec<i64>,
}

/// The keeper on the USELESS CPMM market (with a small position when `open_pos`) for `secs`
/// seconds, two ticks a second (per slot), against an attacker who, after the keeper's ticks of
/// every second `s` with `wrap_at(s)`, sends `[deposit dep_bps of the LP counter, crank_price,
/// withdraw]` (or, with `price_bps`, the price-wrapped `[buy price_bps of Y, crank_price, sell]`).
fn budget_run(
    wrap_at: impl Fn(i64) -> bool,
    secs: i64,
    budget: Option<u64>,
    dep_bps: u64,
    price_bps: u64,
    open_pos: bool,
) -> CostRun {
    use crate::raydium::{USELESS, ray_open, ray_setup};
    let mut cfg = KeeperConfig::default();
    // The global budget (decision A, 2026-10-01) with this market alone: `budget` is `G` (`None`:
    // no budget). The floor at 0 keeps the small position's market covered, so duty 2's
    // liquidity-anomaly coverage — which runs on covered markets only — is what is measured.
    cfg.budget.global_daily_lamports = budget.unwrap_or(0);
    cfg.budget.var_floor_lamports = 0;
    let (mut h, markets) = ray_setup(&[USELESS], cfg);
    let (rm, q) = &markets[0];
    let key = rm.market();
    if open_pos {
        let _ = ray_open(&h, rm, *q / 4);
    }
    let AnyPool::Cpmm(cp) = rm.pool else {
        unreachable!()
    };
    let pool = rm.pool;
    let who = h.with_env(|env| {
        let add = u64::try_from(
            u128::from(cp.state(&env.svm).lp_supply) * u128::from(dep_bps.max(1)) / 10_000,
        )
        .expect("u64");
        let (sol, tok) = lp::lp_needed(env, &cp, add);
        lp::provider(env, &cp, 2 * sol + 1_000_000_000, 2 * tok, 0)
    });
    let mut out = CostRun::default();
    for s in 1..=secs {
        for r in [h.second().clone(), h.slot().clone()] {
            out.bursts += r.bursts.iter().filter(|(m, _)| *m == key).count();
            out.liq += r.liq_covers.iter().filter(|(m, _)| *m == key).count();
            if out.exhausted.is_none()
                && r.crank_failed
                    .iter()
                    .any(|(m, why)| *m == key && why.contains("budget"))
            {
                out.exhausted = Some(s);
            }
            if r.observed(&key) && out.observed.last() != Some(&s) {
                out.observed.push(s);
            }
        }
        if wrap_at(s) && (price_bps > 0 || dep_bps > 0) {
            h.with_env(|env| {
                if price_bps > 0 {
                    let (mint, program) = pool.token();
                    let (_, y) = pool
                        .io_reserves(&env.svm, Direction::Sell)
                        .expect("reserves");
                    let a =
                        u64::try_from(u128::from(y) * u128::from(price_bps) / 10_000).expect("u64");
                    let t = fork::trader(env, &mint, program, a + 1, 0);
                    let owner = t.owner.insecure_clone();
                    let got = pool
                        .quote_exact_in(&env.svm, Direction::Buy, a)
                        .expect("quote")
                        .proceeds;
                    let buy = pool.swap_ix(
                        &env.svm,
                        &solana_signer::Signer::pubkey(&owner),
                        &t,
                        Direction::Buy,
                        Swap::ExactIn(a, 0),
                    );
                    let sell = pool.swap_ix(
                        &env.svm,
                        &solana_signer::Signer::pubkey(&owner),
                        &t,
                        Direction::Sell,
                        Swap::ExactIn(got, 0),
                    );
                    ok(env.send(&[buy, crank_price_ix(rm), sell], &[&owner]));
                } else {
                    let add = u64::try_from(
                        u128::from(cp.state(&env.svm).lp_supply) * u128::from(dep_bps) / 10_000,
                    )
                    .expect("u64");
                    ok(env.send(
                        &[
                            lp::deposit_ix(&cp, &who, add),
                            crank_price_ix(rm),
                            lp::withdraw_ix(&cp, &who, add),
                        ],
                        &[&who.owner],
                    ));
                }
            });
            out.att_txs += 1;
        }
    }
    let m = &h.keeper.metrics.markets[&key];
    (out.lamports, out.cranks) = (m.lamports, m.cranks);
    out
}

/// **M4G-2 and FITS-2 (b): the keeper's cost of an LP-wrapped crank** (regressions). M4F-1's
/// liquidity bursts (60 s per slot from one `[deposit, crank_price, withdraw]`) cost the keeper
/// 25–150× the attacker's fee, and exhausted the example O17 budget (0.03 SOL/day) at +3,059 s
/// with one wrap every 59 s; #99 reverted them. FITS-2 (b) triggers **per-second** coverage for
/// `liq_cover_s` (10 s) instead, on markets with open positions only. On the USELESS CPMM market,
/// two keeper ticks a second, the attacker's 5 % wrap after the keeper's ticks:
///
/// - one wrap buys at most `liq_cover_s` keeper observations (counted: the keeper's observed
///   seconds against a control without it), one liquidity trigger, no per-slot burst;
/// - one wrap every 59 s for an hour: one trigger per wrap, at most 10 keeper cranks per wrap
///   above the control, no burst, the example budget unspent;
/// - 600 s of the same on the market **without** a position: no trigger and exactly the
///   control's observations;
/// - the price-wrapped control (`[buy 3 %, crank_price, sell]`) still starts per-slot bursts.
#[test]
fn m4g2_an_lp_wrapped_crank_buys_at_most_ten_per_second_observations() {
    let budget = Some(30_000_000);
    let control = budget_run(|_| false, 90, budget, 500, 0, true);
    let one = budget_run(|s| s == 30, 90, budget, 500, 0, true);
    let window = |r: &CostRun| r.observed.iter().filter(|&&s| s > 30).count() as i64;
    let extra = window(&one) - window(&control);
    println!(
        "FITS-2 (b), one LP-wrapped crank at +30 s: keeper observed {} s after it (control {}): \
         +{extra} for 1 attacker transaction; {} trigger(s), {} per-slot burst(s)",
        window(&one),
        window(&control),
        one.liq,
        one.bursts
    );
    assert_eq!((one.liq, one.bursts, one.att_txs), (1, 0, 1));
    assert!(
        (1..=10).contains(&extra),
        "one wrap buys at most 10 s: {extra}"
    );

    let run =
        |secs: i64, open: bool, dep: u64| budget_run(|s| s % 59 == 0, secs, budget, dep, 0, open);
    let (quiet, wraps) = (run(3_600, true, 0), run(3_600, true, 500));
    println!(
        "M4G-2 / FITS-2 (b), an LP-wrapped crank every 59 s for an hour with a position: keeper \
         {} cranks, {} lamports (control {} cranks, {} lamports); {} triggers, {} bursts; \
         attacker {} transactions ({:.1} keeper cranks each); budget exhausted at {:?}",
        wraps.cranks,
        wraps.lamports,
        quiet.cranks,
        quiet.lamports,
        wraps.liq,
        wraps.bursts,
        wraps.att_txs,
        (wraps.cranks - quiet.cranks) as f64 / wraps.att_txs.max(1) as f64,
        wraps.exhausted
    );
    assert!(wraps.exhausted.is_none() && wraps.bursts == 0, "{wraps:?}");
    assert_eq!(wraps.liq as u64, wraps.att_txs, "one trigger per wrap");
    assert!(
        wraps.cranks <= quiet.cranks + 10 * wraps.att_txs,
        "at most 10 keeper cranks per wrap: {} vs {} + 10 × {}",
        wraps.cranks,
        quiet.cranks,
        wraps.att_txs
    );

    let (bare, bare_wraps) = (run(600, false, 0), run(600, false, 500));
    println!(
        "FITS-2 (b), a wrap every 59 s for 600 s without a position: keeper {} cranks (control \
         {}); {} triggers; attacker {} transactions",
        bare_wraps.cranks, bare.cranks, bare_wraps.liq, bare_wraps.att_txs
    );
    assert_eq!(bare_wraps.liq, 0, "no liquidity trigger without positions");
    assert_eq!(bare_wraps.observed, bare.observed, "nothing bought");

    let price = budget_run(|s| s % 59 == 0, 600, None, 0, 300, true);
    println!(
        "M4G-2 control, a 3 % price-wrapped crank every 59 s for 600 s: keeper {} cranks, {} \
         lamports; {} bursts; attacker {} transactions",
        price.cranks, price.lamports, price.bursts, price.att_txs
    );
    assert!(price.bursts > 0, "price wraps start bursts");
}

// ---- FITV-2 / FITS-2: the withdraw contest ------------------------------------------------------

/// The withdraw contest's market (FITV-2's reproducer, reused by FITS-2): a 160-SOL λ 99.78 % CPMM
/// pool at `fee` ppm; before the listing the attacker deposits `add_pct` % of the LP counter as
/// its own unlocked LP (100: λ ≈ 49.9 %); `λ_min = lmin`. A listed market with 13 s of cranks, and
/// a position at `qpm` ‰ of its open cap (bisected down until it opens), 3 s of cranks.
struct ContestMarket {
    env: Env,
    rm: RayMarket,
    pool: AnyPool,
    cp: CpmmPool,
    /// The unlocked LP (the attacker in the contests; an honest LP in the genuine withdrawal).
    who: lp::Provider,
    borrower: Borrower,
    nonce: u64,
    /// `(lambda bps, D, q, cap, q/x %)`, printed.
    facts: String,
}

fn contest_market(fee: u64, lmin: u16, add_pct: u64, qpm: u64) -> ContestMarket {
    use shortwin_litesvm_tests::raydium::VENUE_HINT_RAYDIUM_CPMM;
    let mut env = Env::ray_fork();
    let mut cfg = env.config();
    cfg.params.pool_lock_min_bps = lmin.into();
    let lam_cfg = env
        .svm
        .get_account(&pda::config())
        .expect("config")
        .lamports;
    shortwin_litesvm_tests::fixtures::set_zc(&mut env.svm, &pda::config(), &cfg, 0);
    {
        let mut a = env.svm.get_account(&pda::config()).expect("config");
        a.lamports = lam_cfg;
        env.svm.set_account(pda::config(), a).expect("config");
    }
    let base = env.load_ray_pool(FART);
    let pool = sized(&mut env, false, &base, 9_978, 160_000_000_000);
    let AnyPool::Cpmm(cp) = pool else {
        unreachable!()
    };
    set_cpmm_fee(&mut env, &cp, fee);
    let add = cp.state(&env.svm).lp_supply / 100 * add_pct;
    let (sol, tok) = lp::lp_needed(&env, &cp, add);
    let who = lp::provider(&mut env, &cp, 3 * sol + 1_000_000_000, 3 * tok, 0);
    if add > 0 {
        ok(env.send(&[lp::deposit_ix(&cp, &who, add)], &[&who.owner]));
    }
    let lam_now = {
        let counter = shortwin_litesvm_tests::raydium::lp_counter(&env.svm, &pool);
        let supply = shortwin_litesvm_tests::raydium::mint_supply(
            &env.svm,
            &shortwin_litesvm_tests::raydium::lp_mint(&pool),
        );
        shortwin_math::pool_select::lambda_bps(counter, supply)
    };
    ok(env.list_ray(&pool, VENUE_HINT_RAYDIUM_CPMM));
    let q0 = q_of(&env, &pool);
    let rm = env.ray_market_listed(RaySpec::new(FART, q0.saturating_mul(60)), pool);
    env.crank(&rm);
    for _ in 0..12 {
        env.warp(1);
        env.crank(&rm);
    }
    slow_catch_up(&mut env, &rm);
    let params = env.config().params;
    let m = env.market(&rm.market());
    let (x, _) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let d = shortwin_litesvm_tests::raydium::locked_depth(&env.svm, &pool);
    let x0 = shortwin_math::caps::conservative_x0(
        x,
        m.liq_slow.get(),
        m.ema_price.get(),
        Some((d, params.pool_lock_min_bps)),
    )
    .expect("x0");
    let cap = shortwin_math::caps::position_cap(
        x0,
        500,
        u32::try_from(fee).expect("u32"),
        params.fee_size_cap_x100,
    )
    .expect("cap");
    let mut borrower = env.borrower();
    let mut q = cap / 1_000 * qpm;
    let nonce = loop {
        let (res, nonce) = env.open(&rm, &mut borrower, q);
        if res.is_ok() {
            break nonce;
        }
        q = q / 1_000 * 995;
    };
    for _ in 0..3 {
        env.warp(1);
        env.crank(&rm);
    }
    let root = root_xy(&env, &pool);
    let facts = format!(
        "lambda_min {lmin}, pool lambda {lam_now} bps, fee {fee} ppm, sqrt(XY) {root}, D {d}, q {q} \
         ({:.3} % of X, {qpm}‰ of the cap {cap})",
        q as f64 * 100.0 / x as f64
    );
    ContestMarket {
        env,
        rm,
        pool,
        cp,
        who,
        borrower,
        nonce,
        facts,
    }
}

/// `L_ema`, `L_slow` and `L_mlo` over the pool's `√(X·Y)` now.
fn liq_state(h: &Harness, key: &Pubkey, pool: &AnyPool) -> String {
    h.with_env(|env| {
        let m = env.market(key);
        let o = m.observation();
        let r = root_xy(env, pool) as f64;
        let lmlo = ring_liq_lower_median(&o.ring).expect("ring") as f64;
        format!(
            "L_ema {:.3}, L_slow {:.3}, L_mlo {:.3} x sqrt(XY)",
            o.ema.liq as f64 / r,
            o.ema.liq_slow as f64 / r,
            lmlo / r
        )
    })
}

/// What one withdraw-contest run saw: the second of the liquidation (after the pump's start), its
/// event, whether the attacker's wrapped liquidation landed it, the pre-trade spot over the
/// pump's start, and the reference state before it.
type Contest = Option<(i64, shortwin::events::Liquidate, bool, f64, String)>;

/// One withdraw contest (FITV-2's shape; FITS-2's knobs).
#[derive(Clone, Debug)]
struct ContestSpec {
    fee: u64,
    lmin: u16,
    add_pct: u64,
    /// The pump's top, bps of the start price, reached over `dur` s and held `hold` s.
    top: u128,
    dur: i64,
    hold: i64,
    /// Flat seconds before the pump (the attacker drags in them): FITV-2's 30; FITS-2 (b)'s 1,500.
    w: i64,
    /// The position, ‰ of its open cap: FITV-2's 1,000; FITS-2 (a)'s 500.
    qpm: u64,
    /// The attacker's bundles come **after** the keeper's tick in each second, from +2 s (the
    /// adversarial timing: its first wrap lands in a second the keeper's idle cadence skips, so
    /// that second is attacker-only); else **before** it, from +1 s (the verifier's reproducer).
    after: bool,
    /// The keeper's `liq_cover_s` (FITS-2 (b)): `None` the default (10 s); `Some(0)` disables the
    /// liquidity trigger (the keeper of #100, for comparison).
    cover: Option<i64>,
    /// FITX-1 (the held drain): with `attack`, the attacker's unlocked LP is genuinely out of the
    /// pool for the first `held` seconds (withdrawn at +1 s, the attacker cranking every second at
    /// the drained depth — which the keeper reads too, so nothing is foreign), re-deposited at
    /// `held + 1`, from which second the usual wrapped bundles run (0: no held phase).
    held: i64,
    /// The genuine-withdrawal comparison (FITX-1): without `attack`, the unlocked LP leaves the
    /// pool at +1 s for good and nobody else acts — the honest keeper on the pool as the held
    /// drain would have left it had the LP stayed out.
    gone: bool,
}

impl ContestSpec {
    /// FITV-2's defaults, each overridable by its `FITV_*` knob (`FITX_HELD`, `FITX_GONE` for
    /// FITX-1's).
    fn from_env() -> Self {
        Self {
            fee: envv("FITV_FEE", 10_000),
            lmin: envv("FITV_LMIN", 5_000),
            add_pct: envv("FITV_ADD_PCT", 100),
            top: envv("FITV_TOP", 20_500),
            dur: envv("FITV_DUR", 60),
            hold: envv("FITV_HOLD", 300),
            w: envv("FITV_W", 30),
            qpm: envv("FITV_QPM", 1_000),
            after: envv("FITV_AFTER", 0u8) != 0,
            cover: std::env::var("FITV_COVER")
                .ok()
                .and_then(|v| v.parse().ok()),
            held: envv("FITX_HELD", 0),
            gone: envv("FITX_GONE", 0u8) != 0,
        }
    }
}

/// What a contest run measured besides its outcome.
#[derive(Debug, Default)]
struct ContestCost {
    /// Seconds of the flat phase the keeper observed (a crank or an exit).
    keeper_flat_obs: i64,
    /// Of them, the seconds after the held drain's re-deposit (FITX-1; all of them without one).
    keeper_obs_after_held: i64,
    /// The keeper's cranks (its metrics).
    keeper_cranks: u64,
    /// The attacker's LP-wrapped cranks and wrapped liquidation attempts.
    wraps: u64,
    attempts: u64,
    /// Liquidity-coverage triggers the keeper reported (FITS-2 (b)).
    liq_covers: u64,
    /// The wrapped liquidations the program refused, by custom error code.
    refused: std::collections::BTreeMap<Option<u32>, u32>,
    /// The state at the pump's start, and `L_ema / √(X·Y)`, `L_slow / √(X·Y)` then.
    at_pump: String,
    l_ema_at_pump: f64,
    l_slow_at_pump: f64,
}

/// One run of the withdraw contest on [`contest_market`]: `spec.w` s flat, then a genuine pump to
/// `spec.top` over `spec.dur` s, held `spec.hold` s. With `attack`, in every second (before or
/// after the keeper's tick, `spec.after`) the attacker sends `[withdraw all, crank_price,
/// deposit]` (dragging `L_ema` toward the locked depth in any second the keeper does not observe)
/// and `[withdraw all, liquidate, deposit]` (its spot leg on the drained `L_live`; it lands only
/// if the program accepts it). The keeper ticks every second (its own cadence decides when it
/// observes).
fn withdraw_contest(spec: &ContestSpec, attack: bool) -> (Contest, ContestCost) {
    use shortwin::events::Liquidate as LiqEv;
    use shortwin_keeper::events;
    let c = contest_market(spec.fee, spec.lmin, spec.add_pct, spec.qpm);
    let ContestMarket {
        env,
        rm,
        pool,
        cp,
        who,
        borrower,
        nonce,
        facts,
    } = c;
    let key = rm.market();
    let position = pda::position(&key, &borrower.key(), nonce).0;
    let mut cfg = KeeperConfig::default();
    if let Some(cover) = spec.cover {
        cfg.coverage.liq_cover_s = cover;
    }
    let mut h = Harness::new(env, cfg);
    let p0 = h.with_env(|env| spot(env, &pool));
    println!(
        "FITV-2/FITS-2 setup (attack {attack}, after {}, cover {:?}, held {}, gone {}): {facts}",
        spec.after, spec.cover, spec.held, spec.gone
    );
    let mut cost = ContestCost::default();
    let w = spec.w;
    // The attacker's two bundles of one second; `Some` if its liquidation landed.
    let attacker = |h: &mut Harness, cost: &mut ContestCost| -> Option<(LiqEv, f64)> {
        h.with_env(|env| {
            let lp_held = read::token_amount(&env.svm, &who.lp).unwrap_or(0);
            ok(env.send(
                &[
                    lp::withdraw_ix(&cp, &who, lp_held),
                    crank_price_ix(&rm),
                    lp::deposit_ix(&cp, &who, lp_held),
                ],
                &[&who.owner],
            ));
            cost.wraps += 1;
            let lp_held = read::token_amount(&env.svm, &who.lp).unwrap_or(0);
            let p_pre = spot(env, &pool);
            cost.attempts += 1;
            let res = env.send(
                &[
                    lp::withdraw_ix(&cp, &who, lp_held),
                    shortwin_litesvm_tests::engine::liquidate_ix(
                        &rm,
                        &who.key(),
                        &borrower.key(),
                        nonce,
                    ),
                    lp::deposit_ix(&cp, &who, lp_held),
                ],
                &[&who.owner],
            );
            match res {
                Ok(meta) => Some((
                    shortwin_litesvm_tests::events::one::<LiqEv>(&meta),
                    p_pre as f64 / p0 as f64,
                )),
                Err(e) => {
                    *cost.refused.entry(custom_code(&Err(e))).or_default() += 1;
                    None
                }
            }
        })
    };
    // FITX-1: the held drain (the attacker's LP genuinely out for `held` s), then the wraps.
    let held = if attack { spec.held } else { 0 };
    let first = if held > 0 {
        held + 1
    } else if spec.after {
        2
    } else {
        1
    };
    let mut held_lp = 0u64;
    let total = w + spec.dur + spec.hold;
    for s in 1..=total {
        h.idle(1);
        if held > 0 && s <= held {
            h.with_env(|env| {
                if s == 1 {
                    held_lp = read::token_amount(&env.svm, &who.lp).unwrap_or(0);
                    ok(env.send(&[lp::withdraw_ix(&cp, &who, held_lp)], &[&who.owner]));
                }
                ok(env.send(&[crank_price_ix(&rm)], &[&who.owner]));
            });
            if s == held {
                println!(
                    "FITX-1: the held drain's end, +{s} s: {}",
                    liq_state(&h, &key, &pool)
                );
            }
        }
        if held > 0 && s == held + 1 {
            h.with_env(|env| ok(env.send(&[lp::deposit_ix(&cp, &who, held_lp)], &[&who.owner])));
        }
        if !attack && spec.gone && s == 1 {
            h.with_env(|env| {
                let lp_held = read::token_amount(&env.svm, &who.lp).unwrap_or(0);
                ok(env.send(&[lp::withdraw_ix(&cp, &who, lp_held)], &[&who.owner]));
            });
        }
        if s > w {
            let g = 10_000
                + (spec.top - 10_000) * u128::try_from((s - w).min(spec.dur)).expect("u128")
                    / u128::try_from(spec.dur).expect("u128");
            h.with_env(|env| pump_to(env, &pool, p0, g));
        }
        if s == w {
            cost.at_pump = liq_state(&h, &key, &pool);
            (cost.l_ema_at_pump, cost.l_slow_at_pump) = h.with_env(|env| {
                let m = env.market(&key);
                let r = root_xy(env, &pool) as f64;
                (m.ema_liq.get() as f64 / r, m.liq_slow.get() as f64 / r)
            });
        }
        if attack && !spec.after && s >= first {
            let before = liq_state(&h, &key, &pool);
            if let Some((ev, px)) = attacker(&mut h, &mut cost) {
                cost.keeper_cranks = h.keeper.metrics.markets[&key].cranks;
                return (Some((s - w, ev, true, px, before)), cost);
            }
        }
        let pre = h.with_env(|env| spot(env, &pool));
        let before = liq_state(&h, &key, &pool);
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "+{s} s: {:?}", r.errors);
        if s <= w && r.observed(&key) {
            cost.keeper_flat_obs += 1;
            if s > held {
                cost.keeper_obs_after_held += 1;
            }
        }
        cost.liq_covers += r.liq_covers.iter().filter(|(m, _)| *m == key).count() as u64;
        for e in r.exits.iter().filter(|e| e.position == position) {
            if let Some(out) = e.result.landed() {
                match e.kind {
                    ExitKind::Liquidate => {
                        let ev = events::of::<LiqEv>(out).into_iter().next().expect("event");
                        cost.keeper_cranks = h.keeper.metrics.markets[&key].cranks;
                        return (
                            Some((s - w, ev, false, pre as f64 / p0 as f64, before)),
                            cost,
                        );
                    }
                    k => panic!("+{s} s: {k:?}"),
                }
            }
        }
        if attack && spec.after && s >= first {
            let before = liq_state(&h, &key, &pool);
            if let Some((ev, px)) = attacker(&mut h, &mut cost) {
                cost.keeper_cranks = h.keeper.metrics.markets[&key].cranks;
                return (Some((s - w, ev, true, px, before)), cost);
            }
        }
    }
    cost.keeper_cranks = h.keeper.metrics.markets[&key].cranks;
    println!(
        "FITV-2/FITS-2 (attack {attack}): no exit; at the end {}",
        liq_state(&h, &key, &pool)
    );
    (None, cost)
}

/// Prints a contest's outcome and cost.
fn show_contest(name: &str, spec: &ContestSpec, r: &Contest, cost: &ContestCost) {
    match r {
        None => println!(
            "{name}: NOT liquidated in {} s",
            spec.w + spec.dur + spec.hold
        ),
        Some((s, e, by_attacker, px, st)) => println!(
            "{name}: liquidated +{s} s into the pump at spot {px:.3}x (pre-trade) by {} [{st}]: \
             cr_spot {} cr_ema {} bps; B {} (b_ref {}, b_live {}, max_in {}), penalty {} (lenders \
             {}, keeper {}), borrower {}",
            if *by_attacker {
                "the attacker"
            } else {
                "the keeper"
            },
            e.cr_spot_bps,
            e.cr_ema_bps,
            e.buyback_cost,
            e.b_ref,
            e.b_live,
            e.max_in,
            e.penalty,
            e.penalty_lender,
            e.penalty_keeper,
            e.borrower_payout
        ),
    }
    println!(
        "  at the pump: {}; keeper: {} of {} flat seconds observed, {} cranks, {} liquidity \
         trigger(s); attacker: {} wrapped cranks, {} wrapped liquidations (refused {:?})",
        cost.at_pump,
        cost.keeper_flat_obs,
        spec.w,
        cost.keeper_cranks,
        cost.liq_covers,
        cost.wraps,
        cost.attempts,
        cost.refused
    );
}

/// Runs the honest and the attacked contest of `spec` and asserts **no forced liquidation**: the
/// attacker liquidates nothing the honest keeper leaves open, and never before it could. Returns
/// both outcomes and costs.
fn contest_pair(
    name: &str,
    spec: &ContestSpec,
) -> ((Contest, ContestCost), (Contest, ContestCost)) {
    let only: String = envv("FITV_ONLY", "both".to_string());
    let honest = if only == "attack" {
        (None, ContestCost::default())
    } else {
        withdraw_contest(spec, false)
    };
    show_contest(&format!("{name}, honest"), spec, &honest.0, &honest.1);
    let att = if only == "honest" {
        (None, ContestCost::default())
    } else {
        withdraw_contest(spec, true)
    };
    show_contest(&format!("{name}, attack"), spec, &att.0, &att.1);
    match (&honest.0, &att.0) {
        (None, Some(a)) if a.2 => {
            panic!("{name}: a position the honest keeper never liquidates is force-liquidated")
        }
        (Some(hh), Some(a)) => assert!(
            !a.2 || a.0 >= hh.0,
            "{name}: the attacker liquidates {} s before the honest keeper could",
            hh.0 - a.0
        ),
        _ => {}
    }
    (honest, att)
}

/// **FITV-2, a regression** (the final verification of #100). With FIT-2's lows bound
/// (`L_b`'s EMA terms, `L_slow` included, capped at `⌊L_mlo·1.01⌋`), an unlocked LP forced the
/// liquidation of a position the honest keeper never liquidates: withdraw-wrapped cranks in the
/// quiet seconds of the keeper's steady-state cadence dragged `L_ema` and the lows to the locked
/// depth, and a withdraw-wrapped liquidation — its spot leg judged at the drained `L_live` —
/// then passed the exit bound, which the lows had lowered to the drained depth (at 2.050×: lenders
/// 0.512 SOL of penalty, the attacker the keeper's 0.307, the borrower 4.941 SOL). The lows bound
/// is removed: `L_b = max(L_live, min(max(L_ema', L_slow), ⌊D·10⁴/λ_min⌋))` keeps V2-14's `L_slow`
/// credit of the pool's honest depth, so the wrapped liquidation fails the exit bound. The
/// verifier's setup: a 160-SOL λ 99.78 % CPMM pool at 1 %, the attacker's unlocked LP of 100 % of
/// the counter deposited before the listing (λ ≈ 49.9 %), `λ_min` 50 %, a position at its open
/// cap, a genuine pump to 2.05× over 60 s held 300 s. Asserted: **no forced liquidation** — the
/// honest keeper leaves the position open and so does the attacker (and if a variant does
/// liquidate honestly, the attacker's never lands first). On the same tree with the lows bound
/// restored this test fails as found (the attacker's liquidation at +67 s, 2.050×). Knobs:
/// `FITV_FEE`, `FITV_LMIN`, `FITV_ADD_PCT`, `FITV_TOP`, `FITV_DUR`, `FITV_HOLD`, `FITV_W`,
/// `FITV_QPM`, `FITV_AFTER`, `FITV_COVER`, `FITV_ONLY` (λ 25 % at 2.00× and `λ_min` 2,000 at
/// 1.90× / 2.05×, forced before, are not forced now either), and FITX-1's `FITX_HELD` (the held
/// drain, which forces: `fitx1_*`) and `FITX_GONE` (the LP out for good, no attacker).
#[test]
fn fitv2_the_withdraw_contest_forces_no_liquidation() {
    let spec = ContestSpec::from_env();
    let ((honest, _), (att, _)) = contest_pair("FITV-2", &spec);
    let defaults = (
        spec.fee,
        spec.lmin,
        spec.add_pct,
        spec.top,
        spec.dur,
        spec.hold,
        spec.w,
        spec.qpm,
    ) == (10_000, 5_000, 100, 20_500, 60, 300, 30, 1_000)
        && !spec.after
        && spec.cover.is_none()
        && spec.held == 0
        && !spec.gone;
    if defaults {
        assert!(
            honest.is_none() && att.is_none(),
            "the verifier's case: nobody liquidates at 2.05× (honest {:?}, attack {:?})",
            honest.as_ref().map(|r| r.0),
            att.as_ref().map(|r| r.0)
        );
    }
}

/// A simulated transaction of `ixs` signed by `signer` (its fee payer): `Ok` if the program
/// accepts it, else the custom error code (if any). Nothing is written.
fn simulate(
    env: &Env,
    ixs: &[anchor_lang::solana_program::instruction::Instruction],
    signer: &solana_keypair::Keypair,
) -> Result<(), Option<u32>> {
    use solana_signer::Signer as _;
    let tx = solana_transaction::Transaction::new_signed_with_payer(
        ixs,
        Some(&signer.pubkey()),
        &[signer],
        env.svm.latest_blockhash(),
    );
    match env.svm.simulate_transaction(tx) {
        Ok(_) => Ok(()),
        Err(f) => Err(shortwin_litesvm_tests::errors::custom_code(&Err(f))),
    }
}

/// **FITS-2 (a), on the program** (a regression; owner decision of 2026-09-29). The liquidation's
/// spot leg was judged at `L_live`, the one input of "CR-liquidatable" a transaction moves by
/// itself: on the contest market (λ ≈ 49.9 %, the attacker's unlocked LP; 1 %; a position at
/// `FITS_QPM`, default 500 ‰, of its cap, ≈ 1.2 % of `X`), where the 2 % δ covers the drained
/// cost, a `[withdraw all, liquidate, deposit]` turned the spot leg true at the drained depth
/// wherever the EMA leg held and the honest spot leg did not — just after a small retreat from
/// the threshold, while `P_ema` still lags above the spot. The spot leg is now judged at
/// `max(L_live, L_ema')`. The test raises the price in 1 % and then 0.2 % steps (8 s of
/// per-second cranks each) until an honest `liquidate` simulates, overshoots 1.5 % (10 s), then
/// retreats 2 % and cranks 12 s; in every second it simulates the honest `liquidate` and the
/// attacker's wrapped one. Asserted: **the wrapped liquidation is accepted only where the honest
/// one is** (refused `NotLiquidatable` in the retreat's band). On a build with the spot leg at
/// `L_live` it fails: the wrapped one is accepted while the honest one is refused.
#[test]
fn fits2_a_a_withdrawal_inside_the_liquidation_cannot_make_the_spot_leg_true() {
    use shortwin::errors::ShortwinError;
    let qpm: u64 = envv("FITS_QPM", 500);
    let c = contest_market(10_000, 5_000, 100, qpm);
    let ContestMarket {
        mut env,
        rm,
        pool,
        cp,
        who,
        borrower,
        nonce,
        facts,
    } = c;
    println!("FITS-2 (a) sweep: {facts}");
    let keeper = env.funded_keypair();
    let honest = |env: &Env| {
        simulate(
            env,
            &[shortwin_litesvm_tests::engine::liquidate_ix(
                &rm,
                &solana_signer::Signer::pubkey(&keeper),
                &borrower.key(),
                nonce,
            )],
            &keeper,
        )
    };
    let wrapped = |env: &Env| {
        let lp_held = read::token_amount(&env.svm, &who.lp).unwrap_or(0);
        simulate(
            env,
            &[
                lp::withdraw_ix(&cp, &who, lp_held),
                shortwin_litesvm_tests::engine::liquidate_ix(
                    &rm,
                    &who.key(),
                    &borrower.key(),
                    nonce,
                ),
                lp::deposit_ix(&cp, &who, lp_held),
            ],
            &who.owner,
        )
    };
    let p0 = spot(&env, &pool);
    // Holds the price at `g` bps of p0 for `secs` s of per-second cranks (genuine buys up; a
    // genuine sell down), checking both simulations in every second; returns whether the honest
    // one was accepted in the last one.
    let mut seconds = 0u32;
    let mut band = 0u32;
    let mut violations = Vec::new();
    let mut hold = |env: &mut Env, g: u128, secs: u32, band: &mut u32| -> bool {
        let now_p = spot(env, &pool);
        let target = p0 * g / 10_000;
        if target > now_p {
            pump_to(env, &pool, p0, g);
        } else if target < now_p {
            // A genuine sell of tokens down to the target.
            let (x, _) = pool
                .io_reserves(&env.svm, Direction::Sell)
                .expect("reserves");
            let r = (now_p as f64 / target as f64).sqrt();
            let a = (x as f64 * (r - 1.0) / 0.99) as u64;
            let (mint, program) = pool.token();
            let t = fork::trader(env, &mint, program, 0, a + 1);
            ok(fork::swap(
                env,
                &pool,
                &t,
                Direction::Sell,
                Swap::ExactIn(a, 0),
            ));
        }
        let mut last = false;
        for _ in 0..secs {
            env.warp(1);
            env.crank(&rm);
            let (hh, ww) = (honest(env), wrapped(env));
            seconds += 1;
            if ww.is_ok() && hh.is_err() {
                violations.push((spot(env, &pool) as f64 / p0 as f64, hh));
            }
            if hh.is_err() && ww == Err(Some(u32::from(ShortwinError::NotLiquidatable))) {
                *band += 1;
            }
            last = hh.is_ok();
        }
        last
    };
    // Bracket the honest threshold in 1 % steps, then refine in 0.2 % steps.
    let mut g: u128 = 10_000;
    while !hold(&mut env, g, 8, &mut band) {
        g += 100;
        assert!(g < 40_000, "no threshold below 4×");
    }
    let mut lo = g - 100;
    while lo + 20 < g && !hold(&mut env, lo + 20, 8, &mut band) {
        lo += 20;
    }
    let p1 = lo + 20;
    println!(
        "FITS-2 (a) sweep: the honest liquidation simulates from {:.3}×",
        p1 as f64 / 1e4
    );
    // Overshoot, then retreat 2 %: P_ema lags above the spot for a few seconds.
    hold(&mut env, p1 * 10_150 / 10_000, 10, &mut band);
    let before = band;
    hold(&mut env, p1 * 9_950 / 10_000, 12, &mut band);
    println!(
        "FITS-2 (a) sweep: {seconds} s checked; in the retreat {} s where the honest liquidation \
         was refused and the wrapped one NotLiquidatable; violations {violations:?}",
        band - before
    );
    assert!(
        violations.is_empty(),
        "FITS-2 (a): a withdrawal inside the liquidation made it acceptable where the honest state \
         is not: {violations:?}"
    );
    assert!(
        band > 0,
        "the retreat's band was reached (honest refused, wrapped NotLiquidatable)"
    );
    // Back over the threshold: the honest liquidation lands (every hook runs on it, I18 (e) — its
    // spot leg at max(L_live, L_ema') — included), and I18 (e) fires on its event with the spot
    // CR off by one.
    use shortwin_litesvm_tests::invariants::{self, IxEvents};
    pump_to(&mut env, &pool, p0, p1 * 10_200 / 10_000);
    let key = rm.market();
    let mut tries = 0;
    let meta = loop {
        tries += 1;
        assert!(
            tries < 60,
            "the honest liquidation lands over the threshold"
        );
        env.warp(1);
        env.crank(&rm);
        env.warp(1);
        let pre = invariants::snapshot(&env.svm, &[key]);
        let res = env.send(
            &[shortwin_litesvm_tests::engine::liquidate_ix(
                &rm,
                &solana_signer::Signer::pubkey(&keeper),
                &borrower.key(),
                nonce,
            )],
            &[&keeper],
        );
        if let Ok(meta) = res {
            break (pre, meta);
        }
    };
    let (pre, meta) = meta;
    let i18 = |ev: &IxEvents| {
        invariants::confirm::check_ix(
            &env.svm,
            &pre.positions,
            &invariants::confirm::ConfirmFacts {
                created: &ev.created,
                switched: &ev.switched,
                opens: &ev.opens,
                liquidations: &ev.liquidations,
                expiries: &ev.expiries,
                settlements: &ev.settlements,
                armed: &ev.armed,
                disarmed: &ev.disarmed,
                venue_changed: &ev.venue_changed,
            },
        )
        .map_err(|v| v.invariant)
    };
    let mut ev = IxEvents::of(&meta, 0);
    assert_eq!(ev.liquidations.len(), 1);
    assert_eq!(i18(&ev), Ok(()), "the real liquidation");
    ev.liquidations[0].cr_spot_bps += 1;
    assert_eq!(i18(&ev), Err("I18"), "a spot CR off the floored depth's");
}

/// **FITS-2 (a) with the keeper, the verifier's small-position case** (a regression). A position
/// at 500 ‰ of its open cap (≈ 1.23 % of `X`) on the contest market, a pump to 2.025× (under the
/// honest threshold) after FITV-2's 30-s drag: as found, the attacker liquidated at +71 s
/// (2.025×; `L_ema` 0.502, `L_slow` 0.983, `L_mlo` 0.500 × `√(X·Y)`: the drag had taken `L_ema`,
/// and the drained `L_live` the spot leg, to the locked depth; penalty 0.6415 SOL, 0.1925 of it to
/// the attacker) where the honest keeper never does. Now, in both timings — the reproducer's
/// (the attacker before the keeper in every second) and the adversarial one (after it, from a
/// second the keeper's idle cadence skips) — the keeper's liquidity trigger keeps it per second
/// through the drag (FITS-2 (b)), and the spot leg is judged at `max(L_live, L_ema')` (FITS-2 (a)):
/// **no forced liquidation**; the keeper's observations match the attacker's wraps 1:1. The drag
/// here is made of attacker-only seconds; after a genuine held withdrawal the same position is
/// forced again ([`fitx1_a_held_drain_then_the_freeze_forces_what_a_withdrawal_for_good_does`]).
/// Pinned to that drag: `FITX_HELD` / `FITX_GONE` in the environment do not apply here.
#[test]
fn fits2_a_the_small_position_contest_forces_no_liquidation() {
    for after in [false, true] {
        let spec = ContestSpec {
            top: 20_250,
            qpm: 500,
            after,
            held: 0,
            gone: false,
            ..ContestSpec::from_env()
        };
        let name = format!("FITS-2 (a), q 500‰, 2.025×, attacker after the keeper: {after}");
        let ((honest, _), (att, cost)) = contest_pair(&name, &spec);
        assert!(
            honest.is_none() && att.is_none(),
            "{name}: nobody liquidates (honest {:?}, attack {:?})",
            honest.as_ref().map(|r| r.0),
            att.as_ref().map(|r| r.0)
        );
        assert!(
            cost.keeper_flat_obs >= spec.w - 2,
            "{name}: per second through the drag ({} of {})",
            cost.keeper_flat_obs,
            spec.w
        );
    }
}

/// **FITS-2 (b), the ~25-minute drag** (a regression). As found, the cap-sized position (≈ 1.9 %
/// of `X`) with withdraw-wrapped cranks held for 1,500 s in the keeper's idle seconds before a
/// pump to 2.05×: `L_slow` fell to 0.828 × `√(X·Y)` and the attacker's wrapped liquidation passed
/// at +67 s (2.050×: penalty 1.005 SOL, the borrower 4.955 SOL) where the honest keeper never
/// liquidates. Now a foreign bucket liquidity keeps the keeper per second (10 s from each wrap, on
/// a market with positions): in the reproducer's timing no second of the drag is the attacker's;
/// in the adversarial one only the first (`L_ema` moves `dt/(τ + dt)` of the way to the locked
/// depth once, then stays: a bucket with the keeper's honest extreme clamps it), and the spot leg
/// is judged at `max(L_live, L_ema')`. Asserted in both timings: **no forced liquidation**, the
/// keeper observed every second of the drag after the first, and the keeper's cost is the
/// attacker's: one crank per wrapped crank (1:1, printed). Knobs as FITV-2's (`FITV_W` the drag).
/// The drag here is made of attacker-only seconds (`FITX_HELD` / `FITX_GONE` do not apply); after
/// a genuine held withdrawal the same position is forced again
/// ([`fitx1_b_a_held_drain_then_a_25_minute_freeze_forces_the_cap_sized_position`]).
#[test]
fn fits2_b_a_25_minute_withdraw_drag_forces_no_liquidation() {
    for after in [false, true] {
        let spec = ContestSpec {
            w: envv("FITV_W", 1_500),
            after,
            held: 0,
            gone: false,
            ..ContestSpec::from_env()
        };
        let name = format!(
            "FITS-2 (b), a {} s drag, 2.05×, attacker after the keeper: {after}",
            spec.w
        );
        let (_, (att, cost)) = contest_pair(&name, &spec);
        assert!(
            att.as_ref().is_none_or(|a| !a.2),
            "{name}: the attacker's liquidation landed at +{:?} s",
            att.as_ref().map(|a| a.0)
        );
        assert!(
            cost.keeper_flat_obs >= spec.w - 2,
            "{name}: per second through the drag ({} of {})",
            cost.keeper_flat_obs,
            spec.w
        );
        println!(
            "{name}: keeper {} cranks for the attacker's {} wrapped cranks ({:.2} per wrap)",
            cost.keeper_cranks,
            cost.wraps,
            cost.keeper_cranks as f64 / cost.wraps.max(1) as f64
        );
    }
}

/// **FITS-2, the residual of the first wrap** (a documented residual, pinned; ECON §5.8 duty 2).
/// The keeper's liquidity trigger answers a wrap in the second after it: in the adversarial timing
/// (the attacker's `[withdraw all, crank_price, deposit]` after the keeper's tick, from a second its
/// idle cadence skips) that first second is the attacker's, and integrates `dt/(τ + dt)` of the
/// way to the locked depth — here `dt = 1`, `1/6` of the unlocked half: `L_ema` 0.917 × `√(X·Y)`
/// (`dt = 2`, reachable with a roll-only instruction before it, would give `2/7`: 0.857). Every
/// later second carries the keeper's honest extreme, which clamps `L_ema` there while the attacker
/// keeps wrapping (one keeper crank per wrap). The spot leg at `max(L_live, L_ema')` then reads
/// that depth, not the drained one, so a withdraw-wrapped liquidation passes only in a band of
/// ≈ `(q/x)·(1/0.917 − 1)` of price under the honest threshold: on the contest market with
/// the position at 500 ‰ of its cap (1.23 % of `X`) the honest keeper liquidates at a
/// `FITS_HONEST_TOP` (2.040×) top and not at a `FITS_BAND_TOP` (2.038×) one, where the attacker
/// does (measured: +83 s, `cr_spot` 14,981 against `L` 15,000; the borrower 3.245 SOL against
/// 3.382 at the honest 2.040×). Asserted: the drag is that one second (`L_ema` ≥ 0.91) and a
/// forced liquidation, if any, is within 0.3 % of the honest one's price.
#[test]
fn fits2_residual_the_first_wrap_leaves_a_narrow_band() {
    let base = ContestSpec {
        qpm: 500,
        after: true,
        held: 0,
        gone: false,
        ..ContestSpec::from_env()
    };
    let at = |top: u128| ContestSpec {
        top,
        ..base.clone()
    };
    let (band_top, honest_top): (u128, u128) = (
        envv("FITS_BAND_TOP", 20_380),
        envv("FITS_HONEST_TOP", 20_400),
    );
    let honest = withdraw_contest(&at(honest_top), false);
    show_contest(
        &format!("FITS-2 residual, honest at {:.4}×", honest_top as f64 / 1e4),
        &at(honest_top),
        &honest.0,
        &honest.1,
    );
    let att = withdraw_contest(&at(band_top), true);
    show_contest(
        &format!("FITS-2 residual, attack at {:.4}×", band_top as f64 / 1e4),
        &at(band_top),
        &att.0,
        &att.1,
    );
    let px_h = honest
        .0
        .as_ref()
        .expect("the honest keeper liquidates at its top")
        .3;
    assert!(
        att.1.l_ema_at_pump >= 0.91,
        "one attacker-only second of drag: L_ema {:.3}",
        att.1.l_ema_at_pump
    );
    if let Some((_, _, true, px_a, _)) = &att.0 {
        assert!(
            *px_a >= px_h * 0.997,
            "a forced liquidation within 0.3 % of the honest price: {px_a:.4} vs {px_h:.4}"
        );
    }
}

/// One FITX-1 case (the held drain, then the withdraw-wrapped freeze): the honest keeper with the
/// attacker's LP in the pool, the attack (`spec.held` s of LP genuinely out, then the wraps), and
/// the same LP leaving at +1 s for good with nobody else acting (`gone`). Asserts the residual as
/// pinned — the honest keeper leaves the position open; the attacker liquidates it; the freeze
/// holds the genuinely reached low (`L_ema` ≤ `l_ema_max` × `√(X·Y)` at the pump) with the keeper
/// observing every second after the re-deposit — and its bound: the withdrawal for good gets the
/// same liquidation from the honest keeper, at the same price and no later, the borrower's
/// remainder no lower under the attack. Returns the attack's `L_slow / √(X·Y)` at the pump.
fn held_drain_case(name: &str, spec: &ContestSpec, l_ema_max: f64) -> f64 {
    let (honest, honest_cost) = withdraw_contest(spec, false);
    show_contest(&format!("{name}, honest"), spec, &honest, &honest_cost);
    let (att, cost) = withdraw_contest(spec, true);
    show_contest(&format!("{name}, attack"), spec, &att, &cost);
    let gone_spec = ContestSpec {
        gone: true,
        ..spec.clone()
    };
    let (gone, gone_cost) = withdraw_contest(&gone_spec, false);
    show_contest(
        &format!("{name}, the LP out for good (no attacker)"),
        &gone_spec,
        &gone,
        &gone_cost,
    );
    assert!(
        honest.is_none(),
        "{name}: the honest keeper leaves the position open with the LP in the pool ({:?})",
        honest.as_ref().map(|r| r.0)
    );
    // The residual, pinned (ECON §12.12 residual 10; O18): a fix of it flips this assertion.
    let (s_a, ev_a, by_attacker, px_a, _) = att.as_ref().unwrap_or_else(|| {
        panic!("{name}: the held drain and the freeze force the liquidation (the pinned residual)")
    });
    assert!(
        *by_attacker,
        "{name}: the attacker's wrapped liquidation lands"
    );
    assert!(
        cost.l_ema_at_pump <= l_ema_max,
        "{name}: the freeze holds the genuinely reached low: L_ema {:.3} at the pump",
        cost.l_ema_at_pump
    );
    assert!(
        cost.keeper_obs_after_held >= spec.w - spec.held - 2,
        "{name}: the keeper covers every second after the re-deposit ({} of {})",
        cost.keeper_obs_after_held,
        spec.w - spec.held
    );
    let (s_g, ev_g, by_attacker_g, px_g, _) = gone.as_ref().unwrap_or_else(|| {
        panic!("{name}: the honest keeper liquidates the withdrawn pool's position")
    });
    assert!(!*by_attacker_g);
    assert!(
        s_a >= s_g && (px_a - px_g).abs() < 1e-3,
        "{name}: the freeze forces nothing earlier than a withdrawal for good ({s_a} s @{px_a:.4}x \
         vs {s_g} s @{px_g:.4}x)"
    );
    assert!(
        ev_a.borrower_payout + 1_000 >= ev_g.borrower_payout,
        "{name}: the borrower gets no less than after a withdrawal for good ({} vs {})",
        ev_a.borrower_payout,
        ev_g.borrower_payout
    );
    println!(
        "{name}: forced at +{s_a} s @{px_a:.3}x, as the withdrawal for good (+{s_g} s @{px_g:.3}x); \
         borrower {} vs {} lamports; keeper {} cranks for {} wraps; L_ema / L_slow at the pump \
         {:.3} / {:.3}",
        ev_a.borrower_payout,
        ev_g.borrower_payout,
        cost.keeper_cranks,
        cost.wraps,
        cost.l_ema_at_pump,
        cost.l_slow_at_pump
    );
    cost.l_slow_at_pump
}

/// **FITX-1 — the held drain, frozen (a pinned residual;** the verification of #101; ECON §12.12
/// residual 10 and (5b) (vii); owner item O18**).** FITS-2 (b)'s per-second coverage denies a drag
/// its attacker-only seconds, but a covered bucket holds `L_ema` where it is — `[drained,
/// honest]`, whose clamped observation is `L_ema` itself — and cannot lift it. So an unlocked LP
/// withdraws **for real** and keeps its LP out for 20 s (cranking every second at the drained
/// depth, which the keeper reads too: nothing is foreign, no coverage starts), taking `L_ema`
/// genuinely to ≈ 0.51 × the pool's `√(X·Y)`, re-deposits, and then freezes it there with
/// `[withdraw all, crank_price, deposit]` every second (one keeper crank per wrap). Its
/// `[withdraw all, liquidate, deposit]` then meets the spot leg at `max(L_live, L_ema')` = the
/// frozen low and the EMA leg at the same low, and where `δ` covers the drained cost the exit
/// bound passes. Measured (W 60 s, the pump over 60 s): the 1 % position at 500 ‰ of its cap
/// (1.232 % of `X`) at 2.025× — forced at +71 s (penalty 0.6416 SOL, 0.1925 of it the
/// attacker's; the borrower 3.3017 SOL) — and the 25-bps position at its open cap (0.469 % of `X`)
/// at 2.010× — forced at +76 s (the borrower 1.2584 SOL); the honest keeper leaves both open. The
/// bound (asserted): the same LP withdrawing **for good** at +1 s, with no attacker, gets the
/// same liquidation from the honest keeper — the same second, price and buyback cost, the
/// borrower's remainder no lower under the attack — so the freeze forces nothing a genuine
/// withdrawal does not; the attacker is spared keeping its LP out after the 20 s. A fix (e.g. the
/// spot leg floored at the ring's `L_mhi`, which (5b) (vii) evaluates) is an owner decision; it
/// flips the pinned "forced" assertion. Knobs as FITV-2's (`FITV_AFTER=1`: the same, +71 s).
#[test]
fn fitx1_a_held_drain_then_the_freeze_forces_what_a_withdrawal_for_good_does() {
    for (name, fee, qpm, top) in [
        (
            "FITX-1, 1 %, 500 ‰ of the cap, 2.025×",
            10_000u64,
            500u64,
            20_250u128,
        ),
        ("FITX-1, 25 bps, the open cap, 2.010×", 2_500, 1_000, 20_100),
    ] {
        let spec = ContestSpec {
            fee,
            qpm,
            top,
            w: 60,
            held: 20,
            gone: false,
            ..ContestSpec::from_env()
        };
        held_drain_case(name, &spec, 0.53);
    }
}

/// **FITX-1 over a long freeze — the cap-sized position (a pinned residual,** as
/// [`fitx1_a_held_drain_then_the_freeze_forces_what_a_withdrawal_for_good_does`]**).** The FITS-2
/// (b) regression's position (1,000 ‰ of its cap, 1.899 % of `X`), where `δ` does not cover the
/// drained cost while `L_slow` credits the pool's depth: after the 20-s held drain the freeze runs
/// 1,480 s, `L_slow` follows the drained lows (its input `min(lobs, L_mlo)`, every bucket's low the
/// drained depth; τ_slow = 1 h; 0.833 × `√(X·Y)` at the pump), and
/// the wrapped liquidation passes at 2.05× (+67 s; penalty 1.0048 SOL; the borrower 4.9608 SOL)
/// where the honest keeper never liquidates (1,860 s) — at one keeper crank per wrap (1,547 /
/// 1,547). A withdrawal for good at +1 s gets the same liquidation from the honest keeper (+67 s).
#[test]
fn fitx1_b_a_held_drain_then_a_25_minute_freeze_forces_the_cap_sized_position() {
    let spec = ContestSpec {
        w: 1_500,
        qpm: 1_000,
        top: 20_500,
        held: 20,
        gone: false,
        ..ContestSpec::from_env()
    };
    let l_slow = held_drain_case("FITX-1, the cap, a 25-minute freeze, 2.05×", &spec, 0.53);
    assert!(
        l_slow <= 0.86,
        "L_slow follows the drained lows over the freeze: {l_slow:.3} at the pump"
    );
}

/// The keeper's own decision now (the new rule: its plan is `Liquidate`) and the same decision
/// with the spot leg judged at `L_live` (the rule before FITS-2 (a)), on the state the next
/// instruction will see: `(old, new, what)`, `what` the legs, the bound and `L_ema`, `L_slow`
/// over `√(X·Y)`.
fn old_and_new(h: &Harness, key: Pubkey, position: Pubkey) -> (bool, bool, String) {
    use shortwin::engine::bounds;
    use shortwin_keeper::config::CoverageConfig;
    use shortwin_keeper::decide::{self, Plan};
    use shortwin_math::{observe, rent, risk};
    let cv = shortwin_keeper_litesvm::block_on(shortwin_keeper::view::load(h.chain(), &[key]))
        .expect("view");
    let mv = &cv.markets[0];
    let proj = decide::project(&cv.config, mv, cv.clock).expect("projection");
    let pv = mv
        .positions
        .iter()
        .find(|p| p.key == position)
        .expect("position");
    let ev = decide::evaluate(mv, &proj, pv, &CoverageConfig::default()).expect("evaluation");
    let new = ev.plan == Plan::Liquidate;
    let (pos, st, obs, now) = (&pv.pos, &mv.state, &proj.obs, proj.now);
    let cap = u64::from(pos.venue_fee_hard_cap_bps);
    let r_due = rent::due(&pos.pos_rent(), &proj.market.basis()).expect("rent");
    let v_eff = risk::v_eff(pos.v, r_due);
    let p_rs = st
        .price()
        .and_then(|p| observe::robust_spot(p, &obs.last, now, pos.ref_max_age_s));
    let l_live = st.liquidity().unwrap_or(obs.ema.liq);
    let b_rs = st
        .pricing
        .zip(p_rs)
        .and_then(|(pr, p)| pr.capped(cap).buy_repriced(pos.q, l_live, p))
        .map(|b| b.cost);
    let spot_old =
        risk::spot_leg(st.executable, ev.fresh, v_eff, b_rs, pos.cr_liq_bps).expect("leg");
    let max_in = bounds::liq_reference(pos, obs, st, now, proj.depth_cap)
        .expect("reference")
        .and_then(|r| r.b_ref)
        .and_then(|b| bounds::max_in_liq(pos, b, ev.k_s));
    let old = spot_old
        && ev.ema_leg
        && ev.fresh
        && ev.buyback_allowed
        && proj.market.ema_warm == 0
        && ev.b_live.zip(max_in).is_some_and(|(b, m)| b <= m);
    let what = format!(
        "L_ema {:.3} L_slow {:.3}; spot {}/{} ema {} bb {} B/max_in {:.4} {:?}",
        obs.ema.liq as f64 / l_live as f64,
        obs.ema.liq_slow as f64 / l_live as f64,
        u8::from(spot_old),
        u8::from(ev.spot_leg),
        u8::from(ev.ema_leg),
        u8::from(ev.buyback_allowed),
        ev.b_live
            .zip(max_in)
            .map_or(0.0, |(b, m)| b as f64 / m.max(1) as f64),
        ev.plan
    );
    (old, new, what)
}

/// **FITS-2 (c): a genuinely withdrawn pool is still liquidated** (a regression, and the delay
/// measured). On the contest market (λ ≈ 49.9 %; the unlocked LP here an honest one) with a
/// position at `FITS_QPM` (default 500 ‰, ≈ 1.2 % of `X`) of its open cap, the LP withdraws
/// `FITS_WD_PCT` (100) % of its LP for good `FITS_WD` s before a genuine pump to `FITS_TOP`
/// (2.5×) over 60 s (default 10, 0 and −40 s: before, at, and 40 s into the pump, near the
/// threshold), and 90 s into a pump to `FITS_FLAT_TOP` (2.03×) held flat, where the position is
/// healthy at the pool's full depth and liquidatable only at the withdrawn one (the withdrawal
/// alone makes it liquidatable, with `P_ema` caught up) — the pool's depth halves; the keeper
/// ticks every second. The spot leg at
/// `max(L_live, L_ema')` waits for `L_ema` to decay toward the new depth (τ = 5 s), but the EMA
/// leg reads the same `L_ema` in its newest ring entry, so in a pump (`P_rs ≥ P_ema`) the new rule
/// adds nothing to the EMA leg's own lag. Measured each second on the keeper's projection: the
/// first second the rule before FITS-2 (a) (the spot leg at `L_live`) would have accepted the
/// liquidation, the first the new one accepts, and the keeper's landed liquidation. Asserted:
/// liquidated, at most `FITS_SLACK` (default 1) s after the old rule's first second. Measured:
/// 0 s in all four — the pumps at +49 s whenever the withdrawal came; held flat, the old spot leg
/// holds from the withdrawal (+90 s), the new one from +93 s (`L_ema` 2.000 → 1.579 × the new
/// `√(X·Y)`), the EMA leg from +95 s, and both rules and the keeper at +95 s: the EMA leg's own lag
/// binds.
///
/// Not FITS-2 (both rules alike; `L_b` is unchanged): at 1,000 ‰ (1.9 % of the old `X`, 3.8 % of
/// the withdrawn pool's) the exit bound is not met after the withdrawal — `L_b` keeps V2-14's
/// `L_slow` credit (τ_slow = 1 h) of the old depth, up to the M4F-1 cap `⌊D·10⁴/λ_min⌋` (here the
/// old depth), and the δ of 2 % does not cover the doubled impact (`B̂_live/max_in` ≈ 1.009 at
/// 2.5×): the position waits (`LiquidateWait`, spot and EMA legs holding) while `L_slow` decays —
/// measured with `FITS_QPM=1000 FITS_WD=10 FITS_SECS=5400 FITS_SLACK=100000`: liquidated at
/// +2,684 s (≈ 45 min) into the pump, `L_slow` then 1.471 × the new `√(X·Y)`, both rules alike.
#[test]
fn fits2_c_a_genuine_withdrawal_then_a_pump_is_still_liquidated() {
    let top: u128 = envv("FITS_TOP", 25_000);
    let slack: i64 = envv("FITS_SLACK", 1);
    let qpm: u64 = envv("FITS_QPM", 500);
    let wd_pct: u64 = envv("FITS_WD_PCT", 100);
    let show: usize = envv("FITS_SHOW", 12);
    let secs: i64 = envv("FITS_SECS", 360);
    let flat: u128 = envv("FITS_FLAT_TOP", 20_300);
    let cases: Vec<(i64, u128)> = match std::env::var("FITS_WD") {
        Ok(v) => v
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .map(|wd| (wd, top))
            .collect(),
        Err(_) => vec![(10, top), (0, top), (-40, top), (-90, flat)],
    };
    let w: i64 = 30;
    for (wd, top) in cases {
        let c = contest_market(10_000, 5_000, 100, qpm);
        let ContestMarket {
            env,
            rm,
            pool,
            cp,
            who,
            borrower,
            nonce,
            facts,
        } = c;
        println!("FITS-2 (c), withdrawal {wd} s before the pump: {facts}");
        let key = rm.market();
        let position = pda::position(&key, &borrower.key(), nonce).0;
        let mut h = Harness::new(env, KeeperConfig::default());
        let p0 = h.with_env(|env| spot(env, &pool));
        let (mut first_old, mut first_new, mut landed) = (None, None, None);
        let mut trail = Vec::new();
        let mut other = None;
        for s in 1..=(w + secs) {
            h.idle(1);
            if s == w - wd {
                h.with_env(|env| {
                    let lp_held = read::token_amount(&env.svm, &who.lp).unwrap_or(0) / 100 * wd_pct;
                    ok(env.send(&[lp::withdraw_ix(&cp, &who, lp_held)], &[&who.owner]));
                });
            }
            if s > w {
                let g =
                    10_000 + (top - 10_000) * u128::try_from((s - w).min(60)).expect("u128") / 60;
                h.with_env(|env| pump_to(env, &pool, p0, g));
            }
            let (old, new, what) = old_and_new(&h, key, position);
            if old && first_old.is_none() {
                first_old = Some(s);
            }
            if new && first_new.is_none() {
                first_new = Some(s);
            }
            // The seconds around the first acceptance (either rule), or the pump's last ones.
            if s >= w - wd {
                trail.push(format!("+{} s: {what}", s - w));
                if first_old.is_none() && first_new.is_none() && trail.len() > show {
                    trail.remove(0);
                }
            }
            let r = h.tick().clone();
            assert!(r.errors.is_empty(), "+{s} s: {:?}", r.errors);
            if let Some(e) = r
                .exits
                .iter()
                .find(|e| e.position == position && e.result.landed().is_some())
            {
                if e.kind == ExitKind::Liquidate {
                    landed = Some(s);
                } else {
                    other = Some((s, e.kind));
                }
                if h.with_env(|env| read::account(&env.svm, &position).is_none()) {
                    break;
                }
            }
        }
        if let Some((s, kind)) = other {
            println!("FITS-2 (c): {kind:?} landed at +{} s", s - w);
        }
        println!(
            "FITS-2 (c), {wd_pct} % of the LP withdrawn {wd} s before a pump to {:.3}×, q {qpm}‰: \
             the old rule first accepts at +{:?} s, the new at +{:?} s, the keeper's liquidation \
             lands at +{:?} s (into the pump); the seconds before it (over the live sqrt(XY); \
             spot leg old/new):\n    {}",
            top as f64 / 1e4,
            first_old.map(|s| s - w),
            first_new.map(|s| s - w),
            landed.map(|s| s - w),
            trail.join("\n    ")
        );
        let at = landed.expect("the keeper liquidates the withdrawn pool's position");
        let old_at = first_old.expect("the old rule accepts it at some second");
        assert!(
            at - old_at <= slack,
            "withdrawal {wd} s before the pump: {} s after the old rule",
            at - old_at
        );
    }
}
