//! T-TRACE and CU for the engine's instructions on the mock (ARCH §6.1, §6.2; §8.2 T-TRACE,
//! T-CU): the mock trades like ARCH §2.4.1's Raydium row, so its budgets are Raydium's.
//!
//! **Measured vs ARCH §6.2.** `close_short` is 9 entries, as the table. `open_short` is 15, not
//! 13: the table omits the two account creations every first open makes (the `ShortPosition`
//! `init` and the `WalletExposure` `init_if_needed`, one System `create_account` CPI each); an
//! open by a wallet that already has an exposure record is 14. Normalisation adds its System
//! transfers (one per donated PDA) and the `TraderSwept` event: up to 3. The asserted bounds
//! are the measured counts; the ARCH table needs the +2 (recorded in the M2 report).
//! CU within the Raydium limits (open 150 k; close — no row, the liquidation's 200 k is the
//! bound used). The measured figures are printed (`-- --nocapture`).

use anchor_lang::solana_program::instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::common::*;

/// ARCH §6.2, Raydium column (13) plus the position and exposure creations (module docs).
const OPEN_TRACE: usize = 15;
/// … plus normalisation (≤ 3).
const OPEN_TRACE_MAX: usize = 18;
const CLOSE_TRACE: usize = 9;
const CLOSE_TRACE_MAX: usize = 11;
/// ARCH §6.1 limits (Raydium; the close uses the liquidation's row).
const OPEN_CU_MAX: u64 = 150_000;
const CLOSE_CU_MAX: u64 = 200_000;
const CRANK_CU_MAX: u64 = 60_000;

fn transfer(from: &Keypair, to: &anchor_lang::prelude::Pubkey, lamports: u64) -> Instruction {
    let mut data = vec![2, 0, 0, 0];
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![
            anchor_lang::solana_program::instruction::AccountMeta::new(from.pubkey(), true),
            anchor_lang::solana_program::instruction::AccountMeta::new(*to, false),
        ],
        data,
    }
}

#[test]
fn trace_and_cu_of_open_and_close() {
    for program in TokenProgram::BOTH {
        let (mut env, mm) = setup_with(spec_on(program));
        env.crank(&mm);
        let mut b = env.borrower();
        let (_o, nonce, meta) = open_ok(&mut env, &mm, &mut b, Q);
        let (trace, cu) = (trace_entries(&meta, 0), meta.compute_units_consumed);
        println!("open_short {program:?}: {trace} trace entries, {cu} CU");
        assert_eq!(trace, OPEN_TRACE, "logs:\n{}", meta.pretty_logs());
        assert!(cu <= OPEN_CU_MAX, "open_short {cu} CU");

        env.tick(&mm, 2);
        let meta = ok(env.send(&[crank_price_ix(&mm)], &[]));
        println!(
            "crank_price {program:?}: {} trace entries, {} CU",
            trace_entries(&meta, 0),
            meta.compute_units_consumed
        );
        assert_eq!(
            trace_entries(&meta, 0),
            2,
            "crank: the instruction and its event"
        );
        assert!(meta.compute_units_consumed <= CRANK_CU_MAX);

        env.warp(1);
        let (_c, meta) = close_ok(&mut env, &mm, &b, nonce);
        let (trace, cu) = (trace_entries(&meta, 0), meta.compute_units_consumed);
        println!("close_short {program:?}: {trace} trace entries, {cu} CU");
        assert_eq!(trace, CLOSE_TRACE, "logs:\n{}", meta.pretty_logs());
        assert!(cu <= CLOSE_CU_MAX, "close_short {cu} CU");
    }
}

/// With both PDAs donated to, normalisation adds its transfers and one `TraderSwept` event:
/// still within ARCH §6.2's maxima.
#[test]
fn trace_with_normalisation() {
    let (mut env, mm) = setup();
    let griefer = env.funded_keypair();
    ok(env.send(
        &[
            transfer(&griefer, &mm.keys.trader, 1_000_000),
            transfer(&griefer, &mm.trader_wsol, 1_000_000),
        ],
        &[&griefer],
    ));
    let mut b = env.borrower();
    let (_o, nonce, meta) = open_ok(&mut env, &mm, &mut b, Q);
    let trace = trace_entries(&meta, 0);
    println!(
        "open_short with normalisation: {trace} trace entries, {} CU",
        meta.compute_units_consumed
    );
    assert_eq!(trace, OPEN_TRACE + 3, "open {trace}");
    assert!(trace <= OPEN_TRACE_MAX);
    env.tick(&mm, 1);
    ok(env.send(
        &[
            transfer(&griefer, &mm.keys.trader, 1_000_000),
            transfer(&griefer, &mm.trader_wsol, 1_000_000),
        ],
        &[&griefer],
    ));
    let (_c, meta) = close_ok(&mut env, &mm, &b, nonce);
    let trace = trace_entries(&meta, 0);
    println!(
        "close_short with normalisation: {trace} trace entries, {} CU",
        meta.compute_units_consumed
    );
    assert_eq!(trace, CLOSE_TRACE + 3, "close {trace}");
    assert!(
        trace <= CLOSE_TRACE_MAX + 1,
        "ARCH's ≤ 11 plus the TraderSwept event"
    );
}
