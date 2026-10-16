//! The harness itself: program load, clock, token helpers on both programs, and the
//! invariant hooks catching a broken state.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::PauseScope;
use shortwin::state::{MarketVault, PodU128};
use shortwin_litesvm_tests::errors::ok;
use shortwin_litesvm_tests::invariants::{self, check_all};
use shortwin_litesvm_tests::tokens::{self, TokenProgram};
use shortwin_litesvm_tests::{Env, START_SLOT, START_TS, fixtures, ix, mock, pda, read};
use solana_signer::Signer;

#[test]
fn program_loads_with_our_upgrade_authority() {
    let env = Env::new();
    let pd = read::account(&env.svm, &pda::program_data()).unwrap();
    let h = shortwin_venue_state::program_data::read(&pd.owner.to_bytes(), &pd.data).unwrap();
    assert_eq!(
        h.upgrade_authority,
        Some(env.upgrade_authority.pubkey().to_bytes())
    );
    assert!(read::account(&env.svm, &shortwin::ID).unwrap().executable);
}

#[test]
fn clock_is_under_test_control() {
    let mut env = Env::new();
    assert_eq!((env.now(), env.clock().slot), (START_TS, START_SLOT));
    env.warp(10);
    assert_eq!(
        (env.now(), env.clock().slot),
        (START_TS + 10, START_SLOT + 25)
    );
    env.set_clock(START_TS + 86_400, START_SLOT + 216_000);
    assert_eq!(env.now(), START_TS + 86_400);
}

#[test]
fn mints_and_holders_on_both_token_programs() {
    let mut env = Env::new();
    for program in TokenProgram::BOTH {
        let authority = env.funded_keypair();
        let payer = env.payer.insecure_clone();
        let mint = tokens::create_mint(&mut env.svm, &payer, &authority.pubkey(), 6, program);
        assert_eq!(read::account(&env.svm, &mint).unwrap().owner, program.id());
        let h = tokens::holder(&mut env.svm, &payer, &mint, &authority, 5_000, program);
        assert_eq!(read::token_amount(&env.svm, &h.token_account), Some(5_000));
        assert_eq!(read::token_delegate(&env.svm, &h.token_account), None);
    }
}

/// FUZZ-M7-24H-2: a helper that sends around `Env::send` (a litesvm-token builder, an airdrop)
/// may be called twice in a row with the same arguments — and again after the VM is restored
/// from a clone taken just after such a call (the fuzzer's probes) — without LiteSVM refusing
/// the second, byte-identical transaction as `AlreadyProcessed` (a `SyncNative` carries no
/// amount, so any two wraps of one account are identical).
#[test]
fn identical_helper_calls_in_a_row_are_not_duplicates() {
    let mut env = Env::new();
    let payer = env.payer.insecure_clone();
    for program in TokenProgram::BOTH {
        let authority = env.funded_keypair();
        let mint = tokens::create_mint(&mut env.svm, &payer, &authority.pubkey(), 6, program);
        let h = tokens::holder(&mut env.svm, &payer, &mint, &authority, 0, program);
        for _ in 0..2 {
            tokens::mint_to(
                &mut env.svm,
                &payer,
                &mint,
                &h.token_account,
                &authority,
                170_000_001,
                program,
            );
        }
        assert_eq!(
            read::token_amount(&env.svm, &h.token_account),
            Some(340_000_002)
        );
    }
    let owner = env.funded_keypair().pubkey();
    for _ in 0..2 {
        env.fund(&owner, 1_000_000);
    }
    // Two wraps of different amounts: the airdrops differ, the `SyncNative`s do not.
    let wsol = mock::wsol_account(&mut env, &owner, 0);
    for amount in [5_000, 6_000] {
        mock::wrap(&mut env, &wsol, amount);
    }
    // The fuzzer's probe (`wrapped_open`): a send on the live VM, then the VM restored from a
    // clone taken just after a wrap.
    let before = env.svm.clone();
    ok(env.send(&[system_transfer(&payer.pubkey(), &owner, 1)], &[]));
    env.svm = before;
    mock::wrap(&mut env, &wsol, 7_000);
    assert_eq!(read::token_amount(&env.svm, &wsol), Some(18_000));
}

