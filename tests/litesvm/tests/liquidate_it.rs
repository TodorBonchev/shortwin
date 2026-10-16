//! `liquidate` on the mock venue (ARCH §3.6; ECON §5.5, §8.2, §8.3, §8.5; SPEC §5.1–§5.2,
//! §6.3). Every exit here goes through the differential oracle of `liquidate_support.rs`: the
//! program's refusal, or every field of its `Liquidate` event, every lamport and token delta and
//! the lender pool after `pool::repay`, must equal what `shortwin-math` computes on the state
//! the instruction saw. The invariant hooks run after every instruction (`Env::send`).
//!
//! - every waterfall branch (ARCH §5.2): the full waterfall, the keeper floor topping up a small
//!   position (N22, A74), the floor truncated by the borrower's remainder, the penalty truncated
//!   by the vault, an underwater liquidation that truncates rent (the held-model disclosure,
//!   ECON §12.6, O19);
//! - expiry versus liquidation (ECON §8.4): expired and CR-liquidatable pays the penalty,
//!   expired and healthy is `UseExpire`;
//! - every refusal reachable on the mock, in ARCH §3.6's order: `NoRoute` (halt, layout
//!   change), `ReferenceStale`, `NotLiquidatable`, `UseSolSettle` (the S1 budget and the S2
//!   slippage), `UseExpire`, `ExitBoundNotMet`;
//! - what exits never read (I10): pauses, both route-broken flags, a disabled venue, the upgrade
//!   latch, a fee above the hard cap (the risk quotes cap it), an unsafe mint (reported);
//! - trader normalisation, venue over-delivery, the extras sets (T-REPLAY-PDA on the exit),
//!   the strict remaining accounts (I14) and named accounts, Token-2022, several positions of
//!   one wallet, a borrower keeping its own position's keeper legs;
//! - liveness (I11, I-E11) along a genuine rise, T-TRACE and CU, and the hooks' own mutants for
//!   the checks this track added (I11, I13).

#![allow(clippy::result_large_err, clippy::too_many_arguments)]

#[path = "liquidate_support.rs"]
mod support;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::AccountMeta;
use shortwin::PauseScope;
use shortwin::constants::GUARD_RAYDIUM_CPMM;
use shortwin::events::{Liquidate, MintSafetyTripped, TraderSwept};
use shortwin::state::{PLACEHOLDER_MINT, SEED_CONST, SEED_PLACEHOLDER};
use shortwin::venues::{EXTRAS_SET_CURRENT, EXTRAS_SET_PREVIOUS, ExtrasSel};
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_litesvm_tests::engine::{
    VENUE_HINT_MOCK, mock_extras_key, set_route_broken_ix, set_venue_extras_ix,
};
use shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS;
use shortwin_litesvm_tests::invariants::{self, IxEvents};
use shortwin_litesvm_tests::ix;
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_math::risk;
use solana_keypair::Keypair;
use solana_signer::Signer;
use support::*;

/// Default position size on the default pool (≈ 3 SOL of notional, Tier C).
const Q: u64 = 3_000_000_000_000;
/// A small position (≈ 0.12 SOL of notional): its penalty's keeper share is below `K_s`.
const Q_SMALL: u64 = 120_000_000_000;

/// A default market with one position of `q`, an actor able to move the pool a long way and a
/// keeper.
fn opened_with(spec: MarketSpec, q: u64) -> (Env, MockMarket, Borrower, u64, Actor, Keypair) {
    let mut env = Env::test_venue();
    let mm = market(&mut env, spec);
    let (b, nonce, _) = open(&mut env, &mm, q);
    env.tick(&mm, 2);
    let actor = env.actor(&mm, 400_000_000_000_000, 3_000_000_000_000);
    let k = keeper(&mut env);
    (env, mm, b, nonce, actor, k)
}

fn opened(q: u64) -> (Env, MockMarket, Borrower, u64, Actor, Keypair) {
    opened_with(MarketSpec::default(), q)
}

/// A genuine rise of `to`× over `secs` seconds, then attempts every second until the
/// liquidation executes.
fn rise_and_liquidate(
    env: &mut Env,
    mm: &MockMarket,
    actor: &Actor,
    k: &Keypair,
    b: &Borrower,
    nonce: u64,
    to: f64,
    secs: u32,
) -> (Exit, Vec<E>) {
    let mut refusals = Vec::new();
    let p0 = spot(env, mm) as f64;
    for s in 1..=secs {
        let m = to.powf(f64::from(s) / f64::from(secs));
        honest_second(env, mm, actor, Some((p0 * m) as u128));
        let ex = liquidate_checked(env, mm, k, &b.key(), nonce);
        if ex.executed() {
            return (ex, refusals);
        }
        refusals.push(ex.refused());
    }
    let (ex, more) = attempts(env, mm, Which::Liquidate, k, &b.key(), nonce, 60);
    refusals.extend(more);
    (ex.expect("liquidated"), refusals)
}

