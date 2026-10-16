//! T-TRACE and T-SIZE on the PumpSwap venue (ARCHITECTURE §5.4, §6.2, §6.3, §6.4, §8.2; C1-1,
//! C1-8): the instruction-trace entries of the worst-case PumpSwap transactions — every top-level
//! instruction (ComputeBudget included) plus every CPI at any depth — stay within
//! `MAX_INSTRUCTION_TRACE_LENGTH = 64`, on LiteSVM with every invariant hook after every
//! instruction; their peak stack height, CU (within ARCH §6.1's limit for the transaction) and
//! wire sizes (legacy, and v0 through the protocol ALT of A28 with its pump part). It covers the
//! `→ PumpSwap` graduation transition, the transition crank plus a borrower's close on the pool
//! in one transaction (the retired straddle bundle's tx3, pump-final D2) and the post-wipe reward
//! claim bundle (claim_venue_rewards was not built).
//!
//! `M6_TRACE_WRITE=1` rewrites `trace.md`.

use std::fmt::Write as _;

use m6_cu_bench::{repo_path, run_trace, worst_case_transactions};

/// `MAX_INSTRUCTION_TRACE_LENGTH` (agave `transaction-context`, ARCH §6.2).
const MAX_TRACE: usize = 64;
/// The packet limit for legacy and v0 transactions (ARCH §6.4).
const PACKET: usize = 1_232;
/// The v1 transaction limit (ARCH §6.4: 4,096 B, 64 inline addresses, no lookup tables).
const V1_PACKET: usize = 4_096;

const HEADER: &str = "# M6 T-TRACE (PumpSwap venue)

Instruction-trace entries (every top-level instruction, ComputeBudget included, plus every CPI \
at any depth; ARCH §6.2) of the worst-case PumpSwap transactions of the release short.win on the \
dumped PumpSwap, pump and pump-fees programs, measured on LiteSVM 0.16.0 with every invariant \
hook after every instruction; the limit is `MAX_INSTRUCTION_TRACE_LENGTH = 64`. The peak stack \
height is the deepest CPI level reached (1 = top level; the mainnet limit is 5, ARCH §6.3); the \
CU is the whole transaction's (the two ComputeBudget instructions cost 150 each), within ARCH \
§6.1's limit for it. Written by `M6_TRACE_WRITE=1 cargo test --manifest-path \
tests/bench/m6_cu/Cargo.toml --test t_trace` (README.md); the exact entries of each row are also \
asserted (their composition is pinned in `tests/litesvm/tests/trace_pumpswap_it.rs`).

Sizes (T-SIZE, ARCH §6.4): the legacy wire size (no lookup table) and the v0 wire size through \
the protocol ALT (A28 with its pump part: pump, PumpSwap and pump-fees, their statics and the \
live recipient lists with their WSOL ATAs; `m6_cu_bench::protocol_alt_addresses`), each with its \
account count; LiteSVM enforces neither. Every client and keeper transaction must fit v0 + ALT \
(≤ 1,232 B, ≤ 64 accounts). Bold: above 1,232 B. The full T-SIZE on PumpSwap (the largest extras \
set, a separate fee payer, both tables, executed as v0) is \
`tests/litesvm/tests/tx_size_pumpswap_it.rs`.

The graduation transition (§2.5) is the crank that detects the migration and rewrites the ring; \
a borrower's first close after the migration may carry that transition crank in its own \
transaction; the post-wipe reward claim (§3.9) is `[create_idempotent ATA(trader, WSOL), \
pAMM claim_cashback, sweep_trader]` — `claim_venue_rewards` was not built (the UVA-reading \
pattern).

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
    if std::env::var_os("M6_TRACE_WRITE").is_some() {
        let path = repo_path("tests/bench/m6_cu/trace.md");
        std::fs::write(&path, &table).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
