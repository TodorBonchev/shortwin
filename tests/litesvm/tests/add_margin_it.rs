//! `add_margin(amount)` on the mock venue (ARCH §3.5; A20, ADOPTED Q12; C0-17; A26).
//!
//! - It **only raises CR**: `v` and `m` rise by `amount`, the position's lamports by the same,
//!   the borrower pays exactly `amount`; a close afterwards pays the borrower exactly `amount`
//!   more, and a position whose vault could not pay its buyback closes once the missing
//!   lamports are added — to the lamport.
//! - **Snapshot rules** (A26): no other field of the position changes (locked terms,
//!   `deadline`, `r_min`, `j_open`, `q`, `s`, `n_open`, `fee_open`, `entry_price`, the arming
//!   state), not even after governance changes every parameter it snapshot.
//! - **C0-17**: `Market` is passed read-only and its bytes never change; neither do the
//!   `MarketVault` or the pool vault.
//! - It is allowed through every gate an open or a lender entry has (pauses, route-broken
//!   flags, a disabled or upgraded venue, a halted pool, an unsafe mint, a position past its
//!   deadline or armed for settlement): it only moves SOL into the position.
//! - Refusals: `ZeroAmount`, a signer that is not the borrower, another market's position,
//!   copies of the market or the position away from their PDAs, a non-Open status, `v` / `m`
//!   overflow, a borrower that cannot pay, wrong System program or event authority, a closed
//!   position, a market of another layout version.
//! - I14 for its named accounts; T-TRACE (3 entries) and CU; a seeded random walk mixing
//!   top-ups with opens, closes, cranks and price moves, the invariant hooks after every
//!   instruction.
//!
//! Loads short.win's test-venue build (`scripts/build-test-venue.sh`, or
//! `SHORTWIN_TEST_VENUE_SO`) and the mock venue (`MOCK_CPAMM_SO`).

#![allow(clippy::result_large_err)]

// The engine tests' shared helpers (not every one is used here).
#[path = "engine_it/common.rs"]
#[allow(unused_imports)]
mod common;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::PauseScope;
use shortwin::constants::GUARD_RAYDIUM_CPMM;
use shortwin::errors::ShortwinError as E;
use shortwin::events::MarginAdded;
use shortwin::state::{ConfigChange, Market, ShortPosition};
use shortwin_litesvm_tests::engine::{add_margin_ix, set_route_broken_ix};
use shortwin_litesvm_tests::errors::assert_anchor_err;
use shortwin_litesvm_tests::fixtures;
use shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS;
use shortwin_litesvm_tests::ix;
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};
use solana_keypair::Keypair;
use solana_signer::Signer;

use common::*;

/// ARCH §6.1 has no `add_margin` row; the lender-SOL class limit (`claim_sol` /
/// `cover_deficit` / `sync_lender`, the other System-transfer instructions that do not write
/// `Market`) applies.
const ADD_MARGIN_CU_MAX: u64 = 60_000;
/// The instruction, its System transfer and its event (ARCH §6.2 style count).
const ADD_MARGIN_TRACE: usize = 3;

const SOL: u64 = 1_000_000_000;

fn opened_with(spec: MarketSpec) -> (Env, MockMarket, Borrower, u64) {
    let (mut env, mm) = setup_with(spec);
    env.crank(&mm);
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 2);
    (env, mm, b, nonce)
}

fn opened() -> (Env, MockMarket, Borrower, u64) {
    opened_with(MarketSpec::default())
}

fn position_key(mm: &MockMarket, b: &Pubkey, nonce: u64) -> Pubkey {
    pda::position(&mm.market(), b, nonce).0
}

fn add(env: &mut Env, mm: &MockMarket, b: &Borrower, nonce: u64, amount: u64) -> TxResult {
    let ix = add_margin_ix(mm, &b.key(), nonce, amount);
    let kp = b.kp.insecure_clone();
    env.send(&[ix], &[&kp])
}

fn raw(env: &Env, key: &Pubkey) -> Vec<u8> {
    env.svm.get_account(key).map(|a| a.data).unwrap_or_default()
}