/// A gap to the price at which `B̂(q) ≈ cost`, held until the liquidation executes.
fn gap_to_cost(
    env: &mut Env,
    mm: &MockMarket,
    actor: &Actor,
    k: &Keypair,
    b: &Borrower,
    nonce: u64,
    q: u64,
    cost: u64,
) -> (Option<Exit>, Vec<E>) {
    let target = price_for_cost(env, mm, q, cost);
    gap(env, mm, actor, target);
    attempts(env, mm, Which::Liquidate, k, &b.key(), nonce, 60)
}

fn penalty_rate(pos: &shortwin::state::ShortPosition, base: u64) -> u64 {
    (u128::from(base) * u128::from(pos.penalty_bps)).div_ceil(10_000) as u64
}

// ---- waterfall branches ------------------------------------------------------------------

/// ARCH §5.2 `liquidate`, no truncation: `B` → rent (15 % protocol) → penalty 8 % of
/// `min(B̂_live,risk, B̂_ref)` split 50/20/30 (the lenders' half vests) → the borrower. The
/// penalty's keeper share exceeds `K_s`, so no floor top-up.
#[test]
fn full_waterfall_on_a_genuine_rise() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    let vault0 = env.market_vault(&mm.market()).vault();
    let (ex, refusals) = rise_and_liquidate(&mut env, &mm, &actor, &k, &b, nonce, 2.4, 30);
    let ev = ex.liquidated();
    let p = ex.predicted();
    let pos = &p.view.pos;
    assert!(
        refusals
            .iter()
            .all(|e| matches!(e, E::NotLiquidatable | E::ExitBoundNotMet)),
        "{refusals:?}"
    );
    assert!(!ev.expired);
    assert_eq!(ev.delta_bps, 200, "the fixed δ_liq (decision 1)");
    assert_eq!(ev.rent, p.r_due, "rent paid in full");
    let base = ev.b_live_risk.min(ev.b_ref);
    assert_eq!(ev.penalty, penalty_rate(pos, base), "penalty untruncated");
    assert_eq!(ev.penalty_keeper, ev.penalty * 3_000 / 10_000);
    assert_eq!(ev.penalty_protocol, ev.penalty * 2_000 / 10_000);
    assert_eq!(
        ev.penalty_lender,
        ev.penalty - ev.penalty_keeper - ev.penalty_protocol,
        "lenders take the rounding"
    );
    assert!(ev.penalty_keeper >= p.k_s);
    assert_eq!(ev.keeper_topup, 0, "no floor above the crossover vault");
    assert!(ev.borrower_payout > 0);
    assert!(u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * 10_200);
    assert!(ev.cr_spot_bps < u64::from(pos.cr_liq_bps));
    assert!(ev.cr_ema_bps < u64::from(pos.cr_liq_bps));
    // The lenders' penalty share vests; their rent share is immediate (the oracle checked the
    // pool state exactly).
    let vault1 = env.market_vault(&mm.market()).vault();
    let rent_protocol = ev.rent * 1_500 / 10_000;
    assert_eq!(
        vault1.lender_sol - vault0.lender_sol,
        ev.rent - rent_protocol + ev.penalty_lender
    );
    assert_eq!(
        vault1.protocol_fees - vault0.protocol_fees,
        rent_protocol + ev.penalty_protocol
    );
    let m = env.market(&mm.market());
    assert!(m.pool().vest_bal >= ev.penalty_lender);
    // The exposure record is closed with the position.
    assert!(read::account(&env.svm, &pda::exposure(&mm.market(), &b.key()).0).is_none());
    assert_eq!(m.open_positions, 0);
    assert_eq!(m.borrowed_q, 0);
}

/// N22 (A74, R3B-10): the penalty's keeper share of a small position is below `K_s`; the
/// borrower's remainder tops the keeper up to exactly `K_s` (I12: never less than settling).
#[test]
fn keeper_floor_tops_up_a_small_liquidation() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q_SMALL);
    let (ex, _) = rise_and_liquidate(&mut env, &mm, &actor, &k, &b, nonce, 2.4, 30);
    let ev = ex.liquidated();
    let k_s = ex.predicted().k_s;
    assert!(ev.penalty_keeper < k_s);
    assert_eq!(ev.keeper_topup, k_s - ev.penalty_keeper);
    assert_eq!(
        ev.penalty_keeper + ev.keeper_topup,
        k_s,
        "the keeper gets K_s"
    );
    assert!(ev.borrower_payout > 0, "paid from the borrower's remainder");
}

