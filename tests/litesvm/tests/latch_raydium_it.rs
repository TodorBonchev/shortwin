//! T-LATCH on the dumped ProgramData (ARCH §2.3.2, §3.1 "Venue-upgrade latch", §5.5, §6.5, §8.2,
//! §9 M3; A7, R4B-7; I17; U5, U26), on the mainnet fork of `shortwin_litesvm_tests::fork`: the
//! real AMM v4 (1,406,429 B) and CPMM (793,869 B) ProgramData accounts, dumped at their mainnet
//! deploy slots, which `initialize_config` recorded as `guard_slot` (`fork::FIXTURE_GUARD_SLOTS`:
//! CPMM is its 2026-10 redeploy, 452,376,460).
//!
//! The upgrade mainnet itself made is replayed too (M10 WP-2): CPMM's 2026-09-10 build
//! (445,763,504, the slot `VENUE_GUARD_SLOTS_2026_09_27` names) upgraded by the loader to the
//! redeploy's ELF, which latches Raydium opens until Emergency revalidates the loader's slot.
//!
//! An upgrade here is a **real** one: the upgradeable loader's own `Upgrade` instruction, with a
//! buffer holding the dumped ELF and the program's real upgrade authority (the Squads v4 vault
//! `FytDrVz…`, R-RAY §1.1) as signer — the one key no test holds, so its signature is forged
//! (`fork::admin_send`: LiteSVM's signature check is off for that one transaction; the hooks
//! run). The loader itself writes the new deploy slot into the ProgramData header, as on
//! mainnet, and the program keeps running (the same bytes, redeployed).
//!
//! What it settles:
//!
//! - the dumped headers are the ones `VenueGuard` records, at the compile-time addresses, and an
//!   unchanged header is not an upgrade (no write);
//! - after an upgrade of **either** Raydium program, `flag_venue_upgraded` (anyone) sets the bit
//!   and records the loader's deploy slot; opens are refused on **every** Raydium market — AMM
//!   v4, CPMM, CPMM with a Token-2022 base — (mask R4B-7), while cranks, closes, top-ups,
//!   `sol_settle`, a liquidation after a real pump (swapping on the upgraded program) and an
//!   expiry keep working (I10: exits never read the latch);
//! - `revalidate_venue(venue, expected_slot)` clears it only for the slot the loader wrote; a
//!   second upgrade during review is flagged again and invalidates the reviewed slot, also when
//!   it lands unflagged between review and execution;
//! - both Raydium bits must be clear before a Raydium market opens again; pump-side and Jupiter
//!   upgrades never block Raydium opens;
//! - the ProgramData account is checked by address, owner and tag on the real accounts, for both
//!   guard indices (CPMM's forged after a real upgrade: M10 G-A7);
//! - the residual the design discloses (ARCH §3.1, §8.5): an open that lands after the upgrade
//!   and before the flag is accepted; `[flag, open]` in one transaction is not;
//! - U5: `flag_venue_upgraded` and `revalidate_venue` cost the same CU on the real 1.4 MB and
//!   0.79 MB accounts and on a 10 MiB ProgramData (pump's size), far inside §6.1's 400 k;
//! - AIR-01: the loader's permissionless `ExtendProgram` (a stranger's, alone or bundled with the
//!   flag) is not an upgrade once the Emergency revalidated the deploy naming its trimmed ELF's
//!   sha256 (the fixtures' OtterSec hashes), and does not refuse a revalidation staged before it;
//!   an upgrade next to an extension (a same-size changed ELF, CPMM's larger 2026-10 ELF) still
//!   latches; the hash path's CU (≈ ELF/2 + tail/250) stays inside the transaction limit.
//!
//! Every transaction goes through `Env::send` (or `fork::admin_send`): the invariant hooks run
//! after each one (I17 reads each `Open` against the latch just before its instruction).

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::types::TransactionMetadata;
use shortwin::constants::{
    GUARD_JUPITER, GUARD_PUMP, GUARD_PUMP_AMM, GUARD_PUMP_FEES, GUARD_RAYDIUM_CPMM,
    GUARD_RAYDIUM_V4, VENUE_GUARD_SLOTS_2026_09_27, VENUE_PROGRAM_DATA,
};
use shortwin::errors::ShortwinError as E;
use shortwin::events::{
    Close, Expire, Liquidate, MarginAdded, Open, VenueRevalidated, VenueUpgradeFlagged,
};
use shortwin::instructions::flag_venue_upgraded::trimmed_elf_hash_of;
use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, add_margin_ix, expire_ix, liquidate_ix, open_short_ix, sol_settle_ix,
};
use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
use shortwin_litesvm_tests::fork::{
    self, CpmmDeploy, CpmmFixture, FIXTURE_GUARD_SLOTS, RaydiumProgram, V4Fixture,
};
use shortwin_litesvm_tests::raydium::{RayFixture, RayMarket, RaySpec};
use shortwin_litesvm_tests::{Env, TxResult, events, fixtures, ix, pda, read};
use shortwin_math::fees::Direction;
use shortwin_venue_state::program_data;
use solana_account::Account;
use solana_signer::Signer;

const FART: RayFixture = RayFixture::V4(V4Fixture::Fartcoin);
const USELESS: RayFixture = RayFixture::Cpmm(CpmmFixture::Useless);
const T22: RayFixture = RayFixture::Cpmm(CpmmFixture::Token2022);

/// The Raydium upgrade authority: the Squads v4 vault of both programs (R-RAY §1.1).
const RAYDIUM_UPGRADE_AUTHORITY: Pubkey =
    Pubkey::from_str_const("FytDrVzDybM1TwFQPGb8qaxZR7dBCzNeqT3vtQsceZQK");

/// The ProgramData sizes of ARCH §6.5 [V9].
const V4_PROGRAM_DATA_LEN: usize = 1_406_429;
const CPMM_PROGRAM_DATA_LEN: usize = 793_869;
/// Pump's and PumpSwap's ProgramData size (ARCH §6.5): the U5 worst case.
const PUMP_PROGRAM_DATA_LEN: usize = 10_485_760;
/// ARCH §6.1's PROPOSED transaction limit for `flag_venue_upgraded` / `revalidate_venue`.
const LATCH_CU_LIMIT: u64 = 400_000;

fn guard_index(program: RaydiumProgram) -> u8 {
    match program {
        RaydiumProgram::AmmV4 => GUARD_RAYDIUM_V4,
        RaydiumProgram::Cpmm => GUARD_RAYDIUM_CPMM,
    }
}

/// The market on the upgraded program's own venue (its swaps run the redeployed ELF).
fn own_fixture(program: RaydiumProgram) -> RayFixture {
    match program {
        RaydiumProgram::AmmV4 => FART,
        RaydiumProgram::Cpmm => USELESS,
    }
}

// ---- helpers --------------------------------------------------------------------------------

/// The ProgramData header as the loader wrote it.
fn header(env: &Env, program: RaydiumProgram) -> program_data::ProgramDataHeader {
    let a = read::account(&env.svm, &program.program_data().address).expect("ProgramData");
    program_data::read(&a.owner.to_bytes(), &a.data).expect("a ProgramData header")
}

/// `SysvarRent111111111111111111111111111111111`.
const SYSVAR_RENT: Pubkey = Pubkey::from_str_const("SysvarRent111111111111111111111111111111111");
/// `SysvarC1ock11111111111111111111111111111111`.
const SYSVAR_CLOCK: Pubkey = Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111");

