//! T-LATCH (ARCH §8.2; M1 with a mock ProgramData, M3 on the dumped accounts):
//! `flag_venue_upgraded` sets the bit only on a changed deploy slot and refreshes
//! `flagged_slot` on a second change while set; `revalidate_venue(venue, expected_slot)` fails
//! on any other slot; the masks (a pAMM upgrade blocks curve opens, a CPMM upgrade blocks AMM v4
//! opens); no write when unchanged. "Opens refused" itself is asserted by I17 once
//! `open_short` exists (M2/M3); here the masks are checked on the on-chain latch state.

use anchor_lang::error::ErrorCode as AnchorErrorCode;
use anchor_lang::prelude::Pubkey;
use shortwin::constants::{
    GUARD_JUPITER, GUARD_PUMP, GUARD_PUMP_AMM, GUARD_PUMP_FEES, GUARD_RAYDIUM_CPMM,
    GUARD_RAYDIUM_V4, VENUE_PROGRAM_DATA,
};
use shortwin::errors::ShortwinError;
use shortwin::events::{VenueRevalidated, VenueUpgradeFlagged};
use shortwin::state::VenueKind;
use shortwin_litesvm_tests::errors::{assert_anchor_err, assert_err, ok};
use shortwin_litesvm_tests::{Env, TxResult, events, fixtures, ix, pda, read};
use solana_account::Account;
use solana_signer::Signer;

fn flag(env: &mut Env, venue: u8) -> TxResult {
    env.send(&[ix::flag_venue_upgraded(venue)], &[])
}

fn revalidate(env: &mut Env, venue: u8, expected_slot: u64) -> TxResult {
    let pause = env.pause_authority.insecure_clone();
    env.send(
        &[ix::revalidate_venue(&pause.pubkey(), venue, expected_slot)],
        &[&pause],
    )
}

fn latched_env() -> Env {
    let mut env = Env::initialized();
    env.set_all_venue_program_data();
    env
}

#[test]
fn unchanged_slot_is_not_an_upgrade_and_writes_nothing() {
    let mut env = latched_env();
    for venue in 0..6u8 {
        let before = read::account(&env.svm, &pda::venue_guard()).map(|a| a.data);
        assert_err(&flag(&mut env, venue), ShortwinError::VenueNotUpgraded);
        let after = read::account(&env.svm, &pda::venue_guard()).map(|a| a.data);
        assert_eq!(before, after, "venue {venue}: the guard must not change");
    }
    assert_eq!(env.venue_guard().upgraded, 0);
}

#[test]
fn upgrade_flags_refreshes_and_revalidates() {
    let mut env = latched_env();
    let venue = GUARD_PUMP;
    let i = usize::from(venue);
    let old = shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS[i];

    // First upgrade: anyone flags it.
    env.set_venue_program_data(venue, old + 1_000);
    let t1 = env.now();
    let meta = ok(flag(&mut env, venue));
    let e = events::one::<VenueUpgradeFlagged>(&meta);
    assert_eq!(
        (e.venue, e.guard_slot, e.programdata_slot, e.flagged_at),
        (venue, old, old + 1_000, t1)
    );
    let g = env.venue_guard();
    assert!(g.is_flagged(venue));
    assert_eq!((g.flagged_slot[i], g.flagged_at[i]), (old + 1_000, t1));
    assert_eq!(
        g.guard_slot[i], old,
        "the validated slot is kept until revalidation"
    );

    // Same slot again while flagged: nothing to do.
    assert_err(&flag(&mut env, venue), ShortwinError::VenueNotUpgraded);

    // A second upgrade during review is visible.
    env.warp(30);
    env.set_venue_program_data(venue, old + 2_000);
    let t2 = env.now();
    let meta = ok(flag(&mut env, venue));
    let e = events::one::<VenueUpgradeFlagged>(&meta);
    assert_eq!((e.programdata_slot, e.flagged_at), (old + 2_000, t2));
    let g = env.venue_guard();
    assert!(g.is_flagged(venue));
    assert_eq!((g.flagged_slot[i], g.flagged_at[i]), (old + 2_000, t2));

    // The reviewers validated the first upgrade: that slot no longer matches.
    assert_err(
        &revalidate(&mut env, venue, old + 1_000),
        ShortwinError::VenueSlotMismatch,
    );
    assert!(env.venue_guard().is_flagged(venue));

    // Revalidating the slot actually deployed clears the bit.
    let meta = ok(revalidate(&mut env, venue, old + 2_000));
    let e = events::one::<VenueRevalidated>(&meta);
    assert_eq!(
        (
            e.venue,
            e.guard_slot,
            e.programdata_slot,
            e.expected_slot,
            e.flagged_at
        ),
        (venue, old, old + 2_000, old + 2_000, t2)
    );
    let g = env.venue_guard();
    assert!(!g.is_flagged(venue));
    assert_eq!(g.guard_slot[i], old + 2_000);

    // Now the deployed slot is the validated one: no upgrade.
    assert_err(&flag(&mut env, venue), ShortwinError::VenueNotUpgraded);
}