/// The floor is paid only from what the borrower has left: with a remainder below `K_s − kp`
/// the keeper gets the whole remainder and the borrower nothing; lenders and the protocol are
/// untouched by it.
#[test]
fn keeper_floor_is_limited_to_the_borrowers_remainder() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q_SMALL);
    let v = view(&mut env, &mm, &b.key(), nonce);
    let pr = predict(&v);
    let pos = &v.pos;
    let room = pos.v - pr.r_due;
    // B·(1 + p) ≈ V − R − w with w a quarter of the floor's gap.
    let b0 = room * 10_000 / (10_000 + u64::from(pos.penalty_bps));
    let kp = penalty_rate(pos, b0) * 3_000 / 10_000;
    let w = pr.k_s.saturating_sub(kp) / 4;
    let target = (room - w) * 10_000 / (10_000 + u64::from(pos.penalty_bps));
    let (ex, _) = gap_to_cost(&mut env, &mm, &actor, &k, &b, nonce, Q_SMALL, target);
    let ex = ex.expect("liquidated");
    let ev = ex.liquidated();
    let p = ex.predicted();
    assert_eq!(ev.rent, p.r_due, "rent in full");
    assert_eq!(
        ev.penalty,
        penalty_rate(&p.view.pos, ev.b_live_risk.min(ev.b_ref)),
        "penalty in full"
    );
    assert!(ev.keeper_topup > 0);
    assert!(
        ev.penalty_keeper + ev.keeper_topup < p.k_s,
        "floor truncated"
    );
    assert_eq!(ev.borrower_payout, 0, "the remainder went to the keeper");
}

/// The vault runs out inside the penalty: `π = V − B − r`, the borrower gets nothing and no
/// floor can be paid (ECON §8.3 step 3; no reserve in the bound, v0.4.1).
#[test]
fn penalty_truncated_by_the_vault() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    let v = view(&mut env, &mm, &b.key(), nonce);
    let pr = predict(&v);
    let target = (v.pos.v - pr.r_due) * 10_000 / 10_400;
    let (ex, _) = gap_to_cost(&mut env, &mm, &actor, &k, &b, nonce, Q, target);
    let ex = ex.expect("liquidated");
    let ev = ex.liquidated();
    let p = ex.predicted();
    assert_eq!(ev.rent, p.r_due);
    let full = penalty_rate(&p.view.pos, ev.b_live_risk.min(ev.b_ref));
    assert!(ev.penalty < full, "penalty truncated");
    assert_eq!(ev.penalty, p.view.pos.v - ev.buyback_cost - ev.rent);
    assert_eq!((ev.keeper_topup, ev.borrower_payout), (0, 0));
    assert_eq!(ev.penalty_keeper, ev.penalty * 3_000 / 10_000);
}

/// An underwater liquidation (`V − R < B̂_live ≤ V − K_s`) on an aged position at high
/// utilisation, whose rent due exceeds `K_s`: rent is truncated to `V − B`, no penalty, the
/// keeper and the borrower get nothing, and the lenders' rent shortfall is trued up in the pool
/// (the ECON §12.6 disclosure of v0.4.1's no-reserve bound, O19).
#[test]
fn underwater_liquidation_truncates_rent() {
    let spec = MarketSpec {
        lend: 3_400_000_000_000,
        ..MarketSpec::default()
    };
    let (mut env, mm, b, nonce, actor, k) = opened_with(spec, Q);
    let u = env.market(&mm.market());
    assert!(u.borrowed_q * 10_000 / (u.borrowed_q + u.tokens_free) >= 8_800);
    // Six days at 88 % utilisation (Tier C: 440 %/yr of notional).
    for _ in 0..6 {
        env.warp(86_400);
        env.crank(&mm);
    }
    env.tick(&mm, 3);
    let v = view(&mut env, &mm, &b.key(), nonce);
    let pr = predict(&v);
    assert!(pr.r_due > pr.k_s, "rent due {} ≤ K_s {}", pr.r_due, pr.k_s);
    let target = v.pos.v - (pr.r_due + pr.k_s) / 2;
    let (ex, _) = gap_to_cost(&mut env, &mm, &actor, &k, &b, nonce, Q, target);
    let ex = ex.expect("liquidated");
    let ev = ex.liquidated();
    let p = ex.predicted();
    assert!(ev.buyback_cost > p.view.pos.v - p.r_due, "underwater");
    assert_eq!(ev.rent, p.view.pos.v - ev.buyback_cost, "rent truncated");
    assert!(ev.rent < p.r_due);
    assert_eq!((ev.penalty, ev.keeper_topup, ev.borrower_payout), (0, 0, 0));
    assert!(ev.cr_spot_bps < 10_000, "CR below 100 %");
}

// ---- expiry versus liquidation ------------------------------------------------------------

/// ECON §8.4: an expired position that is also CR-liquidatable is liquidated with the penalty
/// (`expired` in the event) and `expire` refuses it (`UseLiquidate`); an expired, healthy
/// position is `UseExpire` for `liquidate`.
#[test]
fn expired_positions_liquidate_only_when_cr_liquidatable() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    let pos = env.position(&mm, &b.key(), nonce);
    // Healthy at the deadline.
    env.warp(pos.deadline - env.now() - 5);
    env.tick(&mm, 5);
    assert!(env.now() >= pos.deadline);
    let ex = liquidate_checked(&mut env, &mm, &k, &b.key(), nonce);
    assert_eq!(ex.refused(), E::UseExpire);
    // A genuine rise after the deadline makes it CR-liquidatable as well.
    let p0 = spot(&env, &mm) as f64;
    for s in 1..=90u32 {
        let m = 2.4f64.powf(f64::from(s.min(30)) / 30.0);
        honest_second(&mut env, &mm, &actor, Some((p0 * m) as u128));
        let saved = env.svm.clone();
        let ex = liquidate_checked(&mut env, &mm, &k, &b.key(), nonce);
        if ex.executed() {
            let ev = ex.liquidated();
            assert!(ev.expired);
            assert_eq!(
                ev.penalty,
                penalty_rate(&ex.predicted().view.pos, ev.b_live_risk.min(ev.b_ref))
            );
            // The same second, `expire` refuses.
            env.svm = saved;
            let ex = expire_checked(&mut env, &mm, &k, &b.key(), nonce);
            assert_eq!(ex.refused(), E::UseLiquidate);
            return;
        }
        assert!(matches!(ex.refused(), E::UseExpire | E::ExitBoundNotMet));
    }
    panic!("never CR-liquidatable");
}

