//! `expire` on the mock venue (ARCH §3.7; ECON §8.2, §8.4; SPEC §4 "Duration"). Every exit goes
//! through the differential oracle of `liquidate_support.rs` (the refusal, or every event field,
//! lamport and token delta and the lender pool after `pool::repay`, recomputed from
//! `shortwin-math` on the state the instruction saw); the invariant hooks run after every
//! instruction.
//!
//! - the expiry waterfall (ARCH §5.2): rent, then the 2 % expiry fee to lenders (vesting), then
//!   the expiry keeper reward `K_e` (and its cap), then the borrower; rent and the fee senior in
//!   the bound (I11's expiry clause, checked by the hook on every expiry);
//! - the preconditions in order: `NotExpired`, `ReferenceStale`, `UseLiquidate` (expired **and**
//!   CR-liquidatable pays the penalty), `UseSolSettle`, `ExitBoundNotMet` on the expiry reference
//!   `min(P_ema, min over the ring of p_i, lb_lo)` ("nothing is urgent at expiry");
//! - what exits never read (I10), trader normalisation, over-delivery, the extras sets, the
//!   hooks' mutants for the expiry checks (I11, I13), T-TRACE and CU.

#![allow(clippy::result_large_err)]

#[path = "liquidate_support.rs"]
mod support;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::AccountMeta;
use shortwin::ExtraEntryArg;
use shortwin::PauseScope;
use shortwin::constants::GUARD_RAYDIUM_CPMM;
use shortwin::events::{Expire, MintSafetyTripped, TraderSwept};
use shortwin::state::ShortPosition;
use shortwin::venues::{EXTRAS_SET_CURRENT, ExtrasSel};
use shortwin_litesvm_tests::engine::{
    VENUE_HINT_MOCK, mock_extras_key, set_route_broken_ix, set_venue_extras_ix,
};
use shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS;
use shortwin_litesvm_tests::invariants::{self, IxEvents};
use shortwin_litesvm_tests::ix;
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};
use shortwin_math::{rent, risk};
use solana_keypair::Keypair;
use solana_signer::Signer;
use support::*;

const Q: u64 = 3_000_000_000_000;

fn opened_in(mut env: Env, spec: MarketSpec) -> (Env, MockMarket, Borrower, u64, Actor, Keypair) {
    let mm = market(&mut env, spec);
    let (b, nonce, _) = open(&mut env, &mm, Q);
    env.tick(&mm, 2);
    let actor = env.actor(&mm, 400_000_000_000_000, 3_000_000_000_000);
    let k = keeper(&mut env);
    (env, mm, b, nonce, actor, k)
}

fn opened() -> (Env, MockMarket, Borrower, u64, Actor, Keypair) {
    opened_in(Env::test_venue(), MarketSpec::default())
}

/// Daily cranks up to `secs_before` seconds before the deadline, then one crank per second up
/// to it (the keeper watches positions near their deadline every second, ECON §5.8).
fn to_deadline(env: &mut Env, mm: &MockMarket, pos: &ShortPosition, secs_before: i64) {
    while env.now() + 86_400 < pos.deadline - secs_before {
        env.warp(86_400);
        env.crank(mm);
    }
    let gap = pos.deadline - secs_before - env.now();
    if gap > 0 {
        env.warp(gap);
        env.crank(mm);
    }
}

fn fee_on(pos: &ShortPosition, base: u64) -> u64 {
    (u128::from(base) * u128::from(pos.expiry_fee_bps)).div_ceil(10_000) as u64
}

fn k_e(pos: &ShortPosition, b_ref: u64) -> u64 {
    risk::k_e(b_ref, &pos.expiry_keeper_terms()).unwrap()
}