#[test]
fn an_upgrade_landing_between_review_and_execution_is_not_validated() {
    let mut env = latched_env();
    let venue = GUARD_RAYDIUM_CPMM;
    let old = shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
    env.set_venue_program_data(venue, old + 10);
    ok(flag(&mut env, venue));
    // The Squads proposal names old + 10; another upgrade lands before it executes.
    env.set_venue_program_data(venue, old + 20);
    assert_err(
        &revalidate(&mut env, venue, old + 10),
        ShortwinError::VenueSlotMismatch,
    );
    assert!(env.venue_guard().is_flagged(venue));
}

#[test]
fn only_the_emergency_vault_revalidates() {
    let mut env = latched_env();
    let venue = GUARD_PUMP_AMM;
    let slot = shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS[usize::from(venue)] + 5;
    env.set_venue_program_data(venue, slot);
    ok(flag(&mut env, venue));
    for signer in [
        env.governance.insecure_clone(),
        env.upgrade_authority.insecure_clone(),
        env.funded_keypair(),
    ] {
        let res = env.send(
            &[ix::revalidate_venue(&signer.pubkey(), venue, slot)],
            &[&signer],
        );
        assert_err(&res, ShortwinError::Unauthorized);
    }
    assert!(env.venue_guard().is_flagged(venue));
    ok(revalidate(&mut env, venue, slot));
}

#[test]
fn masks_follow_arch_v041() {
    let mut env = latched_env();
    // A pAMM upgrade blocks curve opens (a curve position exits on PumpSwap) and PumpSwap opens,
    // not Raydium.
    let venue = GUARD_PUMP_AMM;
    env.set_venue_program_data(venue, 1);
    ok(flag(&mut env, venue));
    let g = env.venue_guard();
    assert!(g.blocks(VenueKind::PumpCurve.guard_mask()));
    assert!(g.blocks(VenueKind::PumpSwap.guard_mask()));
    assert!(!g.blocks(VenueKind::RaydiumV4.guard_mask()));
    assert!(!g.blocks(VenueKind::RaydiumCpmm.guard_mask()));
    assert!(!g.blocks(VenueKind::None.guard_mask()));
    ok(revalidate(&mut env, venue, 1));

    // A CPMM upgrade blocks AMM v4 opens too (a fast path can move a market between them).
    let venue = GUARD_RAYDIUM_CPMM;
    env.set_venue_program_data(venue, 2);
    ok(flag(&mut env, venue));
    let g = env.venue_guard();
    assert!(g.blocks(VenueKind::RaydiumV4.guard_mask()));
    assert!(g.blocks(VenueKind::RaydiumCpmm.guard_mask()));
    assert!(!g.blocks(VenueKind::PumpCurve.guard_mask()));
    ok(revalidate(&mut env, venue, 2));

    // pfee blocks both pump venues; pump itself only the curve; Jupiter (v1.1) none of the MVP
    // venues.
    for (venue, curve, swap) in [
        (GUARD_PUMP_FEES, true, true),
        (GUARD_PUMP, true, false),
        (GUARD_JUPITER, false, false),
        (GUARD_RAYDIUM_V4, false, false),
    ] {
        env.set_venue_program_data(venue, 3);
        ok(flag(&mut env, venue));
        let g = env.venue_guard();
        assert_eq!(
            g.blocks(VenueKind::PumpCurve.guard_mask()),
            curve,
            "{venue}"
        );
        assert_eq!(g.blocks(VenueKind::PumpSwap.guard_mask()), swap, "{venue}");
        ok(revalidate(&mut env, venue, 3));
        assert_eq!(env.venue_guard().upgraded, 0);
    }
}