/// Writes `p` over the position account (keeping its lamports).
fn write_position(env: &mut Env, key: &Pubkey, p: &ShortPosition) {
    let lamports = lamports(env, key);
    let rent = env
        .svm
        .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
    fixtures::set_borsh(&mut env.svm, key, p, ShortPosition::SPACE, lamports - rent);
}

fn happy_path(program: TokenProgram) {
    let (mut env, mm, b, nonce) = opened_with(spec_on(program));
    let key = position_key(&mm, &b.key(), nonce);
    let p0 = env.position(&mm, &b.key(), nonce);
    let lamports0 = lamports(&env, &key);
    let borrower0 = lamports(&env, &b.key());
    let market0 = raw(&env, &mm.market());
    let vault0 = raw(&env, &mm.keys.market_vault);
    let vault_lamports0 = lamports(&env, &mm.keys.market_vault);
    let pool_vault0 = tokens(&env, &mm.keys.pool_vault);
    let amount = 3 * SOL / 2;

    let ix = add_margin_ix(&mm, &b.key(), nonce, amount);
    // C0-17: the market is passed read-only; the position is the one writable program account.
    let meta_of = |k: &Pubkey| ix.accounts.iter().find(|m| m.pubkey == *k).cloned();
    assert!(
        !meta_of(&mm.market()).unwrap().is_writable,
        "market read-only"
    );
    assert!(meta_of(&key).unwrap().is_writable);
    assert!(meta_of(&b.key()).unwrap().is_signer);
    let meta = ok(env.send(&[ix], &[&b.kp]));

    let p1 = env.position(&mm, &b.key(), nonce);
    assert_eq!(p1.v, p0.v + amount, "v += amount");
    assert_eq!(p1.m, p0.m + amount, "m += amount");
    let mut rest = p1.clone();
    rest.v = p0.v;
    rest.m = p0.m;
    assert_eq!(rest, p0, "nothing else in the position changes (A26)");
    assert_eq!(lamports(&env, &key), lamports0 + amount);
    assert_eq!(
        lamports(&env, &b.key()),
        borrower0 - amount,
        "the borrower pays exactly the margin (the harness payer pays the fee)"
    );
    assert_eq!(raw(&env, &mm.market()), market0, "Market not written");
    assert_eq!(raw(&env, &mm.keys.market_vault), vault0);
    assert_eq!(lamports(&env, &mm.keys.market_vault), vault_lamports0);
    assert_eq!(tokens(&env, &mm.keys.pool_vault), pool_vault0);

    let ev: MarginAdded = events::one(&meta);
    assert_eq!(
        (ev.market, ev.position, ev.amount, ev.ts, ev.slot),
        (mm.market(), key, amount, env.now(), env.clock().slot)
    );
    let trace = trace_entries(&meta, 0);
    let cu = meta.compute_units_consumed;
    println!("add_margin {program:?}: {trace} trace entries, {cu} CU");
    assert_eq!(trace, ADD_MARGIN_TRACE, "logs:\n{}", meta.pretty_logs());
    assert!(cu <= ADD_MARGIN_CU_MAX, "add_margin {cu} CU");
}

#[test]
fn adds_margin_spl() {
    happy_path(TokenProgram::Spl);
}

#[test]
fn adds_margin_token_2022() {
    happy_path(TokenProgram::Token2022);
}

/// Top-ups accumulate, also several in one transaction (the hooks check after each).
#[test]
fn top_ups_accumulate_within_and_across_transactions() {
    let (mut env, mm, b, nonce) = opened();
    let p0 = env.position(&mm, &b.key(), nonce);
    let ixs: Vec<Instruction> = [1u64, 2, 3]
        .iter()
        .map(|k| add_margin_ix(&mm, &b.key(), nonce, k * 100_000_000))
        .collect();
    let meta = ok(env.send(&ixs, &[&b.kp]));
    assert_eq!(events::of::<MarginAdded>(&meta).len(), 3);
    env.warp(1);
    ok(add(&mut env, &mm, &b, nonce, 1));
    let p = env.position(&mm, &b.key(), nonce);
    assert_eq!(p.v, p0.v + 600_000_001);
    assert_eq!(p.m, p0.m + 600_000_001);
}

