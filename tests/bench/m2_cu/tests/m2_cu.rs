//! M2 CU bench (ARCHITECTURE §6.1, §8.2 T-CU, §9 M2): every M2 instruction on Mollusk 0.15.1
//! against the mock venue, cross-checked on LiteSVM 0.16.0, within its budget; the trace
//! entries of each instruction next to ARCH §6.2's figure. See README.md.
//!
//! ```sh
//! anchor build --tools-version v1.57 --arch v3   # target/deploy/mock_cpamm.so
//! scripts/build-test-venue.sh                    # target/test-venue/deploy/shortwin.so
//! cargo test --manifest-path tests/bench/m2_cu/Cargo.toml -- --nocapture
//! ```
//!
//! `SHORTWIN_TEST_VENUE_SO` / `MOCK_CPAMM_SO` override the ELFs; `M2_CU_WRITE=1` rewrites
//! `results.md`. Every M2 handler has landed: a case that does not execute fails.

use std::fmt::Write as _;

use m2_cu_bench::{Outcome, cases, measure, repo_path};

const HEADER: &str = "# M2 CU bench

Compute units of every M2 instruction (ARCH §9 M2) on short.win's **test-venue** SBF build \
(`scripts/build-test-venue.sh`: `anchor build --tools-version v1.57 --arch v3 -- --features \
test-venue`) against the TEST-ONLY mock venue, measured with Mollusk 0.15.1 on account states \
built by the LiteSVM harness, with the same SPL Token (p-token), Token-2022 and mock ELFs; \
LiteSVM 0.16.0 runs the same instruction as a cross-check and gives its instruction-trace \
entries. Written by `M2_CU_WRITE=1 cargo test --manifest-path tests/bench/m2_cu/Cargo.toml` \
(README.md); the test fails when a Mollusk figure exceeds its budget, the two harnesses \
disagree, or a case does not execute.

| Instruction | Case | Mollusk CU | LiteSVM CU | Budget | Budget source | Trace | ARCH §6.2 (Raydium) |
|---|---|---|---|---|---|---|---|
";

#[test]
fn m2_cu_bench() {
    let mut table = String::from(HEADER);
    let mut failures = Vec::new();
    for case in cases() {
        let arch = case.trace.map_or("—".to_owned(), |t| {
            format!("{} (≤ {})", t.arch, t.arch_max)
        });
        let (mollusk, litesvm, trace) = match measure(&case) {
            Ok(Outcome::Measured {
                mollusk,
                litesvm,
                trace,
            }) => {
                if mollusk > case.budget.cu {
                    failures.push(format!(
                        "{} ({}): {mollusk} CU > budget {}",
                        case.handler, case.case, case.budget.cu
                    ));
                }
                if mollusk != litesvm {
                    failures.push(format!(
                        "{} ({}): Mollusk {mollusk} CU != LiteSVM {litesvm} CU",
                        case.handler, case.case
                    ));
                }
                (mollusk.to_string(), litesvm.to_string(), trace.to_string())
            }
            Err(e) => {
                failures.push(e);
                ("*failed*".to_owned(), "—".to_owned(), "—".to_owned())
            }
        };
        writeln!(
            table,
            "| `{}` | {} | {mollusk} | {litesvm} | {} | {} | {trace} | {arch} |",
            case.handler, case.case, case.budget.cu, case.budget.source
        )
        .unwrap();
    }
    println!("{table}");
    if std::env::var_os("M2_CU_WRITE").is_some() {
        let path = repo_path("tests/bench/m2_cu/results.md");
        std::fs::write(&path, &table).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
