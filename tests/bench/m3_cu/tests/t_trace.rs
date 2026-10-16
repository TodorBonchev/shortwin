//! T-TRACE on Raydium (ARCHITECTURE §6.2, §6.3, §6.4, §8.2; C1-1, C1-8; U4): the
//! instruction-trace entries of the worst-case Raydium transactions — every top-level
//! instruction (ComputeBudget included) plus every CPI at any depth — stay within
//! `MAX_INSTRUCTION_TRACE_LENGTH = 64`, on LiteSVM with every invariant hook after every
//! instruction; their peak stack height, CU and wire sizes (legacy, and v0 with the protocol
//! ALT of A28: a T-SIZE preview). Also checks the counting against the runtime: one open more
//! than fits is refused by the runtime itself.
//!
//! `M3_TRACE_WRITE=1` rewrites `trace.md`.

use std::fmt::Write as _;

use m3_cu_bench::{
    FART, T22, label, opens_of_one_wallet, repo_path, run_trace, worst_case_transactions,
};
use solana_signer::Signer;

/// `MAX_INSTRUCTION_TRACE_LENGTH` (agave `transaction-context`, ARCH §6.2).
const MAX_TRACE: usize = 64;
/// The packet limit for legacy and v0 transactions (ARCH §6.4).
const PACKET: usize = 1_232;
/// The v1 transaction limit (ARCH §6.4: 4,096 B, 64 inline addresses, no lookup tables).
const V1_PACKET: usize = 4_096;

const HEADER: &str = "# M3 T-TRACE (Raydium)

Instruction-trace entries (every top-level instruction, ComputeBudget included, plus every CPI \
at any depth; ARCH §6.2) of the worst-case Raydium transactions of the release short.win on the \
dumped AMM v4 and CPMM programs, measured on LiteSVM 0.16.0 with every invariant hook after every \
instruction; the limit is `MAX_INSTRUCTION_TRACE_LENGTH = 64`. The peak stack height is the \
deepest CPI level reached (1 = top level; the mainnet limit is 5, ARCH §6.3). Written by \
`M3_TRACE_WRITE=1 cargo test --manifest-path tests/bench/m3_cu/Cargo.toml --test t_trace` \
(README.md); the exact entries of each row are also asserted (their composition is pinned in \
`tests/litesvm/tests/trace_raydium_it.rs`).

Sizes (T-SIZE preview, ARCH §6.4): the legacy wire size (no lookup table) and the v0 wire size \
with the Raydium part of the protocol ALT (A28; `shortwin_litesvm_tests::raydium::protocol_alt_addresses`), \
each with its account count; LiteSVM enforces neither. Every client and keeper transaction \
must fit v0 + ALT (≤ 1,232 B, ≤ 64 accounts), except the keeper's crank batches, which ARCH §6.4 \
sends as **v1** transactions (≤ 4,096 B, every key inline, ≤ 64 accounts; the legacy encoding \
stands in for v1's here). Bold: above 1,232 B.

| Transaction | Entries | Expected | Peak height | CU | Legacy B / accounts | v0 + ALT B / accounts | ARCH §6.2 |
|---|---|---|---|---|---|---|---|
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
                if m.peak > 3 {
                    failures.push(format!("{}: peak stack height {}", t.name, m.peak));
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
                    "{} | {} | {} | {} | {} | {}",
                    m.entries,
                    t.expected,
                    m.peak,
                    m.cu,
                    size(m.legacy.0, m.legacy.1),
                    size(m.v0.0, m.v0.1)
                )
            }
            Err(e) => {
                failures.push(e);
                format!("*failed* | {} | — | — | — | —", t.expected)
            }
        };
        writeln!(table, "| {} | {row} | {} |", t.name, t.arch).unwrap();
    }
    println!("{table}");
    if std::env::var_os("M3_TRACE_WRITE").is_some() {
        let path = repo_path("tests/bench/m3_cu/trace.md");
        std::fs::write(&path, &table).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Five opens of one wallet are one entry-set too many (2 + 15 + 4·14 = 73): the runtime
/// refuses the transaction with `MaxInstructionTraceLengthExceeded`, before any hook could blame
/// short.win; four fit (59, `worst_case_transactions_fit_the_trace`).
#[test]
fn five_opens_exceed_the_trace() {
    for f in [FART, T22] {
        let (mut env, ixs, signers) = opens_of_one_wallet(f, 5);
        let refs: Vec<&solana_keypair::Keypair> = signers.iter().collect();
        let err = env.send(&ixs, &refs).expect_err("73 entries");
        assert!(
            format!("{:?}", err.err).contains("MaxInstructionTraceLengthExceeded"),
            "{}: {:?}",
            label(f),
            err.err
        );
    }
}

/// U25 on Raydium (ARCH §6.4, R4B-9): four `crank_price` of Raydium markets fit one v0
/// transaction with the protocol ALT (the row above: 1,113 B), five do not — keeper batches
/// beyond four are v1 transactions.
#[test]
fn four_cranks_fit_a_v0_transaction_five_do_not() {
    use anchor_lang::prelude::Pubkey;
    use shortwin_litesvm_tests::raydium::{protocol_alt_addresses, v0_size};
    let alt = Pubkey::new_unique();
    for (k, fits) in [(4, true), (5, false)] {
        let (env, ixs, _) = m3_cu_bench::tx_crank_batch(k);
        let (bytes, accounts) = v0_size(&env.payer.pubkey(), &ixs, &alt, &protocol_alt_addresses());
        println!("ComputeBudget ×2 + crank_price ×{k}: v0 + ALT {bytes} B / {accounts} accounts");
        assert_eq!(bytes <= PACKET, fits, "{k} cranks: {bytes} B");
    }
}
