//! T-LATCH on the dumped pump ProgramData (ARCH §2.3.2, §3.1 "Venue-upgrade latch", §5.5, §6.5,
//! §8.2, §9 M5; A7, R4B-7; I17; U5), on the pump fork of `shortwin_litesvm_tests::fork`: the
//! real pump (10,485,760 B), pump-fees (948,989 B) and PumpSwap (10,485,760 B) ProgramData
//! accounts, dumped at their mainnet deploy slots, which `initialize_config` recorded as
//! `guard_slot` (`fork::FIXTURE_GUARD_SLOTS`: since the 2026-10-13 re-pin pump's redeploy of
//! 2026-10-13, PumpSwap's of 2026-10-08 and pump-fees' of 2026-10-12).
//!
//! The upgrades mainnet itself made are replayed too (M10 pump revalidation; pump-final P2): the
//! 2026-10-02 builds (`fork::PumpDeploy::Oct2026`, the slots `VENUE_GUARD_SLOTS_2026_10_03` names —
//! the first with pump's v3 trades, which short.win's curve adapter needs; the 2026-09 builds have
//! none) upgraded by the loader to the current builds' ELFs, with positions opened on the old
//! builds and exited on the new ones — a synthetic exit past `rT` among them (pump-final §14
//! T-LATCH) — which latch curve opens until Emergency revalidates the loader's slots.
//!
//! An upgrade here is a **real** one: the upgradeable loader's own `Upgrade` instruction, with a
//! buffer holding the redeploy's dumped ELF (`fork::PumpDeploy::CURRENT`) and the program's real
//! upgrade authority (one key holds all three programs, ARCH §5.5) as signer — the one key no
//! test holds, so its signature is forged (`fork::admin_send`: LiteSVM's signature check is off
//! for that one transaction; the hooks run). The loader itself writes the new deploy slot into
//! the ProgramData header, as on mainnet, and the program keeps running (the same bytes
//! redeployed; the replay's new bytes).
//!
//! What it settles:
//!
//! - the dumped headers are the ones `VenueGuard` records, at the compile-time addresses, with the
//!   sizes of ARCH §6.5 and one upgrade authority; an unchanged header is not an upgrade (no
//!   write);
//! - after an upgrade of **pump, pump-fees or PumpSwap**, `flag_venue_upgraded` (anyone) sets the
//!   bit and records the loader's deploy slot; opens are refused on **every** curve market — an
//!   SPL Token curve and a Token-2022 curve — (the curve's mask holds all three, R4B-7), while
//!   cranks, top-ups, `sol_settle`, `sweep_trader`, a close, a liquidation after a real pump and
//!   an expiry keep working — their buybacks run on the **upgraded** pump program when pump is
//!   the one upgraded (I10: exits never read the latch);
//! - `revalidate_venue(venue, expected_slot)` clears it only for the slot the loader wrote; a
//!   second upgrade during review is flagged again and invalidates the reviewed slot, also when
//!   it lands unflagged between review and execution;
//! - all three pump bits must be clear before a curve market opens again; Raydium and Jupiter
//!   upgrades never block curve opens;
//! - the ProgramData account is checked by address, owner and tag on the real accounts;
//! - the residual the design discloses (ARCH §3.1, §8.5): an open that lands after the upgrade
//!   and before the flag is accepted; `[flag, open]` in one transaction is not;
//! - U5: `flag_venue_upgraded` and `revalidate_venue` cost the same CU on the real 10 MiB pump
//!   and PumpSwap ProgramData as on pump-fees' 0.95 MB, far inside §6.1's 400 k;
//! - AIR-01: pump and PumpSwap (at the 10 MiB cap) cannot be extended; a stranger's extension of
//!   pump-fees is not an upgrade once the Emergency revalidated it naming its trimmed ELF's sha256
//!   (the fixture's pin), curve opens keep landing, and the hash path costs ≈ 0.44 M CU; an
//!   upgrade of pump-fees after an extension still latches.
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
    GUARD_JUPITER, GUARD_MASK_PUMP_CURVE, GUARD_PUMP, GUARD_PUMP_FEES, GUARD_RAYDIUM_CPMM,
    GUARD_RAYDIUM_V4, VENUE_GUARD_SLOTS_2026_09_27, VENUE_GUARD_SLOTS_2026_10_03,
    VENUE_PROGRAM_DATA,
};
use shortwin::errors::ShortwinError as E;
use shortwin::events::{
    Close, Expire, Liquidate, MarginAdded, Open, TraderSwept, VenueRevalidated, VenueUpgradeFlagged,
};
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::engine::{
    Borrower, add_margin_ix, expire_ix, liquidate_ix, open_short_ix, sol_settle_ix,
};
use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
use shortwin_litesvm_tests::fork::{self, PumpCoin, PumpCurve, PumpDeploy, PumpProgram};
use shortwin_litesvm_tests::pump::{self, CurveUser, PumpMarket, PumpSpec};
use shortwin_litesvm_tests::{Env, TxResult, events, ix, pda, read};
use shortwin_venue_state::{program_data, pump as vpump};
use solana_account::Account;
use solana_signer::Signer;

const SPL: PumpCoin = PumpCoin::Legacy115;
const T22: PumpCoin = PumpCoin::T22_115;
/// The 151-byte curve near completion (the synthetic exit across the upgrade).
const NEAR: PumpCoin = PumpCoin::Near;

/// The ProgramData sizes of ARCH §6.5 [V9].
const PUMP_PROGRAM_DATA_LEN: usize = 10_485_760;
const PUMP_FEES_PROGRAM_DATA_LEN: usize = 948_989;
/// ARCH §6.1's PROPOSED transaction limit for `flag_venue_upgraded` / `revalidate_venue`.
const LATCH_CU_LIMIT: u64 = 400_000;

fn size_of(program: PumpProgram) -> usize {
    match program {
        PumpProgram::Pump | PumpProgram::Amm => PUMP_PROGRAM_DATA_LEN,
        PumpProgram::Fees => PUMP_FEES_PROGRAM_DATA_LEN,
    }
}

// ---- helpers --------------------------------------------------------------------------------

/// The ProgramData header as the loader wrote it.
fn header(env: &Env, program: PumpProgram) -> program_data::ProgramDataHeader {
    let a = read::account(&env.svm, &program.program_data().address).expect("ProgramData");
    program_data::read(&a.owner.to_bytes(), &a.data).expect("a ProgramData header")
}

/// `SysvarRent111111111111111111111111111111111`.
const SYSVAR_RENT: Pubkey = Pubkey::from_str_const("SysvarRent111111111111111111111111111111111");
/// `SysvarC1ock11111111111111111111111111111111`.
const SYSVAR_CLOCK: Pubkey = Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111");

