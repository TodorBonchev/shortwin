//! Shared scenario helpers of the pool-selection tests.

#![allow(dead_code)]

use shortwin::events::{PoolProposed, PoolSwitchCancelled, PoolSwitched};
use shortwin::state::Market;
use shortwin_litesvm_tests::pool_select::{self, PlantSpec, plant_cpmm, plant_v4, spec_like};
use shortwin_litesvm_tests::raydium::locked_depth;

use crate::common::*;

/// The planted pool kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plant {
    /// AMM v4, cloned from Fartcoin's main pool.
    V4,
    /// CPMM, cloned from USELESS (config 0, 0.25 %).
    Cpmm,
}

/// Plants a pool of `kind` for `base`'s token: `base`'s reserves scaled by `num/den` (its
/// price), WSOL first or not, locked share `lambda_bps` of `base`'s LP counter.
pub fn plant(
    env: &mut Env,
    base: &AnyPool,
    kind: Plant,
    sol_first: bool,
    (num, den): (u64, u64),
    lambda_bps: u64,
) -> AnyPool {
    let spec = spec_like(&env.svm, base, sol_first, lambda_bps).scaled(num, den);
    plant_spec(env, kind, spec)
}

/// Plants a pool of `kind` per `spec`.
pub fn plant_spec(env: &mut Env, kind: Plant, spec: PlantSpec) -> AnyPool {
    match kind {
        Plant::V4 => AnyPool::V4(plant_v4(env, V4Fixture::Fartcoin, spec)),
        Plant::Cpmm => AnyPool::Cpmm(plant_cpmm(env, CpmmFixture::Useless, spec)),
    }
}

/// `λ_bps` of a pool now.
pub fn lambda_of(env: &Env, pool: &AnyPool) -> u64 {
    let counter = raydium::lp_counter(&env.svm, pool);
    let supply = raydium::mint_supply(&env.svm, &raydium::lp_mint(pool));
    u64::from(shortwin_math::pool_select::lambda_bps(counter, supply))
}

/// `D` of a pool now.
pub fn d_of(env: &Env, pool: &AnyPool) -> u128 {
    locked_depth(&env.svm, pool)
}

/// The market of `pool`'s mint.
pub fn market_of(env: &Env, pool: &AnyPool) -> Market {
    env.market(&pda::market(&pool.token().0).0)
}

/// A list-only listing of `pool`'s mint (must succeed).
pub fn list_auto(env: &mut Env, pool: &AnyPool) {
    let (mint, program) = pool.token();
    ok(env.list_auto(&mint, program));
}

/// A proposal that must succeed with an adoption: the event.
pub fn adopt_ok(env: &mut Env, incumbent: Option<&AnyPool>, candidate: &AnyPool) -> PoolSwitched {
    let meta = ok(env.propose(incumbent, candidate));
    events::one(&meta)
}

/// A proposal that must succeed with a window opened or improved: the event.
pub fn window_ok(env: &mut Env, incumbent: &AnyPool, candidate: &AnyPool) -> PoolProposed {
    let meta = ok(env.propose(Some(incumbent), candidate));
    assert!(events::of::<PoolSwitched>(&meta).is_empty());
    events::one(&meta)
}

/// An execution that must succeed: `Ok(switched)` or `Err(cancelled)`.
pub fn execute_ok(
    env: &mut Env,
    incumbent: &AnyPool,
    candidate: &AnyPool,
) -> Result<PoolSwitched, PoolSwitchCancelled> {
    let meta = ok(env.execute_switch(incumbent, candidate));
    let mut switched = events::of::<PoolSwitched>(&meta).into_iter();
    let mut cancelled = events::of::<PoolSwitchCancelled>(&meta).into_iter();
    match (switched.next(), cancelled.next()) {
        (Some(s), None) => Ok(s),
        (None, Some(c)) => Err(c),
        (s, c) => panic!(
            "one of PoolSwitched / PoolSwitchCancelled expected: switched {}, cancelled {}",
            s.is_some(),
            c.is_some()
        ),
    }
}

/// `q` capped at 90 % of the per-position cap for the market of `cand`'s mint to carry its
/// positions onto `cand` (switch only if all fit, owner decision of 2026-09-29;
/// `pool_select::carried_caps` on the market as stored): the size of a position a test moves onto
/// `cand` through a collection window.
pub fn fit_q(env: &Env, cand: &AnyPool, q: u64) -> u64 {
    q.min(pool_select::carried_caps(env, cand).0 / 10 * 9)
}

/// Halts `pool` (AMM v4 status 2 = disabled; CPMM swap bit).
pub fn halt(env: &mut Env, pool: &AnyPool) {
    match pool {
        AnyPool::V4(p) => fork::set_v4_status(&mut env.svm, p, 2),
        AnyPool::Cpmm(p) => fork::set_cpmm_status(&mut env.svm, p, 4),
    }
}

/// Resumes a halted `pool` (AMM v4 status 6; CPMM status 0).
pub fn resume(env: &mut Env, pool: &AnyPool) {
    match pool {
        AnyPool::V4(p) => fork::set_v4_status(&mut env.svm, p, 6),
        AnyPool::Cpmm(p) => fork::set_cpmm_status(&mut env.svm, p, 0),
    }
}

/// `n` cranks of `rm`, `step` seconds apart (the first after `step`).
pub fn crank_every(env: &mut Env, rm: &RayMarket, step: i64, n: u32) {
    for _ in 0..n {
        env.warp(step);
        env.crank(rm);
    }
}

/// The market's pool now.
pub fn pool_now(env: &Env, rm: &RayMarket) -> Pubkey {
    pool_select::cached_pool(&env.svm, &rm.market())
}

/// The lender stake and default size of a market adopted onto `pool`: `(RayMarket, q)` after
/// the listing window (the lender deposits `40·q`).
pub fn adopted_market(env: &mut Env, pool: AnyPool) -> (RayMarket, u64) {
    let q = q_of(env, &pool);
    let fixture = pool_select::fixture_of(&pool);
    let rm = env.ray_market_listed(RaySpec::new(fixture, q.saturating_mul(40)), pool);
    env.crank(&rm);
    (rm, q)
}