/// It only raises CR: a close in the same second pays the borrower exactly the margin more;
/// the buyback and the rent are the same.
#[test]
fn a_close_pays_the_margin_back_exactly() {
    let (mut env, mm, b, nonce) = opened();
    let saved = env.svm.clone();
    let (plain, _) = close_ok(&mut env, &mm, &b, nonce);
    env.svm = saved;
    let amount = 777_777_777;
    ok(add(&mut env, &mm, &b, nonce, amount));
    let (topped, _) = close_ok(&mut env, &mm, &b, nonce);
    assert_eq!(topped.buyback_cost, plain.buyback_cost);
    assert_eq!(topped.rent, plain.rent, "rent does not depend on V");
    assert_eq!(topped.rent_protocol, plain.rent_protocol);
    assert_eq!(topped.borrower_payout, plain.borrower_payout + amount);
}

/// A position whose vault cannot pay its buyback (`B̂ + R + ert > V`, `InsufficientVault`)
/// closes once exactly the missing lamports are added: one lamport less still fails.
#[test]
fn margin_unblocks_an_insufficient_vault_to_the_lamport() {
    let (mut env, mm, b, nonce) = opened();
    let actor = env.actor(&mm, 0, 5_000_000_000_000);
    let spot = env.spot(&mm);
    env.push_price(&mm, &actor, spot * 4);
    let close = |env: &mut Env| {
        let ix = close_short_ix(
            &mm,
            &b.key(),
            nonce,
            u64::MAX,
            shortwin::venues::ExtrasSel::NONE,
            &[],
        );
        env.send(&[ix], &[&b.kp])
    };
    assert_err(&close(&mut env), E::InsufficientVault);
    // Learn B̂ and R at this second on a copy (a large top-up closes).
    let saved = env.svm.clone();
    ok(add(&mut env, &mm, &b, nonce, 100 * SOL));
    let (probe, _) = close_ok(&mut env, &mm, &b, nonce);
    env.svm = saved;
    let v = env.position(&mm, &b.key(), nonce).v;
    assert_eq!(
        probe.b_live, probe.buyback_cost,
        "the mock executes its quote"
    );
    // ert = 0 on the mock: the vault must hold B̂ + R.
    let need = probe.b_live + probe.rent - v;
    ok(add(&mut env, &mm, &b, nonce, need - 1));
    assert_err(&close(&mut env), E::InsufficientVault);
    ok(add(&mut env, &mm, &b, nonce, 1));
    let meta = ok(close(&mut env));
    let c: shortwin::events::Close = events::one(&meta);
    assert_eq!(c.borrower_payout, 0, "the vault held exactly B + R");
}

