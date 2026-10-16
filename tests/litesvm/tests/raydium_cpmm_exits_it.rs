//! `raydium_v4_support`'s keeper-exit differential oracle on Raydium **CPMM** (M10, the fuzz
//! follow-ups; ARCH §3.6, §3.7, §8.3 I9 / I11 / I12 / I-E11): the oracle is venue-generic since
//! then — the hooks' venue view prices a CPMM pool with each direction's fees and its creator-fee
//! side, the route flag is the market's own venue bit, the fee view of a pool that cannot execute
//! is CPMM's — and the fuzzer runs every keeper `liquidate` / `expire` on a CPMM market through it
//! (`tests/fuzz`, `…:cpmm-differential-…`). Here it is held to the dumped CPMM ELF (the 2026-10
//! redeploy) on every fixture pool: USELESS (SPL), the Token-2022 pool, the creator-fee pool with
//! its creator fee on the SOL side (mainnet's) and moved to the token side (U11's exact out).
//!
//! | Test | Checked |
//! |---|---|
//! | `exits_on_every_cpmm_pool` | per pool: a healthy position's `liquidate` / `expire` refused `NotLiquidatable` / `NotExpired`, as predicted; a whale's real pump (×1.8 on Tier A, ×2.15 on Tier B), then keeper seconds until the liquidation executes — each attempt `exit_alone`-checked (the decision, every event field, lamport and token, the lender pool after `pool::repay`); the price back, a new position at its deadline: `liquidate` refused `UseExpire` and the expiry executed, checked the same way |
//! | `the_oracle_sees_the_creator_fee_side` | on the creator-fee pool with its creator fee on the token side, the oracle's `B̂_live` is the venue's charge to the lamport, and a view with the side flipped (the fee on the SOL input) prices it differently: a fee view that ignored U11 would fail `exit_checked` |
//!
//! Every instruction goes through `Env::send` (the invariant hooks after every instruction).

// `TxResult` is LiteSVM's own result type (see the harness's lib.rs).
#![allow(clippy::result_large_err)]

#[path = "raydium_v4_support.rs"]
mod support;

use shortwin_litesvm_tests::fork::{CpmmFixture, Swap};
use shortwin_math::fees::Direction;
use shortwin_math::quote::Pricing;
use shortwin_venue_state::raydium_cpmm;
use support::*;

const USELESS: RayFixture = RayFixture::Cpmm(CpmmFixture::Useless);
const T22: RayFixture = RayFixture::Cpmm(CpmmFixture::Token2022);
const CREATOR_FEE: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorFee);

/// A listed CPMM market past its listing window and price warm-up, the oracle fresh, and the
/// size `q` of a 2-SOL position. `creator_on_token` moves the pool's creator fee to the token
/// side first (U11; the creator-fee pool, whose SOL is token 0).
fn market(fixture: RayFixture, creator_on_token: bool) -> (Env, RayMarket, u64) {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(fixture);
    let AnyPool::Cpmm(p) = pool else {
        unreachable!()
    };
    if creator_on_token {
        assert!(p.sol_is_token0);
        fork::set_cpmm_creator_fee(&mut env.svm, &p, true, raydium_cpmm::CREATOR_FEE_ON_TOKEN_1);
    }
    let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let q = (u128::from(x) * 2_000_000_000 / u128::from(y)) as u64;
    ok(env.list_ray(&pool, fixture.hint()));
    let rm = env.ray_market_listed(
        RaySpec {
            fixture,
            lend: q * 40,
            warmup: 3_601,
        },
        pool,
    );
    env.fresh_oracle();
    env.crank(&rm);
    (env, rm, q)
}

/// The pump that makes a fresh position liquidatable with its buyback still allowed: Tier A
/// (USELESS, Token-2022) liquidates above ≈ 1.6×, Tier B (the creator-fee pool) above ≈ 1.85×
/// (`raydium_cpmm_e2e`'s journeys).
fn pump_for(fixture: RayFixture) -> f64 {
    if fixture == CREATOR_FEE { 2.15 } else { 1.8 }
}

/// A real swap by `w` moving the spot by about `mult` (a buy of `Y·(√m − 1)` lamports, or a sale
/// of `X·(1/√m − 1)` tokens), its source topped up first.
fn move_price(env: &mut Env, rm: &RayMarket, w: &Trader, mult: f64) {
    let (x, y) = rm.reserves(&env.svm).expect("reserves");
    let r = mult.sqrt();
    let (dir, amount) = if mult > 1.0 {
        let amount = (y as f64 * (r - 1.0)) as u64;
        let have = tokens(env, &w.wsol);
        if have < amount {
            fork::wrap(env, &w.wsol, amount - have);
        }
        (Direction::Buy, amount)
    } else {
        let amount = (x as f64 * (1.0 / r - 1.0)) as u64;
        let have = tokens(env, &w.token);
        fork::set_token_amount(&mut env.svm, &w.token, have + amount);
        (Direction::Sell, amount)
    };
    ok(fork::swap(env, &rm.pool, w, dir, Swap::ExactIn(amount, 0)));
}

/// Keeper seconds (a crank each) until the checked `which` of `(b, nonce)` executes; every
/// refusal on the way is the oracle's predicted code.
fn checked_until(
    env: &mut Env,
    rm: &RayMarket,
    which: Which,
    keeper: &Keypair,
    b: &Pubkey,
    nonce: u64,
    max_s: u32,
) -> (Exit, Vec<E>) {
    let mut refused = Vec::new();
    for _ in 0..max_s {
        env.warp(1);
        env.crank(rm);
        let e = exit_alone(env, rm, which, keeper, b, nonce);
        if e.executed() {
            return (e, refused);
        }
        refused.push(e.refused());
    }
    panic!("{which:?} never executed; refusals {refused:?}");
}