/// Sends the upgradeable loader's own `Upgrade` of `program` to the dumped ELF: a new buffer
/// (`UpgradeableLoaderState::Buffer { authority: Some(authority) }` ‖ ELF), then `Upgrade`
/// (bincode tag 3, the loader's account order) signed by the program's upgrade authority, whose
/// signature is forged (`fork::admin_send`). Returns the result and the buffer.
fn send_upgrade(env: &mut Env, program: RaydiumProgram) -> (TxResult, Pubkey) {
    send_upgrade_elf(env, program, program.elf())
}

/// [`send_upgrade`] to `elf` instead of the dumped ELF.
fn send_upgrade_elf(env: &mut Env, program: RaydiumProgram, elf: &[u8]) -> (TxResult, Pubkey) {
    let authority = header(env, program)
        .upgrade_authority
        .map(Pubkey::new_from_array)
        .expect("an upgradeable program");
    let mut data = vec![1, 0, 0, 0, 1];
    data.extend_from_slice(authority.as_ref());
    data.extend_from_slice(elf);
    let buffer = Pubkey::new_unique();
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            buffer,
            Account {
                lamports,
                data,
                owner: bpf_loader_upgradeable::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .expect("buffer");
    let spill = env.funded_keypair().pubkey();
    let upgrade = Instruction {
        program_id: bpf_loader_upgradeable::ID,
        accounts: vec![
            AccountMeta::new(program.program_data().address, false),
            AccountMeta::new(program.id(), false),
            AccountMeta::new(buffer, false),
            AccountMeta::new(spill, false),
            AccountMeta::new_readonly(SYSVAR_RENT, false),
            AccountMeta::new_readonly(SYSVAR_CLOCK, false),
            AccountMeta::new_readonly(authority, true),
        ],
        data: vec![3, 0, 0, 0],
    };
    (fork::admin_send(env, upgrade, &[authority]), buffer)
}

/// A **real** upgrade of `program` by the upgradeable loader ([`send_upgrade`]): the loader
/// writes the current slot into the ProgramData header and the ELF after it. Returns that slot.
fn loader_upgrade(env: &mut Env, program: RaydiumProgram) -> u64 {
    loader_upgrade_elf(env, program, program.elf())
}

/// [`loader_upgrade`] to `elf` instead of the dumped ELF (the loader zeroes the rest of the
/// account).
fn loader_upgrade_elf(env: &mut Env, program: RaydiumProgram, elf: &[u8]) -> u64 {
    let slot = env.clock().slot;
    let authority = header(env, program).upgrade_authority;
    let len = read::account(&env.svm, &program.program_data().address)
        .unwrap()
        .data
        .len();
    let (res, buffer) = send_upgrade_elf(env, program, elf);
    ok(res);
    let h = header(env, program);
    assert_eq!(h.slot, slot, "the loader records the upgrade's slot");
    assert_eq!(h.upgrade_authority, authority, "the authority is kept");
    let pd = read::account(&env.svm, &program.program_data().address).unwrap();
    assert_eq!(pd.data.len(), len, "an upgrade never resizes the account");
    let start = program_data::HEADER_LEN;
    assert_eq!(
        &pd.data[start..start + elf.len()],
        elf,
        "the redeployed ELF"
    );
    assert!(
        pd.data[start + elf.len()..].iter().all(|&b| b == 0),
        "the rest of the account zeroed"
    );
    assert!(
        read::account(&env.svm, &buffer).is_none_or(|b| b.data.len() <= start),
        "the loader emptied the buffer"
    );
    slot
}

fn flag(env: &mut Env, venue: u8) -> TxResult {
    env.send(&[ix::flag_venue_upgraded(venue)], &[])
}

fn revalidate(env: &mut Env, venue: u8, slot: u64) -> TxResult {
    let ea = env.pause_authority.insecure_clone();
    env.send(&[ix::revalidate_venue(&ea.pubkey(), venue, slot)], &[&ea])
}

/// `SetComputeUnitLimit(1.4 M)`, then `ix`: once the account grew since the review, the latch
/// instructions hash the venue's trimmed ELF (AIR-01; half a CU per byte, CPMM's 0.72 MB ELF
/// ≈ 0.36 M CU), above the 200 k default.
fn with_hash_budget(ix: Instruction) -> Vec<Instruction> {
    vec![shortwin_litesvm_tests::pump::cu_limit_ix(1_400_000), ix]
}

/// `flag_venue_upgraded(venue)` with [`with_hash_budget`].
fn flag_hashing(env: &mut Env, venue: u8) -> TxResult {
    env.send(&with_hash_budget(ix::flag_venue_upgraded(venue)), &[])
}

/// The sha256 of `program`'s trimmed ELF as its ProgramData holds it now — what
/// `solana-verify get-program-hash` prints, and what the Emergency names when it revalidates the
/// deploy it reviewed (AIR-01).
fn reviewed_hash(env: &Env, program: RaydiumProgram) -> [u8; 32] {
    let a = read::account(&env.svm, &program.program_data().address).expect("ProgramData");
    trimmed_elf_hash_of(&a.owner.to_bytes(), &a.data)
        .expect("a ProgramData")
        .expect("small enough to hash")
}

/// Lower-case hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The Emergency's `revalidate_venue` of the deploy at `slot` whose trimmed ELF hashes to
/// `reviewed` ([`reviewed_hash`]), with [`with_hash_budget`].
fn revalidate_reviewed(
    env: &mut Env,
    program: RaydiumProgram,
    slot: u64,
    reviewed: [u8; 32],
) -> TxResult {
    let ea = env.pause_authority.insecure_clone();
    let ix = ix::revalidate_venue_reviewed(&ea.pubkey(), guard_index(program), slot, reviewed);
    env.send(&with_hash_budget(ix), &[&ea])
}

/// A stranger's `ExtendProgram` of `program`'s ProgramData by `additional_bytes` (≥ 10 KiB under
/// SIMD-0431; `fork::extend_program_ix`): the loader appends zeros, keeps the ELF and writes the
/// current slot into the header. Returns that slot.
fn stranger_extends(env: &mut Env, program: RaydiumProgram, additional_bytes: u32) -> u64 {
    let key = program.program_data().address;
    let before = read::account(&env.svm, &key).unwrap().data;
    let slot = env.clock().slot;
    let stranger = env.funded_keypair();
    let extend = fork::extend_program_ix(&program.id(), &stranger.pubkey(), additional_bytes);
    ok(env.send(&[extend], &[&stranger]));
    let after = read::account(&env.svm, &key).unwrap().data;
    assert_eq!(
        after.len(),
        before.len() + additional_bytes as usize,
        "the account grew"
    );
    assert_eq!(header(env, program).slot, slot, "the extension's slot");
    let start = program_data::HEADER_LEN;
    assert_eq!(
        &after[start..before.len()],
        &before[start..],
        "the ELF untouched"
    );
    assert!(after[before.len()..].iter().all(|&b| b == 0), "zeros");
    slot
}

/// AMM v4's dumped ELF with one byte of a panic-location string in `.rodata` changed
/// (`program/src/math.rs` → `program/src/meth.rs`): another build of the same size, which the
/// loader deploys (it verifies the code, not the read-only data).
fn v4_elf_one_byte_changed() -> Vec<u8> {
    let mut elf = RaydiumProgram::AmmV4.elf().to_vec();
    let at = elf
        .windows(19)
        .position(|w| w == b"program/src/math.rs")
        .expect("the string")
        + 13;
    assert_eq!(elf[at], b'a');
    elf[at] = b'e';
    elf
}

/// The position size of `ray_it`: `min(X/400, 2 SOL of notional)`.
fn q_of(env: &Env, rm: &RayMarket) -> u64 {
    let (x, y) = rm.reserves(&env.svm).expect("reserves");
    (u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y)) as u64
}