#[test]
fn installed_market_passes_the_invariants() {
    let mut env = Env::initialized();
    for program in TokenProgram::BOTH {
        let (k, _) = env.install_market(program, 6);
        assert!(check_all(&env.svm, &k.market).is_ok());
        assert_eq!(read::token_amount(&env.svm, &k.pool_vault), Some(0));
    }
    assert_eq!(env.tracked_markets().len(), 2);
    env.assert_invariants();
}

#[test]
fn invariant_checker_catches_violations() {
    let mut env = Env::initialized();
    let (k, _) = env.install_market(TokenProgram::Spl, 6);

    // I2: lender_sol recorded without the lamports behind it.
    let mut v: MarketVault = read::zc(&env.svm, &k.market_vault).unwrap();
    v.lender_sol = 1_000_000;
    fixtures::set_zc(&mut env.svm, &k.market_vault, &v, 0);
    let err = check_all(&env.svm, &k.market).unwrap_err();
    assert_eq!(err.invariant, "I2");
    // …and with the lamports it holds again.
    fixtures::set_zc(&mut env.svm, &k.market_vault, &v, 1_000_000);
    assert!(check_all(&env.svm, &k.market).is_ok());

    // I8: the calendar's aggregate out of step with its buckets.
    v.lock_norm = PodU128::new(5);
    fixtures::set_zc(&mut env.svm, &k.market_vault, &v, 1_000_000);
    assert_eq!(check_all(&env.svm, &k.market).unwrap_err().invariant, "I8");
    v.lock_cal[3] = PodU128::new(5);
    fixtures::set_zc(&mut env.svm, &k.market_vault, &v, 1_000_000);
    assert!(check_all(&env.svm, &k.market).is_ok());

    // I1: T above what the pool vault holds.
    let mut m = env.market(&k.market);
    m.tokens_free = 10;
    fixtures::set_zc(&mut env.svm, &k.market, &m, 0);
    assert_eq!(check_all(&env.svm, &k.market).unwrap_err().invariant, "I1");

    // Snapshots see tracked markets.
    let snap = invariants::snapshot(&env.svm, env.tracked_markets());
    assert_eq!(snap.markets.len(), 1);
    assert!(snap.guard.is_some());
}

/// A System Program `Transfer` (the harness has no system-interface dependency).
fn system_transfer(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![AccountMeta::new(*from, true), AccountMeta::new(*to, false)],
        data,
    }
}

/// ARCH §8.3 "after every instruction": a transaction of N instructions is checked N times
/// (each proper prefix on a clone of the VM, then the whole).
#[test]
fn hooks_run_after_every_instruction_of_a_transaction() {
    let mut env = Env::initialized();
    let (k, _) = env.install_market(TokenProgram::Spl, 6);
    let recipient = env.fee_recipient.pubkey();
    let before = env.hook_checkpoints;
    ok(env.send(&[ix::sweep_protocol_fees(&k.market, &recipient)], &[]));
    assert_eq!(env.hook_checkpoints, before + 1);
    let sweeps = vec![ix::sweep_protocol_fees(&k.market, &recipient); 3];
    ok(env.send(&sweeps, &[]));
    assert_eq!(env.hook_checkpoints, before + 4);
}

/// A state that is broken only between two instructions of one transaction is caught: the
/// first instruction leaves `MarketVault` short of its I2 floor (here: the floor was injected
/// short, and the admin instruction leaves it so), the second repairs it with a donation. The
/// end state passes; the check after instruction 1 does not.
#[test]
#[should_panic(expected = "(after instruction 1 of 2 in one transaction)")]
fn a_violation_between_two_instructions_is_caught() {
    let mut env = Env::initialized();
    let (k, _) = env.install_market(TokenProgram::Spl, 6);
    let mut v: MarketVault = read::zc(&env.svm, &k.market_vault).unwrap();
    v.lender_sol = 1_000_000;
    fixtures::set_zc(&mut env.svm, &k.market_vault, &v, 0);
    let pause = env.pause_authority.insecure_clone();
    let payer = env.payer.pubkey();
    let _ = env.send(
        &[
            ix::set_pause(&pause.pubkey(), PauseScope::Global, true, None),
            system_transfer(&payer, &k.market_vault, 1_000_000),
        ],
        &[&pause],
    );
}