/// The expiry waterfall at the deadline (ECON §8.4): `B` → rent (15 % protocol) → the expiry
/// fee `⌈2 %·min(B̂_live,risk, B̂_ref)⌉` to lenders (it vests) → `K_e = 0.001 SOL + 0.1 % of
/// B̂_ref` → the borrower; the bound is `B̂_ref·(1 + δ_exp)`. One second earlier: `NotExpired`.
#[test]
fn expiry_waterfall_at_the_deadline() {
    let (mut env, mm, b, nonce, _actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 10);
    for _ in 0..9 {
        env.warp(1);
        env.crank(&mm);
    }
    assert_eq!(env.now(), pos.deadline - 1);
    assert_eq!(
        expire_checked(&mut env, &mm, &k, &b.key(), nonce).refused(),
        E::NotExpired
    );
    assert_eq!(
        liquidate_checked(&mut env, &mm, &k, &b.key(), nonce).refused(),
        E::NotLiquidatable
    );
    env.warp(1);
    let vault0 = env.market_vault(&mm.market()).vault();
    let ex = expire_checked(&mut env, &mm, &k, &b.key(), nonce);
    let ev = ex.expired();
    let p = ex.predicted();
    assert_eq!(ev.rent, p.r_due, "rent in full");
    assert_eq!(ev.expiry_fee, fee_on(&pos, ev.b_live_risk.min(ev.b_ref)));
    assert_eq!(ev.pi_ref, fee_on(&pos, ev.b_ref));
    assert_eq!(ev.keeper_reward, k_e(&pos, ev.b_ref));
    assert_eq!(ev.keeper_reward, 1_000_000 + ev.b_ref / 1_000);
    assert!(ev.borrower_payout > 0);
    assert_eq!(
        ev.max_in,
        ev.b_ref * (10_000 + u64::from(pos.delta_exp_bps)) / 10_000,
        "the δ_exp bound binds (rent and fee leave room)"
    );
    let vault1 = env.market_vault(&mm.market()).vault();
    let rent_protocol = ev.rent * 1_500 / 10_000;
    assert_eq!(
        vault1.lender_sol - vault0.lender_sol,
        ev.rent - rent_protocol + ev.expiry_fee,
        "the fee is 100 % lenders"
    );
    assert_eq!(vault1.protocol_fees - vault0.protocol_fees, rent_protocol);
    assert!(
        env.market(&mm.market()).pool().vest_bal >= ev.expiry_fee,
        "the fee vests"
    );
    assert!(read::account(&env.svm, &pda::exposure(&mm.market(), &b.key()).0).is_none());
}

/// At high utilisation a week of rent is large; it is still senior to everything but the
/// buyback (the reserve `V − R − π_ref − ert` of the expiry bound, ARCH §3.7).
#[test]
fn rent_and_fee_stay_senior_at_high_utilisation() {
    let spec = MarketSpec {
        lend: 3_400_000_000_000,
        ..MarketSpec::default()
    };
    let (mut env, mm, b, nonce, _actor, k) = opened_in(Env::test_venue(), spec);
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 5);
    let (ex, _) = attempts(&mut env, &mm, Which::Expire, &k, &b.key(), nonce, 10);
    let ex = ex.expect("expired");
    let ev = ex.expired();
    let p = ex.predicted();
    assert!(
        p.r_due > p.k_s,
        "a week at 88 %: rent {} > K_s {}",
        p.r_due,
        p.k_s
    );
    assert_eq!(ev.rent, p.r_due);
    assert_eq!(ev.expiry_fee, fee_on(&pos, ev.b_live_risk.min(ev.b_ref)));
}

/// `K_e` is capped at `expiry_keeper_cap` (locked at open): with a 0.0015 SOL cap the keeper
/// gets exactly the cap.
#[test]
fn the_expiry_keeper_reward_is_capped() {
    let env = env_with(|a| a.params.expiry_keeper_cap = 1_500_000);
    let (mut env, mm, b, nonce, _actor, k) = opened_in(env, MarketSpec::default());
    let pos = env.position(&mm, &b.key(), nonce);
    assert_eq!(pos.expiry_keeper_cap, 1_500_000);
    to_deadline(&mut env, &mm, &pos, 5);
    let (ex, _) = attempts(&mut env, &mm, Which::Expire, &k, &b.key(), nonce, 10);
    let ev = ex.expect("expired").expired().keeper_reward;
    assert_eq!(ev, 1_500_000);
}