/// Allowed through every gate that stops opens or lender entries, on a halted venue, after the
/// deadline, and while armed for settlement; the arming state is left alone.
#[test]
fn allowed_through_every_gate() {
    let (mut env, mm, b, nonce) = opened();
    let key = position_key(&mm, &b.key(), nonce);
    let ea = env.pause_authority.insecure_clone();
    let ek = ea.pubkey();
    let mut total = 0;
    let mut step = |env: &mut Env, what: &str| {
        total += 1_000_000;
        let res = add(env, &mm, &b, nonce, 1_000_000);
        assert!(res.is_ok(), "{what}: {res:?}");
    };
    ok(env.send(
        &[ix::set_pause(&ek, PauseScope::Global, true, None)],
        &[&ea],
    ));
    step(&mut env, "global pause");
    ok(env.send(
        &[ix::set_pause(
            &ek,
            PauseScope::Market,
            true,
            Some(mm.market()),
        )],
        &[&ea],
    ));
    step(&mut env, "market pause");
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
    step(&mut env, "route broken (market and venue bit)");
    ok(env.send(&[ix::disable_venue(&ek, 5)], &[&ea]));
    step(&mut env, "venue disabled");
    env.set_all_venue_program_data();
    env.set_venue_program_data(
        GUARD_RAYDIUM_CPMM,
        FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_CPMM)] + 7,
    );
    ok(env.send(&[ix::flag_venue_upgraded(GUARD_RAYDIUM_CPMM)], &[]));
    step(&mut env, "venue upgraded (latch set)");
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = true);
    step(&mut env, "pool disabled");
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.disabled = false;
        k.layout_version = 1;
    });
    step(&mut env, "pool layout changed");
    // A freeze authority appears: flag_unsafe_mint sets `mint_unsafe`.
    let mut acc = env.svm.get_account(&mm.mint()).unwrap();
    acc.data[46..50].copy_from_slice(&1u32.to_le_bytes());
    acc.data[50..82].copy_from_slice(&[7u8; 32]);
    env.svm.set_account(mm.mint(), acc).unwrap();
    ok(env.send(&[ix::flag_unsafe_mint(&mm.mint())], &[]));
    assert!(
        env.market(&mm.market())
            .has_flag(shortwin::state::MARKET_FLAG_MINT_UNSAFE)
    );
    step(&mut env, "mint flagged unsafe");
    // Armed for settlement (written as sol_settle would): the arming state is untouched.
    let mut p = env.position(&mm, &b.key(), nonce);
    p.s_armed = 1;
    p.s_count = 17;
    p.s_since = env.now() - 5;
    p.s_last = env.now() - 1;
    write_position(&mut env, &key, &p);
    step(&mut env, "armed");
    let q = env.position(&mm, &b.key(), nonce);
    assert_eq!(
        (q.s_armed, q.s_count, q.s_since, q.s_last),
        (1, 17, p.s_since, p.s_last)
    );
    // Past the deadline.
    let wait = q.deadline - env.now() + 3_600;
    env.warp(wait);
    step(&mut env, "expired");
    let end = env.position(&mm, &b.key(), nonce);
    assert_eq!(end.v, p.v + 2_000_000);
    assert_eq!(total, 10_000_000);
}

/// A26: governance changes every parameter a position snapshots (Params) and the EMA and kink
/// versions while it is open; a top-up afterwards leaves the position's terms as they were at
/// open.
#[test]
fn snapshots_survive_governance_changes() {
    let (mut env, mm, b, nonce) = opened();
    let p0 = env.position(&mm, &b.key(), nonce);
    let mut params = env.config().params;
    let mut tiers = env.config().tiers;
    params.penalty_lender_bps = 6_000;
    params.penalty_protocol_bps = 1_000;
    params.rent_protocol_bps = 2_500;
    params.expiry_fee_bps = 300;
    params.settle_keeper_fixed = 5_000_000;
    params.delta_liq_bps = 300;
    params.venue_fee_hard_cap_bps = 800;
    params.s3_wait_s = 1_200;
    for t in &mut tiers {
        t.penalty_bps += 100;
        t.rate_base_bps += 500;
    }
    let gov = env.governance.insecure_clone();
    ok(env.send(
        &[ix::propose_config(
            &gov.pubkey(),
            ConfigChange::Params { params, tiers },
        )],
        &[&gov],
    ));
    env.warp(i64::from(env.config().timelock_s));
    let apply = env.apply_config_ix();
    ok(env.send(&[apply], &[]));
    assert_eq!(env.config().params.rent_protocol_bps, 2_500);
    ok(add(&mut env, &mm, &b, nonce, SOL));
    let p1 = env.position(&mm, &b.key(), nonce);
    let mut rest = p1.clone();
    rest.v = p0.v;
    rest.m = p0.m;
    assert_eq!(rest, p0, "every snapshot term is the open's");
}

#[test]
fn zero_is_refused() {
    let (mut env, mm, b, nonce) = opened();
    assert_err(&add(&mut env, &mm, &b, nonce, 0), E::ZeroAmount);
}