/// Sends the upgradeable loader's own `Upgrade` of `program` to its dumped ELF: a new buffer
/// (`UpgradeableLoaderState::Buffer { authority: Some(authority) }` ‖ ELF), then `Upgrade`
/// (bincode tag 3, the loader's account order) signed by the program's upgrade authority, whose
/// signature is forged (`fork::admin_send`). Returns the result and the buffer.
fn send_upgrade(env: &mut Env, program: PumpProgram) -> (TxResult, Pubkey) {
    send_upgrade_elf(env, program, program.elf())
}

/// [`send_upgrade`] to `elf` instead of the dumped ELF.
fn send_upgrade_elf(env: &mut Env, program: PumpProgram, elf: &[u8]) -> (TxResult, Pubkey) {
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
fn loader_upgrade(env: &mut Env, program: PumpProgram) -> u64 {
    let slot = env.clock().slot;
    let authority = header(env, program).upgrade_authority;
    let (res, buffer) = send_upgrade(env, program);
    ok(res);
    let h = header(env, program);
    assert_eq!(h.slot, slot, "the loader records the upgrade's slot");
    assert_eq!(h.upgrade_authority, authority, "the authority is kept");
    let pd = read::account(&env.svm, &program.program_data().address).unwrap();
    assert_eq!(
        pd.data.len(),
        size_of(program),
        "the account keeps its size"
    );
    let start = program_data::HEADER_LEN;
    assert_eq!(
        &pd.data[start..start + program.elf().len()],
        program.elf(),
        "the redeployed ELF"
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

/// [`send_upgrade_elf`], checked as [`loader_upgrade`] checks it (an upgrade never resizes the
/// account; the loader zeroes everything after the ELF). Returns the upgrade's slot.
fn loader_upgrade_elf(env: &mut Env, program: PumpProgram, elf: &[u8]) -> u64 {
    let slot = env.clock().slot;
    let key = program.program_data().address;
    let len = read::account(&env.svm, &key).unwrap().data.len();
    let (res, _) = send_upgrade_elf(env, program, elf);
    ok(res);
    assert_eq!(header(env, program).slot, slot);
    let pd = read::account(&env.svm, &key).unwrap();
    assert_eq!(pd.data.len(), len, "an upgrade never resizes the account");
    let start = program_data::HEADER_LEN;
    assert_eq!(
        &pd.data[start..start + elf.len()],
        elf,
        "the redeployed ELF"
    );
    assert!(pd.data[start + elf.len()..].iter().all(|&b| b == 0));
    slot
}

/// `SetComputeUnitLimit(1.4 M)`, then `ix`: once the account grew since the review, the latch
/// instructions hash the venue's trimmed ELF (AIR-01; half a CU per byte, pump-fees' 0.85 MB
/// ELF ≈ 0.43 M CU), above the 200 k default.
fn with_hash_budget(ix: Instruction) -> Vec<Instruction> {
    vec![pump::cu_limit_ix(1_400_000), ix]
}

/// `flag_venue_upgraded(venue)` with [`with_hash_budget`].
fn flag_hashing(env: &mut Env, venue: u8) -> TxResult {
    env.send(&with_hash_budget(ix::flag_venue_upgraded(venue)), &[])
}

/// The sha256 of `program`'s trimmed ELF as its ProgramData holds it now — what
/// `solana-verify get-program-hash` prints, and what the Emergency names when it revalidates the
/// deploy it reviewed (AIR-01).
fn reviewed_hash(env: &Env, program: PumpProgram) -> [u8; 32] {
    let a = read::account(&env.svm, &program.program_data().address).expect("ProgramData");
    shortwin::instructions::flag_venue_upgraded::trimmed_elf_hash_of(&a.owner.to_bytes(), &a.data)
        .expect("a ProgramData")
        .expect("small enough to hash")
}

/// The Emergency's `revalidate_venue` of the deploy at `slot` whose trimmed ELF hashes to
/// `reviewed` ([`reviewed_hash`]), with [`with_hash_budget`].
fn revalidate_reviewed(
    env: &mut Env,
    program: PumpProgram,
    slot: u64,
    reviewed: [u8; 32],
) -> TxResult {
    let ea = env.pause_authority.insecure_clone();
    let ix = ix::revalidate_venue_reviewed(&ea.pubkey(), program.guard(), slot, reviewed);
    env.send(&with_hash_budget(ix), &[&ea])
}

/// A stranger's `ExtendProgram` of `program`'s ProgramData by `additional_bytes`
/// (`fork::extend_program_ix`; a result, since pump and PumpSwap sit at the 10 MiB cap).
fn extend(env: &mut Env, program: PumpProgram, additional_bytes: u32) -> TxResult {
    let stranger = env.funded_keypair();
    let ix = fork::extend_program_ix(&program.id(), &stranger.pubkey(), additional_bytes);
    env.send(&[ix], &[&stranger])
}

/// [`extend`], which lands: the loader appends zeros, keeps the ELF and writes the current slot
/// into the header. Returns that slot.
fn stranger_extends(env: &mut Env, program: PumpProgram, additional_bytes: u32) -> u64 {
    let key = program.program_data().address;
    let before = read::account(&env.svm, &key).unwrap().data;
    let slot = env.clock().slot;
    ok(extend(env, program, additional_bytes));
    let after = read::account(&env.svm, &key).unwrap().data;
    assert_eq!(after.len(), before.len() + additional_bytes as usize);
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

/// pump-fees' dumped ELF with one byte of a source-path string in `.rodata` changed
/// (`programs/pump-fees/src/state.rs` → `…/stete.rs`): another build of the same size, which the
/// loader deploys (it verifies the code, not the read-only data).
fn fees_elf_one_byte_changed() -> Vec<u8> {
    const PATH: &[u8] = b"programs/pump-fees/src/state.rs";
    let mut elf = PumpProgram::Fees.elf().to_vec();
    let at = elf
        .windows(PATH.len())
        .position(|w| w == PATH)
        .expect("the string")
        + 25;
    assert_eq!(elf[at], b'a');
    elf[at] = b'e';
    elf
}

/// `vT/200`, the `pump_it` position size.
fn q_of(env: &Env, c: &PumpCurve) -> u64 {
    c.state(&env.svm).virtual_token_reserves / 200
}

/// Lists `c`, lends `40·Q`, waits out the warm-up, cranks once.
fn add_market(env: &mut Env, c: PumpCurve) -> (PumpMarket, u64) {
    ok(env.list_pump(&c));
    let q = q_of(env, &c);
    let pm = env.pump_market_listed(PumpSpec::new(c.coin, q.saturating_mul(40)), c);
    env.crank(&pm);
    (pm, q)
}

/// A fork with markets on an SPL Token curve and a Token-2022 curve.
fn two_markets() -> (Env, [(PumpMarket, u64); 2]) {
    let (mut env, first) = Env::fork_pump_coin(SPL);
    let a = add_market(&mut env, first);
    let c = env.load_pump_coin(T22);
    let b = add_market(&mut env, c);
    (env, [a, b])
}

fn crank_all(env: &mut Env, markets: &[(PumpMarket, u64)]) {
    for (pm, _) in markets {
        env.crank(pm);
    }
}

fn try_open(env: &mut Env, pm: &PumpMarket, q: u64) -> TxResult {
    let mut b = env.borrower();
    env.open(pm, &mut b, q).0
}

fn open_ok(env: &mut Env, pm: &PumpMarket, q: u64) -> (Borrower, u64) {
    let mut b = env.borrower();
    let (res, nonce) = env.open(pm, &mut b, q);
    let meta = ok(res);
    let ev: Open = events::one(&meta);
    assert_eq!(ev.q, q);
    (b, nonce)
}

/// The keeper's exit transaction: ARCH §6.1's CU limit for a curve exit, then `ix`.
fn keeper_tx(ix: Instruction) -> Vec<Instruction> {
    vec![pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU), ix]
}

/// A buyer takes 40 % of `vT` on `pm`'s curve (a real `buy_v2`: the price × 2.8), then the
/// keeper cranks every second until `liquidate` executes (as `pump_it::exits`; not yet:
/// `NotLiquidatable`, `ExitBoundNotMet`, or `ReferenceStale` — no fresh completed bucket yet,
/// the keeper cranks one second and acts the next, ECON §5.4): the event.
fn liquidate_after_a_pump(env: &mut Env, pm: &PumpMarket, b: &Borrower, nonce: u64) -> Liquidate {
    env.tick(pm, 1);
    let u = CurveUser::new(env, &pm.curve);
    let vt = pm.curve.state(&env.svm).virtual_token_reserves;
    ok(u.buy(env, &pm.curve, vt / 10 * 4, u64::MAX));
    let keeper = env.funded_keypair();
    for _ in 0..900 {
        env.tick(pm, 1);
        let res = env.send(
            &keeper_tx(liquidate_ix(pm, &keeper.pubkey(), &b.key(), nonce)),
            &[&keeper],
        );
        match custom_code(&res) {
            None => return events::one(&ok(res)),
            Some(c)
                if c == u32::from(E::NotLiquidatable)
                    || c == u32::from(E::ExitBoundNotMet)
                    || c == u32::from(E::ReferenceStale) => {}
            Some(c) => panic!("liquidate failed with {c}"),
        }
    }
    panic!("never liquidated");
}

/// Lamports to the trader PDA of `pm` (a donation `sweep_trader` recovers).
fn donate_lamports(env: &mut Env, pm: &PumpMarket, lamports: u64) {
    let griefer = env.funded_keypair();
    let mut data = vec![2, 0, 0, 0];
    data.extend_from_slice(&lamports.to_le_bytes());
    ok(env.send(
        &[Instruction {
            program_id: anchor_lang::system_program::ID,
            accounts: vec![
                AccountMeta::new(griefer.pubkey(), true),
                AccountMeta::new(pm.trader(), false),
            ],
            data,
        }],
        &[&griefer],
    ));
}

// ---- tests ----------------------------------------------------------------------------------

/// The dumped ProgramData are the real accounts at the compile-time addresses, with the deploy
/// slots of `fork::FIXTURE_GUARD_SLOTS` (the re-pin's builds), the sizes of ARCH §6.5 and
/// one upgrade authority for all three programs (ARCH §5.5); `initialize_config` recorded exactly
/// those slots. An unchanged header is not an upgrade and writes nothing.
#[test]
fn the_dumped_headers_are_the_recorded_slots() {
    let (mut env, _) = Env::fork_pump_coin(T22);
    let mut authorities = Vec::new();
    for program in PumpProgram::ALL {
        let venue = program.guard();
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
        assert_eq!(
            a.data.len(),
            size_of(program),
            "{program:?}: ARCH §6.5 size"
        );
        let h = header(&env, program);
        assert_eq!(h.slot, fork::FIXTURE_GUARD_SLOTS[i], "{program:?}");
        authorities.push(h.upgrade_authority.expect("upgradeable"));
        assert_eq!(env.venue_guard().guard_slot[i], h.slot, "recorded at init");

        let before = read::account(&env.svm, &pda::venue_guard()).unwrap().data;
        assert_err(&flag(&mut env, venue), E::VenueNotUpgraded);
        assert_eq!(
            read::account(&env.svm, &pda::venue_guard()).unwrap().data,
            before,
            "{program:?}: no write when unchanged"
        );
    }
    assert!(
        authorities.iter().all(|a| *a == authorities[0]),
        "one upgrade authority holds pump, pump-fees and PumpSwap"
    );
    assert_eq!(env.venue_guard().upgraded, 0);
}

/// ARCH §3.1 end to end on each pump program: a real upgrade, flagged in a later slot by anyone,
/// refuses opens on an SPL Token and a Token-2022 curve market (the curve's mask holds pump,
/// pump-fees and PumpSwap, R4B-7); every exit keeps working — a top-up, `sol_settle`,
/// `sweep_trader`, a close, a liquidation after a real pump and an expiry, whose buybacks run on
/// the upgraded pump when pump is the one upgraded; only the loader's slot revalidates, and
/// opens resume.
#[test]
fn a_real_upgrade_latches_opens_on_every_curve_market() {
    for program in PumpProgram::ALL {
        let venue = program.guard();
        assert_ne!(
            GUARD_MASK_PUMP_CURVE & (1 << venue),
            0,
            "in the curve's mask"
        );
        let i = usize::from(venue);
        let dumped = fork::FIXTURE_GUARD_SLOTS[i];
        let (mut env, markets) = two_markets();
        let [(spl, qs), (t22, qt)] = &markets;

        // Positions opened before the upgrade.
        let (b_liq, n_liq) = open_ok(&mut env, spl, *qs);
        let (b_close, n_close) = open_ok(&mut env, t22, *qt);
        let (b_exp, n_exp) = open_ok(&mut env, t22, *qt);
        let (b_top, n_top) = open_ok(&mut env, spl, *qs);

        // The upgrade.
        env.warp(1);
        let s = loader_upgrade(&mut env, program);
        assert_ne!(s, dumped);

        // The residual (ARCH §3.1, §8.5): until someone flags it, opens still land.
        env.warp(1);
        open_ok(&mut env, t22, *qt);

        // Anyone flags it in a later slot (no signer but the fee payer; the keeper's duty 10).
        env.warp(1);
        assert!(env.clock().slot > s, "the flag lands after the upgrade");
        let meta = ok(flag(&mut env, venue));
        let ev: VenueUpgradeFlagged = events::one(&meta);
        let flagged_at = env.now();
        assert_eq!(
            (ev.venue, ev.guard_slot, ev.programdata_slot, ev.flagged_at),
            (venue, dumped, s, flagged_at)
        );
        let g = env.venue_guard();
        assert!(g.is_flagged(venue));
        assert_eq!((g.flagged_slot[i], g.flagged_at[i]), (s, flagged_at));
        assert_eq!(g.guard_slot[i], dumped, "kept until revalidation");
        assert_err(&flag(&mut env, venue), E::VenueNotUpgraded);

        // Opens refused on both curve markets.
        for (pm, q) in &markets {
            assert_err(&try_open(&mut env, pm, *q), E::VenueUpgraded);
        }

        // Exits never read the latch (I10): cranks record, a top-up, a no-op settlement, a
        // sweep of the trader ...
        crank_all(&mut env, &markets);
        let meta = ok(env.send(
            &[add_margin_ix(spl, &b_top.key(), n_top, 1_000_000_000)],
            &[&b_top.kp],
        ));
        let _: MarginAdded = events::one(&meta);
        let keeper = env.funded_keypair();
        env.tick(t22, 1);
        let settle = sol_settle_ix(&env, t22, &keeper.pubkey(), &b_exp.key(), n_exp);
        ok(env.send(&[settle], &[&keeper]));
        donate_lamports(&mut env, t22, 2_000_000);
        let meta = ok(env.send(&[pump::sweep_trader_pump_ix(&env, t22)], &[]));
        let ev: TraderSwept = events::one(&meta);
        assert_eq!(ev.lamports, 2_000_000);
        // ... a close on the Token-2022 curve ...
        env.warp(1);
        let vault0 = read::token_amount(&env.svm, &t22.keys.pool_vault).unwrap();
        let meta = ok(env.close(t22, &b_close, n_close));
        let c: Close = events::one(&meta);
        assert_eq!(c.q, *qt);
        assert_eq!(
            read::token_amount(&env.svm, &t22.keys.pool_vault).unwrap() - vault0,
            *qt,
            "q bought back into the lending vault"
        );
        // ... a liquidation after a real pump on the SPL curve ...
        let vault0 = read::token_amount(&env.svm, &spl.keys.pool_vault).unwrap();
        let liq = liquidate_after_a_pump(&mut env, spl, &b_liq, n_liq);
        assert!(liq.buyback_cost > 0);
        assert_eq!(
            read::token_amount(&env.svm, &spl.keys.pool_vault).unwrap() - vault0,
            *qs,
            "Q bought back into the lending vault"
        );
        assert!(
            read::account(
                &env.svm,
                &pda::position(&spl.market(), &b_liq.key(), n_liq).0
            )
            .is_none()
        );
        // ... and an expiry on the Token-2022 curve.
        let deadline = env.position(t22, &b_exp.key(), n_exp).deadline;
        let now = env.now();
        env.warp(deadline + 1 - now);
        env.tick(t22, 3);
        let meta = ok(env.send(
            &keeper_tx(expire_ix(t22, &keeper.pubkey(), &b_exp.key(), n_exp)),
            &[&keeper],
        ));
        let e: Expire = events::one(&meta);
        assert!(e.buyback_cost > 0 && e.expiry_fee > 0);
        assert!(
            read::account(
                &env.svm,
                &pda::position(&t22.market(), &b_exp.key(), n_exp).0
            )
            .is_none()
        );
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
            (venue, dumped, s, s, flagged_at)
        );
        let g = env.venue_guard();
        assert!(!g.is_flagged(venue));
        assert_eq!(g.guard_slot[i], s);
        assert_err(&flag(&mut env, venue), E::VenueNotUpgraded);
        for (pm, q) in &markets {
            ok(try_open(&mut env, pm, *q));
        }
    }
}

/// R4B-7: a second real upgrade of pump during review refreshes the flag and invalidates the
/// reviewed slot — also when it lands **unflagged** between the review and the Emergency
/// transaction.
#[test]
fn a_second_upgrade_during_review_is_visible() {
    let program = PumpProgram::Pump;
    let venue = GUARD_PUMP;
    let i = usize::from(venue);
    let (mut env, markets) = two_markets();
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
    for (pm, q) in &markets {
        assert_err(&try_open(&mut env, pm, *q), E::VenueUpgraded);
    }
    assert_err(&revalidate(&mut env, venue, s1), E::VenueSlotMismatch);
    let ev: VenueRevalidated = events::one(&ok(revalidate(&mut env, venue, s2)));
    assert_eq!((ev.expected_slot, ev.flagged_at), (s2, t2));
    for (pm, q) in &markets {
        ok(try_open(&mut env, pm, *q));
    }
}

/// All three pump programs upgraded (one authority, one Squads batch): revalidating two leaves
/// every curve market latched by the third (the curve's mask, R4B-7); opens resume once all
/// three are revalidated.
#[test]
fn all_three_pump_bits_must_clear() {
    let (mut env, markets) = two_markets();
    env.warp(1);
    let slots: Vec<(u8, u64)> = PumpProgram::ALL
        .iter()
        .map(|p| (p.guard(), loader_upgrade(&mut env, *p)))
        .collect();
    env.warp(1);
    for (venue, _) in &slots {
        ok(flag(&mut env, *venue));
    }
    assert_eq!(env.venue_guard().upgraded, GUARD_MASK_PUMP_CURVE);
    for (k, (venue, slot)) in slots.iter().enumerate() {
        crank_all(&mut env, &markets);
        for (pm, q) in &markets {
            assert_err(&try_open(&mut env, pm, *q), E::VenueUpgraded);
        }
        ok(revalidate(&mut env, *venue, *slot));
        println!("revalidated {} of 3", k + 1);
    }
    assert_eq!(env.venue_guard().upgraded, 0);
    for (pm, q) in &markets {
        ok(try_open(&mut env, pm, *q));
    }
}

/// The masks from the curve's side: Raydium AMM v4, CPMM and Jupiter upgrades (their ProgramData
/// are not on the pump fork; synthetic headers at the compile-time addresses) set their bits and
/// never block a curve open.
#[test]
fn raydium_and_jupiter_upgrades_leave_curve_opens_alone() {
    let (mut env, markets) = two_markets();
    for venue in [GUARD_RAYDIUM_V4, GUARD_RAYDIUM_CPMM, GUARD_JUPITER] {
        let slot = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)] + 1;
        env.set_venue_program_data(venue, slot);
        ok(flag(&mut env, venue));
    }
    let bits = (1u32 << GUARD_RAYDIUM_V4) | (1 << GUARD_RAYDIUM_CPMM) | (1 << GUARD_JUPITER);
    assert_eq!(env.venue_guard().upgraded, bits);
    assert_eq!(bits & GUARD_MASK_PUMP_CURVE, 0);
    for (pm, q) in &markets {
        ok(try_open(&mut env, pm, *q));
    }
}