/// Markets a transaction names or creates are tracked without the test asking.
#[test]
fn markets_a_transaction_names_or_creates_are_tracked() {
    let mut env = Env::initialized();
    // Created by `create_market`.
    let authority = env.funded_keypair();
    let payer = env.payer.insecure_clone();
    let mint = tokens::create_mint(
        &mut env.svm,
        &payer,
        &authority.pubkey(),
        6,
        TokenProgram::Token2022,
    );
    let creator = env.funded_keypair();
    let create = ix::create_market(
        &creator.pubkey(),
        &mint,
        &TokenProgram::Token2022.id(),
        0,
        &pda::canonical_pool(&mint),
    );
    ok(env.send(&[create], &[&creator]));
    assert_eq!(env.tracked_markets(), &[pda::market(&mint).0]);

    // Written straight into the VM (not through `Env::install_market`), then named.
    let mint = tokens::create_mint(
        &mut env.svm,
        &payer,
        &authority.pubkey(),
        6,
        TokenProgram::Spl,
    );
    let config = env.config();
    let now = env.now();
    let k = fixtures::install_market(
        &mut env.svm,
        &mint,
        6,
        TokenProgram::Spl,
        &payer.pubkey(),
        now,
        &config,
    );
    assert_eq!(env.tracked_markets().len(), 1);
    let recipient = env.fee_recipient.pubkey();
    ok(env.send(&[ix::sweep_protocol_fees(&k.market, &recipient)], &[]));
    assert!(env.tracked_markets().contains(&k.market));
}

// ---- instruction-level hooks (M1-FID-3, M1-FID-5) ----------------------------------------

/// A funded market: one lender with 1,000,000 raw units deposited.
fn funded(
    env: &mut Env,
) -> (
    fixtures::MarketKeys,
    ix::LenderKeys,
    solana_keypair::Keypair,
) {
    let (k, authority) = env.install_market(TokenProgram::Spl, 6);
    let payer = env.payer.insecure_clone();
    let h = tokens::holder(
        &mut env.svm,
        &payer,
        &k.mint,
        &authority,
        1_000_000,
        TokenProgram::Spl,
    );
    let lk = ix::LenderKeys {
        owner: h.owner.pubkey(),
        mint: k.mint,
        token_program: k.token_program,
        owner_token_account: h.token_account,
    };
    let owner = h.owner.insecure_clone();
    ok(env.send(&[ix::deposit(&lk, 1_000_000, 0)], &[&owner]));
    (k, lk, owner)
}

/// M1-FID-5: "admin-signed" (I4) is decided per instruction, from its own signer metas: a
/// lender's withdrawal in a transaction an admin co-signs for `set_pause` is the lender's, and
/// passes in either order (the review's probe panicked with "I4 violated").
#[test]
fn i4_is_decided_per_instruction() {
    let mut env = Env::initialized();
    let (_k, lk, owner) = funded(&mut env);
    let pause = env.pause_authority.insecure_clone();
    ok(env.send(
        &[
            ix::set_pause(&pause.pubkey(), PauseScope::Global, true, None),
            ix::withdraw(&lk, 1_000, 0),
        ],
        &[&pause, &owner],
    ));
    ok(env.send(
        &[
            ix::withdraw(&lk, 1_000, 0),
            ix::set_pause(&pause.pubkey(), PauseScope::Global, false, None),
        ],
        &[&pause, &owner],
    ));
    // The per-instruction signer test itself.
    let admins = env.admin_keys();
    assert!(invariants::is_admin_signed(
        &ix::set_pause(&pause.pubkey(), PauseScope::Global, true, None),
        &admins
    ));
    assert!(!invariants::is_admin_signed(
        &ix::withdraw(&lk, 1_000, 0),
        &admins
    ));
}

/// M1-FID-5: an admin-signed instruction that did move funds is still caught — I4 on the
/// state just before it (here a lender's balance drop injected between the snapshot and the
/// check stands in for such an instruction).
#[test]
fn i4_catches_an_admin_instruction_that_moves_funds() {
    let mut env = Env::initialized();
    let (k, _lk, _owner) = funded(&mut env);
    let pre = invariants::snapshot(&env.svm, &[k.market]);
    let mut acc = env.svm.get_account(&k.pool_vault).unwrap();
    let amount = u64::from_le_bytes(acc.data[64..72].try_into().unwrap());
    acc.data[64..72].copy_from_slice(&(amount - 1).to_le_bytes());
    env.svm.set_account(k.pool_vault, acc).unwrap();
    let ev = invariants::IxEvents::default();
    let v = invariants::check_ix(&env.svm, &pre, true, &ev).unwrap_err();
    assert_eq!(v.invariant, "I4");
    assert!(invariants::check_ix(&env.svm, &pre, false, &ev).is_ok());
}