// ---- refusals ------------------------------------------------------------------------------

#[test]
fn healthy_positions_are_not_liquidatable() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    assert_eq!(
        liquidate_checked(&mut env, &mm, &k, &b.key(), nonce).refused(),
        E::NotLiquidatable
    );
    // +50 %: still above L on both legs.
    ramp(&mut env, &mm, &actor, 1.5, 10);
    env.tick(&mm, 30);
    assert_eq!(
        liquidate_checked(&mut env, &mm, &k, &b.key(), nonce).refused(),
        E::NotLiquidatable
    );
}

/// ECON §5.4: without a completed bucket from the last `ref_max_age_s` seconds the position
/// waits (`ReferenceStale`); the keeper cranks one second and acts the next.
#[test]
fn a_stale_reference_waits_for_a_crank() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    ramp(&mut env, &mm, &actor, 2.4, 30);
    env.tick(&mm, 20);
    env.warp(3);
    let ix = exit_ix(Which::Liquidate, &mm, &k.pubkey(), &b.key(), nonce);
    assert_err(&env.send(&[ix], &[&k]), E::ReferenceStale);
    // A crank in the same second completes no bucket that is fresh enough...
    env.crank(&mm);
    assert_eq!(
        liquidate_checked(&mut env, &mm, &k, &b.key(), nonce).refused(),
        E::ReferenceStale
    );
    // ...the next second's instruction rolls it.
    env.warp(1);
    assert!(liquidate_checked(&mut env, &mm, &k, &b.key(), nonce).executed());
}

/// A gap past `V − K_s − ert`: once the legs hold, the normal buyback is not allowed and the
/// position belongs to `sol_settle` (S1 territory; ECON §5.6).
#[test]
fn a_gap_into_s1_is_use_sol_settle() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    let v = view(&mut env, &mm, &b.key(), nonce);
    let pr = predict(&v);
    let target = v.pos.v - pr.k_s / 2;
    let (ex, refusals) = gap_to_cost(&mut env, &mm, &actor, &k, &b, nonce, Q, target);
    assert!(ex.is_none());
    assert_eq!(*refusals.last().unwrap(), E::UseSolSettle);
    assert!(
        refusals
            .iter()
            .all(|e| matches!(e, E::NotLiquidatable | E::UseSolSettle))
    );
}

/// S2 territory: a pool drained so thin that buying back `q` costs a slippage above σ_max
/// (Tier C: 20 %) — `UseSolSettle` even though the vault could pay.
#[test]
fn a_thin_pool_is_s2_territory() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    // Liquidatable at ×2.1 (CR ≈ 143 %), then the LPs leave all but 5.5·q tokens: σ ≈ 22 %.
    ramp(&mut env, &mm, &actor, 2.1, 30);
    env.tick(&mm, 20);
    let r = mock::reserves(&env.svm, mm.mock());
    let keep = 5.5 * Q as f64 / r.base as f64;
    ok(mock::remove_liquidity(
        &mut env,
        &mm.pool,
        (r.base as f64 * (1.0 - keep)) as u64,
        (r.quote as f64 * (1.0 - keep)) as u64,
    ));
    env.tick(&mm, 1);
    let (ex, refusals) = attempts(&mut env, &mm, Which::Liquidate, &k, &b.key(), nonce, 5);
    assert!(ex.is_none());
    assert_eq!(*refusals.last().unwrap(), E::UseSolSettle);
    let v = view(&mut env, &mm, &b.key(), nonce);
    let pr = predict(&v);
    assert!(pr.cr_liquidatable());
    assert!(
        pr.sigma_live > u64::from(v.pos.max_liq_slip_bps),
        "σ above σ_max"
    );
    assert!(
        pr.b_live.unwrap().cost + v.pos.ert <= v.pos.v - pr.k_s,
        "the budget alone would allow it"
    );
}

/// ECON §12.2: a keeper's front-run beyond the fixed bound makes its own bundle revert
/// (`ExitBoundNotMet`): the position waits; a front-run inside the bound executes.
#[test]
fn a_front_run_beyond_the_bound_reverts_the_bundle() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    ramp(&mut env, &mm, &actor, 2.4, 30);
    env.tick(&mm, 20);
    let attacker = env.actor(&mm, 0, 500_000_000_000);
    let big = env.sandwich_ixs(&mm, &attacker, Dir::QuoteIn, 60_000_000_000);
    let ex = exit_checked(
        &mut env,
        &mm,
        Which::Liquidate,
        &k,
        &b.key(),
        nonce,
        &[big.front],
        &[big.back],
        &[&attacker.kp],
    );
    assert_eq!(ex.refused(), E::ExitBoundNotMet);
    let small = env.sandwich_ixs(&mm, &attacker, Dir::QuoteIn, 1_000_000_000);
    let ex = exit_checked(
        &mut env,
        &mm,
        Which::Liquidate,
        &k,
        &b.key(),
        nonce,
        &[small.front],
        &[small.back],
        &[&attacker.kp],
    );
    let ev = ex.liquidated();
    assert!(u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * 10_200);
}

