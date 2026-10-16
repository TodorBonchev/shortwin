//! `close_short` refusals and what closes never read (ARCH §3.5; I10, V2-1).

use shortwin::PauseScope;
use shortwin::constants::GUARD_RAYDIUM_CPMM;
use shortwin::errors::ShortwinError as E;
use shortwin::events::MintSafetyTripped;
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::engine::set_route_broken_ix;
use shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS;
use shortwin_litesvm_tests::ix;
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};
use solana_signer::Signer;

use crate::common::*;

fn opened() -> (Env, MockMarket, Borrower, u64) {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 2);
    (env, mm, b, nonce)
}

fn try_close(env: &mut Env, mm: &MockMarket, b: &Borrower, nonce: u64, max: u64) -> TxResult {
    let ix = close_short_ix(mm, &b.key(), nonce, max, ExtrasSel::NONE, &[]);
    let kp = b.kp.insecure_clone();
    env.send(&[ix], &[&kp])
}

#[test]
fn buyback_above_the_borrowers_bound() {
    let (mut env, mm, b, nonce) = opened();
    let b_hat = quote_buy(&env, &mm, Q);
    assert_err(
        &try_close(&mut env, &mm, &b, nonce, b_hat - 1),
        E::BuybackAboveMax,
    );
    ok(try_close(&mut env, &mm, &b, nonce, b_hat));
}

#[test]
fn vault_cannot_pay_the_buyback() {
    let (mut env, mm, b, nonce) = opened();
    // The price more than triples (V ≈ 3·N_open at Tier C): B̂ + R > V.
    let actor = env.actor(&mm, 0, 5_000_000_000_000);
    let spot = env.spot(&mm);
    env.push_price(&mm, &actor, spot * 4);
    assert_err(
        &try_close(&mut env, &mm, &b, nonce, u64::MAX),
        E::InsufficientVault,
    );
}

#[test]
fn no_route_on_a_halted_or_changed_venue() {
    let (mut env, mm, b, nonce) = opened();
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = true);
    assert_err(&try_close(&mut env, &mm, &b, nonce, u64::MAX), E::NoRoute);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.disabled = false;
        k.layout_version = 1;
    });
    assert_err(&try_close(&mut env, &mm, &b, nonce, u64::MAX), E::NoRoute);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.layout_version = 0);
    ok(try_close(&mut env, &mm, &b, nonce, u64::MAX));
}

#[test]
fn only_the_borrower_closes() {
    let (mut env, mm, b, nonce) = opened();
    let other = env.borrower();
    // Another signer with this position's address: `has_one = borrower`.
    let mut ix = close_short_ix(&mm, &other.key(), nonce, u64::MAX, ExtrasSel::NONE, &[]);
    ix.accounts[3].pubkey = pda::position(&mm.market(), &b.key(), nonce).0;
    let res = env.send(&[ix], &[&other.kp]);
    assert!(res.is_err());
    ok(try_close(&mut env, &mm, &b, nonce, u64::MAX));
}

/// I10 / V2-1: pauses, the route-broken flags, the latch, a stale oracle and a fee above the
/// hard cap never stop a borrower's close.
#[test]
fn exits_ignore_pauses_flags_latch_and_the_fee_cap() {
    let (mut env, mm, b, nonce) = opened();
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
    // A venue fee above the global hard cap (500 bps): the borrower's bound allows it.
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.fee_ppm = 60_000);
    let (close, _) = close_ok(&mut env, &mm, &b, nonce);
    assert!(close.buyback_cost > 0);
}

/// ARCH §3.0 step 3 (M1-SEC-11): a mint that fails the filter now never blocks an exit; the
/// close reports it with a `MintSafetyTripped` log event.
#[test]
fn an_unsafe_mint_does_not_block_a_close() {
    let (mut env, mm, b, nonce) = opened();
    // Give the mint a freeze authority (the filter's first refusal) by rewriting its bytes.
    let mut acc = env.svm.get_account(&mm.mint()).unwrap();
    acc.data[46..50].copy_from_slice(&1u32.to_le_bytes());
    acc.data[50..82].copy_from_slice(&[7u8; 32]);
    env.svm.set_account(mm.mint(), acc).unwrap();
    let (_close, meta) = close_ok(&mut env, &mm, &b, nonce);
    let tripped: Vec<MintSafetyTripped> = events::logged(&meta);
    assert_eq!(tripped.len(), 1);
    assert_eq!(tripped[0].mint, mm.mint());
}