/// Lists `fixture`, lends `40·Q`, waits out the listing window, cranks once.
fn add_market(env: &mut Env, fixture: RayFixture) -> (RayMarket, u64) {
    let pool = env.load_ray_pool(fixture);
    ok(env.list_ray(&pool, fixture.hint()));
    let (x, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let q = (u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y)) as u64;
    let rm = env.ray_market_listed(RaySpec::new(fixture, q.saturating_mul(40)), pool);
    env.crank(&rm);
    assert_eq!(q, q_of(env, &rm));
    (rm, q)
}

/// A fork with markets on AMM v4 (Fartcoin), CPMM (USELESS) and CPMM with a Token-2022 base.
fn three_markets() -> (Env, [(RayMarket, u64); 3]) {
    let mut env = Env::ray_fork();
    let m = [
        add_market(&mut env, FART),
        add_market(&mut env, USELESS),
        add_market(&mut env, T22),
    ];
    (env, m)
}

fn crank_all(env: &mut Env, markets: &[(RayMarket, u64)]) {
    for (rm, _) in markets {
        env.crank(rm);
    }
}

fn try_open(env: &mut Env, rm: &RayMarket, q: u64) -> TxResult {
    let mut b = env.borrower();
    env.open(rm, &mut b, q).0
}

fn open_ok(env: &mut Env, rm: &RayMarket, q: u64) -> (Borrower, u64) {
    let mut b = env.borrower();
    let (res, nonce) = env.open(rm, &mut b, q);
    let meta = ok(res);
    let ev: Open = events::one(&meta);
    assert_eq!(ev.q, q);
    (b, nonce)
}

/// A whale's real buy of `sol` lamports of the token on `rm`'s pool.
fn whale_buy(env: &mut Env, rm: &RayMarket, sol: u64) {
    let (tok, program) = rm.pool.token();
    let whale = fork::trader(env, &tok, program, sol + 1_000_000_000, 0);
    ok(fork::swap(
        env,
        &rm.pool,
        &whale,
        Direction::Buy,
        fork::Swap::ExactIn(sol, 0),
    ));
}

/// Pumps `rm`'s price ≈ 2.2× with a real buy, then cranks second by second until `liquidate`
/// executes (as `ray_it::exits`): the event.
fn liquidate_after_a_pump(env: &mut Env, rm: &RayMarket, b: &Borrower, nonce: u64) -> Liquidate {
    let (_, y) = rm.reserves(&env.svm).unwrap();
    whale_buy(env, rm, y / 1_000 * 484);
    let keeper = env.funded_keypair();
    for _ in 0..40 {
        env.tick(rm, 1);
        let res = env.send(
            &[liquidate_ix(rm, &keeper.pubkey(), &b.key(), nonce)],
            &[&keeper],
        );
        match custom_code(&res) {
            None => return events::one(&ok(res)),
            Some(c) if c == u32::from(E::NotLiquidatable) || c == u32::from(E::ExitBoundNotMet) => {
            }
            Some(c) => panic!("liquidate failed with {c}"),
        }
    }
    panic!("never liquidated");
}

// ---- tests ----------------------------------------------------------------------------------

/// The dumped ProgramData are the real accounts at the compile-time addresses, with the deploy
/// slots and the upgrade authority of ARCH §2.3.2 / R-RAY §1.1 and the sizes of ARCH §6.5;
/// `initialize_config` recorded exactly those slots. An unchanged header is not an upgrade and
/// writes nothing.
#[test]
fn the_dumped_headers_are_the_recorded_slots() {
    let mut env = Env::ray_fork();
    for (program, len) in [
        (RaydiumProgram::AmmV4, V4_PROGRAM_DATA_LEN),
        (RaydiumProgram::Cpmm, CPMM_PROGRAM_DATA_LEN),
    ] {
        let venue = guard_index(program);
        let i = usize::from(venue);
        let key = program.program_data().address;
        assert_eq!(key, VENUE_PROGRAM_DATA[i], "the compile-time address");
        assert_eq!(
            key,
            bpf_loader_upgradeable::get_program_data_address(&program.id()),
            "the program's own ProgramData"
        );
        let a = read::account(&env.svm, &key).unwrap();
        assert_eq!(a.owner, bpf_loader_upgradeable::ID);
        assert_eq!(a.data.len(), len, "{program:?}: ARCH §6.5 size");
        let h = header(&env, program);
        assert_eq!(h.slot, FIXTURE_GUARD_SLOTS[i], "{program:?}");
        assert_eq!(
            h.upgrade_authority,
            Some(RAYDIUM_UPGRADE_AUTHORITY.to_bytes()),
            "{program:?}: the Squads v4 vault"
        );
        assert_eq!(env.venue_guard().guard_slot[i], h.slot, "recorded at init");

        let before = read::account(&env.svm, &pda::venue_guard()).unwrap().data;
        assert_err(&flag(&mut env, venue), E::VenueNotUpgraded);
        assert_eq!(
            read::account(&env.svm, &pda::venue_guard()).unwrap().data,
            before,
            "{program:?}: no write when unchanged"
        );
    }
    assert_eq!(env.venue_guard().upgraded, 0);
}