/// `has_one = borrower`: another signer cannot top up (or touch) someone else's position, even
/// with the right position address.
#[test]
fn only_the_borrower_adds_margin() {
    let (mut env, mm, b, nonce) = opened();
    let other = env.borrower();
    let mut ix = add_margin_ix(&mm, &other.key(), nonce, SOL);
    ix.accounts[2].pubkey = position_key(&mm, &b.key(), nonce);
    assert_err(&env.send(&[ix], &[&other.kp]), E::AccountMismatch);
}

/// `has_one = market`: a position of another market, or this market with another market's
/// position.
#[test]
fn market_and_position_must_match() {
    let (mut env, mm, b, nonce) = opened();
    let mm2 = env.mock_market(MarketSpec::default());
    env.crank(&mm2);
    let mut b2 = env.borrower();
    let (_o, n2, _) = open_ok(&mut env, &mm2, &mut b2, Q);
    // Market 2 named with market 1's position.
    let mut ix = add_margin_ix(&mm, &b.key(), nonce, SOL);
    ix.accounts[1].pubkey = mm2.market();
    assert_err(&env.send(&[ix], &[&b.kp]), E::AccountMismatch);
    // Market 1 named with market 2's position (its own borrower signs).
    let mut ix = add_margin_ix(&mm2, &b2.key(), n2, SOL);
    ix.accounts[1].pubkey = mm.market();
    assert_err(&env.send(&[ix], &[&b2.kp]), E::AccountMismatch);
    ok(add(&mut env, &mm, &b, nonce, SOL));
    ok(add(&mut env, &mm2, &b2, n2, SOL));
}

/// Copies of the market or the position at another address fail their PDA constraints
/// (`Market::is_pda`, `ShortPosition::is_pda`, stored bumps).
#[test]
fn copies_away_from_their_pdas_are_refused() {
    let (mut env, mm, b, nonce) = opened();
    env.check_invariants = false; // the copies are not real positions / markets
    let key = position_key(&mm, &b.key(), nonce);
    let fake_pos = Pubkey::new_unique();
    let acc = env.svm.get_account(&key).unwrap();
    env.svm.set_account(fake_pos, acc).unwrap();
    let mut ix = add_margin_ix(&mm, &b.key(), nonce, SOL);
    ix.accounts[2].pubkey = fake_pos;
    assert_anchor_err(
        &env.send(&[ix], &[&b.kp]),
        anchor_lang::error::ErrorCode::ConstraintSeeds,
    );
    let fake_market = Pubkey::new_unique();
    let acc = env.svm.get_account(&mm.market()).unwrap();
    env.svm.set_account(fake_market, acc).unwrap();
    let mut ix = add_margin_ix(&mm, &b.key(), nonce, SOL);
    ix.accounts[1].pubkey = fake_market;
    assert_anchor_err(
        &env.send(&[ix], &[&b.kp]),
        anchor_lang::error::ErrorCode::ConstraintSeeds,
    );
}

/// A position that is not Open (status 1 = ExitInProgress, reserved for v1.1) is refused.
#[test]
fn a_position_not_open_is_refused() {
    let (mut env, mm, b, nonce) = opened();
    let key = position_key(&mm, &b.key(), nonce);
    let mut p = env.position(&mm, &b.key(), nonce);
    p.status = shortwin::state::POSITION_EXIT_IN_PROGRESS;
    write_position(&mut env, &key, &p);
    assert_err(&add(&mut env, &mm, &b, nonce, SOL), E::AccountMismatch);
}

