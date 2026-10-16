//! "Switch only if all fit" (owner decision of 2026-09-29; ARCH §2.6, §4.6, ECON §6.1): a pool
//! adoption that carries open positions — a collection window's, on a Raydium market or on a
//! list-only market with positions — is allowed only if every open position fits the
//! candidate's normal opening caps (the per-position impact and fee-scaled size caps on its
//! conservative locked-depth `X₀`, bounded in O(1) by `min(max_q_hwm, ΣB_Q)`; the per-market
//! impact and fee-scaled OI caps on `ΣB_Q`; the tier the market would have on it), at `P_ref =
//! min(P_ema, P_cand)` and at `P_ema`. A proposal of a candidate that does not fit is refused
//! (`CarriedPositionsTooLarge`); a window whose candidate no longer fits is cancelled at the
//! execution. The exact integer boundaries are the math crate's and the engine's unit tests
//! (`caps::carried_fit`, `engine::pool_select::carried_fit`); here each dimension is shown on
//! the fork with candidates planted 1 % either side of it, on both Raydium programs, and every
//! instruction runs the I15 hook (which re-checks every open position's own `q`). The verification
//! of #99 (ECON #100) adds FIT-1's documented residual (a dump-wrapped proposal: the guarantee is
//! the execution's `X₀`) and FIT-3's disclosed residual (a closed large position keeps the mark up:
//! the switch is refused and the positions settle in S3 at fair value at their due second). The
//! fee-scaled caps take `min(min_open_fee_ppm, φ_cand, CARRIED_FEE_CAP_PPM)` (FIT-2, FITS-1):
//! every candidate here charges the incumbent's 0.25 % — the ceiling itself — so neither the mark
//! nor the ceiling changes anything (the 1 % cases, a 25 bps → 1 % and a 1 % → 1 % switch, are the
//! keeper's `freeze` scenarios and the engine's unit tests).
//!
//! The market: a planted AMM v4 incumbent of ≈ 380 SOL at Fartcoin's price and locked share
//! (fee 0.25 %: the fee-scaled caps bind, 0.75 % of `X₀` per position and 1.875 % per market),
//! lent a fifth of its token reserve; candidates are planted at a fraction of it (a 40 % one
//! qualifies for the fast path).

use shortwin::events::{POOL_KIND_FAST, PoolSwitched};
use shortwin_litesvm_tests::fixtures::set_zc;
use shortwin_litesvm_tests::fork::{CpmmPool, Swap};
use shortwin_litesvm_tests::lp;
use shortwin_litesvm_tests::pool_select::{
    carried_caps, execute_pool_switch_ix, pool_key, propose_pool_ix,
};
use shortwin_litesvm_tests::raydium::candidate_read_set;
use shortwin_math::fees::Direction;
use shortwin_venue_state::raydium_cpmm;

use crate::common::*;
use crate::support::*;

/// The incumbent (module docs), after its listing window, with 5 integrated seconds.
fn incumbent() -> (Env, RayMarket) {
    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let lam = lambda_of(&env, &base);
    let inc = plant(&mut env, &base, Plant::V4, true, (1, 100), lam);
    ok(env.list_ray(&inc, VENUE_HINT_V4));
    let (x, _) = inc.io_reserves(&env.svm, Direction::Sell).unwrap();
    let rm = env.ray_market_listed(RaySpec::new(FART, x / 5), inc);
    env.crank(&rm);
    crank_every(&mut env, &rm, 1, 5);
    (env, rm)
}

/// The incumbent's token reserve.
fn x_of(env: &Env, pool: &AnyPool) -> u64 {
    pool.io_reserves(&env.svm, Direction::Sell).unwrap().0
}

/// A candidate of `kind` at `ppm` millionths of the incumbent's reserves (its price and locked
/// share, WSOL first).
fn candidate(env: &mut Env, rm: &RayMarket, kind: Plant, ppm: u64) -> AnyPool {
    let lam = lambda_of(env, &rm.pool);
    plant(env, &rm.pool, kind, true, (ppm, 1_000_000), lam)
}

/// The size (ppm of the incumbent) at which a candidate's cap would be `need`, from a 40 %
/// reference candidate whose cap is `at_ref` (every cap scales with the pool at one price), times
/// `permille/1,000`.
fn scale_for(need: u64, at_ref: u64, permille: u64) -> u64 {
    u64::try_from(
        400_000u128 * u128::from(need) * u128::from(permille) / (u128::from(at_ref) * 1_000),
    )
    .unwrap()
}

/// Halts the incumbent and runs its `pool_fail` chain for 150 s (the fast path armed).
fn arm(env: &mut Env, rm: &RayMarket) {
    halt(env, &rm.pool);
    crank_every(env, rm, 30, 5);
}

