//! T-LATCH on the dumped PumpSwap ProgramData (ARCHITECTURE §2.3.2, §3.1 "Venue-upgrade latch",
//! §5.5, §6.5, §8.2, §9 M6; A7, R4B-7; I17; U5), on the PumpSwap fork of
//! `shortwin_litesvm_tests::pamm` / `fork`: the real PumpSwap (10,485,760 B) and pump-fees
//! (948,989 B) ProgramData accounts, dumped at their mainnet deploy slots, which
//! `initialize_config` recorded as `guard_slot` (`fork::FIXTURE_GUARD_SLOTS`: the 2026-10-02
//! redeploys).
//!
//! The upgrade mainnet itself made is replayed too (M10 pump revalidation): the 2026-09 builds
//! (`fork::PumpDeploy::Sep2026`, the slots `VENUE_GUARD_SLOTS_2026_09_27` names) upgraded by the
//! loader to the 2026-10-02 redeploys' ELFs, with positions opened on the old builds and exited on
//! the new ones, which latch PumpSwap opens until Emergency revalidates the loader's slots.
//!
//! An upgrade here is a **real** one: the upgradeable loader's own `Upgrade` instruction, with a
//! buffer holding the redeploy's dumped ELF (`fork::PumpDeploy::CURRENT`) and the program's real
//! upgrade authority as signer (its signature forged for that one transaction,
//! `fork::admin_send`; the hooks run). The loader writes the new deploy slot into the ProgramData
//! header, as on mainnet, and the program keeps running.
//!
//! What it settles for M6:
//!
//! - after a real upgrade of **PumpSwap or pump-fees**, `flag_venue_upgraded` (anyone) sets the bit
//!   and records the loader's deploy slot; opens on a PumpSwap market are refused (`VenueUpgraded`)
//!   — the PumpSwap mask holds pAMM and pump-fees (R4B-7) — while a close, a liquidation after a
//!   real pump, a crank, `sweep_trader` and a no-op `sol_settle` keep working (exits never read the
//!   latch, I10);
//! - a **pump (curve)** upgrade does **not** refuse PumpSwap opens (the mask excludes it);
//! - `revalidate_venue(venue, expected_slot)` clears it only for the slot the loader wrote; both
//!   PumpSwap bits must be clear before an open is accepted again;
//! - U5: `flag_venue_upgraded` and `revalidate_venue` cost the same CU on the real 10 MiB PumpSwap
//!   ProgramData as on pump-fees' 0.95 MB, far inside §6.1's 400 k.
//! - the ProgramData account is checked for `GUARD_PUMP_AMM` and `GUARD_PUMP_FEES` (M10 G-A7):
//!   wrong keys, and a moved-slot header under another owner or loader state, are
//!   `VenueAccountMismatch` for both instructions; nothing is latched until the real account moves.
//!
//! Every transaction goes through `Env::send` (or `fork::admin_send`): the invariant hooks run
//! after each one.

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::constants::{
    GUARD_PUMP, GUARD_PUMP_AMM, GUARD_PUMP_FEES, VENUE_GUARD_SLOTS_2026_09_27,
};
use shortwin::errors::ShortwinError as E;
use shortwin::events::{
    Close, Expire, Liquidate, Open, TraderSwept, VenueRevalidated, VenueUpgradeFlagged,
};
use shortwin_litesvm_tests::engine::{
    Borrower, close_short_ix, crank_price_ix, expire_ix, liquidate_ix, sol_settle_ix,
};
use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
use shortwin_litesvm_tests::fork::{self, PumpDeploy, PumpProgram};
use shortwin_litesvm_tests::pamm::{self, PammCoin, PammMarket, PammPool, PammUser};
use shortwin_litesvm_tests::pump::cu_limit_ix;
use shortwin_litesvm_tests::{Env, TxResult, events, ix, pda, read};
use shortwin_venue_state::{program_data, pump as vpump};
use solana_account::Account;
use solana_signer::Signer;

const SOL: u64 = 1_000_000_000;
const MATURE: PammCoin = PammCoin::Mature;
const SPL261: PammCoin = PammCoin::Legacy261;
const PAMM_PROGRAM_DATA_LEN: usize = 10_485_760;
const PAMM_FEES_PROGRAM_DATA_LEN: usize = 948_989;
const LATCH_CU_LIMIT: u64 = 400_000;