/// `v` or `m` past `u64::MAX` is `MathOverflow`, and no lamport moves.
#[test]
fn overflow_is_refused_before_any_transfer() {
    let (mut env, mm, b, nonce) = opened();
    let key = position_key(&mm, &b.key(), nonce);
    let p0 = env.position(&mm, &b.key(), nonce);
    // `m` near the top (I2 still holds: it bounds `v`).
    let mut p = p0.clone();
    p.m = u64::MAX - 5;
    write_position(&mut env, &key, &p);
    let before = lamports(&env, &b.key());
    assert_err(&add(&mut env, &mm, &b, nonce, 6), E::MathOverflow);
    assert_eq!(lamports(&env, &b.key()), before);
    ok(add(&mut env, &mm, &b, nonce, 5));
    // `v` near the top breaks I2 on purpose: hooks off.
    env.check_invariants = false;
    let mut p = env.position(&mm, &b.key(), nonce);
    p.m = p0.m;
    p.v = u64::MAX - 5;
    write_position(&mut env, &key, &p);
    assert_err(&add(&mut env, &mm, &b, nonce, 6), E::MathOverflow);
}

/// A borrower that cannot pay: the System transfer fails and nothing changes.
#[test]
fn a_borrower_that_cannot_pay() {
    let (mut env, mm, b, nonce) = opened();
    let p0 = env.position(&mm, &b.key(), nonce);
    let have = lamports(&env, &b.key());
    let res = add(&mut env, &mm, &b, nonce, have + 1);
    assert!(res.is_err());
    assert_eq!(env.position(&mm, &b.key(), nonce), p0);
    assert_eq!(lamports(&env, &b.key()), have);
}

/// A closed position cannot be topped up.
#[test]
fn a_closed_position_cannot_be_topped_up() {
    let (mut env, mm, b, nonce) = opened();
    close_ok(&mut env, &mm, &b, nonce);
    assert_anchor_err(
        &add(&mut env, &mm, &b, nonce, SOL),
        anchor_lang::error::ErrorCode::AccountNotInitialized,
    );
}

/// A market of another layout version is refused (`ConfigVersion`).
#[test]
fn a_market_of_another_version_is_refused() {
    let (mut env, mm, b, nonce) = opened();
    env.check_invariants = false;
    let mut m: Market = env.market(&mm.market());
    m.version += 1;
    let extra =
        lamports(&env, &mm.market()) - env.svm.minimum_balance_for_rent_exemption(Market::SPACE);
    fixtures::set_zc(&mut env.svm, &mm.market(), &m, extra);
    assert_err(&add(&mut env, &mm, &b, nonce, SOL), E::ConfigVersion);
}

/// I14 (ARCH §8.3) for `add_margin`'s named accounts: each one replaced by a foreign key, or
/// omitted, fails; the market passed writable changes nothing (it is never written); trailing
/// accounts carry nothing (the instruction reads no remaining account) and are ignored.
#[test]
fn named_accounts_are_key_checked() {
    let (mut env, mm, b, nonce) = opened();
    let base = add_margin_ix(&mm, &b.key(), nonce, SOL);
    // [borrower, market, position, system_program, event_authority, program]
    assert_eq!(base.accounts.len(), 6);
    for slot in 0..base.accounts.len() {
        let mut ix = base.clone();
        let m = &mut ix.accounts[slot];
        *m = AccountMeta {
            pubkey: Pubkey::new_unique(),
            is_signer: false,
            is_writable: m.is_writable,
        };
        let signers: Vec<&Keypair> = if slot == 0 { vec![] } else { vec![&b.kp] };
        let res = env.send(&[ix], &signers);
        assert!(res.is_err(), "slot {slot} replaced");
        let mut ix = base.clone();
        ix.accounts.remove(slot);
        let res = env.send(&[ix], &signers);
        assert!(res.is_err(), "slot {slot} omitted");
    }
    // Swapped market and position.
    let mut ix = base.clone();
    ix.accounts.swap(1, 2);
    assert!(env.send(&[ix], &[&b.kp]).is_err());
    // The market passed writable: still never written.
    let market0 = raw(&env, &mm.market());
    let mut ix = base.clone();
    ix.accounts[1].is_writable = true;
    ok(env.send(&[ix], &[&b.kp]));
    assert_eq!(raw(&env, &mm.market()), market0);
    // A trailing account is ignored.
    let mut ix = base.clone();
    ix.accounts
        .push(AccountMeta::new(Pubkey::new_unique(), false));
    ok(env.send(&[ix], &[&b.kp]));
    ok(env.send(&[base], &[&b.kp]));
}

