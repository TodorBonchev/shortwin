//! M3 CU bench (ARCHITECTURE §6.1, §8.2 T-RAY-CU / T-CU, §9 M3; M4 adds `propose_pool` and
//! `execute_pool_switch`): every Raydium-path instruction
//! of the release short.win on the dumped AMM v4 and CPMM programs (SPL and Token-2022 bases),
//! on Mollusk 0.15.1, cross-checked on LiteSVM 0.16.0, within its ARCH §6.1 budget; the trace
//! entries and the peak stack height of each instruction next to ARCH §6.2's figure; the venue
//! CPI's own CU. See README.md.
//!
//! ```sh
//! anchor build --tools-version v1.57 --arch v3   # target/deploy/shortwin.so (release)
//! cargo test --manifest-path tests/bench/m3_cu/Cargo.toml --test m3_cu -- --nocapture
//! ```
//!
//! `SHORTWIN_SO` overrides the ELF; `M3_CU_WRITE=1` rewrites `results.md`. A case that does not
//! execute, a figure above its budget or a disagreement of the two harnesses fails.

use std::fmt::Write as _;

use m3_cu_bench::{cases, measure, repo_path};

const HEADER: &str = "# M3 CU bench (Raydium)

Compute units of every Raydium-path instruction (ARCH §9 M3: T-RAY-CU, T-CU; M4: `propose_pool` \
and `execute_pool_switch`) of the **release** \
short.win (`anchor build --tools-version v1.57 --arch v3`) against the **dumped mainnet** Raydium \
AMM v4 and CPMM programs and pools (`tests/fixtures/`), measured with Mollusk 0.15.1 on account \
states built by the LiteSVM harness on the mainnet fork, with the same short.win, Raydium, SPL \
Token (p-token) and Token-2022 ELFs; LiteSVM 0.16.0 (mainnet feature set) runs the same \
instruction as a cross-check and gives its instruction-trace entries, its peak stack height and \
the Raydium program's own CU (its token CPIs included). AMM v4 is SPL Token only, so the \
Token-2022 rows are CPMM's. Written by `M3_CU_WRITE=1 cargo test --manifest-path \
tests/bench/m3_cu/Cargo.toml --test m3_cu` (README.md); the test fails when a Mollusk figure \
exceeds its budget, the two harnesses disagree, or a case does not execute.

| Instruction | Case | Mollusk CU | LiteSVM CU | Venue CPI CU | Budget | Budget source | Trace | ARCH §6.2 (Raydium) | Peak height |
|---|---|---|---|---|---|---|---|---|---|
";

#[test]
fn m3_cu_bench() {
    let mut table = String::from(HEADER);
    let mut failures = Vec::new();
    for case in cases() {
        let arch = case.trace.map_or("—".to_owned(), |t| {
            format!("{} (≤ {})", t.arch, t.arch_max)
        });
        let row = match measure(&case) {
            Ok(m) => {
                if m.mollusk > case.budget.cu {
                    failures.push(format!(
                        "{} ({}): {} CU > budget {}",
                        case.handler, case.case, m.mollusk, case.budget.cu
                    ));
                }
                if m.mollusk != m.litesvm {
                    failures.push(format!(
                        "{} ({}): Mollusk {} CU != LiteSVM {} CU",
                        case.handler, case.case, m.mollusk, m.litesvm
                    ));
                }
                if m.trace > 64 {
                    failures.push(format!(
                        "{} ({}): {} entries",
                        case.handler, case.case, m.trace
                    ));
                }
                if m.peak > 3 {
                    failures.push(format!(
                        "{} ({}): peak stack height {} > 3 (ARCH §6.3)",
                        case.handler, case.case, m.peak
                    ));
                }
                (
                    m.mollusk.to_string(),
                    m.litesvm.to_string(),
                    m.venue_cu.map_or("—".to_owned(), |c| c.to_string()),
                    m.trace.to_string(),
                    m.peak.to_string(),
                )
            }
            Err(e) => {
                failures.push(e);
                (
                    "*failed*".to_owned(),
                    "—".to_owned(),
                    "—".to_owned(),
                    "—".to_owned(),
                    "—".to_owned(),
                )
            }
        };
        let (mollusk, litesvm, venue, trace, peak) = row;
        writeln!(
            table,
            "| `{}` | {} | {mollusk} | {litesvm} | {venue} | {} | {} | {trace} | {arch} | {peak} |",
            case.handler, case.case, case.budget.cu, case.budget.source
        )
        .unwrap();
    }
    println!("{table}");
    if std::env::var_os("M3_CU_WRITE").is_some() {
        let path = repo_path("tests/bench/m3_cu/results.md");
        std::fs::write(&path, &table).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
