//! Adoption without a failure (ARCH §2.6 rule 2, decision 7): only while the market has no
//! open position, with `pool_switch_margin_bps` (25 %) of locked depth over the incumbent;
//! `PositionsOpen` otherwise. AMM v4 ↔ CPMM switches carry the price state over (rule 4) and the
//! market trades on the new pool.

use shortwin::events::POOL_KIND_ZERO_POSITION;
use shortwin::state::raydium_cache;
use shortwin_math::quote::reprice;

use crate::common::*;
use crate::support::*;

/// The 25 % margin, to the pool: 24 % more locked depth is refused, 26 % adopted; the
/// adoption keeps the age and `P_ema`, rewrites `L_ema` and every ring entry's liquidity to the
/// pool's anchor `⌊D·10⁴/λ_min⌋` and marks the post-change warm-up (M4R), restarts the listing
/// window, and the market trades on the new pool (a planted AMM v4 pool with WSOL as the pc) once
/// the window has run and the warm-up is over.
#[test]
fn the_switch_margin_after_the_window() {
    let (mut env, rm, _) = setup(FART);
    crank_every(&mut env, &rm, 1, 5);
    let lam = lambda_of(&env, &rm.pool);
    let d0 = env.market(&rm.market()).pool_d.get();
    let below = plant(&mut env, &rm.pool, Plant::V4, false, (124, 100), lam);
    let above = plant(&mut env, &rm.pool, Plant::V4, false, (126, 100), lam);
    assert!(d_of(&env, &below) * 10_000 < d0 * 12_500);
    assert!(d_of(&env, &above) * 10_000 >= d0 * 12_500);
    assert_err(&env.propose(Some(&rm.pool), &below), E::PoolNotDominant);
    let before = env.market(&rm.market());
    env.warp(1);
    let e = adopt_ok(&mut env, Some(&rm.pool), &above);
    assert_eq!((e.kind, e.d_inc), (POOL_KIND_ZERO_POSITION, d0));
    let m = env.market(&rm.market());
    let AnyPool::V4(p) = &above else {
        unreachable!()
    };
    assert_eq!(m.pool_cache[raydium_cache::RAY_POOL], p.pool);
    assert_eq!(m.graduated_at, before.graduated_at, "age kept");
    assert_eq!(m.pool_seen_at, env.now(), "opens wait the window again");
    assert_eq!(
        m.ema_price.get(),
        before.ema_price.get(),
        "P_ema carries over"
    );
    let lambda_min = u64::from(env.config().params.pool_lock_min_bps);
    assert_eq!(
        m.ema_liq.get(),
        shortwin_math::pool_select::depth_at_lambda(d_of(&env, &above), lambda_min),
        "the anchor (M4R)"
    );
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1), "marked");
    for r in m.ring.iter().take(usize::from(m.ring_count)) {
        assert_eq!(
            (r.l.get(), r.llo.get(), r.lhi.get()),
            (m.ema_liq.get(), m.ema_liq.get(), m.ema_liq.get())
        );
        assert_eq!(
            (r.x.get(), r.y.get()),
            reprice(r.l.get(), r.p.get()).unwrap()
        );
    }
    // Opens wait the new window; then the market trades on the new pool.
    let rm2 = rm.on_pool(&env.svm, above);
    let q = q_for(&env, &rm2);
    let mut b = env.borrower();
    assert_err(&env.open(&rm2, &mut b, q).0, E::PoolListWindow);
    env.warp(3_600 - raydium::PRICE_WARMUP_CRANKS);
    env.warm_up_price(&rm2);
    assert_eq!(env.market(&rm.market()).ema_warm, 0);
    let (_, nonce, _) = open_ok(&mut env, &rm2, &mut b, q);
    env.warp(2);
    env.crank(&rm2);
    close_ok(&mut env, &rm2, &b, nonce);
    // The old pool is no longer the market's: its ReadSet is refused.
    assert_err(
        &env.send(&[crank_price_ix(&rm)], &[]),
        E::VenueAccountMismatch,
    );
}

/// Decision 7: with a position open, no adoption without a failure — not even of a pool with
/// ten times the locked depth (`PositionsOpen`); once the last position closes, the same
/// proposal is adopted.
#[test]
fn open_positions_pin_the_pool() {
    let (mut env, rm, q) = setup(FART);
    let lam = lambda_of(&env, &rm.pool);
    let deep = plant(&mut env, &rm.pool, Plant::Cpmm, true, (10, 1), lam);
    let mut b = env.borrower();
    let (_, n1, _) = open_ok(&mut env, &rm, &mut b, q);
    let (_, n2, _) = open_ok(&mut env, &rm, &mut b, q / 2);
    env.warp(2);
    assert_err(&env.propose(Some(&rm.pool), &deep), E::PositionsOpen);
    close_ok(&mut env, &rm, &b, n1);
    env.warp(2);
    assert_err(&env.propose(Some(&rm.pool), &deep), E::PositionsOpen);
    close_ok(&mut env, &rm, &b, n2);
    env.warp(1);
    let e = adopt_ok(&mut env, Some(&rm.pool), &deep);
    assert_eq!(e.kind, POOL_KIND_ZERO_POSITION);
    assert_eq!(
        env.market(&rm.market()).venue_kind(),
        Some(VenueKind::RaydiumCpmm)
    );
}

/// AMM v4 → CPMM → AMM v4 on Fartcoin and CPMM → AMM v4 on USELESS: each switch changes the
/// venue kind (ARCH §2.6 rule 4), the SOL side follows the new pool, and positions opened on
/// each pool (after its window) close there.
#[test]
fn switches_between_the_programs() {
    for fixture in [FART, USELESS] {
        let (mut env, rm, _) = setup(fixture);
        let lam = lambda_of(&env, &rm.pool);
        let mut cur = rm;
        let kinds = if fixture == FART {
            [Plant::Cpmm, Plant::V4]
        } else {
            [Plant::V4, Plant::Cpmm]
        };
        for (i, kind) in kinds.into_iter().enumerate() {
            let sol_first = i % 2 == 0;
            let next = plant(&mut env, &cur.pool, kind, sol_first, (13, 10), lam);
            env.warp(1);
            adopt_ok(&mut env, Some(&cur.pool), &next);
            let want = match kind {
                Plant::V4 => VenueKind::RaydiumV4,
                Plant::Cpmm => VenueKind::RaydiumCpmm,
            };
            assert_eq!(env.market(&cur.market()).venue_kind(), Some(want));
            cur = cur.on_pool(&env.svm, next);
            // The listing window, the keeper's observations at its end (the warm-up, M4R).
            env.warp(3_601 - raydium::PRICE_WARMUP_CRANKS);
            env.warm_up_price(&cur);
            let q = q_for(&env, &cur);
            let mut b = env.borrower();
            let (_, nonce, _) = open_ok(&mut env, &cur, &mut b, q);
            env.warp(2);
            env.crank(&cur);
            close_ok(&mut env, &cur, &b, nonce);
        }
    }
}