/// ARCH §3.1 end to end on each Raydium program: a real upgrade, flagged in a later slot by
/// anyone, refuses opens on AMM v4, CPMM and Token-2022 CPMM markets; every exit keeps
/// working, including a liquidation whose buyback swaps on the upgraded program and an expiry;
/// only the loader's slot revalidates, and opens resume.
#[test]
fn a_real_upgrade_latches_opens_on_every_raydium_market() {
    for program in RaydiumProgram::ALL {
        let venue = guard_index(program);
        let i = usize::from(venue);
        let dumped = FIXTURE_GUARD_SLOTS[i];
        let (mut env, markets) = three_markets();
        let [(fart, qf), (useless, qu), (t22, qt)] = &markets;
        let own = markets
            .iter()
            .find(|(rm, _)| rm.fixture == own_fixture(program))
            .unwrap();

        // Positions opened before the upgrade.
        let (b_liq, n_liq) = open_ok(&mut env, &own.0, own.1);
        let (bf, nf) = open_ok(&mut env, fart, *qf);
        let (bu, nu) = open_ok(&mut env, useless, *qu);
        let (bt, nt) = open_ok(&mut env, t22, *qt);

        // The upgrade.
        env.warp(1);
        let s = loader_upgrade(&mut env, program);
        assert_ne!(s, dumped);

        // The residual (ARCH §3.1, §8.5): until someone flags it, opens still land.
        open_ok(&mut env, &own.0, own.1 / 4);

        // Anyone flags it in a later slot (no signer but the fee payer; the keeper's duty 10).
        env.warp(1);
        assert!(env.clock().slot > s, "the flag lands after the upgrade");
        let meta = ok(flag(&mut env, venue));
        let ev: VenueUpgradeFlagged = events::one(&meta);
        let now = env.now();
        assert_eq!(
            (ev.venue, ev.guard_slot, ev.programdata_slot, ev.flagged_at),
            (venue, dumped, s, now)
        );
        let g = env.venue_guard();
        assert!(g.is_flagged(venue));
        assert_eq!((g.flagged_slot[i], g.flagged_at[i]), (s, now));
        assert_eq!(g.guard_slot[i], dumped, "kept until revalidation");
        assert_err(&flag(&mut env, venue), E::VenueNotUpgraded);

        // Opens refused on every Raydium market (the mask holds both programs).
        for (rm, q) in &markets {
            assert_err(&try_open(&mut env, rm, *q), E::VenueUpgraded);
        }

        // Exits never read the latch (I10): cranks record, a top-up, a no-op settlement ...
        crank_all(&mut env, &markets);
        let meta = ok(env.send(
            &[add_margin_ix(fart, &bf.key(), nf, 1_000_000_000)],
            &[&bf.kp],
        ));
        let _: MarginAdded = events::one(&meta);
        let keeper = env.funded_keypair();
        env.tick(useless, 1);
        let settle = sol_settle_ix(&env, useless, &keeper.pubkey(), &bu.key(), nu);
        ok(env.send(&[settle], &[&keeper]));
        // ... a liquidation after a real pump, buying back on the upgraded program ...
        let vault0 = read::token_amount(&env.svm, &own.0.keys.pool_vault).unwrap();
        let liq = liquidate_after_a_pump(&mut env, &own.0, &b_liq, n_liq);
        assert!(liq.buyback_cost > 0);
        assert_eq!(
            read::token_amount(&env.svm, &own.0.keys.pool_vault).unwrap() - vault0,
            own.1,
            "Q bought back into the lending vault"
        );
        assert!(
            read::account(
                &env.svm,
                &pda::position(&own.0.market(), &b_liq.key(), n_liq).0
            )
            .is_none()
        );
        // ... a close on the other program ...
        let (b_close, n_close, rm_close) = if program == RaydiumProgram::AmmV4 {
            (&bu, nu, useless)
        } else {
            (&bf, nf, fart)
        };
        env.warp(1);
        let meta = ok(env.close(rm_close, b_close, n_close));
        let _: Close = events::one(&meta);
        // ... and an expiry (Token-2022 base).
        let pos = env.position(t22, &bt.key(), nt);
        let slot = env.clock().slot;
        env.set_clock(pos.deadline + 1, slot + 1_600_000);
        env.tick(t22, 3);
        let meta = ok(env.send(
            &[expire_ix(t22, &keeper.pubkey(), &bt.key(), nt)],
            &[&keeper],
        ));
        let e: Expire = events::one(&meta);
        assert!(e.buyback_cost > 0 && e.expiry_fee > 0);
        assert!(read::account(&env.svm, &pda::position(&t22.market(), &bt.key(), nt).0).is_none());
        assert!(
            env.venue_guard().is_flagged(venue),
            "exits leave the latch alone"
        );

        // Only the slot the loader wrote revalidates.
        for wrong in [dumped, s + 1, s - 1] {
            assert_err(&revalidate(&mut env, venue, wrong), E::VenueSlotMismatch);
        }
        crank_all(&mut env, &markets);
        let meta = ok(revalidate(&mut env, venue, s));
        let ev: VenueRevalidated = events::one(&meta);
        assert_eq!(
            (
                ev.venue,
                ev.guard_slot,
                ev.programdata_slot,
                ev.expected_slot,
                ev.flagged_at
            ),
            (venue, dumped, s, s, now)
        );
        let g = env.venue_guard();
        assert!(!g.is_flagged(venue));
        assert_eq!(g.guard_slot[i], s);
        assert_err(&flag(&mut env, venue), E::VenueNotUpgraded);
        for (rm, q) in &markets {
            ok(try_open(&mut env, rm, *q / 2));
        }
    }
}

/// R4B-7: a second real upgrade during review refreshes the flag and invalidates the reviewed
/// slot — also when it lands **unflagged** between the review and the Emergency transaction.
#[test]
fn a_second_upgrade_during_review_is_visible() {
    let program = RaydiumProgram::AmmV4;
    let venue = guard_index(program);
    let i = usize::from(venue);
    let (mut env, markets) = three_markets();
    env.warp(1);
    let s1 = loader_upgrade(&mut env, program);
    env.warp(1);
    ok(flag(&mut env, venue));
    let t1 = env.now();

    // The reviewers approve s1; another upgrade lands before their transaction executes.
    env.warp(30);
    let s2 = loader_upgrade(&mut env, program);
    assert!(s2 > s1);
    assert_eq!(env.venue_guard().flagged_slot[i], s1, "not flagged yet");
    assert_err(&revalidate(&mut env, venue, s1), E::VenueSlotMismatch);

    // Flagged again: the refresh is visible.
    env.warp(1);
    let meta = ok(flag(&mut env, venue));
    let t2 = env.now();
    assert!(t2 > t1);
    let ev: VenueUpgradeFlagged = events::one(&meta);
    assert_eq!((ev.programdata_slot, ev.flagged_at), (s2, t2));
    let g = env.venue_guard();
    assert_eq!((g.flagged_slot[i], g.flagged_at[i]), (s2, t2));
    assert_err(&flag(&mut env, venue), E::VenueNotUpgraded);
    crank_all(&mut env, &markets);
    for (rm, q) in &markets {
        assert_err(&try_open(&mut env, rm, *q), E::VenueUpgraded);
    }
    assert_err(&revalidate(&mut env, venue, s1), E::VenueSlotMismatch);
    let ev: VenueRevalidated = events::one(&ok(revalidate(&mut env, venue, s2)));
    assert_eq!((ev.expected_slot, ev.flagged_at), (s2, t2));
    for (rm, q) in &markets {
        ok(try_open(&mut env, rm, *q));
    }
}

/// Both Raydium programs upgraded: revalidating one leaves every Raydium market latched by the
/// other (the mask is both bits, R4B-7); opens resume once both are revalidated.
#[test]
fn both_raydium_bits_must_clear() {
    let (mut env, markets) = three_markets();
    env.warp(1);
    let s_v4 = loader_upgrade(&mut env, RaydiumProgram::AmmV4);
    let s_cp = loader_upgrade(&mut env, RaydiumProgram::Cpmm);
    env.warp(1);
    ok(flag(&mut env, GUARD_RAYDIUM_V4));
    ok(flag(&mut env, GUARD_RAYDIUM_CPMM));
    ok(revalidate(&mut env, GUARD_RAYDIUM_V4, s_v4));
    crank_all(&mut env, &markets);
    for (rm, q) in &markets {
        assert_err(&try_open(&mut env, rm, *q), E::VenueUpgraded);
    }
    ok(revalidate(&mut env, GUARD_RAYDIUM_CPMM, s_cp));
    assert_eq!(env.venue_guard().upgraded, 0);
    for (rm, q) in &markets {
        ok(try_open(&mut env, rm, *q));
    }
}

/// The masks from the Raydium side: pump, PumpSwap, pump-fees and Jupiter upgrades (their
/// ProgramData are not dumped; synthetic headers at the compile-time addresses) set their bits
/// and never block a Raydium open.
#[test]
fn pump_side_upgrades_leave_raydium_opens_alone() {
    let (mut env, markets) = three_markets();
    for venue in [GUARD_PUMP, GUARD_PUMP_AMM, GUARD_PUMP_FEES, GUARD_JUPITER] {
        let slot = FIXTURE_GUARD_SLOTS[usize::from(venue)] + 1;
        env.set_venue_program_data(venue, slot);
        ok(flag(&mut env, venue));
    }
    let bits = (1u32 << GUARD_PUMP)
        | (1 << GUARD_PUMP_AMM)
        | (1 << GUARD_PUMP_FEES)
        | (1 << GUARD_JUPITER);
    assert_eq!(env.venue_guard().upgraded, bits);
    for (rm, q) in &markets {
        ok(try_open(&mut env, rm, *q));
    }
}