/// A venue that cannot execute (an admin halt, a changed state layout) is `NoRoute`
/// (ARCH §3.6 step 1; A63): the position waits for S3.
#[test]
fn no_route_on_a_halted_or_changed_venue() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    ramp(&mut env, &mm, &actor, 2.4, 30);
    env.tick(&mm, 20);
    let saved = env.svm.clone();
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = true);
    env.tick(&mm, 1);
    assert_eq!(
        liquidate_checked(&mut env, &mm, &k, &b.key(), nonce).refused(),
        E::NoRoute
    );
    env.svm = saved;
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.layout_version = 1);
    env.tick(&mm, 1);
    assert_eq!(
        liquidate_checked(&mut env, &mm, &k, &b.key(), nonce).refused(),
        E::NoRoute
    );
}

// ---- what exits never read (I10) -----------------------------------------------------------

fn pause_and_flag_everything(env: &mut Env, mm: &MockMarket) {
    let ea = env.pause_authority.insecure_clone();
    let ek = ea.pubkey();
    ok(env.send(
        &[ix::set_pause(&ek, PauseScope::Global, true, None)],
        &[&ea],
    ));
    ok(env.send(
        &[ix::set_pause(
            &ek,
            PauseScope::Market,
            true,
            Some(mm.market()),
        )],
        &[&ea],
    ));
    ok(env.send(
        &[set_route_broken_ix(
            &ek,
            PauseScope::Market,
            0,
            true,
            Some(mm.market()),
        )],
        &[&ea],
    ));
    ok(env.send(
        &[set_route_broken_ix(&ek, PauseScope::Global, 5, true, None)],
        &[&ea],
    ));
    ok(env.send(&[ix::disable_venue(&ek, 5)], &[&ea]));
    env.set_all_venue_program_data();
    env.set_venue_program_data(
        GUARD_RAYDIUM_CPMM,
        FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_CPMM)] + 7,
    );
    ok(env.send(&[ix::flag_venue_upgraded(GUARD_RAYDIUM_CPMM)], &[]));
}

/// I10 (V2-1): pauses, the route-broken flags, a disabled venue and the upgrade latch never
/// gate a liquidation; a venue fee above the hard cap is charged (execution) but every risk
/// quote and the penalty base take the capped fee; a mint that fails the filter now is reported
/// (`MintSafetyTripped`) and the liquidation goes through.
#[test]
fn liquidations_ignore_pauses_flags_latch_fee_cap_and_the_mint_filter() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    ramp(&mut env, &mm, &actor, 2.4, 30);
    env.tick(&mm, 20);
    pause_and_flag_everything(&mut env, &mm);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.fee_ppm = 60_000);
    let mut acc = env.svm.get_account(&mm.mint()).unwrap();
    acc.data[46..50].copy_from_slice(&1u32.to_le_bytes());
    acc.data[50..82].copy_from_slice(&[7u8; 32]);
    env.svm.set_account(mm.mint(), acc).unwrap();
    env.tick(&mm, 1);
    let (ex, _) = attempts(&mut env, &mm, Which::Liquidate, &k, &b.key(), nonce, 30);
    let ex = ex.expect("liquidated");
    let ev = ex.liquidated();
    assert!(
        ev.b_live_risk < ev.b_live,
        "the risk quote caps the fee at 5 %"
    );
    assert_eq!(
        ev.penalty,
        penalty_rate(&ex.predicted().view.pos, ev.b_live_risk.min(ev.b_ref))
    );
    let tripped: Vec<MintSafetyTripped> = events::logged(ex.meta());
    assert_eq!(tripped.len(), 1);
    assert_eq!(tripped[0].mint, mm.mint());
}

// ---- trader grief, over-delivery, extras ---------------------------------------------------

/// ARCH §2.4 rule 2: lamports donated to `trader` and to the `trader_wsol` address are swept to
/// lenders as a vesting lump before the leg (`TraderSwept`); the waterfall is unchanged.
#[test]
fn donations_to_the_trader_are_swept_before_the_leg() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    ramp(&mut env, &mm, &actor, 2.4, 30);
    env.tick(&mm, 20);
    let trader = mm.keys.trader;
    env.fund(&trader, 3_000_000);
    env.fund(&mm.trader_wsol, 5_000_000);
    let (ex, _) = attempts(&mut env, &mm, Which::Liquidate, &k, &b.key(), nonce, 30);
    let ex = ex.expect("liquidated");
    let swept: Vec<TraderSwept> = events::of(ex.meta());
    assert_eq!(swept.len(), 1);
    assert_eq!(swept[0].lamports, 8_000_000);
    assert_eq!(read::lamports(&env.svm, &trader), 0);
    assert_eq!(read::lamports(&env.svm, &mm.trader_wsol), 0);
}