#[test]
fn every_venue_program_latches_independently() {
    let mut env = latched_env();
    for venue in 0..6u8 {
        env.set_venue_program_data(venue, 7 + u64::from(venue));
        ok(flag(&mut env, venue));
    }
    assert_eq!(env.venue_guard().upgraded, 0b11_1111);
    ok(revalidate(
        &mut env,
        GUARD_PUMP_FEES,
        7 + u64::from(GUARD_PUMP_FEES),
    ));
    assert_eq!(env.venue_guard().upgraded, 0b11_1011);
}

#[test]
fn programdata_account_is_key_owner_and_tag_checked() {
    let mut env = latched_env();
    // Another venue's ProgramData (a real, changed one) is refused for this venue.
    env.set_venue_program_data(GUARD_PUMP_AMM, 1);
    let res = env.send(
        &[ix::flag_venue_upgraded_with(
            GUARD_PUMP,
            VENUE_PROGRAM_DATA[usize::from(GUARD_PUMP_AMM)],
        )],
        &[],
    );
    assert_err(&res, ShortwinError::VenueAccountMismatch);

    // A look-alike ProgramData at a random address is refused.
    let fake = Pubkey::new_unique();
    fixtures::set_program_data(&mut env.svm, &fake, 1, None, 0);
    let res = env.send(&[ix::flag_venue_upgraded_with(GUARD_PUMP, fake)], &[]);
    assert_err(&res, ShortwinError::VenueAccountMismatch);

    // The right address but not owned by the upgradeable loader.
    let key = VENUE_PROGRAM_DATA[usize::from(GUARD_PUMP)];
    let header = shortwin_venue_state::program_data::encode_header(1, None);
    let lamports = env.svm.minimum_balance_for_rent_exemption(header.len());
    env.svm
        .set_account(
            key,
            Account {
                lamports,
                data: header.to_vec(),
                owner: anchor_lang::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    assert_err(
        &flag(&mut env, GUARD_PUMP),
        ShortwinError::VenueAccountMismatch,
    );

    // The right address and owner but not a ProgramData (tag 2 = Program).
    let mut data = header.to_vec();
    data[0] = 2;
    env.svm
        .set_account(
            key,
            Account {
                lamports,
                data,
                owner: anchor_lang::solana_program::bpf_loader_upgradeable::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    assert_err(
        &flag(&mut env, GUARD_PUMP),
        ShortwinError::VenueAccountMismatch,
    );

    // A venue index outside the six.
    let res = env.send(
        &[ix::flag_venue_upgraded_with(6, VENUE_PROGRAM_DATA[0])],
        &[],
    );
    assert_err(&res, ShortwinError::InvalidParams);

    // A missing ProgramData (never created) cannot be read.
    let mut env = Env::initialized();
    let res = flag(&mut env, GUARD_PUMP);
    assert!(res.is_err());
    // The wrong guard account is refused by its seeds.
    let mut ix = ix::flag_venue_upgraded(GUARD_PUMP);
    ix.accounts[0].pubkey = pda::config();
    let res = env.send(&[ix], &[]);
    assert_anchor_err(&res, AnchorErrorCode::AccountDiscriminatorMismatch);
}