/// `flag_venue_upgraded` / `revalidate_venue` take only the venue's own ProgramData, by
/// address, owner and tag, on the real accounts (`VenueAccountMismatch`).
#[test]
fn the_program_data_account_is_checked() {
    let mut env = Env::ray_fork();
    let ea = env.pause_authority.insecure_clone();
    let v4 = RaydiumProgram::AmmV4;
    let cp = RaydiumProgram::Cpmm;
    let s = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_V4)];
    let wrong_keys = [
        cp.program_data().address,
        v4.id(),
        pda::program_data(),
        Pubkey::new_unique(),
    ];
    for key in wrong_keys {
        assert_err(
            &env.send(&[ix::flag_venue_upgraded_with(GUARD_RAYDIUM_V4, key)], &[]),
            E::VenueAccountMismatch,
        );
        assert_err(
            &env.send(
                &[ix::revalidate_venue_with(
                    &ea.pubkey(),
                    GUARD_RAYDIUM_V4,
                    s,
                    key,
                )],
                &[&ea],
            ),
            E::VenueAccountMismatch,
        );
    }
    let key = v4.program_data().address;
    let real = read::account(&env.svm, &key).unwrap();
    // The right address with another owner, or another loader state (a `Buffer` tag).
    let mut forged = real.clone();
    forged.owner = anchor_lang::system_program::ID;
    env.svm.set_account(key, forged).unwrap();
    assert_err(&flag(&mut env, GUARD_RAYDIUM_V4), E::VenueAccountMismatch);
    assert_err(
        &revalidate(&mut env, GUARD_RAYDIUM_V4, s),
        E::VenueAccountMismatch,
    );
    let mut forged = real.clone();
    forged.data[0] = 1;
    env.svm.set_account(key, forged).unwrap();
    assert_err(&flag(&mut env, GUARD_RAYDIUM_V4), E::VenueAccountMismatch);
    assert_err(
        &revalidate(&mut env, GUARD_RAYDIUM_V4, s),
        E::VenueAccountMismatch,
    );
    env.svm.set_account(key, real).unwrap();
    assert_err(&flag(&mut env, GUARD_RAYDIUM_V4), E::VenueNotUpgraded);
    assert_eq!(env.venue_guard().upgraded, 0);
}

/// G-A7 on CPMM (M10; ARCH §3.1): the ProgramData account of `GUARD_RAYDIUM_CPMM` is checked as
/// AMM v4's is. AMM v4's ProgramData, the CPMM program id itself, short.win's own ProgramData and
/// a fresh key are `VenueAccountMismatch` for `flag_venue_upgraded` and `revalidate_venue`; so is
/// the right address after a **real** upgrade by the loader (its header holds the new deploy slot,
/// which the flag would latch) when it is owned by another program or carries another loader
/// state (a `Buffer` tag). The controls: the real account at the fixture's slot is
/// `VenueNotUpgraded`; after the upgrade the real account flags exactly the CPMM bit, and
/// `revalidate_venue` at the loader's slot clears it.
#[test]
fn the_cpmm_program_data_account_is_checked() {
    let mut env = Env::ray_fork();
    let ea = env.pause_authority.insecure_clone();
    let v4 = RaydiumProgram::AmmV4;
    let cp = RaydiumProgram::Cpmm;
    let s = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_CPMM)];
    let wrong_keys = [
        v4.program_data().address,
        cp.id(),
        pda::program_data(),
        Pubkey::new_unique(),
    ];
    for key in wrong_keys {
        assert_err(
            &env.send(
                &[ix::flag_venue_upgraded_with(GUARD_RAYDIUM_CPMM, key)],
                &[],
            ),
            E::VenueAccountMismatch,
        );
        assert_err(
            &env.send(
                &[ix::revalidate_venue_with(
                    &ea.pubkey(),
                    GUARD_RAYDIUM_CPMM,
                    s,
                    key,
                )],
                &[&ea],
            ),
            E::VenueAccountMismatch,
        );
    }
    assert_err(&flag(&mut env, GUARD_RAYDIUM_CPMM), E::VenueNotUpgraded);
    env.warp(1);
    let slot = loader_upgrade(&mut env, cp);
    assert_ne!(slot, s);
    let key = cp.program_data().address;
    let real = read::account(&env.svm, &key).unwrap();
    let mut forged = real.clone();
    forged.owner = anchor_lang::system_program::ID;
    env.svm.set_account(key, forged).unwrap();
    assert_err(&flag(&mut env, GUARD_RAYDIUM_CPMM), E::VenueAccountMismatch);
    assert_err(
        &revalidate(&mut env, GUARD_RAYDIUM_CPMM, slot),
        E::VenueAccountMismatch,
    );
    let mut forged = real.clone();
    forged.data[0] = 1;
    env.svm.set_account(key, forged).unwrap();
    assert_err(&flag(&mut env, GUARD_RAYDIUM_CPMM), E::VenueAccountMismatch);
    assert_err(
        &revalidate(&mut env, GUARD_RAYDIUM_CPMM, slot),
        E::VenueAccountMismatch,
    );
    assert_eq!(env.venue_guard().upgraded, 0, "nothing latched");
    env.svm.set_account(key, real).unwrap();
    ok(flag(&mut env, GUARD_RAYDIUM_CPMM));
    assert_eq!(
        env.venue_guard().upgraded,
        1 << GUARD_RAYDIUM_CPMM,
        "exactly the CPMM bit"
    );
    ok(revalidate(&mut env, GUARD_RAYDIUM_CPMM, slot));
    assert_eq!(env.venue_guard().upgraded, 0);
}

/// The latch is per instruction: in one transaction, `[flag, open]` refuses the open (the whole
/// transaction reverts, flag included), while `[open, flag]` is the residual of ARCH §3.1
/// within one transaction — the open precedes the flag and lands.
#[test]
fn a_flag_and_an_open_in_one_transaction() {
    let (mut env, markets) = three_markets();
    let (rm, q) = &markets[1];
    env.warp(1);
    loader_upgrade(&mut env, RaydiumProgram::Cpmm);
    env.warp(1);
    crank_all(&mut env, &markets);
    let b = env.borrower();
    let open = open_short_ix(
        rm,
        &b.key(),
        0,
        *q,
        0,
        u64::MAX,
        u64::MAX,
        shortwin::venues::ExtrasSel::NONE,
        &[],
    );
    let flag_ix = ix::flag_venue_upgraded(GUARD_RAYDIUM_CPMM);
    let res = env.send(&[flag_ix.clone(), open.clone()], &[&b.kp]);
    assert_err(&res, E::VenueUpgraded);
    assert_eq!(env.venue_guard().upgraded, 0, "reverted with the open");
    let meta = ok(env.send(&[open, flag_ix], &[&b.kp]));
    let _: Open = events::one(&meta);
    let _: VenueUpgradeFlagged = events::one(&meta);
    assert!(env.venue_guard().is_flagged(GUARD_RAYDIUM_CPMM));
    assert_err(&try_open(&mut env, rm, *q), E::VenueUpgraded);
}