/// A62 (V2-10): a venue that delivers more than `q` on the buy does not freeze the exit; the
/// surplus stays in `pool_vault` as a donation (`token_surplus`).
#[test]
fn over_delivery_is_kept_as_a_donation() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    ramp(&mut env, &mm, &actor, 2.4, 30);
    env.tick(&mm, 20);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.over_delivery = 12_345);
    let (ex, _) = attempts(&mut env, &mm, Which::Liquidate, &k, &b.key(), nonce, 30);
    assert_eq!(ex.expect("liquidated").liquidated().token_surplus, 12_345);
}

fn seed_const(b: &[u8]) -> SeedPartArg {
    let mut bytes = [0u8; 32];
    bytes[..b.len()].copy_from_slice(b);
    SeedPartArg {
        kind: SEED_CONST,
        len: b.len() as u8,
        bytes,
    }
}

fn set_buy_extras(env: &mut Env, entries: Vec<ExtraEntryArg>) {
    let ea = env.pause_authority.insecure_clone();
    let ix = set_venue_extras_ix(&ea.pubkey(), VENUE_HINT_MOCK, 1, entries);
    ok(env.send(&[ix], &[&ea]));
}

/// T-REPLAY-PDA on the exit path (A41, V2-6): a venue change that adds a per-mint PDA trailing
/// account freezes `extras = none` liquidations until Emergency publishes a template; the
/// `current` set then works, and a hostile `current` write is bypassed with `previous`.
#[test]
fn extras_sets_follow_a_venue_change_on_liquidations() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    ramp(&mut env, &mm, &actor, 2.4, 30);
    env.tick(&mm, 20);
    let prog = Pubkey::new_unique();
    let (pda_key, bump) = Pubkey::find_program_address(&[b"extra", mm.mint().as_ref()], &prog);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.required_extras = vec![pda_key]
    });
    let ix = exit_ix(Which::Liquidate, &mm, &k.pubkey(), &b.key(), nonce);
    assert_eq!(
        custom_code(&env.send(&[ix], &[&k])),
        Some(6_011),
        "the mock's ExtrasMismatch"
    );
    let template = ExtraEntryArg {
        kind: 2,
        writable: false,
        key: prog,
        seeds: vec![
            seed_const(b"extra"),
            SeedPartArg {
                kind: SEED_PLACEHOLDER,
                len: PLACEHOLDER_MINT,
                bytes: [0; 32],
            },
        ],
    };
    set_buy_extras(&mut env, vec![template]);
    // A hostile write replaces it; `previous` still holds the template.
    set_buy_extras(
        &mut env,
        vec![ExtraEntryArg {
            kind: 1,
            writable: false,
            key: Pubkey::new_unique(),
            seeds: vec![],
        }],
    );
    let good = [
        AccountMeta::new_readonly(mock_extras_key(), false),
        AccountMeta::new_readonly(pda_key, false),
    ];
    let sel = ExtrasSel {
        set: EXTRAS_SET_PREVIOUS,
        bumps: [bump, 0, 0, 0],
    };
    let ix = liquidate_ix(&mm, &k.pubkey(), &b.key(), nonce, sel, &good);
    // The current (hostile) set with the good accounts is refused by the parser.
    let bad = liquidate_ix(
        &mm,
        &k.pubkey(),
        &b.key(),
        nonce,
        ExtrasSel {
            set: EXTRAS_SET_CURRENT,
            bumps: [bump, 0, 0, 0],
        },
        &good,
    );
    assert_err(&env.send(&[bad], &[&k]), E::BadExtras);
    let ex = exit_checked_with(
        &mut env,
        &mm,
        Which::Liquidate,
        &k,
        &b.key(),
        nonce,
        ix,
        &[],
        &[],
        &[],
    );
    assert!(ex.executed());
}

// ---- accounts (I14) --------------------------------------------------------------------------

/// I14 on `liquidate` and `expire`: every venue slot is mandatory and key-checked — omitting,
/// replacing, swapping or appending an account reverts before anything moves.
#[test]
fn remaining_accounts_are_strict() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    ramp(&mut env, &mm, &actor, 2.4, 30);
    env.tick(&mm, 20);
    for which in [Which::Liquidate, Which::Expire] {
        let base = exit_ix(which, &mm, &k.pubkey(), &b.key(), nonce);
        let named = base.accounts.len() - 8;
        let mut cases: Vec<(
            String,
            anchor_lang::solana_program::instruction::Instruction,
        )> = Vec::new();
        for slot in 0..8 {
            let mut ix = base.clone();
            ix.accounts.remove(named + slot);
            cases.push((format!("omit {slot}"), ix));
            let mut ix = base.clone();
            ix.accounts[named + slot].pubkey = Pubkey::new_unique();
            cases.push((format!("replace {slot}"), ix));
        }
        for (a, c) in [(0, 1), (1, 2), (3, 4), (5, 6), (6, 7)] {
            let mut ix = base.clone();
            ix.accounts.swap(named + a, named + c);
            cases.push((format!("swap {a} {c}"), ix));
        }
        let mut ix = base.clone();
        ix.accounts
            .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));
        cases.push(("append".into(), ix));
        for (what, ix) in cases {
            let before = env.position(&mm, &b.key(), nonce);
            let res = env.send(&[ix], &[&k]);
            assert!(res.is_err(), "{which:?} {what} must fail");
            assert!(
                matches!(
                    code_of(&res),
                    Some(c) if c == u32::from(E::BadRemainingAccounts)
                        || c == u32::from(E::VenueAccountMismatch)
                ) || code_of(&res).is_none(),
                "{which:?} {what}: {:?}",
                code_of(&res)
            );
            assert_eq!(env.position(&mm, &b.key(), nonce), before);
        }
    }
}

