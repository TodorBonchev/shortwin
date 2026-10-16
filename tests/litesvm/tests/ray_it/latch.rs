//! T-LATCH on the dumped ProgramData (ARCH §3.1, §8.2; A7; masks v0.4.1, R4B-7; I17): the real
//! AMM v4 (1.4 MB) and CPMM (0.79 MB) ProgramData accounts at their mainnet deploy slots, which
//! `initialize_config` recorded. A changed deploy slot of **either** Raydium program, once
//! flagged, refuses opens on **both** Raydium venues (a fast path can move a market between
//! them); exits keep working; `revalidate_venue` with the reviewed slot re-enables opens, and
//! only that slot.

use shortwin::constants::{GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4};
use shortwin::events::VenueUpgradeFlagged;
use shortwin_litesvm_tests::fork::{FIXTURE_GUARD_SLOTS, RaydiumProgram};
use shortwin_litesvm_tests::ix;

use crate::common::*;

fn flag(env: &mut Env, venue: u8) -> TxResult {
    env.send(&[ix::flag_venue_upgraded(venue)], &[])
}

fn revalidate(env: &mut Env, venue: u8, slot: u64) -> TxResult {
    let ea = env.pause_authority.insecure_clone();
    env.send(&[ix::revalidate_venue(&ea.pubkey(), venue, slot)], &[&ea])
}

fn try_open(env: &mut Env, rm: &RayMarket, q: u64) -> TxResult {
    let mut b = env.borrower();
    env.open(rm, &mut b, q).0
}

#[test]
fn either_raydium_upgrade_blocks_opens_on_both() {
    for (program, venue) in [
        (RaydiumProgram::AmmV4, GUARD_RAYDIUM_V4),
        (RaydiumProgram::Cpmm, GUARD_RAYDIUM_CPMM),
    ] {
        let mut env = Env::ray_fork();
        let (fart, qf) = add_market(&mut env, FART);
        let (useless, qu) = add_market(&mut env, USELESS);
        let dumped = FIXTURE_GUARD_SLOTS[usize::from(venue)];
        assert_eq!(
            program.program_data().account.data[4..12],
            dumped.to_le_bytes(),
            "the dumped ProgramData carries the recorded deploy slot"
        );
        // The unchanged ProgramData is not an upgrade.
        assert_err(&flag(&mut env, venue), E::VenueNotUpgraded);
        // Positions opened before the upgrade.
        let mut bf = env.borrower();
        let (_, nf, _) = open_ok(&mut env, &fart, &mut bf, qf);
        let mut bu = env.borrower();
        let (_, nu, _) = open_ok(&mut env, &useless, &mut bu, qu);
        // The upgrade and its flag (the real 1.4 MB / 0.79 MB account is read).
        fork::set_deploy_slot(&mut env.svm, program, dumped + 777);
        let meta = ok(flag(&mut env, venue));
        println!(
            "flag_venue_upgraded({program:?}) on the dumped ProgramData: {} CU",
            meta.compute_units_consumed
        );
        let ev: VenueUpgradeFlagged = events::one(&meta);
        assert_eq!((ev.venue, ev.programdata_slot), (venue, dumped + 777));
        assert_err(&try_open(&mut env, &fart, qf), E::VenueUpgraded);
        assert_err(&try_open(&mut env, &useless, qu), E::VenueUpgraded);
        // Exits never read the latch (I10).
        env.warp(1);
        close_ok(&mut env, &fart, &bf, nf);
        close_ok(&mut env, &useless, &bu, nu);
        // A second upgrade during review: the reviewed slot no longer validates.
        fork::set_deploy_slot(&mut env.svm, program, dumped + 999);
        ok(flag(&mut env, venue));
        assert_err(
            &revalidate(&mut env, venue, dumped + 777),
            E::VenueSlotMismatch,
        );
        ok(revalidate(&mut env, venue, dumped + 999));
        assert_eq!(env.venue_guard().upgraded, 0);
        ok(try_open(&mut env, &fart, qf));
        ok(try_open(&mut env, &useless, qu));
    }
}