/// U5 (ARCH §6.1, §6.5): the latch instructions cost the same on the real 1.4 MB AMM v4 and
/// 0.79 MB CPMM ProgramData as on a 10 MiB one (pump's size): the input-region copy is not
/// metered in CU. Each stays far inside §6.1's 400 k. (Without an extension nothing is hashed;
/// `air01_the_hash_path_fits_the_compute_budget` measures the AIR-01 path.)
#[test]
fn u5_latch_cost_does_not_grow_with_the_program_data() {
    let mut env = Env::ray_fork();
    let ea = env.pause_authority.insecure_clone();
    let mut flags: Vec<(String, u64)> = Vec::new();
    let mut revals: Vec<(String, u64)> = Vec::new();
    let cu = |meta: &TransactionMetadata| meta.compute_units_consumed;
    env.warp(1);
    for program in RaydiumProgram::ALL {
        let s = loader_upgrade(&mut env, program);
        env.warp(1);
        let venue = guard_index(program);
        flags.push((format!("{program:?}"), cu(&ok(flag(&mut env, venue)))));
        revals.push((
            format!("{program:?}"),
            cu(&ok(revalidate(&mut env, venue, s))),
        ));
    }
    // Pump's ProgramData size, synthetic (not dumped): header ‖ zeros — at Jupiter's address
    // too, which takes the Raydium programs' path (an AIR-01 fingerprint slot, none recorded).
    for (venue, name) in [
        (GUARD_JUPITER, "jupiter (10 MiB)"),
        (GUARD_PUMP, "pump (10 MiB)"),
    ] {
        let key = VENUE_PROGRAM_DATA[usize::from(venue)];
        let s = FIXTURE_GUARD_SLOTS[usize::from(venue)] + 1;
        fixtures::set_program_data(
            &mut env.svm,
            &key,
            s,
            Some(&Pubkey::new_unique()),
            PUMP_PROGRAM_DATA_LEN - program_data::HEADER_LEN,
        );
        assert_eq!(
            read::account(&env.svm, &key).unwrap().data.len(),
            PUMP_PROGRAM_DATA_LEN
        );
        flags.push((name.into(), cu(&ok(flag(&mut env, venue)))));
        revals.push((
            name.into(),
            cu(&ok(env.send(
                &[ix::revalidate_venue(&ea.pubkey(), venue, s)],
                &[&ea],
            ))),
        ));
    }
    for (what, list) in [
        ("flag_venue_upgraded", &flags),
        ("revalidate_venue", &revals),
    ] {
        for (name, units) in list {
            println!("{what} on the {name} ProgramData: {units} CU");
            assert!(*units <= LATCH_CU_LIMIT, "{what} {name}: {units} CU");
        }
        // 1.4 MB, 0.79 MB and 10 MiB on one path: equal to the unit.
        assert!(
            list[..3].iter().all(|(_, u)| *u == list[0].1),
            "{what}: {list:?} — the cost must not depend on the account size"
        );
        // Pump has no fingerprint slot: it skips the lookup's few instructions (AIR-01).
        assert!(
            list[3].1 <= list[0].1 && list[0].1 - list[3].1 <= 32,
            "{what}: {list:?}"
        );
    }
}

/// The loader's own rule the latch relies on: one upgrade per slot per program, so a flagged
/// slot names exactly one deployment.
#[test]
fn one_upgrade_per_slot() {
    let mut env = Env::ray_fork();
    env.warp(1);
    loader_upgrade(&mut env, RaydiumProgram::AmmV4);
    let (res, _) = send_upgrade(&mut env, RaydiumProgram::AmmV4);
    let err = res.expect_err("a second upgrade in the same slot");
    assert!(
        err.meta
            .logs
            .iter()
            .any(|l| l.contains("Program was deployed in this block already")),
        "{}",
        err.meta.pretty_logs()
    );
    env.warp(1);
    loader_upgrade(&mut env, RaydiumProgram::AmmV4);
}

/// Anchor's discriminator of `create_creator_fee_share`
/// (`sha256("global:create_creator_fee_share")[..8]`): an instruction only the 2026-10 CPMM build
/// has (`raydium-cp-swap@bdeee23`).
const CPMM_CREATE_CREATOR_FEE_SHARE: [u8; 8] = [0x7f, 0x1f, 0xbc, 0x77, 0xab, 0xec, 0x80, 0x24];
/// Anchor's `InstructionFallbackNotFound`: the discriminator names no instruction.
const ANCHOR_INSTRUCTION_FALLBACK_NOT_FOUND: u32 = 101;
/// Anchor's `AccountNotEnoughKeys`.
const ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS: u32 = 3005;

/// Which CPMM build executes: `create_creator_fee_share` without its accounts fails with
/// `InstructionFallbackNotFound` on the 2026-09 build and with `AccountNotEnoughKeys` on the
/// redeploy (which dispatches it, then finds no accounts).
fn cpmm_build_running(env: &mut Env) -> CpmmDeploy {
    let mut data = CPMM_CREATE_CREATOR_FEE_SHARE.to_vec();
    data.extend_from_slice(&0u64.to_le_bytes());
    let probe = Instruction {
        program_id: RaydiumProgram::Cpmm.id(),
        accounts: vec![],
        data,
    };
    let res = env.send(&[probe], &[]);
    match custom_code(&res) {
        Some(ANCHOR_INSTRUCTION_FALLBACK_NOT_FOUND) => CpmmDeploy::Sep2026,
        Some(ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS) => CpmmDeploy::Oct2026,
        other => panic!("unexpected CPMM answer {other:?}: {res:?}"),
    }
}