/// Opens `q` for a new borrower (must succeed): its nonce.
fn open_new(env: &mut Env, rm: &RayMarket, q: u64) -> (Borrower, u64) {
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(env, rm, &mut b, q);
    (b, nonce)
}

/// Per position, on both Raydium programs: with one position of 0.3 % of the incumbent's `X`,
/// a candidate planted 1 % too thin for it (its per-position cap just under `q`) is refused
/// `CarriedPositionsTooLarge` and opens no window; one 1 % deeper than needed opens the window,
/// and the execution adopts it with the position — its terms unchanged, the mark `q`.
#[test]
fn a_position_above_the_candidates_cap_is_refused() {
    for kind in [Plant::Cpmm, Plant::V4] {
        let (mut env, rm) = incumbent();
        let q = x_of(&env, &rm.pool) / 10_000 * 30;
        let (b, nonce) = open_new(&mut env, &rm, q);
        crank_every(&mut env, &rm, 1, 3);
        arm(&mut env, &rm);
        let reference = candidate(&mut env, &rm, kind, 400_000);
        let (pos_ref, _) = carried_caps(&env, &reference);
        let short = candidate(&mut env, &rm, kind, scale_for(q, pos_ref, 990));
        let fits = candidate(&mut env, &rm, kind, scale_for(q, pos_ref, 1_010));
        let (cap_short, cap_fits) = (carried_caps(&env, &short).0, carried_caps(&env, &fits).0);
        println!("{kind:?}: q {q}; caps {cap_short} (1 % short) and {cap_fits} (1 % over)");
        assert!(cap_short < q && cap_fits >= q, "{kind:?}");
        assert_err(
            &env.propose(Some(&rm.pool), &short),
            E::CarriedPositionsTooLarge,
        );
        assert_eq!(env.market(&rm.market()).fp_t0, 0, "{kind:?}: no window");
        let pos0 = env.position(&rm, &b.key(), nonce);
        window_ok(&mut env, &rm.pool, &fits);
        crank_every(&mut env, &rm, 30, 2);
        let e = execute_ok(&mut env, &rm.pool, &fits).unwrap_or_else(|_| panic!("cancelled"));
        assert_eq!((e.kind, e.candidate), (POOL_KIND_FAST, pool_key(&fits)));
        assert_eq!(env.position(&rm, &b.key(), nonce), pos0, "locked terms");
        let m = env.market(&rm.market());
        assert_eq!((m.open_positions, m.max_q_hwm), (1, q));
    }
}

/// Per market, on both Raydium programs: three positions of 0.2 % of the incumbent's `X` each
/// (every one within the per-position cap of both candidates) whose sum `ΣQ` is 1 % above the
/// OI cap of one candidate (refused: the per-market dimension) and 1 % under that of a deeper
/// one (the window, then the adoption).
#[test]
fn positions_whose_sum_is_above_the_candidates_oi_cap_are_refused() {
    for kind in [Plant::Cpmm, Plant::V4] {
        let (mut env, rm) = incumbent();
        let q = x_of(&env, &rm.pool) / 10_000 * 20;
        for _ in 0..3 {
            open_new(&mut env, &rm, q);
        }
        crank_every(&mut env, &rm, 1, 3);
        arm(&mut env, &rm);
        let reference = candidate(&mut env, &rm, kind, 400_000);
        let (_, oi_ref) = carried_caps(&env, &reference);
        let short = candidate(&mut env, &rm, kind, scale_for(3 * q, oi_ref, 990));
        let fits = candidate(&mut env, &rm, kind, scale_for(3 * q, oi_ref, 1_010));
        let (pos_short, oi_short) = carried_caps(&env, &short);
        let (_, oi_fits) = carried_caps(&env, &fits);
        println!("{kind:?}: 3 × q {q}; OI caps {oi_short} (1 % short) and {oi_fits}");
        assert!(
            q <= pos_short,
            "{kind:?}: every position fits the thinner one"
        );
        assert!(oi_short < 3 * q && oi_fits >= 3 * q, "{kind:?}");
        assert_err(
            &env.propose(Some(&rm.pool), &short),
            E::CarriedPositionsTooLarge,
        );
        window_ok(&mut env, &rm.pool, &fits);
        crank_every(&mut env, &rm, 30, 2);
        let e = execute_ok(&mut env, &rm.pool, &fits).unwrap_or_else(|_| panic!("cancelled"));
        assert_eq!(e.candidate, pool_key(&fits));
        assert_eq!(env.market(&rm.market()).open_positions, 3);
    }
}