/// `flag_venue_upgraded` / `revalidate_venue` take only the venue's own ProgramData, by
/// address, owner and tag, on the real pump accounts (`VenueAccountMismatch`).
#[test]
fn the_program_data_account_is_checked() {
    let (mut env, _) = Env::fork_pump_coin(T22);
    let ea = env.pause_authority.insecure_clone();
    let s = fork::FIXTURE_GUARD_SLOTS[usize::from(GUARD_PUMP)];
    let wrong_keys = [
        PumpProgram::Fees.program_data().address,
        PumpProgram::Amm.program_data().address,
        PumpProgram::Pump.id(),
        pda::program_data(),
        Pubkey::new_unique(),
    ];
    for key in wrong_keys {
        assert_err(
            &env.send(&[ix::flag_venue_upgraded_with(GUARD_PUMP, key)], &[]),
            E::VenueAccountMismatch,
        );
        assert_err(
            &env.send(
                &[ix::revalidate_venue_with(&ea.pubkey(), GUARD_PUMP, s, key)],
                &[&ea],
            ),
            E::VenueAccountMismatch,
        );
    }
    let key = PumpProgram::Pump.program_data().address;
    let real = read::account(&env.svm, &key).unwrap();
    // The right address with another owner, or another loader state (a `Buffer` tag).
    let mut forged = real.clone();
    forged.owner = anchor_lang::system_program::ID;
    env.svm.set_account(key, forged).unwrap();
    assert_err(&flag(&mut env, GUARD_PUMP), E::VenueAccountMismatch);
    assert_err(
        &revalidate(&mut env, GUARD_PUMP, s),
        E::VenueAccountMismatch,
    );
    let mut forged = real.clone();
    forged.data[0] = 1;
    env.svm.set_account(key, forged).unwrap();
    assert_err(&flag(&mut env, GUARD_PUMP), E::VenueAccountMismatch);
    assert_err(
        &revalidate(&mut env, GUARD_PUMP, s),
        E::VenueAccountMismatch,
    );
    env.svm.set_account(key, real).unwrap();
    assert_err(&flag(&mut env, GUARD_PUMP), E::VenueNotUpgraded);
    assert_eq!(env.venue_guard().upgraded, 0);
}