/// The named accounts: the borrower must be the position's (and the exposure its record), the
/// position a PDA of this market.
#[test]
fn named_accounts_are_checked() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    ramp(&mut env, &mm, &actor, 2.4, 30);
    env.tick(&mm, 20);
    let other = env.borrower();
    // The other wallet as borrower (its own exposure PDA): the position's `has_one` refuses.
    let mut ix = exit_ix(Which::Liquidate, &mm, &k.pubkey(), &b.key(), nonce);
    ix.accounts[1].pubkey = other.key();
    assert!(env.send(&[ix], &[&k]).is_err());
    // The right borrower with another wallet's exposure record.
    let mut other_b = env.borrower();
    let (res, _) = env.open(&mm, &mut other_b, Q);
    ok(res);
    let mut ix = exit_ix(Which::Liquidate, &mm, &k.pubkey(), &b.key(), nonce);
    ix.accounts[5].pubkey = pda::exposure(&mm.market(), &other_b.key()).0;
    assert!(env.send(&[ix], &[&k]).is_err());
    // The keeper must sign: a borrower account passed as keeper without a signature is not a
    // valid transaction; a stranger may keep (permissionless).
    assert!(liquidate_checked(&mut env, &mm, &k, &b.key(), nonce).executed());
}

// ---- several positions, self-keeping, Token-2022 --------------------------------------------

/// One wallet's second position is untouched; the exposure record stays with its `q_open`.
#[test]
fn a_wallets_other_position_and_exposure_survive() {
    // Two positions of 2·10¹² fit the wallet cap (25 % of the market's OI cap).
    let q = 2_000_000_000_000;
    let (mut env, mm, mut b, nonce, actor, k) = opened(q);
    let (res, nonce2) = env.open(&mm, &mut b, q);
    ok(res);
    let (ex, _) = rise_and_liquidate(&mut env, &mm, &actor, &k, &b, nonce, 2.4, 30);
    assert!(ex.executed());
    let exposure: shortwin::state::WalletExposure =
        read::borsh(&env.svm, &pda::exposure(&mm.market(), &b.key()).0).expect("exposure");
    assert_eq!(exposure.q_open, q);
    // The second position was liquidatable too: it is liquidated separately.
    let ex = liquidate_checked(&mut env, &mm, &k, &b.key(), nonce2);
    assert!(ex.executed());
    assert!(read::account(&env.svm, &pda::exposure(&mm.market(), &b.key()).0).is_none());
}

/// The borrower may keep its own position: it receives the keeper legs and the remainder.
#[test]
fn a_borrower_may_be_its_own_keeper() {
    let (mut env, mm, b, nonce, actor, _k) = opened(Q);
    ramp(&mut env, &mm, &actor, 2.4, 30);
    env.tick(&mm, 20);
    let me = b.kp.insecure_clone();
    let before = read::lamports(&env.svm, &b.key());
    let (ex, _) = attempts(&mut env, &mm, Which::Liquidate, &me, &b.key(), nonce, 30);
    let ex = ex.expect("liquidated");
    let ev = ex.liquidated();
    let p = ex.predicted();
    let after = read::lamports(&env.svm, &b.key());
    assert_eq!(
        after - before,
        ev.penalty_keeper + ev.keeper_topup + ev.borrower_payout + p.view.pos_lamports
            - p.view.pos.v
            + p.view.exposure_lamports
    );
}

#[test]
fn token_2022_base() {
    let spec = MarketSpec {
        program: TokenProgram::Token2022,
        ..MarketSpec::default()
    };
    let (mut env, mm, b, nonce, actor, k) = opened_with(spec, Q);
    let (ex, _) = rise_and_liquidate(&mut env, &mm, &actor, &k, &b, nonce, 2.4, 30);
    assert!(ex.executed());
}

// ---- liveness, trace and CU ------------------------------------------------------------------