/// Lowers `pool`'s one-way fee from 0.25 % to 0.10 % (the pool admin's `SetParams` on AMM v4, the
/// `AmmConfig` trade rate on CPMM): its fee-scaled size cap `3·φ·X₀` falls with it.
fn lower_fee(env: &mut Env, pool: &AnyPool) {
    match pool {
        AnyPool::V4(p) => fork::set_v4_swap_fee(&mut env.svm, p, 10, 10_000),
        AnyPool::Cpmm(p) => {
            let config = shortwin_litesvm_tests::raydium::cpmm_config_key(&env.svm, p);
            let acc = env.svm.get_account(&config).unwrap();
            let c = raydium_cpmm::read_config(&acc.owner.to_bytes(), &acc.data).unwrap();
            fork::set_cpmm_config_rates(
                &mut env.svm,
                &config,
                1_000,
                c.protocol_fee_rate,
                c.fund_fee_rate,
                c.creator_fee_rate,
            );
        }
    }
}

/// At the execution, on both Raydium programs and on a list-only market with a position: a
/// window whose candidate's fee falls during the window (0.25 % → 0.10 %: its fee-scaled size
/// cap falls under the position) is cancelled (`PoolSwitchCancelled`), the position stays where
/// it was; on a Raydium market the chain stays armed and a candidate the position fits opens the
/// next window and is adopted.
#[test]
fn a_window_whose_candidate_no_longer_fits_is_cancelled() {
    for (kind, list_only) in [
        (Plant::Cpmm, false),
        (Plant::V4, false),
        (Plant::Cpmm, true),
    ] {
        let (mut env, rm) = incumbent();
        let q = x_of(&env, &rm.pool) / 10_000 * 30;
        open_new(&mut env, &rm, q);
        crank_every(&mut env, &rm, 1, 3);
        if list_only {
            // The M5 transition simulated (M4-SEC-4): a list-only market with its position.
            let mut m = env.market(&rm.market());
            m.venue = VenueKind::None as u8;
            m.pool_cache = [Pubkey::default(); 10];
            m.pool_d.set(0);
            set_zc(&mut env.svm, &rm.market(), &m, 0);
        } else {
            arm(&mut env, &rm);
        }
        let reference = candidate(&mut env, &rm, kind, 400_000);
        let (pos_ref, _) = carried_caps(&env, &reference);
        let cand = candidate(&mut env, &rm, kind, scale_for(q, pos_ref, 1_050));
        let meta = if list_only {
            ok(env.propose(None, &cand))
        } else {
            ok(env.propose(Some(&rm.pool), &cand))
        };
        assert!(events::of::<PoolSwitched>(&meta).is_empty(), "a window");
        assert!(env.market(&rm.market()).fp_t0 > 0);
        lower_fee(&mut env, &cand);
        assert!(carried_caps(&env, &cand).0 < q, "{kind:?}: no longer fits");
        env.warp(60);
        if !list_only {
            env.crank(&rm);
        }
        let meta = if list_only {
            ok(env.execute_switch_list_only(&cand))
        } else {
            ok(env.execute_switch(&rm.pool, &cand))
        };
        assert!(
            events::of::<PoolSwitched>(&meta).is_empty(),
            "{kind:?}: cancelled"
        );
        let c: shortwin::events::PoolSwitchCancelled = events::one(&meta);
        assert_eq!(c.candidate, pool_key(&cand));
        let m = env.market(&rm.market());
        assert_eq!((m.fp_t0, m.open_positions), (0, 1));
        if list_only {
            assert_eq!(m.venue_kind(), Some(VenueKind::None), "not adopted");
            println!("list-only: a window whose candidate's fee fell was cancelled");
            continue;
        }
        assert_eq!(pool_now(&env, &rm), pool_key(&rm.pool), "not adopted");
        // The chain is still armed: a deeper V4 candidate the position fits is adopted.
        let v4_ref = candidate(&mut env, &rm, Plant::V4, 400_000);
        let (pos_ref, _) = carried_caps(&env, &v4_ref);
        let next = candidate(
            &mut env,
            &rm,
            Plant::V4,
            scale_for(q, pos_ref, 1_050).max(600_000),
        );
        window_ok(&mut env, &rm.pool, &next);
        crank_every(&mut env, &rm, 30, 2);
        let e = execute_ok(&mut env, &rm.pool, &next).unwrap_or_else(|_| panic!("cancelled"));
        assert_eq!(e.candidate, pool_key(&next));
        println!("{kind:?}: cancelled at the execution, then adopted the next candidate");
    }
}