/// The latch is per instruction: in one transaction, `[flag, open]` refuses the open (the whole
/// transaction reverts, flag included), while `[open, flag]` is the residual of ARCH §3.1
/// within one transaction — the open precedes the flag and lands.
#[test]
fn a_flag_and_an_open_in_one_transaction() {
    let (mut env, markets) = two_markets();
    let (pm, q) = &markets[1];
    env.warp(1);
    loader_upgrade(&mut env, PumpProgram::Fees);
    env.warp(1);
    crank_all(&mut env, &markets);
    let b = env.borrower();
    let open = open_short_ix(
        pm,
        &b.key(),
        0,
        *q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let flag_ix = ix::flag_venue_upgraded(GUARD_PUMP_FEES);
    let cu = pump::cu_limit_ix(400_000);
    let res = env.send(&[cu.clone(), flag_ix.clone(), open.clone()], &[&b.kp]);
    assert_err(&res, E::VenueUpgraded);
    assert_eq!(env.venue_guard().upgraded, 0, "reverted with the open");
    let meta = ok(env.send(&[cu, open, flag_ix], &[&b.kp]));
    let _: Open = events::one(&meta);
    let _: VenueUpgradeFlagged = events::one(&meta);
    assert!(env.venue_guard().is_flagged(GUARD_PUMP_FEES));
    assert_err(&try_open(&mut env, pm, *q), E::VenueUpgraded);
}

/// U5 (ARCH §6.1, §6.5) on the real accounts: after real upgrades, the latch instructions cost
/// the same on the 10 MiB pump and PumpSwap ProgramData as on pump-fees' 0.95 MB: the
/// input-region copy is not metered in CU (pump-fees, which has an AIR-01 fingerprint slot, pays
/// only the few instructions of its lookup). Each stays far inside §6.1's 400 k.
#[test]
fn u5_latch_cost_does_not_grow_with_the_program_data() {
    let (mut env, _) = Env::fork_pump_coin(T22);
    let mut flags: Vec<(String, u64)> = Vec::new();
    let mut revals: Vec<(String, u64)> = Vec::new();
    let cu = |meta: &TransactionMetadata| meta.compute_units_consumed;
    env.warp(1);
    for program in PumpProgram::ALL {
        let s = loader_upgrade(&mut env, program);
        env.warp(1);
        let venue = program.guard();
        let name = format!("{program:?} ({} B)", size_of(program));
        flags.push((name.clone(), cu(&ok(flag(&mut env, venue)))));
        revals.push((name, cu(&ok(revalidate(&mut env, venue, s)))));
    }
    for (what, list) in [
        ("flag_venue_upgraded", &flags),
        ("revalidate_venue", &revals),
    ] {
        for (name, units) in list {
            println!("{what} on the {name} ProgramData: {units} CU");
            assert!(*units <= LATCH_CU_LIMIT, "{what} {name}: {units} CU");
        }
        // Pump and PumpSwap (10 MiB each) take the same path: equal to the unit.
        assert_eq!(list[0].1, list[2].1, "{what}: {list:?}");
        let (lo, hi) = list
            .iter()
            .fold((u64::MAX, 0), |(lo, hi), (_, u)| (lo.min(*u), hi.max(*u)));
        assert!(
            hi - lo <= AIR01_LOOKUP_CU,
            "{what}: {list:?} — the cost must not depend on the account size"
        );
    }
}

/// The most the AIR-01 fingerprint lookup adds on a venue that has a fingerprint slot (pump-fees,
/// AMM v4, CPMM, Jupiter) over one that has none (pump, PumpSwap), with nothing recorded.
const AIR01_LOOKUP_CU: u64 = 32;

/// The loader's own rule the latch relies on: one upgrade per slot per program, so a flagged
/// slot names exactly one deployment (on the 10 MiB pump ProgramData).
#[test]
fn one_upgrade_per_slot() {
    let (mut env, _) = Env::fork_pump_coin(T22);
    env.warp(1);
    loader_upgrade(&mut env, PumpProgram::Pump);
    let (res, _) = send_upgrade(&mut env, PumpProgram::Pump);
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
    loader_upgrade(&mut env, PumpProgram::Pump);
}

/// The upgrades mainnet made on 2026-10-08/12/13, replayed with the real accounts (as
/// `latch_raydium_it::the_2026_10_cpmm_redeploy_latches_until_revalidated` for CPMM): the fork
/// starts on the 2026-10-02 builds of pump, pump-fees and PumpSwap (group `pump_programs_2026_10`,
/// the ProgramData at the slots `VENUE_GUARD_SLOTS_2026_10_03` names and `initialize_config`
/// records — the first build with pump's v3 trades, which short.win's curve adapter needs since
/// pump-final §6; the 2026-09 builds have none, `buy_v3` there is 101), positions are opened by
/// `sell_v3` on both 115-byte curves — whose sales grow them to 141 bytes, the 2026-10-02 build's
/// size; the Token-2022 curve's rent reserve held at `minimum_balance(115)`, so its first sale pays
/// 132,080 lamports of growth out of `S` — and the loader's own `Upgrade` installs the current
/// builds' ELFs (`PumpDeploy::CURRENT`: pump 2026-10-13, PumpSwap 2026-10-08, pump-fees
/// 2026-10-12) in mainnet's order (PumpSwap, pump, pump-fees). The programs answer as the new
/// builds. Flagged by anyone, they refuse opens on every curve market, while a top-up, a close, a
/// liquidation after a real pump and an expiry run on the new pump by `buy_v3` — whose buybacks
/// grow the curves to 166 bytes, the Token-2022 one for another 127,000 lamports of rent, which
/// I13 re-quotes on the build pump's ProgramData holds (`PumpDeploy::held`). A third market on the
/// near-complete 151-byte curve carries a position opened on the 2026-10-02 build (whose `buy_v3`
/// cannot buy past `rT`, T-PUMP-MATH-V3 (g)); after the upgrade the crowd leaves `rT = q/2` and
/// its close is a **synthetic** exit on the new pump (pump-final §5.2: `rT` on the curve, `q − rT`
/// from the would-be pool, `PumpMigrating` in the same instruction), latched or not. Only the
/// loader's slots revalidate, all three before a curve opens again; then opens land on the
/// redeploy.
#[test]
fn the_2026_10_08_pump_redeploys_latch_until_revalidated() {
    // The fork as of 2026-10-03: the 2026-10-02 builds, the baseline the program constant names.
    let mut env = Env::new();
    let mut args = env.default_init_args();
    args.guard_slot = VENUE_GUARD_SLOTS_2026_10_03;
    ok(env.initialize_config(args));
    for program in PumpProgram::ALL {
        fork::load_pump_deploy(&mut env.svm, program, PumpDeploy::Oct2026);
    }
    env.load_pump_statics();
    let spl_curve = env.load_pump_coin(SPL);
    let t22_curve = env.load_pump_coin(T22);
    let near_curve = env.load_pump_coin(NEAR);
    env.set_pump_rent();
    env.set_pump_clock();
    let old = |program: PumpProgram| VENUE_GUARD_SLOTS_2026_10_03[usize::from(program.guard())];
    for program in PumpProgram::ALL {
        assert_eq!(old(program), PumpDeploy::Oct2026.deploy_slot(program));
        assert_eq!(header(&env, program).slot, old(program), "{program:?}");
        assert_eq!(
            env.venue_guard().guard_slot[usize::from(program.guard())],
            old(program)
        );
        assert_eq!(
            PumpDeploy::held(&env.svm, program),
            Some(PumpDeploy::Oct2026)
        );
        assert_err(&flag(&mut env, program.guard()), E::VenueNotUpgraded);
    }
    assert_eq!(
        env.pump_build_running(PumpProgram::Pump),
        PumpDeploy::Oct2026
    );
    assert_eq!(
        env.pump_build_running(PumpProgram::Amm),
        PumpDeploy::Oct2026
    );

    let markets = [
        add_market(&mut env, spl_curve),
        add_market(&mut env, t22_curve),
    ];
    let [(spl, qs), (t22, qt)] = &markets;
    let (near, qn) = add_market(&mut env, near_curve);
    let (b_syn, n_syn) = open_ok(&mut env, &near, qn);
    let curve_len =
        |env: &Env, pm: &PumpMarket| read::account(&env.svm, &pm.curve.curve).unwrap().data.len();
    // The Token-2022 curve's rent reserve held at `minimum_balance(115)` (the dump's is
    // over-funded at the old rent): each build's growth charges the trader its rent. The reserve
    // is `lamports − rQ −` the fee buckets v3 trades keep on the curve.
    let rent = |env: &Env, len: usize| env.svm.minimum_balance_for_rent_exemption(len);
    let reserve = |env: &Env, pm: &PumpMarket| {
        let acc = read::account(&env.svm, &pm.curve.curve).unwrap();
        let (creator_fee, protocol_fees) = fork::curve_fee_buckets(&acc.data);
        acc.lamports - pm.curve.state(&env.svm).real_quote_reserves - creator_fee - protocol_fees
    };
    assert_eq!(curve_len(&env, t22), vpump::CURVE_MIN_LEN);
    let rq = t22.curve.state(&env.svm).real_quote_reserves;
    let mut acc = env.svm.get_account(&t22.curve.curve).unwrap();
    acc.lamports = rq + rent(&env, vpump::CURVE_MIN_LEN);
    env.svm.set_account(t22.curve.curve, acc).unwrap();
    let old_growth = rent(&env, PumpDeploy::Oct2026.curve_len()) - rent(&env, vpump::CURVE_MIN_LEN);
    assert_eq!(old_growth, 132_080);
    let sale = t22.curve.quote_sell(&env.svm, *qt).unwrap();

    let (b_liq, n_liq) = open_ok(&mut env, spl, *qs);
    let mut b_close = env.borrower();
    let (res, n_close) = env.open(t22, &mut b_close, *qt);
    let open: Open = events::one(&ok(res));
    assert_eq!(
        open.s,
        sale.proceeds - old_growth,
        "S net of the 2026-10-02 growth"
    );
    let (b_exp, n_exp) = open_ok(&mut env, t22, *qt);
    let (b_top, n_top) = open_ok(&mut env, spl, *qs);
    for pm in [spl, t22] {
        assert_eq!(
            curve_len(&env, pm),
            PumpDeploy::Oct2026.curve_len(),
            "the 2026-10-02 build's growth"
        );
    }
    assert_eq!(
        reserve(&env, t22),
        rent(&env, PumpDeploy::Oct2026.curve_len())
    );

    // The redeploys, in mainnet's order: the loader writes each slot and the current ELF.
    let mut slots = Vec::new();
    for program in [PumpProgram::Amm, PumpProgram::Pump, PumpProgram::Fees] {
        env.warp(1);
        let s = loader_upgrade_elf(&mut env, program, PumpDeploy::CURRENT.elf(program));
        assert_ne!(s, old(program));
        assert!(
            PumpDeploy::held(&env.svm, program)
                .is_some_and(|d| d.same_build(PumpDeploy::CURRENT, program)),
            "{program:?}"
        );
        slots.push((program, s));
    }
    env.warp(1);
    for program in [PumpProgram::Pump, PumpProgram::Amm] {
        assert!(
            env.pump_build_running(program)
                .same_build(PumpDeploy::CURRENT, program),
            "{program:?}"
        );
    }

    // Anyone flags each in a later slot; opens are refused on both curve markets.
    for (program, s) in &slots {
        let venue = program.guard();
        let meta = ok(flag(&mut env, venue));
        let ev: VenueUpgradeFlagged = events::one(&meta);
        assert_eq!(
            (ev.venue, ev.guard_slot, ev.programdata_slot),
            (venue, old(*program), *s)
        );
    }
    assert_eq!(env.venue_guard().upgraded, GUARD_MASK_PUMP_CURVE);
    for (pm, q) in &markets {
        assert_err(&try_open(&mut env, pm, *q), E::VenueUpgraded);
    }
    assert_err(&try_open(&mut env, &near, qn), E::VenueUpgraded);

    // Exits never read the latch (I10), and buy back on the new pump: a top-up, a close on the
    // Token-2022 curve (its buyback grows it to 166 bytes) ...
    crank_all(&mut env, &markets);
    let meta = ok(env.send(
        &[add_margin_ix(spl, &b_top.key(), n_top, 1_000_000_000)],
        &[&b_top.kp],
    ));
    let _: MarginAdded = events::one(&meta);
    env.warp(1);
    let vault0 = read::token_amount(&env.svm, &t22.keys.pool_vault).unwrap();
    let meta = ok(env.close(t22, &b_close, n_close));
    let c: Close = events::one(&meta);
    assert_eq!(c.q, *qt);
    assert_eq!(
        read::token_amount(&env.svm, &t22.keys.pool_vault).unwrap() - vault0,
        *qt,
        "q bought back into the lending vault"
    );
    assert_eq!(curve_len(&env, t22), PumpDeploy::CURRENT.curve_len());
    assert_eq!(
        reserve(&env, t22),
        rent(&env, PumpDeploy::CURRENT.curve_len()),
        "the redeploy's growth, 127,000 more, out of the buyback (I13 re-quotes it)"
    );
    // ... a synthetic close on the near-complete curve: the crowd leaves `rT = q/2`, the close's
    // `buy_v3(q)` completes the curve past `rT` on the new pump (the 2026-10-02 build could not) ...
    let u = CurveUser::new(&mut env, &near.curve);
    let rt = near.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(&mut env, &near.curve, rt - qn / 2, u64::MAX));
    env.warp(1);
    let vault0 = read::token_amount(&env.svm, &near.keys.pool_vault).unwrap();
    let meta = ok(env.close(&near, &b_syn, n_syn));
    let c: Close = events::one(&meta);
    assert_eq!(c.q, qn);
    assert_eq!(
        read::token_amount(&env.svm, &near.keys.pool_vault).unwrap() - vault0,
        qn
    );
    assert_eq!(
        fork::pump_post_complete_buy_events(&meta).len(),
        1,
        "a pool part"
    );
    let s = near.curve.state(&env.svm);
    assert!(
        s.complete && s.post_complete_base_out == qn - qn / 2,
        "pcB = q − rT"
    );
    assert_eq!(
        env.market(&near.market()).venue_kind(),
        Some(shortwin::state::VenueKind::PumpMigrating)
    );
    assert_eq!(
        env.venue_guard().upgraded,
        GUARD_MASK_PUMP_CURVE,
        "still latched"
    );
    // ... a liquidation after a real pump on the SPL Token curve ...
    let vault0 = read::token_amount(&env.svm, &spl.keys.pool_vault).unwrap();
    let liq = liquidate_after_a_pump(&mut env, spl, &b_liq, n_liq);
    assert!(liq.buyback_cost > 0);
    assert_eq!(
        read::token_amount(&env.svm, &spl.keys.pool_vault).unwrap() - vault0,
        *qs,
        "Q bought back into the lending vault"
    );
    assert_eq!(curve_len(&env, spl), PumpDeploy::CURRENT.curve_len());
    // ... and an expiry on the Token-2022 curve.
    let keeper = env.funded_keypair();
    let deadline = env.position(t22, &b_exp.key(), n_exp).deadline;
    let now = env.now();
    env.warp(deadline + 1 - now);
    env.tick(t22, 3);
    let meta = ok(env.send(
        &keeper_tx(expire_ix(t22, &keeper.pubkey(), &b_exp.key(), n_exp)),
        &[&keeper],
    ));
    let e: Expire = events::one(&meta);
    assert!(e.buyback_cost > 0);
    assert_eq!(env.venue_guard().upgraded, GUARD_MASK_PUMP_CURVE);

    // Only each loader slot revalidates, the 2026-10-03 baseline does not; every bit must clear.
    for (k, (program, s)) in slots.iter().enumerate() {
        let venue = program.guard();
        for wrong in [old(*program), s + 1] {
            assert_err(&revalidate(&mut env, venue, wrong), E::VenueSlotMismatch);
        }
        crank_all(&mut env, &markets);
        let ev: VenueRevalidated = events::one(&ok(revalidate(&mut env, venue, *s)));
        assert_eq!((ev.guard_slot, ev.expected_slot), (old(*program), *s));
        assert_eq!(env.venue_guard().guard_slot[usize::from(venue)], *s);
        if k + 1 < slots.len() {
            for (pm, q) in &markets {
                assert_err(&try_open(&mut env, pm, *q), E::VenueUpgraded);
            }
        }
    }
    assert_eq!(env.venue_guard().upgraded, 0);
    for (pm, q) in &markets {
        ok(try_open(&mut env, pm, *q));
    }
    env.warp(1);
    assert_eq!(
        env.pump_build_running(PumpProgram::Pump),
        PumpDeploy::CURRENT
    );
}

