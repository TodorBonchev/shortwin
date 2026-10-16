//! The fast path (ARCH §2.6 rule 3, §3.9; A71, A72; V1-7, V2-1, V2-7, R3A-4, R3B-8; ECON §10.3;
//! T-RAY-SWITCH, T-VENUE-SWITCH, I15): the `pool_fail` chain, the 120 s arming, the 60 s
//! collection window keeping the deepest candidate, the execution with open positions, and the
//! cancellations.

use shortwin::PauseScope;
use shortwin::events::{POOL_KIND_FAST, PoolProposed};
use shortwin::state::ShortPosition;
use shortwin_litesvm_tests::engine::set_route_broken_ix;
use shortwin_math::observe::ring_medians;

use crate::common::*;
use crate::support::*;

/// A Fartcoin market with two open positions whose pool is then halted.
fn halted_with_positions() -> (Env, RayMarket, Borrower, Vec<u64>) {
    let (mut env, rm, q) = setup(FART);
    crank_every(&mut env, &rm, 1, 5);
    let mut b = env.borrower();
    let (_, n1, _) = open_ok(&mut env, &rm, &mut b, q);
    let (_, n2, _) = open_ok(&mut env, &rm, &mut b, q / 3);
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    (env, rm, b, vec![n1, n2])
}

/// The positions of `b` on `rm` (by nonce).
fn positions(env: &Env, rm: &RayMarket, b: &Borrower, nonces: &[u64]) -> Vec<ShortPosition> {
    nonces
        .iter()
        .map(|n| env.position(rm, &b.key(), *n))
        .collect()
}