/// The upgrade mainnet made on 2026-10 (slot 452,376,460), replayed with the real accounts: the
/// fork starts on CPMM's 2026-09-10 build (its ProgramData at 445,763,504, which
/// `VENUE_GUARD_SLOTS_2026_09_27` names and `initialize_config` records), positions are open on
/// both programs, and the loader's own `Upgrade` installs the redeploy's ELF. Flagged by anyone, it
/// refuses opens on every Raydium market while exits keep working on the new ELF; only the loader's
/// slot revalidates, after which opens resume and the market keeps trading on the redeploy.
#[test]
fn the_2026_10_cpmm_redeploy_latches_until_revalidated() {
    let venue = GUARD_RAYDIUM_CPMM;
    let i = usize::from(venue);
    let old = VENUE_GUARD_SLOTS_2026_09_27[i];
    assert_eq!(old, CpmmDeploy::Sep2026.deploy_slot());

    // The fork as of 2026-09-27: the old CPMM build, the baseline the program constant names.
    let mut env = Env::new();
    let mut args = env.default_init_args();
    args.guard_slot = VENUE_GUARD_SLOTS_2026_09_27;
    ok(env.initialize_config(args));
    fork::load_program(&mut env.svm, RaydiumProgram::AmmV4);
    fork::load_cpmm_deploy(&mut env.svm, CpmmDeploy::Sep2026);
    env.set_fork_clock();
    assert_eq!(header(&env, RaydiumProgram::Cpmm).slot, old);
    assert_eq!(env.venue_guard().guard_slot[i], old);
    assert_eq!(cpmm_build_running(&mut env), CpmmDeploy::Sep2026);
    assert_err(&flag(&mut env, venue), E::VenueNotUpgraded);

    let markets = [add_market(&mut env, FART), add_market(&mut env, USELESS)];
    let [(fart, qf), (useless, qu)] = &markets;
    let (bu, nu) = open_ok(&mut env, useless, *qu);
    let (bf, nf) = open_ok(&mut env, fart, *qf);

    // The redeploy: the loader writes its slot and the 2026-10 ELF (`RaydiumProgram::Cpmm.elf()`).
    env.warp(1);
    let s = loader_upgrade(&mut env, RaydiumProgram::Cpmm);
    assert_ne!(s, old);
    env.warp(1);
    assert_eq!(cpmm_build_running(&mut env), CpmmDeploy::Oct2026);

    let meta = ok(flag(&mut env, venue));
    let ev: VenueUpgradeFlagged = events::one(&meta);
    assert_eq!(
        (ev.venue, ev.guard_slot, ev.programdata_slot),
        (venue, old, s)
    );
    for (rm, q) in &markets {
        assert_err(&try_open(&mut env, rm, *q), E::VenueUpgraded);
    }

    // Exits never read the latch (I10); the CPMM close buys back on the new ELF.
    crank_all(&mut env, &markets);
    env.warp(1);
    let meta = ok(env.close(useless, &bu, nu));
    let _: Close = events::one(&meta);
    let meta = ok(env.send(
        &[add_margin_ix(fart, &bf.key(), nf, 1_000_000_000)],
        &[&bf.kp],
    ));
    let _: MarginAdded = events::one(&meta);
    assert!(env.venue_guard().is_flagged(venue));

    // Only the loader's slot revalidates; the 2026-09 baseline does not.
    for wrong in [old, s + 1] {
        assert_err(&revalidate(&mut env, venue, wrong), E::VenueSlotMismatch);
    }
    crank_all(&mut env, &markets);
    let meta = ok(revalidate(&mut env, venue, s));
    let ev: VenueRevalidated = events::one(&meta);
    assert_eq!((ev.guard_slot, ev.expected_slot), (old, s));
    assert_eq!(env.venue_guard().guard_slot[i], s);
    for (rm, q) in &markets {
        ok(try_open(&mut env, rm, *q / 2));
    }
    // A direct user swap executes on the redeploy.
    whale_buy(&mut env, useless, 1_000_000_000);
    assert_eq!(cpmm_build_running(&mut env), CpmmDeploy::Oct2026);
}

// ---- AIR-01: an extension is not an upgrade ----------------------------------------------------

/// ★ AIR-01 (I-01). The loader's `ExtendProgram` is permissionless (Agave 4.2) and rewrites the
/// ProgramData deploy slot without touching the ELF, while `flag_venue_upgraded` latched on any
/// slot change: anyone stopped every Raydium open with `[ExtendProgram(venue),
/// flag_venue_upgraded(venue)]` for ≈ 0.07 SOL of rent a trip. Reproduced on the pre-fix build:
/// the flag after a stranger's 10 KiB extension set the bit. Now, once the Emergency has
/// revalidated the deploy naming the sha256 of its trimmed ELF, an extension — alone, bundled
/// with the flag, or again in a later slot — is `VenueNotUpgraded` and writes nothing, and opens
/// keep landing on every Raydium market.
#[test]
fn air01_an_extension_is_not_an_upgrade() {
    for program in RaydiumProgram::ALL {
        let venue = guard_index(program);
        let dumped = FIXTURE_GUARD_SLOTS[usize::from(venue)];
        let (mut env, markets) = three_markets();
        // The deploy runbook's first revalidation: the reviewed ELF's hash goes on chain.
        let reviewed = reviewed_hash(&env, program);
        ok(revalidate_reviewed(&mut env, program, dumped, reviewed));

        env.warp(1);
        let e = stranger_extends(&mut env, program, 10_240);
        assert_ne!(e, dumped, "the deploy slot moved");
        env.warp(1);
        let before = read::account(&env.svm, &pda::venue_guard()).unwrap().data;
        assert_err(&flag_hashing(&mut env, venue), E::VenueNotUpgraded);
        assert_eq!(
            read::account(&env.svm, &pda::venue_guard()).unwrap().data,
            before,
            "{program:?}: nothing written"
        );
        // The attack in one transaction, then the extension alone again in later slots.
        for _ in 0..2 {
            env.warp(1);
            let stranger = env.funded_keypair();
            let mut ixs = vec![fork::extend_program_ix(
                &program.id(),
                &stranger.pubkey(),
                10_240,
            )];
            ixs.extend(with_hash_budget(ix::flag_venue_upgraded(venue)));
            assert_err(&env.send(&ixs, &[&stranger]), E::VenueNotUpgraded);
            stranger_extends(&mut env, program, 20_480);
            env.warp(1);
            assert_err(&flag_hashing(&mut env, venue), E::VenueNotUpgraded);
        }
        assert_eq!(env.venue_guard().upgraded, 0, "{program:?}: never latched");
        crank_all(&mut env, &markets);
        for (rm, q) in &markets {
            ok(try_open(&mut env, rm, *q));
        }
    }
}

/// ★ AIR-01: an extension landing between the Emergency's review of an upgrade and its
/// transaction no longer refuses the revalidation. Reproduced on the pre-fix build:
/// `revalidate_venue(venue, s)` after a stranger's extension failed `VenueSlotMismatch` (exact
/// slot), and the stranger could re-extend before every attempt. Now the revalidation names the
/// reviewed trimmed ELF, and a later deploy slot is accepted while the account holds exactly that
/// ELF: the bit clears, `guard_slot` records the extension's slot, and the next extension latches
/// nothing (the reviewed fingerprint is on chain).
#[test]
fn air01_an_extension_during_review_does_not_block_the_revalidation() {
    for program in RaydiumProgram::ALL {
        let venue = guard_index(program);
        let i = usize::from(venue);
        let (mut env, markets) = three_markets();
        env.warp(1);
        let s = loader_upgrade(&mut env, program);
        env.warp(1);
        ok(flag(&mut env, venue));
        // The Emergency reviews the deploy at `s`; two extensions land before its transaction.
        let reviewed = reviewed_hash(&env, program);
        env.warp(1);
        stranger_extends(&mut env, program, 10_240);
        env.warp(1);
        let e = stranger_extends(&mut env, program, 10_240);
        env.warp(1);
        let meta = ok(revalidate_reviewed(&mut env, program, s, reviewed));
        let ev: VenueRevalidated = events::one(&meta);
        assert_eq!((ev.expected_slot, ev.programdata_slot), (s, e));
        let g = env.venue_guard();
        assert!(!g.is_flagged(venue), "{program:?}: cleared");
        assert_eq!(
            g.guard_slot[i], e,
            "the deploy slot the account carries now"
        );
        crank_all(&mut env, &markets);
        for (rm, q) in &markets {
            ok(try_open(&mut env, rm, *q));
        }
        env.warp(1);
        stranger_extends(&mut env, program, 10_240);
        env.warp(1);
        assert_err(&flag_hashing(&mut env, venue), E::VenueNotUpgraded);
    }
}