/// Since pump-final §6 every curve leg trades v3: on the 2026-09 builds (no `buy_v3` / `sell_v3`:
/// pump's fallback 101) a curve open fails inside pump's CPI and nothing is written — the latch
/// replays above start from the 2026-10-02 builds.
#[test]
fn the_2026_09_builds_have_no_v3_trades() {
    let mut env = Env::new();
    let mut args = env.default_init_args();
    args.guard_slot = VENUE_GUARD_SLOTS_2026_09_27;
    ok(env.initialize_config(args));
    for program in PumpProgram::ALL {
        fork::load_pump_deploy(&mut env.svm, program, PumpDeploy::Sep2026);
    }
    env.load_pump_statics();
    let curve = env.load_pump_coin(T22);
    env.set_pump_rent();
    env.set_pump_clock();
    let (pm, q) = add_market(&mut env, curve);
    let before = read::account(&env.svm, &pm.curve.curve);
    let res = try_open(&mut env, &pm, q);
    assert_eq!(custom_code(&res), Some(101), "InstructionFallbackNotFound");
    assert_eq!(read::account(&env.svm, &pm.curve.curve), before);
}
// ---- AIR-01: an extension is not an upgrade ----------------------------------------------------

/// ★ AIR-01 (I-01) on the pump side. pump and PumpSwap sit at the loader's 10 MiB cap, so no one
/// can extend them; pump-fees (0.95 MB) can be, and its bit is in both pump masks: a stranger's
/// `[ExtendProgram(pump-fees), flag_venue_upgraded]` stopped every curve and PumpSwap open.
/// Reproduced on the pre-fix build: the flag after the extension set the bit. Now, once the
/// Emergency has revalidated pump-fees naming the sha256 of its trimmed ELF, the extension —
/// alone or bundled with the flag — is `VenueNotUpgraded` and writes nothing, and curve opens keep
/// landing.
#[test]
fn air01_an_extension_of_pump_fees_is_not_an_upgrade() {
    let (mut env, markets) = two_markets();
    for program in [PumpProgram::Pump, PumpProgram::Amm] {
        assert_eq!(size_of(program), 10 * 1024 * 1024);
        assert!(
            extend(&mut env, program, 10_240).is_err(),
            "{program:?}: at the cap, no extension"
        );
    }
    let fees = PumpProgram::Fees;
    let venue = fees.guard();
    let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
    // The program hashes exactly what `solana-verify` hashes (the fixture's pin,
    // `tests/fixtures/README.md`: `8ad2ca93…b8b8`, the 2026-10-12 build since the 2026-10-13
    // re-pin; `f166c296…62fd` on the 2026-10-02 one).
    let reviewed = reviewed_hash(&env, fees);
    let hex: String = reviewed.iter().map(|b| format!("{b:02x}")).collect();
    assert!(
        hex.starts_with("8ad2ca93") && hex.ends_with("b8b8"),
        "{hex}"
    );
    ok(revalidate_reviewed(&mut env, fees, dumped, reviewed));

    env.warp(1);
    stranger_extends(&mut env, fees, 10_240);
    env.warp(1);
    let before = read::account(&env.svm, &pda::venue_guard()).unwrap().data;
    let res = flag_hashing(&mut env, venue);
    assert_err(&res, E::VenueNotUpgraded);
    assert_eq!(
        read::account(&env.svm, &pda::venue_guard()).unwrap().data,
        before,
        "nothing written"
    );
    // The compute budget of the hash path on the largest fingerprinted ELF.
    let units = res.expect_err("refused").meta.compute_units_consumed;
    let elf = fees.elf().len() as u64;
    println!("pump-fees: flag_venue_upgraded after an extension: {units} CU (trimmed ELF {elf} B)");
    assert!(units > elf / 2 && units < elf / 2 + 30_000, "{units} CU");
    env.warp(1);
    let stranger = env.funded_keypair();
    let mut ixs = vec![fork::extend_program_ix(
        &fees.id(),
        &stranger.pubkey(),
        10_240,
    )];
    ixs.extend(with_hash_budget(ix::flag_venue_upgraded(venue)));
    assert_err(&env.send(&ixs, &[&stranger]), E::VenueNotUpgraded);
    assert_eq!(env.venue_guard().upgraded, 0);
    env.warp(1);
    crank_all(&mut env, &markets);
    for (pm, q) in &markets {
        ok(try_open(&mut env, pm, *q));
    }
}