/// An `Open` event (the M2 event I17 reads) on `venue`.
fn open_on(market: Pubkey, venue: shortwin::state::VenueKind) -> shortwin::events::Open {
    shortwin::events::Open {
        market,
        slot: 0,
        ts: 0,
        position: Pubkey::new_unique(),
        borrower: Pubkey::new_unique(),
        nonce: 0,
        q: 1,
        s: 1,
        m: 1,
        v: 2,
        n_open: 1,
        fee_open: 0,
        tier: 3,
        entry_price: 1,
        spot_wad: 1,
        ema_wad: 1,
        b_spot: 1,
        b_ema: 1,
        deadline: 0,
        venue: venue as u8,
        venue_rent: 0,
    }
}

/// M1-FID-3: I17 reads the latch **just before** the open's instruction. After
/// `flag_venue_upgraded` (an earlier instruction of the same transaction, or an earlier
/// transaction) an open on a masked venue is a violation; after `revalidate_venue` it is not.
/// `Env::send` hands instruction `k` the state after instructions `1..k−1`.
#[test]
fn i17_reads_the_latch_just_before_each_instruction() {
    use shortwin::state::VenueKind;
    let mut env = Env::initialized();
    env.set_all_venue_program_data();
    let clear = invariants::snapshot(&env.svm, &[]);
    let pause = env.pause_authority.insecure_clone();
    let flagged_slot = shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS[0] + 1;
    env.set_venue_program_data(0, flagged_slot);
    // Flag and revalidate in one transaction: each instruction is checked on its own pre-state.
    ok(env.send(
        &[
            ix::flag_venue_upgraded(0),
            ix::revalidate_venue(&pause.pubkey(), 0, flagged_slot),
        ],
        &[&pause],
    ));
    assert_eq!(env.venue_guard().upgraded, 0);
    env.set_venue_program_data(0, flagged_slot + 1);
    ok(env.send(&[ix::flag_venue_upgraded(0)], &[]));
    let flagged = invariants::snapshot(&env.svm, &[]);
    let market = Pubkey::new_unique();
    let ev = |venue| invariants::IxEvents {
        opens: vec![open_on(market, venue)],
        ..Default::default()
    };
    // pump is in the curve's and not in Raydium's mask.
    let v = invariants::check_ix(&env.svm, &flagged, false, &ev(VenueKind::PumpCurve)).unwrap_err();
    assert_eq!(v.invariant, "I17");
    assert!(invariants::check_ix(&env.svm, &flagged, false, &ev(VenueKind::RaydiumV4)).is_ok());
    assert!(invariants::check_ix(&env.svm, &clear, false, &ev(VenueKind::PumpCurve)).is_ok());
}

/// M1-FID-3 / M1-FID-4: a lender's deposit and withdrawal in one transaction are each checked
/// against the state just before them (the withdrawal against the post-deposit state, where
/// its position exists; the old transaction-level hook skipped any transaction with two lender
/// events).
#[test]
fn a_bundle_is_checked_instruction_by_instruction() {
    let mut env = Env::initialized();
    let (k, authority) = env.install_market(TokenProgram::Spl, 6);
    let payer = env.payer.insecure_clone();
    let h = tokens::holder(
        &mut env.svm,
        &payer,
        &k.mint,
        &authority,
        1_000_000,
        TokenProgram::Spl,
    );
    let owner = h.owner.insecure_clone();
    let lk = ix::LenderKeys {
        owner: owner.pubkey(),
        mint: k.mint,
        token_program: k.token_program,
        owner_token_account: h.token_account,
    };
    let before = env.hook_checkpoints;
    ok(env.send(
        &[
            ix::deposit(&lk, 1_000_000, 0),
            ix::withdraw(&lk, 400_000_000, 0),
            ix::withdraw(&lk, 600_000_000, 0),
        ],
        &[&owner],
    ));
    assert_eq!(env.hook_checkpoints, before + 3);
    assert!(env.lender(&k.market, &owner.pubkey()).is_none(), "closed");
}