/// The expiry reference is the lowest recent robust price, `min(P_ema, min over the ring of
/// p_i, lb_lo)`: after a +3 % move at the deadline the keeper waits (`ExitBoundNotMet`) until
/// the whole ring has caught up — nothing is urgent at expiry (ECON §8.2).
#[test]
fn the_expiry_reference_is_the_lowest_recent_price() {
    let (mut env, mm, b, nonce, actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 0);
    let target = spot(&env, &mm) * 103 / 100;
    gap(&mut env, &mm, &actor, target);
    let (ex, refusals) = attempts(&mut env, &mm, Which::Expire, &k, &b.key(), nonce, 40);
    let ex = ex.expect("expired eventually");
    assert!(
        refusals.len() >= 8 && refusals.iter().all(|e| *e == E::ExitBoundNotMet),
        "{refusals:?}"
    );
    let ev = ex.expired();
    assert!(u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * 10_100);
}

/// A gap at expiry: the EMA has not caught up (not CR-liquidatable), the vault cannot pay the
/// buyback — `UseSolSettle`; once the EMA leg holds, `expire` gives way to `liquidate`
/// (`UseLiquidate`).
#[test]
fn a_gap_at_expiry_is_use_sol_settle_then_use_liquidate() {
    let (mut env, mm, b, nonce, actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 0);
    let v = view(&mut env, &mm, &b.key(), nonce);
    let pr = predict(&v);
    let target = price_for_cost(&env, &mm, Q, v.pos.v - pr.k_s / 2);
    gap(&mut env, &mm, &actor, target);
    let (ex, refusals) = attempts(&mut env, &mm, Which::Expire, &k, &b.key(), nonce, 30);
    assert!(ex.is_none());
    assert_eq!(refusals[0], E::UseSolSettle);
    assert_eq!(*refusals.last().unwrap(), E::UseLiquidate);
}

#[test]
fn a_stale_reference_waits_for_a_crank() {
    let (mut env, mm, b, nonce, _actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 0);
    env.warp(3);
    assert_eq!(
        expire_checked(&mut env, &mm, &k, &b.key(), nonce).refused(),
        E::ReferenceStale
    );
    env.crank(&mm);
    env.warp(1);
    assert!(expire_checked(&mut env, &mm, &k, &b.key(), nonce).executed());
}

/// ECON §12.2 on the expiry bound: a keeper's front-run beyond `δ_exp` (1 %) reverts its
/// bundle; inside it the expiry goes through and rent and the fee are unchanged.
#[test]
fn a_front_run_beyond_the_expiry_bound_reverts() {
    let (mut env, mm, b, nonce, _actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 0);
    env.tick(&mm, 10);
    let attacker = env.actor(&mm, 0, 200_000_000_000);
    let big = env.sandwich_ixs(&mm, &attacker, Dir::QuoteIn, 20_000_000_000);
    let ex = exit_checked(
        &mut env,
        &mm,
        Which::Expire,
        &k,
        &b.key(),
        nonce,
        &[big.front],
        &[big.back],
        &[&attacker.kp],
    );
    assert_eq!(ex.refused(), E::ExitBoundNotMet);
    let saved = env.svm.clone();
    let honest = expire_checked(&mut env, &mm, &k, &b.key(), nonce);
    let (h_rent, h_fee) = (honest.expired().rent, honest.expired().expiry_fee);
    env.svm = saved;
    let small = env.sandwich_ixs(&mm, &attacker, Dir::QuoteIn, 2_000_000_000);
    let ex = exit_checked(
        &mut env,
        &mm,
        Which::Expire,
        &k,
        &b.key(),
        nonce,
        &[small.front],
        &[small.back],
        &[&attacker.kp],
    );
    let ev = ex.expired();
    // Rent and the fee are paid in full (the fee's base moves by a few lamports: the front-run's
    // venue fees deepen `L_live`, hence `L_b`, and lower `B̂_ref`, as in the sim).
    assert_eq!(ev.rent, h_rent, "rent senior");
    assert_eq!(
        ev.expiry_fee,
        fee_on(&pos, ev.b_live_risk.min(ev.b_ref)),
        "fee in full"
    );
    assert!(ev.expiry_fee.abs_diff(h_fee) <= h_fee / 10_000);
    assert!(ev.buyback_cost > honest.expired().buyback_cost);
}

#[test]
fn no_route_on_a_halted_venue() {
    let (mut env, mm, b, nonce, _actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 0);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = true);
    env.tick(&mm, 1);
    assert_eq!(
        expire_checked(&mut env, &mm, &k, &b.key(), nonce).refused(),
        E::NoRoute
    );
}