/// T-RAY-SWITCH / T-VENUE-SWITCH end to end, with open positions (ARCH §9 M4: "a fast path
/// with open positions keeps locked terms and EMA continuity"):
///
/// 1. The incumbent halts (AMM v4 status 2): every crank observes `pool_fail`; the chain starts
///    at the first observation and runs continuously (cranks ≤ 60 s apart).
/// 2. Before `pool_fast_route_s` a proposal is refused (`PositionsOpen`: no failure yet).
/// 3. At 120 s the first qualifying proposal (40 % of `pool_d`) opens the collection window;
///    a shallower one is refused, a deeper one (60 %) replaces it, a slightly shallower one
///    after it is refused again; a candidate below `pool_fast_min_bps` could not have opened it.
/// 4. The execution waits for the window (`PoolSwitchNotDue`), then adopts the **deepest**
///    candidate seen (not the first, V2-7), a CPMM pool: at the adoption instruction `P_ema`,
///    `ema_ts`, `ema_count` and every ring entry's price are unchanged (continuity), every
///    entry's liquidity is the reset `L_ema` — the new pool's anchor `⌊D·10⁴/λ_min⌋`, no sample
///    (M4R) — `L_slow ≤` its locked depth `D`, the state is marked (`ema_warm = 8`, `ema_guard =
///    1`), the positions are byte for byte what they were (A26). After the warm-up (8
///    integrations) `P_ema` is re-seeded from the new pool (its spot), `L_ema` is its depth,
///    the guard is clear and the settlement box is ≤ 1 % wide in `L` from then on.
/// 5. Both positions close on the new pool.
#[test]
fn the_fast_path_with_open_positions() {
    let (mut env, rm, b, nonces) = halted_with_positions();
    let lam = lambda_of(&env, &rm.pool);
    let d0 = env.market(&rm.market()).pool_d.get();
    let first = plant(&mut env, &rm.pool, Plant::V4, true, (40, 100), lam);
    let shallow = plant(&mut env, &rm.pool, Plant::V4, false, (35, 100), lam);
    let best = plant(&mut env, &rm.pool, Plant::Cpmm, true, (60, 100), lam);
    let almost = plant(&mut env, &rm.pool, Plant::Cpmm, false, (59, 100), lam);
    let tiny = plant(&mut env, &rm.pool, Plant::V4, true, (24, 100), lam);
    // 1–2. The chain, and no adoption before it has run for 120 s.
    env.warp(1);
    env.crank(&rm);
    let m = env.market(&rm.market());
    let since = m.pool_fail_since;
    assert_eq!(
        since,
        env.now(),
        "the chain starts at the first failing observation"
    );
    crank_every(&mut env, &rm, 30, 3);
    assert_eq!(env.market(&rm.market()).pool_fail_since, since);
    env.warp(29);
    assert_err(&env.propose(Some(&rm.pool), &first), E::PositionsOpen);
    env.warp(1);
    // 3. The window: opened by a qualifying proposal, kept by the deepest.
    assert_eq!(env.now() - since, 120);
    assert_err(&env.propose(Some(&rm.pool), &tiny), E::PoolNotDominant);
    let e: PoolProposed = window_ok(&mut env, &rm.pool, &first);
    let window_s = i64::from(env.config().params.pool_fast_window_s);
    let t0 = env.now();
    assert_eq!(
        (e.kind, e.candidate, e.d_cand, e.d_inc, e.window_ends),
        (
            POOL_KIND_FAST,
            pool_select_key(&first),
            d_of(&env, &first),
            d0,
            t0 + window_s
        )
    );
    let m = env.market(&rm.market());
    assert_eq!(
        (m.fp_t0, m.fp_d.get(), m.fp_venue),
        (t0, e.d_cand, VenueKind::RaydiumV4 as u8)
    );
    env.warp(10);
    assert_err(&env.propose(Some(&rm.pool), &shallow), E::PoolNotDominant);
    env.warp(10);
    let e = window_ok(&mut env, &rm.pool, &best);
    assert_eq!(
        (e.candidate, e.window_ends),
        (pool_select_key(&best), t0 + window_s),
        "not extended"
    );
    env.warp(10);
    assert_err(&env.propose(Some(&rm.pool), &almost), E::PoolNotDominant);
    // 4. The execution.
    env.warp(29);
    assert_err(&env.execute_switch(&rm.pool, &best), E::PoolSwitchNotDue);
    env.warp(1);
    let before = env.market(&rm.market());
    let pos_before = positions(&env, &rm, &b, &nonces);
    let ev = execute_ok(&mut env, &rm.pool, &best).unwrap_or_else(|_| panic!("cancelled"));
    assert_eq!(
        (ev.kind, ev.candidate, ev.d_inc),
        (POOL_KIND_FAST, pool_select_key(&best), d0)
    );
    let m = env.market(&rm.market());
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumCpmm));
    assert_eq!(
        (m.ema_price.get(), m.ema_ts, m.ema_count),
        (before.ema_price.get(), before.ema_ts, before.ema_count),
        "EMA continuity"
    );
    let live = {
        let (x, y) = best
            .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
            .unwrap();
        (u128::from(x) * u128::from(y)).isqrt()
    };
    let d_best = d_of(&env, &best);
    let lambda_min = u64::from(env.config().params.pool_lock_min_bps);
    let l_new = shortwin_math::pool_select::depth_at_lambda(d_best, lambda_min);
    assert!(
        l_new >= live,
        "the anchor {l_new} covers √(X·Y) {live} (λ ≥ λ_min)"
    );
    assert_eq!(
        (m.ema_liq.get(), m.liq_slow.get()),
        (l_new, d_best.min(before.liq_slow.get())),
        "L_ema the anchor, L_slow at most the locked depth (M4R)"
    );
    assert_eq!(
        (m.ema_warm, m.ema_guard, m._pad1a),
        (8, 1, [0, 0]),
        "marked"
    );
    for (r, r0) in m
        .ring
        .iter()
        .zip(before.ring.iter())
        .take(usize::from(m.ring_count))
    {
        assert_eq!(
            (r.ts, r.p.get(), r.lo.get(), r.hi.get()),
            (r0.ts, r0.p.get(), r0.lo.get(), r0.hi.get())
        );
        assert_eq!((r.l.get(), r.llo.get(), r.lhi.get()), (l_new, l_new, l_new));
    }
    assert_eq!((m.pool_fail_since, m.no_route_since, m.fp_t0), (0, 0, 0));
    assert_eq!(m.graduated_at, before.graduated_at);
    assert_eq!(m.pool_seen_at, env.now(), "opens wait the new window");
    assert_eq!(
        positions(&env, &rm, &b, &nonces),
        pos_before,
        "locked terms (A26)"
    );
    // The warm-up: 9 per-second cranks integrate 8 buckets of the new pool.
    let rm2 = rm.on_pool(&env.svm, best);
    let spot = {
        let (x, y) = best
            .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
            .unwrap();
        u128::from(y) * shortwin_math::num::WAD / u128::from(x)
    };
    crank_every(&mut env, &rm2, 1, 9);
    let m = env.market(&rm.market());
    assert_eq!((m.ema_warm, m.ema_guard), (0, 0), "confirmed at the gate");
    assert_eq!(
        m.ema_price.get(),
        spot,
        "P_ema re-seeded at the new pool's price"
    );
    assert!(
        m.ema_liq.get() <= live + 2 && m.ema_liq.get() + 2 >= live,
        "L_ema {} vs √(X·Y) {live}",
        m.ema_liq.get()
    );
    // T-VENUE-SWITCH: the box is ≤ fv_dev_bps wide in L from the gate on.
    let fv_dev = u128::from(pos_before[0].fv_dev_bps);
    for s in 0..6 {
        let (_, _, l_lo, l_hi) = ring_medians(&env.market(&rm.market()).ring_math()).unwrap();
        assert!(
            (l_hi - l_lo) * 10_000 <= fv_dev * l_lo,
            "second {s}: box [{l_lo}, {l_hi}]"
        );
        env.warp(1);
        env.crank(&rm2);
    }
    // 5. The positions exit on the new pool.
    for n in &nonces {
        close_ok(&mut env, &rm2, &b, *n);
    }
    assert_eq!(env.market(&rm.market()).open_positions, 0);
}