fn journey(fixture: RayFixture, creator_on_token: bool) {
    let label = format!("{fixture:?} (creator fee on the token side: {creator_on_token})");
    let (mut env, rm, q) = market(fixture, creator_on_token);
    let keeper = env.funded_keypair();
    let mut b1 = env.borrower();
    let (res, n1) = env.open(&rm, &mut b1, q);
    ok(res);

    // A healthy position: both keeper exits refused, as predicted.
    env.warp(1);
    env.crank(&rm);
    let e = exit_alone(&mut env, &rm, Which::Liquidate, &keeper, &b1.key(), n1);
    assert_eq!(e.refused(), E::NotLiquidatable, "{label}");
    let e = exit_alone(&mut env, &rm, Which::Expire, &keeper, &b1.key(), n1);
    assert_eq!(e.refused(), E::NotExpired, "{label}");

    // A whale's real pump: keeper seconds until the liquidation executes, every attempt checked.
    let (mint, program) = rm.pool.token();
    let w = fork::trader(&mut env, &mint, program, 0, 0);
    let p0 = spot(&env, &rm);
    move_price(&mut env, &rm, &w, pump_for(fixture));
    let (e, refused) = checked_until(&mut env, &rm, Which::Liquidate, &keeper, &b1.key(), n1, 180);
    let l = e.liquidated();
    assert!(l.buyback_cost > 0 && !l.expired, "{label}");
    println!(
        "{label}: liquidated after {} checked refusals {refused:?}: B {} rent {} penalty {}",
        refused.len(),
        l.buyback_cost,
        l.rent,
        l.penalty
    );

    // The price back, a new position at its deadline: `UseExpire`, then the checked expiry.
    let back = p0 as f64 / spot(&env, &rm) as f64;
    move_price(&mut env, &rm, &w, back);
    for _ in 0..300 {
        env.warp(1);
        env.crank(&rm);
        let (s, e) = (spot(&env, &rm), env.market(&rm.market()).ema_price.get());
        if s.abs_diff(e) * 10_000 <= e * 10 {
            break;
        }
    }
    env.fresh_oracle();
    let mut b2 = env.borrower();
    let (res, n2) = env.open(&rm, &mut b2, q);
    ok(res);
    let deadline = env.position(&rm, &b2.key(), n2).deadline;
    let slot = env.clock().slot;
    env.set_clock(deadline + 1, slot + 6_480_000);
    env.tick(&rm, 3);
    let e = exit_alone(&mut env, &rm, Which::Liquidate, &keeper, &b2.key(), n2);
    assert_eq!(e.refused(), E::UseExpire, "{label}");
    let (e, refused) = checked_until(&mut env, &rm, Which::Expire, &keeper, &b2.key(), n2, 30);
    let x = e.expired();
    assert!(x.expiry_fee > 0 && x.keeper_reward > 0, "{label}");
    println!(
        "{label}: expired after {} checked refusals: B {} rent {} fee {}",
        refused.len(),
        x.buyback_cost,
        x.rent,
        x.expiry_fee
    );
    assert_eq!(env.market(&rm.market()).open_positions, 0);
}

#[test]
fn exits_on_every_cpmm_pool() {
    journey(USELESS, false);
    journey(T22, false);
    journey(CREATOR_FEE, false);
    journey(CREATOR_FEE, true);
}

#[test]
fn the_oracle_sees_the_creator_fee_side() {
    let (mut env, rm, q) = market(CREATOR_FEE, true);
    let keeper = env.funded_keypair();
    let mut b = env.borrower();
    let (res, n) = env.open(&rm, &mut b, q);
    ok(res);
    let (mint, program) = rm.pool.token();
    let w = fork::trader(&mut env, &mint, program, 0, 0);
    move_price(&mut env, &rm, &w, pump_for(CREATOR_FEE));
    // Keeper seconds until the oracle predicts the liquidation (refusals checked on the way).
    let mut v = None;
    for _ in 0..180 {
        env.warp(1);
        env.crank(&rm);
        let cur = view(&mut env, &rm, &b.key(), n);
        if predict(&cur).decision(Which::Liquidate).is_ok() {
            v = Some(cur);
            break;
        }
        exit_alone(&mut env, &rm, Which::Liquidate, &keeper, &b.key(), n);
    }
    let v = v.expect("liquidatable within 180 s");
    let honest = predict(&v).decision(Which::Liquidate).expect("executes");
    let Pricing::Cpmm { buy, sell } = v.m.live else {
        panic!("a CPMM view")
    };
    assert!(
        buy.creator > 0 && !buy.creator_on_input,
        "U11: on the output"
    );
    let mut flipped = v.clone();
    let mut wrong = buy;
    wrong.creator_on_input = true;
    flipped.m.live = Pricing::Cpmm { buy: wrong, sell };
    let off = predict(&flipped)
        .decision(Which::Liquidate)
        .map(|p| p.b_live);
    assert_ne!(off, Ok(honest.b_live), "the side moves B̂_live");
    let e = exit_alone(&mut env, &rm, Which::Liquidate, &keeper, &b.key(), n);
    assert_eq!(e.liquidated().buyback_cost, honest.b_live);
    println!(
        "U11: the venue charged {} (the oracle's B̂_live); with the creator fee on the input {:?}",
        honest.b_live, off
    );
}
