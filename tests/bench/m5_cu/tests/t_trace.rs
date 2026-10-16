//! T-TRACE and the T-SIZE preview on the pump curve (ARCHITECTURE §6.2, §6.3, §6.4, §8.2; C1-1,
//! C1-8; T-STRADDLE's helper rows left with the `complete_curve` helper, pump-final D2): the
//! instruction-trace entries of the worst-case curve transactions — every top-level instruction
//! (ComputeBudget included) plus every CPI at any depth — stay within `MAX_INSTRUCTION_TRACE_LENGTH = 64`, on LiteSVM with every invariant hook
//! after every instruction; their peak stack height, CU (within ARCH §6.1's limit for the
//! transaction) and wire sizes (legacy, and v0 through the protocol ALT of A28 with its pump
//! part). Also checks the counting against the runtime (one open more than fits is refused by the
//! runtime itself) and the crank batches the protocol ALT holds.
//!
//! `M5_TRACE_WRITE=1` rewrites `trace.md`.

use std::fmt::Write as _;

use anchor_lang::prelude::Pubkey;
use m5_cu_bench::{
    compute_budget, four_markets, opens_of_one_wallet, protocol_alt_addresses, repo_path,
    run_trace, worst_case_transactions,
};
use shortwin_litesvm_tests::engine::crank_price_ix;
use shortwin_litesvm_tests::fixtures::MarketKeys;
use shortwin_litesvm_tests::fork::{PumpCoin, PumpCurve};
use shortwin_litesvm_tests::pda;
use shortwin_litesvm_tests::pump::PumpMarket;
use shortwin_litesvm_tests::raydium::{legacy_size, v0_size};
use shortwin_litesvm_tests::tokens::TokenProgram;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// `MAX_INSTRUCTION_TRACE_LENGTH` (agave `transaction-context`, ARCH §6.2).
const MAX_TRACE: usize = 64;
/// The packet limit for legacy and v0 transactions (ARCH §6.4).
const PACKET: usize = 1_232;
/// The v1 transaction limit (ARCH §6.4: 4,096 B, 64 inline addresses, no lookup tables).
const V1_PACKET: usize = 4_096;
/// The keeper's v0 crank batch (ARCH §7.1 duty 7; `KeeperConfig::batch_v0`).
const KEEPER_BATCH_V0: usize = 4;

const HEADER: &str = "# M5 T-TRACE (pump curve)

Instruction-trace entries (every top-level instruction, ComputeBudget included, plus every CPI \
at any depth; ARCH §6.2) of the worst-case curve transactions of the release short.win on the \
dumped pump, pump-fees and PumpSwap programs, measured on LiteSVM 0.16.0 with every invariant hook \
after every instruction; the limit is `MAX_INSTRUCTION_TRACE_LENGTH = 64`. The peak stack height \
is the deepest CPI level reached (1 = top level; the mainnet limit is 5, ARCH §6.3); the CU is the \
whole transaction's (the two ComputeBudget instructions cost 150 each), within ARCH §6.1's limit \
for it. Written by `M5_TRACE_WRITE=1 cargo test --manifest-path tests/bench/m5_cu/Cargo.toml \
--test t_trace` (README.md); the exact entries of each row are also asserted (their composition \
is pinned in `tests/litesvm/tests/trace_pump_it.rs`).

Sizes (T-SIZE preview, ARCH §6.4): the legacy wire size (no lookup table) and the v0 wire size \
through the protocol ALT (A28 with its pump part: pump, PumpSwap and pump-fees, their statics and \
the live recipient lists with their WSOL ATAs; `m5_cu_bench::protocol_alt_addresses`), each with \
its account count; LiteSVM enforces neither. Every client and keeper transaction must fit v0 + \
ALT (≤ 1,232 B, ≤ 64 accounts). Bold: above 1,232 B. The full T-SIZE on the curve (the largest \
extras set, a separate fee payer, both tables, executed as v0) is \
`tests/litesvm/tests/tx_size_pump_it.rs`.