/// I11 / I-E11 (v0.4.1): along a genuine rise, a liquidatable, buyback-allowed position with a
/// fresh bucket never waits while `B̂_live ≤ ⌊B̂_ref·(10⁴ + δ)/10⁴⌋`; and it never executes
/// otherwise.
#[test]
fn liveness_along_a_genuine_rise() {
    let (mut env, mm, b, nonce, actor, k) = opened(Q);
    let p0 = spot(&env, &mm) as f64;
    let mut seen_wait = false;
    for s in 1..=120u32 {
        let m = 3.0f64.powf(f64::from(s.min(90)) / 90.0);
        honest_second(&mut env, &mm, &actor, Some((p0 * m) as u128));
        let ex = liquidate_checked(&mut env, &mm, &k, &b.key(), nonce);
        let p = ex.predicted();
        let due = p.fresh && p.cr_liquidatable() && p.buyback_allowed;
        let within = p.liq.b_ref.is_some_and(|br| {
            u128::from(p.b_live.unwrap().cost) * 10_000
                <= u128::from(br) * u128::from(10_000 + p.view.pos.delta_liq_bps)
        });
        assert_eq!(ex.executed(), due && within, "second {s}");
        if due && !within {
            seen_wait = true;
        }
        if ex.executed() {
            let _ = seen_wait;
            return;
        }
    }
    panic!("never liquidated");
}

/// T-TRACE (ARCH §6.2: Raydium/mock exits 9 entries) and the CU budget (200 k). With a
/// normalisation that sweeps both the trader and the `trader_wsol` address the count is 12: the
/// two System transfers and the `TraderSwept` event CPI, one more than ARCH §6.2's "≤ 11"
/// (which counts the sweep but not its separate event; `close_short` measures the same 12).
#[test]
fn trace_and_compute() {
    for donate in [false, true] {
        let (mut env, mm, b, nonce, actor, k) = opened(Q);
        ramp(&mut env, &mm, &actor, 2.4, 30);
        env.tick(&mm, 20);
        if donate {
            env.fund(&mm.keys.trader, 1_000_000);
            env.fund(&mm.trader_wsol, 1_000_000);
        }
        let (ex, _) = attempts(&mut env, &mm, Which::Liquidate, &k, &b.key(), nonce, 30);
        let meta = ex.expect("liquidated").meta().clone();
        let entries = 1 + meta.inner_instructions.first().map_or(0, Vec::len);
        println!(
            "liquidate: {entries} trace entries, {} CU (donation: {donate})",
            meta.compute_units_consumed
        );
        assert_eq!(entries, if donate { 12 } else { 9 }, "{entries}");
        assert!(meta.compute_units_consumed <= 200_000);
    }
}

// ---- the hooks' own mutants ------------------------------------------------------------------

/// The I11 / I13 checks this track added fire on synthetic events: a spend above `max_in + ert`,
/// a `max_in` above the fixed bound, a venue charge outside `[B̂ − 5, B̂]`.
#[test]
fn hook_mutants_for_liquidations() {
    let (mut env, mm, b, nonce, _actor, _k) = opened(Q);
    env.check_invariants = false;
    let pre = invariants::snapshot(&env.svm, &[mm.market()]);
    let pos = env.position(&mm, &b.key(), nonce);
    let pos_key = pda::position(&mm.market(), &b.key(), nonce).0;
    // The synthetic exits closed the position (I3: an exit leaves no position behind).
    env.svm
        .set_account(pos_key, solana_account::Account::default())
        .unwrap();
    let k_s = risk::k_s(pos.v, &pos.settle_keeper_terms()).unwrap();
    let b_ref = pos.n_open;
    let widen = |x: u64| x * (10_000 + u64::from(pos.delta_liq_bps)) / 10_000;
    let liq = |buyback: u64, b_live: u64, max_in: u64| {
        let penalty = (b_ref * u64::from(pos.penalty_bps)).div_ceil(10_000);
        let pk = penalty * 3 / 10;
        let pp = penalty * 2 / 10;
        let topup = k_s.saturating_sub(pk);
        Liquidate {
            market: mm.market(),
            slot: 0,
            ts: 0,
            position: pos_key,
            keeper: b.key(),
            cr_spot_bps: 0,
            cr_ema_bps: 0,
            b_live,
            b_ref,
            p_ref: 0,
            delta_bps: pos.delta_liq_bps,
            buyback_cost: buyback,
            rent: 0,
            penalty,
            penalty_lender: penalty - pk - pp,
            penalty_protocol: pp,
            penalty_keeper: pk,
            keeper_topup: topup,
            borrower_payout: pos.v - buyback - penalty - topup,
            expired: false,
            token_surplus: 0,
            b_live_risk: b_live,
            max_in,
        }
    };
    let check = |e: Liquidate| {
        let ev = IxEvents {
            liquidations: vec![e],
            ..IxEvents::default()
        };
        invariants::check_ix(&env.svm, &pre, false, &ev)
            .err()
            .map(|v| v.invariant)
    };
    assert_eq!(check(liq(b_ref, b_ref, widen(b_ref))), None);
    assert_eq!(
        check(liq(b_ref + 10, b_ref + 10, b_ref)),
        Some("I11"),
        "B > max_in"
    );
    assert_eq!(
        check(liq(b_ref, b_ref, widen(b_ref) + 1)),
        Some("I11"),
        "max_in above the fixed bound"
    );
    assert_eq!(
        check(liq(b_ref, b_ref + 6, widen(b_ref))),
        Some("I13"),
        "charged below the quote"
    );
}
