//! M1 CU baseline (ARCHITECTURE §6.1, §8.2, §9 M1): every M1 instruction on Mollusk 0.15.1,
//! cross-checked on LiteSVM 0.16.0, within its budget. See README.md.
//!
//! ```sh
//! anchor build --tools-version v1.57 --arch v3          # target/deploy/shortwin.so
//! cargo test --manifest-path tests/bench/m1_cu/Cargo.toml -- --nocapture
//! ```
//!
//! `SHORTWIN_SO` overrides the program path; `M1_CU_WRITE=1` rewrites `results.md`.

use std::fmt::Write as _;

use m1_cu_bench::{Measured, cases, measure, repo_path};

const HEADER: &str = "# M1 CU baseline

Compute units of every M1 instruction (ARCH §9 M1) on the SBF program (`anchor build \
--tools-version v1.57 --arch v3`), measured with Mollusk 0.15.1 on account states built by the \
LiteSVM harness, with the same SPL Token (p-token), Token-2022 and ATA ELFs; LiteSVM 0.16.0 \
runs the same instruction as a cross-check. Written by `M1_CU_WRITE=1 cargo test \
--manifest-path tests/bench/m1_cu/Cargo.toml` (README.md); CI fails when a Mollusk figure \
exceeds its budget or a case fails.

| Instruction | Case | Mollusk CU | LiteSVM CU | Budget | Budget source |
|---|---|---|---|---|---|
";

#[test]
fn m1_cu_baseline() {
    let mut table = String::from(HEADER);
    let mut failures = Vec::new();
    for case in cases() {
        let (mollusk, litesvm) = match measure(&case) {
            Ok(Measured { mollusk, litesvm }) => {
                if mollusk > case.budget.cu {
                    failures.push(format!(
                        "{} ({}): {mollusk} CU > budget {}",
                        case.handler, case.case, case.budget.cu
                    ));
                }
                (mollusk.to_string(), litesvm.to_string())
            }
            Err(e) => {
                failures.push(e);
                ("*failed*".to_owned(), "—".to_owned())
            }
        };
        writeln!(
            table,
            "| `{}` | {} | {mollusk} | {litesvm} | {} | {} |",
            case.handler, case.case, case.budget.cu, case.budget.source
        )
        .unwrap();
    }
    println!("{table}");
    if std::env::var_os("M1_CU_WRITE").is_some() {
        let path = repo_path("tests/bench/m1_cu/results.md");
        std::fs::write(&path, &table).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