/// `[sell tokens into the candidate, the ix, buy them back]` on `pool` by a funded trader:
/// the candidate's spot pushed to about a quarter inside the transaction.
fn dump_wrapped(env: &mut Env, pool: &AnyPool, ix: Instruction) -> TxResult {
    let (x, _) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let (mint, program) = pool.token();
    let t = fork::trader(env, &mint, program, 0, x + 1);
    let owner = t.owner.insecure_clone();
    let out = pool
        .quote_exact_in(&env.svm, Direction::Sell, x)
        .unwrap()
        .proceeds;
    let sell = pool.swap_ix(
        &env.svm,
        &owner.pubkey(),
        &t,
        Direction::Sell,
        Swap::ExactIn(x, 0),
    );
    let buy = pool.swap_ix(
        &env.svm,
        &owner.pubkey(),
        &t,
        Direction::Buy,
        Swap::ExactIn(out, 0),
    );
    env.send(&[sell, ix, buy], &[&owner])
}

/// The fit at `P_ema` as well as at `min(P_ema, P_cand)`: a proposal wrapped in a dump of the
/// candidate (its token reserve doubled, its spot a quarter: at that price alone its per-position
/// cap is ≈ 4× larger, and the position would fit) is refused — `X₀` is a token reserve, `∝
/// 1/√P`, and the evaluation at the manipulation-resistant `P_ema` keeps a dump inside the
/// proposing transaction from loosening the fit's depth terms. Undumped it is refused too. (The
/// candidate's live `X`, which binds only when it trades above `P_ema`, is not protected: FIT-1,
/// [`fit1_a_dump_around_the_proposal_carries_at_most_the_executions_cap`].)
#[test]
fn a_dump_around_the_proposal_does_not_loosen_the_fit() {
    let (mut env, rm) = incumbent();
    let q = x_of(&env, &rm.pool) / 10_000 * 30;
    open_new(&mut env, &rm, q);
    crank_every(&mut env, &rm, 1, 3);
    arm(&mut env, &rm);
    let reference = candidate(&mut env, &rm, Plant::Cpmm, 400_000);
    let (pos_ref, _) = carried_caps(&env, &reference);
    let cand = candidate(&mut env, &rm, Plant::Cpmm, scale_for(q, pos_ref, 950));
    assert!(carried_caps(&env, &cand).0 < q);
    // The fit at the dumped spot alone (what `min(P_ema, P_cand)` would read).
    let m = env.market(&rm.market());
    let params = env.config().params;
    let (x, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
    let d = d_of(&env, &cand);
    let dumped = u128::from(y) / 2 * shortwin_math::num::WAD / (2 * u128::from(x));
    let x0 = shortwin_math::caps::conservative_x0(
        2 * x,
        m.liq_slow.get().min(d),
        dumped,
        Some((d, params.pool_lock_min_bps)),
    )
    .unwrap();
    let cap_dumped =
        shortwin_math::caps::position_cap(x0, 500, 2_500, params.fee_size_cap_x100).unwrap();
    println!(
        "dump guard: q {q}, cap at P_ema {}, at the dumped spot {cap_dumped}",
        carried_caps(&env, &cand).0
    );
    assert!(
        q <= cap_dumped,
        "at the dumped spot alone the position would fit"
    );
    let (mint, _) = cand.token();
    let inc = candidate_read_set(&env.svm, &rm.pool, false);
    let prop = propose_pool_ix(&env.svm, &mint, &inc, &cand);
    assert_err(
        &dump_wrapped(&mut env, &cand, prop),
        E::CarriedPositionsTooLarge,
    );
    assert_err(
        &env.propose(Some(&rm.pool), &cand),
        E::CarriedPositionsTooLarge,
    );
    assert_eq!(env.market(&rm.market()).fp_t0, 0);
}

/// The execution re-judges the fit on the terms no transaction can move (`X₀` without the live
/// `X_cand`, M4-SEC-1 / M4-SEC-3's principle). A λ 50 % CPMM candidate whose token doubled
/// before the halt (its live `X` then 0.71 of its pre-pump reserve, its locked-depth term 0.5): the
/// unlocked LP that wraps a proposal in `[withdraw all its LP, propose, deposit]` halves `X_cand`
/// (0.35) and only refuses its own proposal (the literal open caps), but the same wrap around the
/// execution of the honest window does not cancel it — the window is adopted.
#[test]
fn a_withdrawal_wrapped_around_the_execution_does_not_cancel_the_window() {
    let (mut env, rm) = incumbent();
    // 70 % of the incumbent's reserves at λ 50 %: `D` ≈ 35 % of `pool_d`, a fast-path candidate.
    let cand = plant(
        &mut env,
        &rm.pool,
        Plant::Cpmm,
        true,
        (700_000, 1_000_000),
        5_000,
    );
    let AnyPool::Cpmm(cp) = cand else {
        unreachable!()
    };
    let (mint, program) = cand.token();
    let (_, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
    let a = (y as f64 * (2f64.sqrt() - 1.0) / 0.99) as u64;
    let whale = fork::trader(&mut env, &mint, program, a + 1, 0);
    ok(fork::swap(
        &mut env,
        &cand,
        &whale,
        Direction::Buy,
        Swap::ExactIn(a, 0),
    ));
    let (cap, _) = carried_caps(&env, &cand);
    let q = cap / 100 * 95;
    let (b, nonce) = open_new(&mut env, &rm, q);
    crank_every(&mut env, &rm, 1, 3);
    arm(&mut env, &rm);
    let who = unlocked_lp(&mut env, &cp);
    let held = tokens(&env, &who.lp);
    let inc = candidate_read_set(&env.svm, &rm.pool, false);
    let prop = propose_pool_ix(&env.svm, &mint, &inc, &cand);
    assert_err(
        &env.send(
            &[
                lp::withdraw_ix(&cp, &who, held),
                prop,
                lp::deposit_ix(&cp, &who, held - 4),
            ],
            &[&who.owner],
        ),
        E::CarriedPositionsTooLarge,
    );
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    let held = tokens(&env, &who.lp);
    let exec = execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &cand);
    let meta = ok(env.send(
        &[
            lp::withdraw_ix(&cp, &who, held),
            exec,
            lp::deposit_ix(&cp, &who, held - 4),
        ],
        &[&who.owner],
    ));
    let e: PoolSwitched = events::one(&meta);
    assert_eq!((e.kind, e.candidate), (POOL_KIND_FAST, pool_key(&cand)));
    assert_eq!(env.position(&rm, &b.key(), nonce).q, q);
}

/// The holder of `cp`'s unlocked LP (the planted pool's circulating supply) with the assets to
/// re-deposit it.
fn unlocked_lp(env: &mut Env, cp: &CpmmPool) -> lp::Provider {
    let supply = shortwin_litesvm_tests::raydium::mint_supply(&env.svm, &cp.lp_mint);
    let (x, _) = AnyPool::Cpmm(*cp)
        .io_reserves(&env.svm, Direction::Sell)
        .unwrap();
    lp::provider(env, cp, 1_000_000_000_000, x, supply)
}

/// The mark `max_q_hwm` is conservative and resets at zero: three positions — `a` above the
/// candidate's per-position cap `c` (1.2 `c`), `b` and `d` at 0.6 `c` — and `a` closed: every
/// open position fits, but the mark stays at `a`'s `q` and `ΣQ` = 1.2 `c`, so the bound
/// `min(max_q_hwm, ΣQ)` is above `c` and the candidate is refused. With `b` closed too (`ΣQ` =
/// 0.6 `c`) it fits: the window, the adoption, the mark still `a`'s; the last close on the new
/// pool resets it to 0, and a new open sets it to its own `q`.
#[test]
fn the_mark_is_conservative_and_resets_at_zero() {
    let (mut env, rm) = incumbent();
    let cand = candidate(&mut env, &rm, Plant::Cpmm, 400_000);
    let (c, _) = carried_caps(&env, &cand);
    let (a, b, d) = (c / 10 * 12, c / 10 * 6, c / 10 * 6);
    let (ba, na) = open_new(&mut env, &rm, a);
    let (bb, nb) = open_new(&mut env, &rm, b);
    let (bd, nd) = open_new(&mut env, &rm, d);
    crank_every(&mut env, &rm, 1, 2);
    close_ok(&mut env, &rm, &ba, na);
    let m = env.market(&rm.market());
    assert_eq!((m.open_positions, m.max_q_hwm, m.borrowed_q), (2, a, b + d));
    crank_every(&mut env, &rm, 1, 2);
    arm(&mut env, &rm);
    assert_err(
        &env.propose(Some(&rm.pool), &cand),
        E::CarriedPositionsTooLarge,
    );
    // Resume, close `b`, halt again and re-arm.
    resume(&mut env, &rm.pool);
    crank_every(&mut env, &rm, 1, 2);
    close_ok(&mut env, &rm, &bb, nb);
    let m = env.market(&rm.market());
    assert_eq!((m.open_positions, m.max_q_hwm, m.borrowed_q), (1, a, d));
    crank_every(&mut env, &rm, 1, 2);
    arm(&mut env, &rm);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    assert_eq!(
        env.market(&rm.market()).max_q_hwm,
        a,
        "kept across the switch"
    );
    let rm2 = rm.on_pool(&env.svm, cand);
    crank_every(&mut env, &rm2, 1, 10);
    close_ok(&mut env, &rm2, &bd, nd);
    let m = env.market(&rm.market());
    assert_eq!((m.open_positions, m.max_q_hwm), (0, 0), "reset at 0");
    crank_every(&mut env, &rm2, 60, 61);
    let small = q_of(&env, &cand) / 2;
    open_new(&mut env, &rm2, small);
    assert_eq!(env.market(&rm.market()).max_q_hwm, small);
}

// ---- FIT-1 and FIT-3 (the verification of #99) ----------------------------------------------

/// The literal opening cap of `q` on the market's pool **now**, as `open_short` would compute it
/// (`X₀ = conservative_x0(X_live, L_slow, P_ema, (D, λ_min))`, the impact cap of tiers A–C and
/// the fee-scaled cap at φ = 0.25 %), and that `X₀`.
fn literal_open_cap(env: &Env, rm: &RayMarket, pool: &AnyPool) -> (u64, u64) {
    let m = env.market(&rm.market());
    let params = env.config().params;
    let (x, _) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let d = d_of(env, pool);
    let x0 = shortwin_math::caps::conservative_x0(
        x,
        m.liq_slow.get(),
        m.ema_price.get(),
        Some((d, params.pool_lock_min_bps)),
    )
    .unwrap();
    (
        shortwin_math::caps::position_cap(x0, 500, 2_500, params.fee_size_cap_x100).unwrap(),
        x0,
    )
}

/// **FIT-1, a documented residual** (ECON §6.1 "Where the fit is judged", §12.12 residual 13;
/// ARCH §2.6 rule 3). A candidate whose honest spot is `r`× the frozen `P_ema` (a genuine pump
/// during the incumbent's halt) has a live token reserve `X_cand ≈ D/(λ·√(r·P_ema))`, under the
/// locked-depth term `D/√P_ema`: an open on it right after the adoption is capped on `X_cand`.
/// The honest proposal reads `X_cand` and is refused, but `[sell ≈X tokens into the candidate,
/// propose_pool, buy them back]` lifts `X_cand` inside the proposal — no rule that reads the
/// candidate's reserve or spot can be protected within one transaction, and a band on `P_cand`
/// is satisfied by dumping only to `P_ema` — and `execute_pool_switch` never reads `X_cand` (a
/// withdrawal or a buy around it would otherwise cancel honest windows). What holds is the
/// execution's cap, `X₀ = min(L_slow', D·10⁴/λ_min)/√P_ema` with `L_slow' = min(L_slow, D)`: the
/// carried `q` is at most that, and at most `λ·√(P_cand/P_ema)` × a native open's cap on the
/// adopted pool (≈ 1.4× at the carried shorts' own ≈ 2× liquidation threshold). Knobs:
/// `FIT_R_BPS` (default 15,000), `FIT_Q_PERMILLE` (of the execution's cap; default the midpoint
/// of the two caps).
#[test]
fn fit1_a_dump_around_the_proposal_carries_at_most_the_executions_cap() {
    let r_bps: u128 = std::env::var("FIT_R_BPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15_000);
    let (mut env, rm) = incumbent();
    let lam_bps = 9_978u64;
    let cand = plant(
        &mut env,
        &rm.pool,
        Plant::Cpmm,
        true,
        (600_000, 1_000_000),
        lam_bps,
    );
    // The genuine pump of the candidate (the halt comes later; P_ema follows the incumbent only).
    let (mint, program) = cand.token();
    let (_, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
    let a = (y as f64 * (((r_bps as f64) / 10_000.0).sqrt() - 1.0) / 0.9975) as u64;
    let whale = fork::trader(&mut env, &mint, program, a + 1, 0);
    ok(fork::swap(
        &mut env,
        &cand,
        &whale,
        Direction::Buy,
        Swap::ExactIn(a, 0),
    ));
    let params = env.config().params;
    let d = d_of(&env, &cand);
    // The execution's cap: X₀ without X_cand (`FitAt::Execution`).
    let exec_cap = |env: &Env| {
        let m = env.market(&rm.market());
        let x0 = shortwin_math::caps::conservative_x0(
            u64::MAX,
            m.liq_slow.get().min(d),
            m.ema_price.get(),
            Some((d, params.pool_lock_min_bps)),
        )
        .unwrap();
        shortwin_math::caps::position_cap(x0, 500, 2_500, params.fee_size_cap_x100).unwrap()
    };
    let (lit, _) = carried_caps(&env, &cand);
    let ex = exec_cap(&env);
    let q = match std::env::var("FIT_Q_PERMILLE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Some(pm) => ex / 1_000 * pm,
        None => lit / 2 + ex / 2,
    };
    assert!(ex > lit, "the live X binds on a candidate above P_ema");
    let (b, nonce) = open_new(&mut env, &rm, q);
    crank_every(&mut env, &rm, 1, 3);
    arm(&mut env, &rm);
    let (lit, _) = carried_caps(&env, &cand);
    assert!(q > lit && q <= exec_cap(&env), "q between the two caps");
    // Honest: refused.
    assert_err(
        &env.propose(Some(&rm.pool), &cand),
        E::CarriedPositionsTooLarge,
    );
    // Dump-wrapped: accepted (the residual), and the execution adopts.
    let inc = candidate_read_set(&env.svm, &rm.pool, false);
    let prop = propose_pool_ix(&env.svm, &mint, &inc, &cand);
    ok(dump_wrapped(&mut env, &cand, prop));
    crank_every(&mut env, &rm, 30, 2);
    let e = execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    assert_eq!(e.candidate, pool_key(&cand));
    let m = env.market(&rm.market());
    let pos = env.position(&rm, &b.key(), nonce);
    let (cap_now, x0_now) = literal_open_cap(&env, &rm, &cand);
    let (x_now, y_now) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
    let r = (y_now as f64 / x_now as f64) / (m.ema_price.get() as f64 / 1e18);
    let bound = (lam_bps as f64 / 10_000.0) * r.sqrt();
    println!(
        "FIT-1 r {:.4}: literal cap {lit}, execution cap {} ({:.3}×); adopted q {} ({:.3} % of X \
         {x_now}); a native open's cap now {cap_now} (X₀ {x0_now}): q/cap {:.3} ≤ λ·√r = {bound:.3}",
        r,
        ex,
        ex as f64 / lit as f64,
        pos.q,
        pos.q as f64 * 100.0 / x_now as f64,
        pos.q as f64 / cap_now as f64,
    );
    assert!(pos.q <= exec_cap(&env), "the execution's cap holds");
    assert!(pos.q > cap_now, "the residual: above a native open's cap");
    assert!(
        pos.q as f64 <= cap_now as f64 * bound * 1.01,
        "q/cap {} above λ·√(P_cand/P_ema) = {bound}",
        pos.q as f64 / cap_now as f64
    );
}

/// **FIT-3, a disclosed residual** (ECON §6.1, §12.12; ARCH §2.6 rule 3; the owner-accepted
/// fallback). `max_q_hwm` falls only when `open_positions` returns to 0, so a position opened large
/// and closed while others stay open keeps the mark above them: three honest positions of 0.6 `c`
/// (each, and their OI, fit the 40 % candidate — without the attacker its proposal opens the
/// window); the attacker opens one position at the incumbent's size cap before the halt and closes
/// it the next second, for about the open fee. Once the incumbent halts, every proposal of the
/// candidate is refused `CarriedPositionsTooLarge` while the honest positions stay open, and no
/// window opens. The positions stay on the failed pool and exit through the S3 chain at fair
/// value: each settles S3 exactly at `max(deadline, no_route_since + s3_wait_s, graduated_at +
/// grad_grace_s)` with its locked timers (here the deadline, 900 s after the halt; not a second
/// before), the lenders paid `FV(P_ema, L_ema)` in full.
#[test]
fn fit3_a_closed_large_position_keeps_the_switch_refused_and_the_positions_settle_in_s3() {
    use shortwin::events::SolSettle;
    use shortwin_litesvm_tests::engine::sol_settle_ix;
    for attack in [false, true] {
        let (mut env, rm) = incumbent();
        let cand = candidate(&mut env, &rm, Plant::Cpmm, 400_000);
        let (c, oi) = carried_caps(&env, &cand);
        let honest: Vec<(Borrower, u64)> = (0..3)
            .map(|_| open_new(&mut env, &rm, c / 10 * 6))
            .collect();
        crank_every(&mut env, &rm, 1, 2);
        let market = rm.market();
        if attack {
            let mut a = env.borrower();
            let before = lamports(&env, &a.key());
            // The largest q the incumbent lets it open (bisected down from 1 % of X).
            let x = x_of(&env, &rm.pool);
            let mut q = x / 100;
            let nonce = loop {
                let (res, nonce) = env.open(&rm, &mut a, q);
                if res.is_ok() {
                    break nonce;
                }
                q = q / 100 * 97;
            };
            crank_every(&mut env, &rm, 1, 1);
            close_ok(&mut env, &rm, &a, nonce);
            let m = env.market(&market);
            println!(
                "FIT-3 attacker: opened q {q} ({:.2} c, {:.3} % of the incumbent's X) and closed \
                 it 1 s later for {:.6} SOL; mark {} ({:.2} c), {} honest positions open",
                q as f64 / c as f64,
                q as f64 * 100.0 / x as f64,
                before.saturating_sub(lamports(&env, &a.key())) as f64 / 1e9,
                m.max_q_hwm,
                m.max_q_hwm as f64 / c as f64,
                m.open_positions
            );
            assert!(m.max_q_hwm > c, "the mark pinned above the candidate's cap");
            assert_eq!(m.open_positions, 3);
        }
        let m = env.market(&market);
        println!(
            "FIT-3 attack {attack}: ΣQ {:.2} c (OI cap {:.2} c), mark {:.2} c",
            m.borrowed_q as f64 / c as f64,
            oi as f64 / c as f64,
            m.max_q_hwm as f64 / c as f64
        );
        if !attack {
            // The control: the same honest positions and candidate open the window.
            crank_every(&mut env, &rm, 1, 2);
            arm(&mut env, &rm);
            window_ok(&mut env, &rm.pool, &cand);
            continue;
        }
        // Idle until 900 s before the honest positions' deadline, then the incumbent halts.
        let positions: Vec<_> = honest
            .iter()
            .map(|(b, n)| env.position(&rm, &b.key(), *n))
            .collect();
        let deadline = positions.iter().map(|p| p.deadline).min().unwrap();
        let wait = deadline - 900 - env.now();
        env.warp(wait);
        env.crank(&rm);
        crank_every(&mut env, &rm, 1, 3);
        halt(&mut env, &rm.pool);
        let halted_at = env.now();
        let keeper = env.funded_keypair();
        let mut refused = 0u32;
        let mut settled: Vec<Option<(i64, SolSettle)>> = (0..honest.len()).map(|_| None).collect();
        for s in 1..=1_200i64 {
            env.warp(1);
            if s % 30 == 0 {
                env.crank(&rm);
            }
            if s % 150 == 0 && settled.iter().all(Option::is_none) {
                // The fast path is armed from +150 s: the candidate is refused, every time.
                assert_err(
                    &env.propose(Some(&rm.pool), &cand),
                    E::CarriedPositionsTooLarge,
                );
                refused += 1;
            }
            let m = env.market(&market);
            assert_eq!(m.fp_t0, 0, "+{s} s: no window");
            assert_eq!(
                pool_now(&env, &rm),
                pool_key(&rm.pool),
                "+{s} s: not adopted"
            );
            for (i, ((b, nonce), pos)) in honest.iter().zip(&positions).enumerate() {
                if settled[i].is_some() || m.no_route_since == 0 {
                    continue;
                }
                let due = pos
                    .deadline
                    .max(m.no_route_since + i64::from(pos.s3_wait_s))
                    .max(m.graduated_at + i64::from(pos.grad_grace_s));
                if env.now() < due - 1 {
                    continue;
                }
                let ix = sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), *nonce);
                let meta = ok(env.send(&[ix], &[&keeper]));
                let ev = events::of::<SolSettle>(&meta).into_iter().next();
                if env.now() < due {
                    assert!(ev.is_none(), "+{s} s: settled before S3 was due ({due})");
                    continue;
                }
                let ev = ev.unwrap_or_else(|| panic!("+{s} s: no S3 at its due second {due}"));
                assert_eq!(env.now(), due);
                settled[i] = Some((s, ev));
            }
            if settled.iter().all(Option::is_some) {
                break;
            }
        }
        let p_ema = env.market(&market).ema_price.get();
        for (i, (st, pos)) in settled.iter().zip(&positions).enumerate() {
            let (s, e) = st
                .as_ref()
                .expect("an S3 settlement of every honest position");
            let owed = u128::from(pos.q) * p_ema / shortwin_math::num::WAD;
            println!(
                "FIT-3 residual, position {i}: {refused} proposals refused; S{} at +{s} s after \
                 the halt (deadline +{} s): lenders {:.4} SOL (FV {:.4}; q at P_ema {:.4}), \
                 borrower {:.4} SOL",
                e.condition,
                pos.deadline - halted_at,
                e.lender_amount as f64 / 1e9,
                e.fv as f64 / 1e9,
                owed as f64 / 1e9,
                e.borrower_amount as f64 / 1e9
            );
            assert_eq!(e.condition, 3, "S3");
            assert!(
                e.fv > 0 && e.lender_amount == e.fv,
                "the lenders get FV in full"
            );
            assert!(u128::from(e.lender_amount) >= owed, "at least q at P_ema");
            assert!(e.borrower_amount > 0);
            assert_eq!(e.lender_amount + e.borrower_amount + e.k_s, pos.v);
        }
        assert!(refused >= 2);
        let m = env.market(&market);
        assert_eq!(
            (m.open_positions, m.max_q_hwm, m.min_open_fee_ppm),
            (0, 0, 0),
            "the marks reset at 0"
        );
    }
}