const SYSVAR_RENT: Pubkey = Pubkey::from_str_const("SysvarRent111111111111111111111111111111111");
const SYSVAR_CLOCK: Pubkey = Pubkey::from_str_const("SysvarC1ock11111111111111111111111111111111");

// ---- loader upgrade helpers (as latch_pump_it) --------------------------------------------------

fn header(env: &Env, program: PumpProgram) -> program_data::ProgramDataHeader {
    let a = read::account(&env.svm, &program.program_data().address).expect("ProgramData");
    program_data::read(&a.owner.to_bytes(), &a.data).expect("a ProgramData header")
}

fn size_of(program: PumpProgram) -> usize {
    match program {
        PumpProgram::Amm | PumpProgram::Pump => PAMM_PROGRAM_DATA_LEN,
        PumpProgram::Fees => PAMM_FEES_PROGRAM_DATA_LEN,
    }
}

fn send_upgrade(env: &mut Env, program: PumpProgram, elf: &[u8]) -> (TxResult, Pubkey) {
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

/// A **real** upgrade of `program` by the loader to its dumped ELF (`PumpDeploy::CURRENT`'s);
/// returns the slot it recorded.
fn loader_upgrade(env: &mut Env, program: PumpProgram) -> u64 {
    loader_upgrade_to(env, program, program.elf())
}

/// [`loader_upgrade`] to `elf`.
fn loader_upgrade_to(env: &mut Env, program: PumpProgram, elf: &[u8]) -> u64 {
    let slot = env.clock().slot;
    let (res, _) = send_upgrade(env, program, elf);
    ok(res);
    let h = header(env, program);
    assert_eq!(h.slot, slot, "the loader records the upgrade's slot");
    let pd = read::account(&env.svm, &program.program_data().address).unwrap();
    assert_eq!(
        pd.data.len(),
        size_of(program),
        "the account keeps its size"
    );
    let start = program_data::HEADER_LEN;
    assert_eq!(
        &pd.data[start..start + elf.len()],
        elf,
        "the redeployed ELF"
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

// ---- PumpSwap market setup ----------------------------------------------------------------------

fn q_for(env: &Env, p: &PammPool) -> u64 {
    let r = p.reserves(&env.svm).expect("reserves");
    let base = u128::from(r.base);
    let y = r.effective_quote.max(1);
    let q = (base / 100).min(base * u128::from(10 * SOL) / y);
    u64::try_from(q).expect("u64")
}

fn wait_out_boost(env: &mut Env, pm: &PammMarket) {
    let params = env.config().params;
    loop {
        let m = env.market(&pm.market());
        if pm.pool.boost_amount(&env.svm) <= params.boost_dust_lamports
            || env.now() >= m.graduated_at + i64::from(params.boost_window_s)
        {
            return;
        }
        env.warp(30);
        ok(env.send(&[crank_price_ix(pm)], &[]));
    }
}

fn list_market(env: &mut Env, p: &PammPool) -> (PammMarket, u64) {
    let q = q_for(env, p);
    let curve = pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < vpump::CURVE_MIN_LEN)
    {
        let payer = env.payer.pubkey();
        ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, q.saturating_mul(40), 60);
    wait_out_boost(env, &pm);
    let q = q_for(env, &pm.pool);
    (pm, q)
}

fn setup(coin: PammCoin) -> (Env, PammMarket, u64) {
    let (mut env, p) = Env::fork_pamm_coin(coin);
    let (pm, q) = list_market(&mut env, &p);
    (env, pm, q)
}

fn open(env: &mut Env, pm: &PammMarket, q: u64) -> (Borrower, u64) {
    let mut b = env.borrower();
    let (res, nonce) = env.open(pm, &mut b, q);
    ok(res);
    (b, nonce)
}

fn try_open(env: &mut Env, pm: &PammMarket, q: u64) -> TxResult {
    let mut b = env.borrower();
    env.open(pm, &mut b, q).0
}

fn close(env: &mut Env, pm: &PammMarket, b: &Borrower, nonce: u64) -> TxResult {
    let ix = close_short_ix(
        pm,
        &b.key(),
        nonce,
        u64::MAX,
        shortwin::venues::ExtrasSel::NONE,
        &[],
    );
    let kp = b.kp.insecure_clone();
    env.send(&[cu_limit_ix(400_000), ix], &[&kp])
}

// ---- tests --------------------------------------------------------------------------------------

/// The dumped PumpSwap ProgramData is the recorded slot; an unchanged header is not an upgrade (no
/// write); a real upgrade of PumpSwap refuses opens on a PumpSwap market (`VenueUpgraded`) while a
/// no-op `sol_settle`, a close, a crank and `sweep_trader` keep working (the price is stable, so no
/// EMA disturbance masks the effect); `revalidate_venue` clears it for the loader's slot and opens
/// resume. (A liquidation after a real pump is `a_liquidation_runs_on_the_upgraded_pamm`.)
#[test]
fn a_pamm_upgrade_refuses_opens_and_exits_keep_working() {
    let (mut env, pm, q) = setup(MATURE);
    let (b_close, n_close) = open(&mut env, &pm, q);
    let (b_hold, n_hold) = open(&mut env, &pm, q);

    // The dumped header is the recorded slot; an unchanged header writes nothing.
    let dumped = fork::FIXTURE_GUARD_SLOTS[usize::from(GUARD_PUMP_AMM)];
    assert_eq!(header(&env, PumpProgram::Amm).slot, dumped);
    assert_eq!(
        env.venue_guard().guard_slot[usize::from(GUARD_PUMP_AMM)],
        dumped
    );
    let before = read::account(&env.svm, &pda::venue_guard()).unwrap().data;
    assert_err(&flag(&mut env, GUARD_PUMP_AMM), E::VenueNotUpgraded);
    assert_eq!(
        read::account(&env.svm, &pda::venue_guard()).unwrap().data,
        before,
        "no write when unchanged"
    );

    // A real loader upgrade, flagged in a later slot.
    let slot = loader_upgrade(&mut env, PumpProgram::Amm);
    let meta = ok(flag(&mut env, GUARD_PUMP_AMM));
    let ev: VenueUpgradeFlagged = events::one(&meta);
    assert_eq!(ev.venue, GUARD_PUMP_AMM);
    assert_eq!(
        env.venue_guard().upgraded & (1 << GUARD_PUMP_AMM),
        1 << GUARD_PUMP_AMM
    );

    // Opens are refused.
    let keeper = env.funded_keypair();
    assert_err(&try_open(&mut env, &pm, q), E::VenueUpgraded);

    // A no-op `sol_settle` on a healthy position, then a close — both keep working after the
    // upgrade (the close's buyback runs on the redeployed pAMM).
    env.warp(1);
    ok(env.send(&[crank_price_ix(&pm)], &[]));
    let s = sol_settle_ix(&env, &pm, &keeper.pubkey(), &b_hold.key(), n_hold);
    let meta = ok(env.send(&[s], &[&keeper]));
    assert!(
        events::of::<shortwin::events::SolSettle>(&meta).is_empty(),
        "healthy: no settlement"
    );
    let meta = ok(close(&mut env, &pm, &b_close, n_close));
    let _: Close = events::one(&meta);

    // `sweep_trader` keeps working (a donation to the trader).
    let griefer = env.funded_keypair();
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&1_000_000u64.to_le_bytes());
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
    let meta = ok(env.send(&[pamm::sweep_trader_pamm_ix(&env, &pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(ev.lamports, 1_000_000);

    // Only the loader's slot revalidates; then opens resume (the price is stable, so no EMA
    // deviation blocks it; b_hold is still open, well under the notional cap).
    assert_err(
        &revalidate(&mut env, GUARD_PUMP_AMM, slot + 1),
        E::VenueSlotMismatch,
    );
    let meta = ok(revalidate(&mut env, GUARD_PUMP_AMM, slot));
    let _: VenueRevalidated = events::one(&meta);
    assert_eq!(env.venue_guard().upgraded, 0);
    env.warp(1);
    ok(env.send(&[crank_price_ix(&pm)], &[]));
    ok(try_open(&mut env, &pm, q));
}

/// A liquidation after a real ×2.8 pump keeps working while PumpSwap is flagged upgraded (exits
/// never read the latch, I10; the buyback runs on the redeployed pAMM).
#[test]
fn a_liquidation_runs_on_the_upgraded_pamm() {
    let (mut env, pm, q) = setup(MATURE);
    let (b_liq, n_liq) = open(&mut env, &pm, q);
    loader_upgrade(&mut env, PumpProgram::Amm);
    ok(flag(&mut env, GUARD_PUMP_AMM));
    assert_err(&try_open(&mut env, &pm, q), E::VenueUpgraded);
    let keeper = env.funded_keypair();
    let u = PammUser::new(&mut env, &pm.pool, 2_000 * SOL);
    ok(u.push_price(&mut env, &pm.pool, 28, 10));
    let mut liquidated = false;
    for _ in 0..900 {
        env.warp(1);
        ok(env.send(&[crank_price_ix(&pm)], &[]));
        let ixl = liquidate_ix(&pm, &keeper.pubkey(), &b_liq.key(), n_liq);
        let res = env.send(&[cu_limit_ix(400_000), ixl], &[&keeper]);
        match custom_code(&res) {
            None => {
                let _: Liquidate = events::one(&ok(res));
                liquidated = true;
                break;
            }
            Some(c) if c == u32::from(E::NotLiquidatable) => {}
            Some(c) => panic!("liquidate failed with {c}"),
        }
    }
    assert!(liquidated, "the liquidation runs on the upgraded pAMM");
    assert_eq!(env.market(&pm.market()).open_positions, 0);
}

/// A real pump-fees upgrade also refuses PumpSwap opens (the mask holds pump-fees); a pump (curve)
/// upgrade does **not** (the PumpSwap mask excludes it). Both PumpSwap bits must be clear before an
/// open resumes.
#[test]
fn the_pumpswap_mask_is_pamm_and_pump_fees_only() {
    let (mut env, pm, q) = setup(MATURE);

    // pump-fees upgrade refuses opens.
    let s_fees = loader_upgrade(&mut env, PumpProgram::Fees);
    ok(flag(&mut env, GUARD_PUMP_FEES));
    assert_err(&try_open(&mut env, &pm, q), E::VenueUpgraded);

    // A pump (curve) upgrade on top: still refused, and pump is flagged too.
    let s_pump = loader_upgrade(&mut env, PumpProgram::Pump);
    ok(flag(&mut env, GUARD_PUMP));
    assert_err(&try_open(&mut env, &pm, q), E::VenueUpgraded);

    // Clearing pump alone does not resume PumpSwap opens (pump-fees still set).
    ok(revalidate(&mut env, GUARD_PUMP, s_pump));
    assert_err(&try_open(&mut env, &pm, q), E::VenueUpgraded);

    // Clearing pump-fees resumes them.
    ok(revalidate(&mut env, GUARD_PUMP_FEES, s_fees));
    assert_eq!(env.venue_guard().upgraded, 0);
    env.warp(1);
    ok(env.send(&[crank_price_ix(&pm)], &[]));
    ok(try_open(&mut env, &pm, q));
}

/// A pump (curve) upgrade alone never blocks a PumpSwap open (the mask excludes it).
#[test]
fn a_pump_curve_upgrade_does_not_block_pumpswap_opens() {
    let (mut env, pm, q) = setup(SPL261);
    loader_upgrade(&mut env, PumpProgram::Pump);
    ok(flag(&mut env, GUARD_PUMP));
    assert_eq!(
        env.venue_guard().upgraded & (1 << GUARD_PUMP),
        1 << GUARD_PUMP
    );
    // A PumpSwap open is accepted (pump is not in the PumpSwap mask).
    ok(try_open(&mut env, &pm, q));
}

/// U5: `flag_venue_upgraded` and `revalidate_venue` cost the same CU on the real 10 MiB PumpSwap
/// ProgramData as on pump-fees' 0.95 MB — the input-region copy is not metered — far inside
/// §6.1's 400 k.
#[test]
fn u5_flag_and_revalidate_cost_the_same_whatever_the_programdata_size() {
    let cu = |program: PumpProgram, venue: u8| -> (u64, u64) {
        let (mut env, _) = Env::fork_pamm_coin(MATURE);
        let slot = loader_upgrade(&mut env, program);
        let f = ok(flag(&mut env, venue)).compute_units_consumed;
        let r = ok(revalidate(&mut env, venue, slot)).compute_units_consumed;
        (f, r)
    };
    let (f_amm, r_amm) = cu(PumpProgram::Amm, GUARD_PUMP_AMM);
    let (f_fees, r_fees) = cu(PumpProgram::Fees, GUARD_PUMP_FEES);
    println!(
        "flag pAMM {f_amm} / pump-fees {f_fees}; revalidate pAMM {r_amm} / pump-fees {r_fees}"
    );
    assert!(
        f_amm < LATCH_CU_LIMIT && r_amm < LATCH_CU_LIMIT,
        "within §6.1's 400 k"
    );
    // The copy is not metered: the 10 MiB account costs no more than the 0.95 MB one, which pays
    // only the few instructions of its AIR-01 fingerprint lookup (pump-fees has a fingerprint
    // slot, PumpSwap — at the 10 MiB cap — none; nothing is recorded, nothing hashed).
    assert!(
        f_amm <= f_fees && f_fees - f_amm <= 32,
        "flag: the copy is not metered"
    );
    assert!(
        r_amm <= r_fees && r_fees - r_amm <= 32,
        "revalidate: the copy is not metered"
    );
    // The PumpSwap ProgramData is the ARCH §6.5 size.
    let (env, _) = Env::fork_pamm_coin(MATURE);
    assert_eq!(
        read::account(&env.svm, &PumpProgram::Amm.program_data().address)
            .unwrap()
            .data
            .len(),
        PAMM_PROGRAM_DATA_LEN
    );
}

/// The ProgramData account is checked for both PumpSwap guard indices (M10 G-A7; ARCH §3.1; as
/// `latch_pump_it::the_program_data_account_is_checked` for pump): for `GUARD_PUMP_AMM` and
/// `GUARD_PUMP_FEES`, the other venues' ProgramData, the program id itself, short.win's own
/// ProgramData and a fresh key are `VenueAccountMismatch` for `flag_venue_upgraded` and
/// `revalidate_venue`; so is the right address after a (planted) upgrade — the header's deploy
/// slot moved, which the flag would latch — when it is owned by another program or carries another
/// loader state (a `Buffer` tag). The controls: the real account at the dumped slot is
/// `VenueNotUpgraded`, and with the slot moved the flag lands and sets exactly that bit.
#[test]
fn the_program_data_account_is_checked_for_pamm_and_pump_fees() {
    for (program, guard, other) in [
        (PumpProgram::Amm, GUARD_PUMP_AMM, PumpProgram::Fees),
        (PumpProgram::Fees, GUARD_PUMP_FEES, PumpProgram::Amm),
    ] {
        let (mut env, _) = Env::fork_pamm_coin(SPL261);
        let ea = env.pause_authority.insecure_clone();
        let s = fork::FIXTURE_GUARD_SLOTS[usize::from(guard)];
        let wrong_keys = [
            other.program_data().address,
            PumpProgram::Pump.program_data().address,
            program.id(),
            pda::program_data(),
            Pubkey::new_unique(),
        ];
        for key in wrong_keys {
            assert_err(
                &env.send(&[ix::flag_venue_upgraded_with(guard, key)], &[]),
                E::VenueAccountMismatch,
            );
            assert_err(
                &env.send(
                    &[ix::revalidate_venue_with(&ea.pubkey(), guard, s, key)],
                    &[&ea],
                ),
                E::VenueAccountMismatch,
            );
        }
        let key = program.program_data().address;
        assert_err(&flag(&mut env, guard), E::VenueNotUpgraded);
        fork::set_pump_deploy_slot(&mut env.svm, program, s + 7);
        let moved = read::account(&env.svm, &key).unwrap();
        let mut forged = moved.clone();
        forged.owner = anchor_lang::system_program::ID;
        env.svm.set_account(key, forged).unwrap();
        assert_err(&flag(&mut env, guard), E::VenueAccountMismatch);
        assert_err(&revalidate(&mut env, guard, s + 7), E::VenueAccountMismatch);
        let mut forged = moved.clone();
        forged.data[0] = 1;
        env.svm.set_account(key, forged).unwrap();
        assert_err(&flag(&mut env, guard), E::VenueAccountMismatch);
        assert_err(&revalidate(&mut env, guard, s + 7), E::VenueAccountMismatch);
        assert_eq!(env.venue_guard().upgraded, 0, "{guard}: nothing latched");
        env.svm.set_account(key, moved).unwrap();
        ok(flag(&mut env, guard));
        assert_eq!(
            env.venue_guard().upgraded,
            1 << guard,
            "{guard}: exactly its bit"
        );
    }
}

/// The upgrade mainnet made on 2026-10-02, replayed with the real accounts (as
/// `latch_raydium_it::the_2026_10_cpmm_redeploy_latches_until_revalidated` for CPMM): the fork
/// starts on the 2026-09 builds of PumpSwap, pump-fees and pump (group `pump_programs`, the
/// ProgramData at the slots `VENUE_GUARD_SLOTS_2026_09_27` names and `initialize_config`
/// records), positions are opened on the 261-byte pool — held at `minimum_balance(261)`, so its
/// first sale grows it to the 2026-09 build's 271 bytes and pays 50,800 lamports of rent out of
/// `S` (`Open.venue_rent`) — and on e/acc's 301-byte pool, and the loader's own `Upgrade` installs
/// the redeploys' ELFs (`PumpDeploy::Oct2026`) in mainnet's order (PumpSwap, pump, pump-fees).
/// PumpSwap answers as the new build (`buy_v2` dispatches). Flagged by anyone, PumpSwap and
/// pump-fees refuse opens on both pools, while a close — whose buyback grows the short pool to
/// 287 bytes for another 81,280 lamports, which I13 re-quotes on the build PumpSwap's ProgramData
/// holds (`PumpDeploy::held`) —, a liquidation after a real ×2.8 pump and an expiry run on the
/// new PumpSwap. Only the loader's slots revalidate; clearing pump's bit alone, or PumpSwap's,
/// does not resume opens; with all three clear opens land on the redeploy.
#[test]
fn the_2026_10_pumpswap_redeploy_latches_until_revalidated() {
    // The fork as of 2026-09-27: the 2026-09 builds, the baseline the program constant names.
    let (mut env, short) = Env::fork_pamm_coin_with(SPL261, |a| {
        a.guard_slot = VENUE_GUARD_SLOTS_2026_09_27;
    });
    for program in PumpProgram::ALL {
        fork::load_pump_deploy(&mut env.svm, program, PumpDeploy::Sep2026);
    }
    let mature = env.load_pamm_coin(MATURE);
    let old = |program: PumpProgram| VENUE_GUARD_SLOTS_2026_09_27[usize::from(program.guard())];
    for program in PumpProgram::ALL {
        assert_eq!(old(program), PumpDeploy::Sep2026.deploy_slot(program));
        assert_eq!(header(&env, program).slot, old(program), "{program:?}");
        assert_eq!(
            env.venue_guard().guard_slot[usize::from(program.guard())],
            old(program)
        );
        assert_eq!(
            PumpDeploy::held(&env.svm, program),
            Some(PumpDeploy::Sep2026)
        );
        assert_err(&flag(&mut env, program.guard()), E::VenueNotUpgraded);
    }
    assert_eq!(
        env.pump_build_running(PumpProgram::Amm),
        PumpDeploy::Sep2026
    );

    let (spm, sq) = list_market(&mut env, &short);
    let (mpm, mq) = list_market(&mut env, &mature);
    let rent = |env: &Env, len: usize| env.svm.minimum_balance_for_rent_exemption(len);
    let pool = |env: &Env| {
        let a = read::account(&env.svm, &short.pool).unwrap();
        (a.data.len(), a.lamports)
    };
    let mut acc = env.svm.get_account(&short.pool).unwrap();
    assert_eq!(acc.data.len(), 261);
    acc.lamports = rent(&env, 261);
    env.svm.set_account(short.pool, acc).unwrap();

    let mut b_close = env.borrower();
    let (res, n_close) = env.open(&spm, &mut b_close, sq);
    let first: Open = events::one(&ok(res));
    let sep_len = PumpDeploy::Sep2026.pool_len();
    assert_eq!(first.venue_rent, rent(&env, sep_len) - rent(&env, 261));
    assert_eq!(first.venue_rent, 50_800);
    assert_eq!(
        pool(&env),
        (sep_len, rent(&env, sep_len)),
        "the 2026-09 growth"
    );
    let (b_exp, n_exp) = open(&mut env, &spm, sq);
    let (b_liq, n_liq) = open(&mut env, &mpm, mq);

    // The redeploys, in mainnet's order: the loader writes each slot and the 2026-10-02 ELF.
    let mut slots = Vec::new();
    for program in [PumpProgram::Amm, PumpProgram::Pump, PumpProgram::Fees] {
        env.warp(1);
        // The 2026-10-02 ELFs, not `PumpDeploy::CURRENT`'s (the 2026-10-08/12 builds).
        let s = loader_upgrade_to(&mut env, program, PumpDeploy::Oct2026.elf(program));
        assert_ne!(s, old(program));
        assert_eq!(
            PumpDeploy::held(&env.svm, program),
            Some(PumpDeploy::Oct2026)
        );
        slots.push((program, s));
    }
    env.warp(1);
    assert_eq!(
        env.pump_build_running(PumpProgram::Amm),
        PumpDeploy::Oct2026
    );

    // Anyone flags each in a later slot; opens are refused on both pools.
    for (program, s) in &slots {
        let venue = program.guard();
        let ev: VenueUpgradeFlagged = events::one(&ok(flag(&mut env, venue)));
        assert_eq!(
            (ev.venue, ev.guard_slot, ev.programdata_slot),
            (venue, old(*program), *s)
        );
    }
    let all = (1 << GUARD_PUMP) | (1 << GUARD_PUMP_AMM) | (1 << GUARD_PUMP_FEES);
    assert_eq!(env.venue_guard().upgraded, all);
    for (pm, q) in [(&spm, sq), (&mpm, mq)] {
        assert_err(&try_open(&mut env, pm, q), E::VenueUpgraded);
    }

    // Exits never read the latch (I10) and buy back on the new PumpSwap: a close on the short
    // pool, which the redeploy grows to 287 bytes ...
    env.warp(1);
    for pm in [&spm, &mpm] {
        ok(env.send(&[crank_price_ix(pm)], &[]));
    }
    let _: Close = events::one(&ok(close(&mut env, &spm, &b_close, n_close)));
    let oct_len = PumpDeploy::Oct2026.pool_len();
    assert_eq!(
        pool(&env),
        (oct_len, rent(&env, oct_len)),
        "the redeploy's growth"
    );
    // ... a liquidation after a real ×2.8 pump on e/acc's pool ...
    let keeper = env.funded_keypair();
    let u = PammUser::new(&mut env, &mpm.pool, 2_000 * SOL);
    ok(u.push_price(&mut env, &mpm.pool, 28, 10));
    let mut liquidated = false;
    for _ in 0..900 {
        env.warp(1);
        ok(env.send(&[crank_price_ix(&mpm)], &[]));
        let ixl = liquidate_ix(&mpm, &keeper.pubkey(), &b_liq.key(), n_liq);
        let res = env.send(&[cu_limit_ix(400_000), ixl], &[&keeper]);
        match custom_code(&res) {
            None => {
                let _: Liquidate = events::one(&ok(res));
                liquidated = true;
                break;
            }
            Some(c) if c == u32::from(E::NotLiquidatable) => {}
            Some(c) => panic!("liquidate failed with {c}"),
        }
    }
    assert!(liquidated, "the liquidation runs on the redeployed pAMM");
    // ... and an expiry on the short pool.
    let deadline = env.position(&spm, &b_exp.key(), n_exp).deadline;
    let now = env.now();
    env.warp(deadline + 1 - now);
    env.tick(&spm, 3);
    let ixe = expire_ix(&spm, &keeper.pubkey(), &b_exp.key(), n_exp);
    let e: Expire = events::one(&ok(env.send(&[cu_limit_ix(400_000), ixe], &[&keeper])));
    assert!(e.buyback_cost > 0);
    assert_eq!(
        env.venue_guard().upgraded,
        all,
        "exits leave the latch alone"
    );

    // Only each loader slot revalidates, the 2026-09 baseline does not. pump's bit, then
    // PumpSwap's, cleared alone leave pump-fees' latching PumpSwap opens.
    for program in [PumpProgram::Pump, PumpProgram::Amm, PumpProgram::Fees] {
        let venue = program.guard();
        let s = slots.iter().find(|(p, _)| *p == program).unwrap().1;
        for wrong in [old(program), s + 1] {
            assert_err(&revalidate(&mut env, venue, wrong), E::VenueSlotMismatch);
        }
        let ev: VenueRevalidated = events::one(&ok(revalidate(&mut env, venue, s)));
        assert_eq!((ev.guard_slot, ev.expected_slot), (old(program), s));
        assert_eq!(env.venue_guard().guard_slot[usize::from(venue)], s);
        env.tick(&spm, 1);
        if program != PumpProgram::Fees {
            assert_err(&try_open(&mut env, &spm, sq), E::VenueUpgraded);
        }
    }
    assert_eq!(env.venue_guard().upgraded, 0);
    ok(try_open(&mut env, &spm, sq));
    env.warp(1);
    assert_eq!(
        env.pump_build_running(PumpProgram::Amm),
        PumpDeploy::Oct2026
    );
}