/// AIR-01, the other half on pump-fees: an upgrade next to an extension still latches. pump-fees
/// upgraded to its own ELF with one byte changed (same size, inside the old account) after a
/// stranger's extension sets the bit and refuses curve opens; the old ELF's hash no longer
/// revalidates a later slot, the new one does, and opens resume.
#[test]
fn air01_a_pump_fees_upgrade_after_an_extension_still_latches() {
    let (mut env, markets) = two_markets();
    let fees = PumpProgram::Fees;
    let venue = fees.guard();
    let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)];
    let old = reviewed_hash(&env, fees);
    ok(revalidate_reviewed(&mut env, fees, dumped, old));
    env.warp(1);
    stranger_extends(&mut env, fees, 10_240);
    env.warp(1);
    let s = loader_upgrade_elf(&mut env, fees, &fees_elf_one_byte_changed());
    env.warp(1);
    let meta = ok(flag_hashing(&mut env, venue));
    let ev: VenueUpgradeFlagged = events::one(&meta);
    assert_eq!((ev.guard_slot, ev.programdata_slot), (dumped, s));
    crank_all(&mut env, &markets);
    for (pm, q) in &markets {
        assert_err(&try_open(&mut env, pm, *q), E::VenueUpgraded);
    }
    env.warp(1);
    stranger_extends(&mut env, fees, 10_240);
    env.warp(1);
    assert_err(
        &revalidate_reviewed(&mut env, fees, s, old),
        E::VenueSlotMismatch,
    );
    let new = reviewed_hash(&env, fees);
    assert_ne!(new, old, "another build");
    ok(revalidate_reviewed(&mut env, fees, s, new));
    assert_eq!(env.venue_guard().upgraded, 0);
    crank_all(&mut env, &markets);
    for (pm, q) in &markets {
        ok(try_open(&mut env, pm, *q));
    }
}