fn pool_select_key(p: &AnyPool) -> Pubkey {
    shortwin_litesvm_tests::pool_select::pool_key(p)
}

/// A fee above the global hard cap is `pool_fail` evidence too (the pool still trades): the
/// fast path adopts after 120 s + 60 s. The Emergency route-broken flag alone never arms it
/// (V1-7, R3B-8, T-S3-BROKEN): with the flag set on a healthy pool the `pool_fail` chain stays
/// empty for 10 minutes of cranks and a proposal is refused (`PositionsOpen`), while the S3
/// chain runs.
#[test]
fn pool_fail_is_venue_evidence_only() {
    // The flag.
    let (mut env, rm, q) = setup(FART);
    let mut b = env.borrower();
    open_ok(&mut env, &rm, &mut b, q);
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Market,
            0,
            true,
            Some(rm.market()),
        )],
        &[&ea],
    ));
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    crank_every(&mut env, &rm, 30, 20);
    let m = env.market(&rm.market());
    assert_eq!(
        (m.pool_fail_since, m.last_pool_fail_obs),
        (0, 0),
        "the flag is not pool_fail"
    );
    assert!(m.no_route_since > 0, "the S3 chain runs on the flag");
    assert_err(&env.propose(Some(&rm.pool), &cand), E::PositionsOpen);

    // A fee above the hard cap (6 % > 5 %).
    let (mut env, rm, q) = setup(FART);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    let AnyPool::V4(p) = &rm.pool else {
        unreachable!()
    };
    fork::set_v4_swap_fee(&mut env.svm, p, 600, 10_000);
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (50, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    assert!(env.market(&rm.market()).pool_fail_since > 0);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    let ev = execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    assert_eq!(ev.kind, POOL_KIND_FAST);
    let rm2 = rm.on_pool(&env.svm, cand);
    env.warp(1);
    env.crank(&rm2);
    close_ok(&mut env, &rm2, &b, nonce);
}

/// The chain restarts when observations are more than `S3_OBS_GAP_S` apart (ECON §10.3): cranks
/// every 61 s never arm the fast path.
#[test]
fn a_broken_chain_never_arms() {
    let (mut env, rm, _b, _) = halted_with_positions();
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    for _ in 0..5 {
        env.warp(61);
        env.crank(&rm);
        assert_eq!(env.market(&rm.market()).pool_fail_since, env.now());
    }
    env.warp(59);
    assert_err(&env.propose(Some(&rm.pool), &cand), E::PositionsOpen);
}