/// AIR-01, the other half: an upgrade next to an extension still latches — the case the deploy
/// slot cannot tell from a pure extension. AMM v4 is upgraded to its own ELF with one byte
/// changed (same size, inside the old account), once after a stranger's extension and once
/// before one: each time the flag sets the bit, the old ELF's hash no longer revalidates a later
/// slot, and only the new deploy clears it.
#[test]
fn air01_an_upgrade_next_to_an_extension_still_latches() {
    let program = RaydiumProgram::AmmV4;
    let venue = guard_index(program);
    let dumped = FIXTURE_GUARD_SLOTS[usize::from(venue)];
    let changed = v4_elf_one_byte_changed();
    for extend_first in [true, false] {
        let (mut env, markets) = three_markets();
        let old = reviewed_hash(&env, program);
        ok(revalidate_reviewed(&mut env, program, dumped, old));
        env.warp(1);
        if extend_first {
            stranger_extends(&mut env, program, 10_240);
            env.warp(1);
        }
        let s = loader_upgrade_elf(&mut env, program, &changed);
        env.warp(1);
        if !extend_first {
            stranger_extends(&mut env, program, 10_240);
            env.warp(1);
        }
        let meta = ok(flag_hashing(&mut env, venue));
        let _: VenueUpgradeFlagged = events::one(&meta);
        assert!(
            env.venue_guard().is_flagged(venue),
            "extend first: {extend_first}"
        );
        crank_all(&mut env, &markets);
        for (rm, q) in &markets {
            assert_err(&try_open(&mut env, rm, *q), E::VenueUpgraded);
        }
        // The old ELF's hash cannot revalidate the new deploy at a later slot.
        let now = header(&env, program).slot;
        env.warp(1);
        stranger_extends(&mut env, program, 10_240);
        env.warp(1);
        assert_err(
            &revalidate_reviewed(&mut env, program, s, old),
            E::VenueSlotMismatch,
        );
        assert_ne!(now, header(&env, program).slot);
        let new = reviewed_hash(&env, program);
        assert_ne!(new, old, "another build");
        let current = header(&env, program).slot;
        ok(revalidate_reviewed(&mut env, program, current, new));
        assert_eq!(env.venue_guard().upgraded, 0);
        for (rm, q) in &markets {
            ok(try_open(&mut env, rm, *q / 2));
        }
    }
}

/// AIR-01, the compute budget: once the account grew since the review, `flag_venue_upgraded` and
/// `revalidate_venue` hash the trimmed ELF (`sol_sha256`: half a CU per byte) after finding its
/// end in the zero tail (`sol_memcmp_`: about 1 CU per 250 bytes). Measured on AMM v4 (0.31 MB
/// ELF) after a stranger extended its ProgramData to 1 KiB under the 10 MiB cap — 9 MB of zero
/// tail — and on CPMM (0.72 MB ELF) after a 10 KiB extension: each stays near `ELF/2 + tail/250`
/// over the cheap path and inside the 1.4 M CU transaction limit (pump-fees' 0.85 MB ELF: see
/// `latch_pump_it`). Without an extension nothing is hashed.
#[test]
fn air01_the_hash_path_fits_the_compute_budget() {
    let mut env = Env::ray_fork();
    let cu = |meta: &TransactionMetadata| meta.compute_units_consumed;
    for program in RaydiumProgram::ALL {
        let venue = guard_index(program);
        let dumped = FIXTURE_GUARD_SLOTS[usize::from(venue)];
        let key = program.program_data().address;
        let reviewed = reviewed_hash(&env, program);
        let cheap = cu(&ok(revalidate_reviewed(
            &mut env, program, dumped, reviewed,
        )));
        let a = read::account(&env.svm, &key).unwrap();
        let elf = program_data::trimmed_elf(&a.owner.to_bytes(), &a.data)
            .unwrap()
            .len() as u64;
        let extra = match program {
            RaydiumProgram::AmmV4 => (PUMP_PROGRAM_DATA_LEN - a.data.len() - 1_024) as u32,
            RaydiumProgram::Cpmm => 10_240,
        };
        env.warp(1);
        stranger_extends(&mut env, program, extra);
        let len = read::account(&env.svm, &key).unwrap().data.len() as u64;
        let tail = len - program_data::HEADER_LEN as u64 - elf;
        env.warp(1);
        let refused = flag_hashing(&mut env, venue).expect_err("an extension");
        let flag = refused.meta.compute_units_consumed;
        let reval = cu(&ok(revalidate_reviewed(
            &mut env, program, dumped, reviewed,
        )));
        println!(
            "{program:?}: trimmed ELF {elf} B, zero tail {tail} B — flag_venue_upgraded \
             {flag} CU (refused: an extension), revalidate_venue at a later slot {reval} CU \
             (exact slot: {cheap} CU)"
        );
        for units in [flag, reval] {
            assert!(units > elf / 2 + 5_000, "{program:?}: the ELF was hashed");
            assert!(
                units < cheap + elf / 2 + tail / 200 + 10_000,
                "{program:?}: {units} CU"
            );
            assert!(units < 1_400_000);
        }
    }
}

/// AIR-01 on CPMM's real 2026-10 redeploy (a larger ELF): replayed after a stranger extended the
/// 2026-09 build's ProgramData, so that the new ELF lands inside an account whose length the
/// extension already changed. The extension alone latches nothing; the redeploy does; the
/// 2026-09 ELF's hash no longer revalidates a later slot, the redeploy's does.
#[test]
fn air01_the_2026_10_cpmm_redeploy_latches_after_an_extension() {
    let program = RaydiumProgram::Cpmm;
    let venue = GUARD_RAYDIUM_CPMM;
    let old = VENUE_GUARD_SLOTS_2026_09_27[usize::from(venue)];
    let mut env = Env::new();
    let mut args = env.default_init_args();
    args.guard_slot = VENUE_GUARD_SLOTS_2026_09_27;
    ok(env.initialize_config(args));
    fork::load_program(&mut env.svm, RaydiumProgram::AmmV4);
    fork::load_cpmm_deploy(&mut env.svm, CpmmDeploy::Sep2026);
    env.set_fork_clock();
    let markets = [add_market(&mut env, FART), add_market(&mut env, USELESS)];
    // The program hashes exactly what `solana-verify` / OtterSec hash (the fixtures' pins,
    // `tests/fixtures/README.md`).
    let sep = reviewed_hash(&env, program);
    assert!(hex(&sep).starts_with("bdaff73e") && hex(&sep).ends_with("f8cc"));
    ok(revalidate_reviewed(&mut env, program, old, sep));

    env.warp(1);
    stranger_extends(&mut env, program, 10_240);
    env.warp(1);
    assert_err(&flag_hashing(&mut env, venue), E::VenueNotUpgraded);

    env.warp(1);
    let s = loader_upgrade(&mut env, program);
    assert!(
        program.elf().len() > CpmmDeploy::Sep2026.elf().len(),
        "the redeploy's ELF is larger"
    );
    env.warp(1);
    assert_eq!(cpmm_build_running(&mut env), CpmmDeploy::Oct2026);
    let meta = ok(flag_hashing(&mut env, venue));
    let ev: VenueUpgradeFlagged = events::one(&meta);
    assert_eq!((ev.guard_slot, ev.programdata_slot), (old, s));
    crank_all(&mut env, &markets);
    for (rm, q) in &markets {
        assert_err(&try_open(&mut env, rm, *q), E::VenueUpgraded);
    }
    env.warp(1);
    let e = stranger_extends(&mut env, program, 10_240);
    env.warp(1);
    assert_err(
        &revalidate_reviewed(&mut env, program, s, sep),
        E::VenueSlotMismatch,
    );
    let oct = reviewed_hash(&env, program);
    assert!(hex(&oct).starts_with("93cad245") && hex(&oct).ends_with("a0d2"));
    let meta = ok(revalidate_reviewed(&mut env, program, s, oct));
    let ev: VenueRevalidated = events::one(&meta);
    assert_eq!((ev.expected_slot, ev.programdata_slot), (s, e));
    crank_all(&mut env, &markets);
    for (rm, q) in &markets {
        ok(try_open(&mut env, rm, *q / 2));
    }
}