/// I10: pauses, the route-broken flags, a disabled venue, the upgrade latch, a fee above the
/// hard cap and a mint that fails the filter never block an expiry.
#[test]
fn expiries_ignore_pauses_flags_latch_fee_cap_and_the_mint_filter() {
    let (mut env, mm, b, nonce, _actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 0);
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
    // A venue fee of 5.2 %, above the global hard cap (5 %): the reference and the fee base take
    // the capped risk quote, the leg pays the live fee (within δ_exp of it).
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.fee_ppm = 52_000);
    let mut acc = env.svm.get_account(&mm.mint()).unwrap();
    acc.data[46..50].copy_from_slice(&1u32.to_le_bytes());
    acc.data[50..82].copy_from_slice(&[7u8; 32]);
    env.svm.set_account(mm.mint(), acc).unwrap();
    let (ex, _) = attempts(&mut env, &mm, Which::Expire, &k, &b.key(), nonce, 20);
    let ex = ex.expect("expired");
    let ev = ex.expired();
    assert!(ev.b_live_risk < ev.b_live);
    let tripped: Vec<MintSafetyTripped> = events::logged(ex.meta());
    assert_eq!(tripped.len(), 1);
}

/// A venue fee more than `δ_exp` above the hard cap (6 % against 5 %): the execution quote
/// exceeds the capped reference by more than 1 %, so the expiry waits (`ExitBoundNotMet`) —
/// the fee above the hard cap is S3 evidence and the position reaches `sol_settle(S3)` through
/// the chain (ARCH §5.3); a liquidation's 2 % bound still clears it.
#[test]
fn a_fee_far_above_the_hard_cap_holds_expiries_for_s3() {
    let (mut env, mm, b, nonce, _actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 0);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.fee_ppm = 60_000);
    env.tick(&mm, 1);
    let (ex, refusals) = attempts(&mut env, &mm, Which::Expire, &k, &b.key(), nonce, 15);
    assert!(ex.is_none());
    assert!(
        refusals.iter().all(|e| *e == E::ExitBoundNotMet),
        "{refusals:?}"
    );
    let m = env.market(&mm.market());
    assert!(
        m.no_route_since > 0,
        "the S3 chain runs (fee above the hard cap)"
    );
}

#[test]
fn donations_and_over_delivery_on_expiry() {
    let (mut env, mm, b, nonce, _actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 0);
    env.fund(&mm.keys.trader, 2_000_000);
    env.fund(&mm.trader_wsol, 3_000_000);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.over_delivery = 777);
    env.tick(&mm, 1);
    let (ex, _) = attempts(&mut env, &mm, Which::Expire, &k, &b.key(), nonce, 10);
    let ex = ex.expect("expired");
    assert_eq!(ex.expired().token_surplus, 777);
    let swept: Vec<TraderSwept> = events::of(ex.meta());
    assert_eq!(swept[0].lamports, 5_000_000);
}

