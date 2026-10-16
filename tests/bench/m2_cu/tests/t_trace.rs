//! T-TRACE (ARCHITECTURE §6.2, §8.2; C1-1): the instruction-trace entries of the worst-case M2
//! transactions — every top-level instruction (ComputeBudget included) plus every CPI at any
//! depth — stay within `MAX_INSTRUCTION_TRACE_LENGTH = 64`, on LiteSVM with every invariant
//! hook after every instruction. Also checks the counting against the runtime: a transaction
//! one entry over the limit is refused by the runtime itself.
//!
//! `M2_TRACE_WRITE=1` rewrites `trace.md`.

use std::fmt::Write as _;

use m2_cu_bench::{
    Q, TraceOutcome, compute_budget, repo_path, run_trace, with_position, worst_case_transactions,
};
use shortwin_litesvm_tests::engine::add_margin_ix;
use shortwin_litesvm_tests::tokens::TokenProgram;

/// `MAX_INSTRUCTION_TRACE_LENGTH` (agave `transaction-context`, ARCH §6.2).
const MAX_TRACE: usize = 64;

const HEADER: &str = "# M2 T-TRACE

Instruction-trace entries (every top-level instruction, ComputeBudget included, plus every CPI \
at any depth; ARCH §6.2) of the worst-case M2 transactions on the mock venue, measured on \
LiteSVM 0.16.0 with every invariant hook after every instruction. The limit is \
`MAX_INSTRUCTION_TRACE_LENGTH = 64`. Written by `M2_TRACE_WRITE=1 cargo test --manifest-path \
tests/bench/m2_cu/Cargo.toml --test t_trace` (README.md).

The legacy size is the transaction's wire size without an address lookup table, the figure \
the 1,232-byte packet limit applies to (LiteSVM does not enforce it; T-SIZE, M3–M7, measures \
the real transactions with the protocol ALT of A28).

| Transaction | Entries | CU | Legacy size (B) | ARCH §6.2 |
|---|---|---|---|---|
";

#[test]
fn worst_case_transactions_fit_the_trace() {
    let mut table = String::from(HEADER);
    let mut failures = Vec::new();
    for t in worst_case_transactions() {
        let (entries, cu, size) = match run_trace(&t) {
            Ok(TraceOutcome::Measured {
                entries,
                cu,
                legacy_bytes,
            }) => {
                if entries > MAX_TRACE {
                    failures.push(format!("{}: {entries} entries > {MAX_TRACE}", t.name));
                }
                (entries.to_string(), cu.to_string(), legacy_bytes)
            }
            Err(e) => {
                failures.push(e);
                ("*failed*".to_owned(), "—".to_owned(), 0)
            }
        };
        let size = if size > 1_232 {
            format!("{size} (> 1,232: v0 + ALT or v1)")
        } else {
            size.to_string()
        };
        writeln!(
            table,
            "| {} | {entries} | {cu} | {size} | {} |",
            t.name, t.arch
        )
        .unwrap();
    }
    println!("{table}");
    if std::env::var_os("M2_TRACE_WRITE").is_some() {
        let path = repo_path("tests/bench/m2_cu/trace.md");
        std::fs::write(&path, &table).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Five opens of one wallet are one entry-set too many (2 + 15 + 4·14 = 73): the runtime
/// refuses the transaction; four fit (59, `worst_case_transactions_fit_the_trace`).
#[test]
fn five_opens_exceed_the_trace() {
    let (mut env, ixs, signers) = m2_cu_bench::opens_of_one_wallet(5);
    let refs: Vec<&solana_keypair::Keypair> = signers.iter().collect();
    let err = env.send(&ixs, &refs).expect_err("73 entries");
    assert!(
        format!("{:?}", err.err).contains("MaxInstructionTraceLengthExceeded"),
        "{:?}",
        err.err
    );
}

/// The counting matches the runtime's: ComputeBudget ×2 + 20 top-ups (3 entries each) is 62
/// entries and lands; 21 top-ups (65) is refused by the runtime with
/// `MaxInstructionTraceLengthExceeded`, before any hook could blame short.win.
#[test]
fn the_runtime_enforces_the_counted_limit() {
    let (mut env, mm, b, nonce) = with_position(TokenProgram::Spl);
    let batch = |k: u64| {
        let mut v = compute_budget(1_400_000, 1).to_vec();
        v.extend((0..k).map(|i| add_margin_ix(&mm, &b.key(), nonce, 1_000_000 + i)));
        v
    };
    let meta = shortwin_litesvm_tests::errors::ok(env.send(&batch(20), &[&b.kp]));
    assert_eq!(m2_cu_bench::tx_trace_entries(&meta, 22), 62);
    let res = env.send(&batch(21), &[&b.kp]);
    let err = res.expect_err("65 entries");
    assert_eq!(
        format!("{:?}", err.err),
        "InstructionError(22, MaxInstructionTraceLengthExceeded)",
        "logs:\n{}",
        err.meta.pretty_logs()
    );
    let _ = Q;
}