/// The execution cancels (`PoolSwitchCancelled`, window cleared, nothing adopted) when the
/// incumbent recovered, or when the best candidate is no longer eligible (every LP token of it
/// redeemable, λ = 0 so its locked depth is 0 and under the floor, or halted); a proposal after
/// the window ran out waits for the execution (`PoolWindowClosed`); without a window there is
/// nothing to execute (`NoPendingSwitch`). (Unburned LP that leaves `λ` merely under `λ_min`
/// no longer cancels: M4-SEC-1, `pool_attacks_it::lambda_games`.)
#[test]
fn cancellations() {
    type Spoil = fn(&mut Env, &AnyPool, &AnyPool);
    let recover: Spoil = |env, inc, _| resume(env, inc);
    let unlock: Spoil = |env, _, cand| {
        let counter = raydium::lp_counter(&env.svm, cand);
        raydium::set_mint_supply(&mut env.svm, &raydium::lp_mint(cand), counter);
    };
    let halt_cand: Spoil = |env, _, cand| halt(env, cand);
    for (what, spoil) in [
        ("recovered", recover),
        ("unlocked", unlock),
        ("halted", halt_cand),
    ] {
        let (mut env, rm, _b, _) = halted_with_positions();
        assert_err(&env.execute_switch(&rm.pool, &rm.pool), E::NoPendingSwitch);
        let lam = lambda_of(&env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
        let other = plant(&mut env, &rm.pool, Plant::V4, true, (70, 100), lam);
        crank_every(&mut env, &rm, 30, 5);
        window_ok(&mut env, &rm.pool, &cand);
        crank_every(&mut env, &rm, 30, 2);
        assert_err(&env.propose(Some(&rm.pool), &other), E::PoolWindowClosed);
        spoil(&mut env, &rm.pool, &cand);
        let c = execute_ok(&mut env, &rm.pool, &cand)
            .map(|_| ())
            .expect_err(what);
        assert_eq!(
            (c.kind, c.candidate),
            (POOL_KIND_FAST, pool_select_key(&cand)),
            "{what}"
        );
        let m = env.market(&rm.market());
        assert_eq!((m.fp_t0, m.fp_d.get()), (0, 0), "{what}");
        assert_eq!(
            m.pool_cache[0],
            pool_select_key(&rm.pool),
            "{what}: nothing adopted"
        );
        assert_err(&env.execute_switch(&rm.pool, &cand), E::NoPendingSwitch);
    }
}

/// The venue-upgrade latch (M3-SEC-2 extended to adoptions; `VenueUpgraded`), on the
/// candidate's own program only (M4-SEC-8): with the CPMM program flagged, a CPMM candidate is
/// refused while the halted AMM v4 incumbent's fast path collects AMM v4 candidates; once the AMM
/// v4 program is flagged as well, the window's AMM v4 candidate is **not adoptable now**: the
/// execution cancels the window instead of reverting (M4V-4) and the `pool_fail` chain stays
/// armed, so after `revalidate_venue` of AMM v4 alone — the CPMM review still pending — the same
/// candidate opens a new window at once and is adopted after it. Opens stay on the whole Raydium
/// mask (R4B-7).
#[test]
fn adoptions_wait_for_the_venue_review() {
    use shortwin::constants::{GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4};
    use shortwin_litesvm_tests::fork::{FIXTURE_GUARD_SLOTS, RaydiumProgram};
    use shortwin_litesvm_tests::ix;
    let (mut env, rm, _b, _) = halted_with_positions();
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    let cpmm = plant(&mut env, &rm.pool, Plant::Cpmm, true, (70, 100), lam);
    let deeper = plant(&mut env, &rm.pool, Plant::V4, true, (60, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    let flag = |env: &mut Env, guard: u8, program: RaydiumProgram| {
        let dumped = FIXTURE_GUARD_SLOTS[usize::from(guard)];
        fork::set_deploy_slot(&mut env.svm, program, dumped + 1);
        ok(env.send(&[ix::flag_venue_upgraded(guard)], &[]));
        dumped + 1
    };
    let cpmm_slot = flag(&mut env, GUARD_RAYDIUM_CPMM, RaydiumProgram::Cpmm);
    assert_err(&env.propose(Some(&rm.pool), &cpmm), E::VenueUpgraded);
    window_ok(&mut env, &rm.pool, &cand);
    env.warp(5);
    window_ok(&mut env, &rm.pool, &deeper);
    let v4_slot = flag(&mut env, GUARD_RAYDIUM_V4, RaydiumProgram::AmmV4);
    crank_every(&mut env, &rm, 30, 2);
    let c = execute_ok(&mut env, &rm.pool, &deeper)
        .map(|_| ())
        .expect_err("a latched candidate cancels");
    assert_eq!(
        (c.kind, c.candidate),
        (POOL_KIND_FAST, pool_select_key(&deeper))
    );
    let m = env.market(&rm.market());
    assert_eq!(m.fp_t0, 0, "the window is cleared, not kept");
    assert!(m.pool_fail_since != 0, "the chain stays armed");
    assert_err(&env.propose(Some(&rm.pool), &deeper), E::VenueUpgraded);
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[ix::revalidate_venue(
            &ea.pubkey(),
            GUARD_RAYDIUM_V4,
            v4_slot,
        )],
        &[&ea],
    ));
    window_ok(&mut env, &rm.pool, &deeper);
    crank_every(&mut env, &rm, 30, 2);
    let ev = execute_ok(&mut env, &rm.pool, &deeper).unwrap_or_else(|_| panic!("cancelled"));
    assert_eq!(
        (ev.kind, ev.candidate),
        (POOL_KIND_FAST, pool_select_key(&deeper))
    );
    assert!(
        env.venue_guard().upgraded & (1 << GUARD_RAYDIUM_CPMM) != 0,
        "adopted with the CPMM review still pending"
    );
    let _ = cpmm_slot;
}