/// The extras sets on the expiry's buy leg (A41): a fixed trailing account the venue now
/// requires is followed with `current`.
#[test]
fn extras_on_expiry() {
    let (mut env, mm, b, nonce, _actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 0);
    let extra = Pubkey::new_unique();
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.required_extras = vec![extra]
    });
    let ea = env.pause_authority.insecure_clone();
    let set = set_venue_extras_ix(
        &ea.pubkey(),
        VENUE_HINT_MOCK,
        1,
        vec![ExtraEntryArg {
            kind: 1,
            writable: false,
            key: extra,
            seeds: vec![],
        }],
    );
    ok(env.send(&[set], &[&ea]));
    env.tick(&mm, 1);
    let none = exit_ix(Which::Expire, &mm, &k.pubkey(), &b.key(), nonce);
    assert_eq!(custom_code(&env.send(&[none], &[&k])), Some(6_011));
    let ix = expire_ix(
        &mm,
        &k.pubkey(),
        &b.key(),
        nonce,
        ExtrasSel {
            set: EXTRAS_SET_CURRENT,
            bumps: [0; 4],
        },
        &[
            AccountMeta::new_readonly(mock_extras_key(), false),
            AccountMeta::new_readonly(extra, false),
        ],
    );
    let ex = exit_checked_with(
        &mut env,
        &mm,
        Which::Expire,
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

/// T-TRACE (ARCH §6.2: 9 entries on the mock) and CU (≤ 200 k).
#[test]
fn trace_and_compute() {
    let (mut env, mm, b, nonce, _actor, k) = opened();
    let pos = env.position(&mm, &b.key(), nonce);
    to_deadline(&mut env, &mm, &pos, 0);
    let (ex, _) = attempts(&mut env, &mm, Which::Expire, &k, &b.key(), nonce, 5);
    let meta = ex.expect("expired").meta().clone();
    let entries = 1 + meta.inner_instructions.first().map_or(0, Vec::len);
    println!(
        "expire: {entries} trace entries, {} CU",
        meta.compute_units_consumed
    );
    assert_eq!(entries, 9);
    assert!(meta.compute_units_consumed <= 200_000);
}

/// The expiry checks the hook gained (I11, I13) fire on synthetic events: `B > max_in + ert`,
/// `max_in` above `B̂_ref·(1 + δ_exp)`, `π_ref` not the fee on `B̂_ref`, a keeper reward other
/// than `min(K_e, remainder)`, rent truncated although the bound left room for it (senior), and a
/// venue charge outside `[B̂ − 5, B̂]`.
#[test]
fn hook_mutants_for_expiries() {
    let (mut env, mm, b, nonce, _actor, _k) = opened();
    env.crank(&mm);
    env.check_invariants = false;
    let pre = invariants::snapshot(&env.svm, &[mm.market()]);
    let pos = env.position(&mm, &b.key(), nonce);
    let r_due = rent::due(&pos.pos_rent(), &env.market(&mm.market()).basis()).unwrap();
    let pos_key = pda::position(&mm.market(), &b.key(), nonce).0;
    // The synthetic exits closed the position (I3: an exit leaves no position behind).
    env.svm
        .set_account(pos_key, solana_account::Account::default())
        .unwrap();
    let b_ref = pos.n_open;
    let widen = |x: u64| x * (10_000 + u64::from(pos.delta_exp_bps)) / 10_000;
    let exp = |buyback: u64, b_live: u64, max_in: u64, rent_paid: u64| {
        let fee = fee_on(&pos, b_ref.min(b_live));
        let w = pos.v - buyback - rent_paid - fee;
        let ke = k_e(&pos, b_ref).min(w);
        Expire {
            market: mm.market(),
            slot: 0,
            ts: 0,
            position: pos_key,
            keeper: b.key(),
            b_live,
            b_ref,
            pi_ref: fee_on(&pos, b_ref),
            buyback_cost: buyback,
            rent: rent_paid,
            expiry_fee: fee,
            keeper_reward: ke,
            borrower_payout: w - ke,
            b_live_risk: b_live,
            max_in,
            token_surplus: 0,
        }
    };
    let check = |e: Expire| {
        let ev = IxEvents {
            expiries: vec![e],
            ..IxEvents::default()
        };
        invariants::check_ix(&env.svm, &pre, false, &ev)
            .err()
            .map(|v| v.invariant)
    };
    assert_eq!(check(exp(b_ref, b_ref, widen(b_ref), r_due)), None);
    assert_eq!(check(exp(b_ref + 9, b_ref + 9, b_ref, r_due)), Some("I11"));
    assert_eq!(
        check(exp(b_ref, b_ref, widen(b_ref) + 1, r_due)),
        Some("I11")
    );
    let mut e = exp(b_ref, b_ref, widen(b_ref), r_due);
    e.pi_ref += 1;
    assert_eq!(check(e), Some("I11"), "π_ref");
    let mut e = exp(b_ref, b_ref, widen(b_ref), r_due);
    e.keeper_reward -= 1;
    e.borrower_payout += 1;
    assert_eq!(check(e), Some("I11"), "K_e");
    // Rent short by one lamport although V − R − π_ref ≫ B̂_ref: rent is senior.
    assert_eq!(
        check(exp(b_ref, b_ref, widen(b_ref), r_due - 1)),
        Some("I11")
    );
    assert_eq!(
        check(exp(b_ref, b_ref + 6, widen(b_ref), r_due)),
        Some("I13")
    );
}