A position past the curve's `rT` exits in one transaction (pump v3's synthetic migration, the \
synthetic rows). The keeper's `migrate_v2` is measured after such a completion (`pc ≠ 0`) and \
after a third party's exact one (`pc = 0`; until pump-final P6 the straddle bundle's tx2). The \
bundle's helper rows (tx1, the `complete_curve` helper) are retired with the helper \
(pump-final D2).

| Transaction | Entries | Expected | Peak height | CU | CU limit | Legacy B / accounts | v0 + ALT B / accounts | ARCH §6.2 |
|---|---|---|---|---|---|---|---|---|
";

fn size(bytes: usize, accounts: usize) -> String {
    if bytes > PACKET {
        format!("**{bytes}** / {accounts}")
    } else {
        format!("{bytes} / {accounts}")
    }
}

#[test]
fn worst_case_transactions_fit_the_trace() {
    let mut table = String::from(HEADER);
    let mut failures = Vec::new();
    for t in worst_case_transactions() {
        let row = match run_trace(&t) {
            Ok(m) => {
                if m.entries > MAX_TRACE {
                    failures.push(format!("{}: {} entries > {MAX_TRACE}", t.name, m.entries));
                }
                if m.entries != t.expected {
                    failures.push(format!(
                        "{}: {} entries, expected {}",
                        t.name, m.entries, t.expected
                    ));
                }
                if m.peak != t.peak {
                    failures.push(format!(
                        "{}: peak stack height {}, expected {}",
                        t.name, m.peak, t.peak
                    ));
                }
                if m.cu > t.cu_limit {
                    failures.push(format!("{}: {} CU > {}", t.name, m.cu, t.cu_limit));
                }
                if t.v1 {
                    if m.legacy.0 > V1_PACKET || m.legacy.1 > 64 {
                        failures.push(format!(
                            "{}: v1 {} B / {} accounts",
                            t.name, m.legacy.0, m.legacy.1
                        ));
                    }
                } else if m.v0.0 > PACKET || m.v0.1 > 64 {
                    failures.push(format!(
                        "{}: v0 + ALT {} B / {} accounts",
                        t.name, m.v0.0, m.v0.1
                    ));
                }
                format!(
                    "{} | {} | {} | {} | {} | {} | {}",
                    m.entries,
                    t.expected,
                    m.peak,
                    m.cu,
                    t.cu_limit,
                    size(m.legacy.0, m.legacy.1),
                    size(m.v0.0, m.v0.1)
                )
            }
            Err(e) => {
                failures.push(e);
                format!("*failed* | {} | — | — | {} | — | —", t.expected, t.cu_limit)
            }
        };
        writeln!(table, "| {} | {row} | {} |", t.name, t.arch).unwrap();
    }
    println!("{table}");
    if std::env::var_os("M5_TRACE_WRITE").is_some() {
        let path = repo_path("tests/bench/m5_cu/trace.md");
        std::fs::write(&path, &table).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Six opens of one wallet are one entry-set too many (2 + 13 + 1 + 5·12 = 76, the 1 being the
/// first sale's growth of the 151-byte curve, `NEAR_GROWTH`): the runtime refuses the transaction
/// with `MaxInstructionTraceLengthExceeded`, before any hook could blame short.win; five fill it
/// exactly (64, `worst_case_transactions_fit_the_trace`; pump-final §6.1's `sell_v3` is one
/// entry shorter than `sell_v2`).
#[test]
fn six_opens_exceed_the_trace() {
    let (mut env, ixs, signers) = opens_of_one_wallet(6);
    let refs: Vec<&Keypair> = signers.iter().collect();
    let err = env.send(&ixs, &refs).expect_err("76 entries");
    assert!(
        format!("{:?}", err.err).contains("MaxInstructionTraceLengthExceeded"),
        "{:?}",
        err.err
    );
}

/// A curve market of a synthetic mint, for sizing only: its keys derived as the program derives
/// them (the curve PDA, its base ATA, the canonical pool, the market PDAs); nothing is executed
/// on it.
fn synthetic_market(n: u8) -> PumpMarket {
    let mint = Pubkey::new_from_array([0xa0 + n; 32]);
    let program = TokenProgram::Token2022;
    let curve = pda::bonding_curve(&mint);
    let market = pda::market(&mint).0;
    let trader = pda::trader(&market).0;
    PumpMarket {
        curve: PumpCurve {
            coin: PumpCoin::T22_115,
            mint,
            curve,
            curve_base_ata: pda::ata(&curve, &mint, &program.id()),
            token_program: program,
            quote_mint: shortwin::constants::WSOL_MINT,
        },
        keys: MarketKeys {
            mint,
            token_program: program.id(),
            market,
            market_vault: pda::market_vault(&market).0,
            pool_vault: pda::pool_vault(&market).0,
            trader,
        },
        trader_base_ata: pda::ata(&trader, &mint, &program.id()),
        lender: Keypair::new(),
        lender_account: Pubkey::new_unique(),
        fee_recipient: Pubkey::new_unique(),
        buyback_recipient: Pubkey::new_unique(),
        creator_vault: Pubkey::new_unique(),
    }
}

/// U25 on the curve (ARCH §6.4, R4B-9): the crank batches the protocol ALT holds. A curve crank
/// names 5 inline keys (the market, the mint, the curve, its base ATA, the canonical pool); the
/// four real curve markets of the dump and synthetic ones beyond them (sizing only): the largest
/// `k` whose v0 batch fits 1,232 B through the table is at least the keeper's v0 batch (4), and
/// the keeper's six-crank batch fits a **v1** transaction (4,096 B, 64 inline keys).
#[test]
fn crank_batches_fit_through_the_protocol_alt() {
    let (env, markets) = four_markets();
    let alt = Pubkey::new_unique();
    let addresses = protocol_alt_addresses(&env.svm);
    let payer = env.payer.pubkey();
    let mut cranks: Vec<_> = markets.iter().map(crank_price_ix).collect();
    cranks.extend((0..6).map(|n| crank_price_ix(&synthetic_market(n))));
    let mut largest = 0;
    for k in 1..=cranks.len() {
        let mut ixs = compute_budget(1_400_000, 1).to_vec();
        ixs.extend_from_slice(&cranks[..k]);
        let (bytes, accounts) = v0_size(&payer, &ixs, &alt, &addresses);
        let (legacy, keys) = legacy_size(&payer, &ixs);
        println!(
            "ComputeBudget ×2 + crank_price ×{k}: v0 + ALT {bytes} B / {accounts} accounts; all inline (v1 stand-in) {legacy} B / {keys} keys"
        );
        if bytes <= PACKET && accounts <= 64 {
            largest = k;
        }
        if k == 6 {
            assert!(
                legacy <= V1_PACKET && keys <= 64,
                "the keeper's v1 batch of six: {legacy} B / {keys} keys"
            );
        }
    }
    println!("largest v0 crank batch through the protocol ALT: {largest}");
    assert!(largest >= KEEPER_BATCH_V0, "only {largest} cranks fit v0");
}

/// How many opens of one wallet a **v0** transaction through the protocol ALT holds on the curve
/// (a curve open names ≈ 20 inline keys: the ReadSet's per-market accounts and the TradeExtras
/// the table does not hold). Five fit the trace (64 entries) but not v0: they need a v1
/// transaction (`worst_case_transactions`); the table's figures are printed.
#[test]
fn opens_per_v0_transaction() {
    let mut largest = 0;
    for k in 1..=5u64 {
        let (env, ixs, _) = opens_of_one_wallet(k);
        let (bytes, accounts) = v0_size(
            &env.payer.pubkey(),
            &ixs,
            &Pubkey::new_unique(),
            &protocol_alt_addresses(&env.svm),
        );
        let (legacy, keys) = legacy_size(&env.payer.pubkey(), &ixs);
        println!(
            "ComputeBudget ×2 + {k} open(s) of one wallet: v0 + ALT {bytes} B / {accounts} accounts; all inline (v1) {legacy} B / {keys} keys"
        );
        if bytes <= PACKET && accounts <= 64 {
            largest = k;
        }
        assert!(legacy <= V1_PACKET && keys <= 64, "{k} opens do not fit v1");
    }
    println!("largest v0 batch of opens of one wallet: {largest}");
    assert!(largest >= 1, "a single open must fit v0");
}