/// SOL donated straight to the position (not tracked in `v`) stays untracked; a top-up adds
/// exactly its amount to `v`.
#[test]
fn a_donation_to_the_position_is_not_margin() {
    let (mut env, mm, b, nonce) = opened();
    let key = position_key(&mm, &b.key(), nonce);
    let p0 = env.position(&mm, &b.key(), nonce);
    env.fund(&key, 5 * SOL);
    ok(add(&mut env, &mm, &b, nonce, SOL));
    let p = env.position(&mm, &b.key(), nonce);
    assert_eq!(p.v, p0.v + SOL);
}

/// A tiny xorshift (deterministic, no extra dependency).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A seeded random walk: opens, top-ups, closes, cranks and price moves by several borrowers
/// and an arbitrageur, with every invariant hook (I1–I17 as far as built, and the new I3 / A26
/// rows) after every instruction. Tracks the margin each position received and checks it at
/// every close.
#[test]
fn random_walk_with_top_ups() {
    let seeds: u64 = std::env::var("ADD_MARGIN_WALK_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    for seed in 0..seeds {
        let (mut env, mm) = setup();
        env.crank(&mm);
        let actor = env.actor(&mm, 50_000_000_000_000, 500_000_000_000);
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (seed + 1));
        let mut borrowers: Vec<Borrower> = (0..3).map(|_| env.borrower()).collect();
        // (borrower index, nonce, margin added)
        let mut open: Vec<(usize, u64, u64)> = Vec::new();
        let (mut tops, mut closes) = (0, 0);
        for _ in 0..120 {
            match rng.below(10) {
                0..=1 => {
                    let i = rng.below(3) as usize;
                    let q = Q / 4 + rng.below(Q / 2);
                    let (res, nonce) = env.open(&mm, &mut borrowers[i], q);
                    if res.is_ok() {
                        open.push((i, nonce, 0));
                    }
                }
                2..=4 if !open.is_empty() => {
                    let k = rng.below(open.len() as u64) as usize;
                    let (i, nonce, added) = open[k];
                    let amount = 1 + rng.below(2 * SOL);
                    let before = env.position(&mm, &borrowers[i].key(), nonce);
                    ok(add(&mut env, &mm, &borrowers[i], nonce, amount));
                    let after = env.position(&mm, &borrowers[i].key(), nonce);
                    assert_eq!(after.v, before.v + amount);
                    open[k].2 = added + amount;
                    tops += 1;
                }
                5 if !open.is_empty() => {
                    let k = rng.below(open.len() as u64) as usize;
                    let (i, nonce, added) = open[k];
                    let p = env.position(&mm, &borrowers[i].key(), nonce);
                    assert_eq!(p.v, p.s + p.m, "V = S + M");
                    assert!(p.m >= added);
                    if env.close(&mm, &borrowers[i], nonce).is_ok() {
                        open.swap_remove(k);
                        closes += 1;
                    }
                }
                6..=7 => {
                    let spot = env.spot(&mm);
                    // −4 % … −2 % or +2 % … +4 % (a push to the current spot would round to 0)
                    let pct = [96u128, 97, 98, 102, 103, 104][rng.below(6) as usize];
                    if let Some((dir, amount, push)) =
                        env.push_price_ix(&mm, &actor, spot * pct / 100)
                    {
                        // Fund the input side first, so the walk never runs the actor dry.
                        match dir {
                            Dir::BaseIn => env.fund_base(&mm, &actor, amount),
                            Dir::QuoteIn => mock::wrap(&mut env, &actor.wsol, amount),
                        }
                        ok(env.send(&[push], &[&actor.kp]));
                    }
                }
                _ => {
                    env.warp(1 + rng.below(3) as i64);
                    env.crank(&mm);
                }
            }
        }
        println!(
            "seed {seed}: {tops} top-ups, {closes} closes, {} open, {} hook checkpoints",
            open.len(),
            env.hook_checkpoints
        );
        assert!(tops > 0);
    }
}
