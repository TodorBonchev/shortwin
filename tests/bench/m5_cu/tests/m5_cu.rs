//! M5 CU bench (ARCHITECTURE §6.1, §8.2 T-CU, §9 M5): every curve-path instruction of the release
//! short.win on the dumped pump, pump-fees and PumpSwap programs (SPL Token and Token-2022 curves),
//! on Mollusk 0.15.1, cross-checked on LiteSVM 0.16.0, within its ARCH §6.1 budget; the trace
//! entries and the peak stack height of each instruction next to ARCH §6.2's figure; pump's own
//! CU. See README.md.
//!
//! ```sh
//! anchor build --tools-version v1.57 --arch v3   # target/deploy/shortwin.so (release)
//! cargo test --manifest-path tests/bench/m5_cu/Cargo.toml --test m5_cu -- --nocapture
//! ```
//!
//! `SHORTWIN_SO` overrides the ELF; `M5_CU_WRITE=1` rewrites `results.md`. A case that does not
//! execute, a figure above its budget, a disagreement of the two harnesses, a trace above 64
//! entries or a peak stack height above 3 fails.

use std::fmt::Write as _;

use m5_cu_bench::{cases, measure, repo_path};

const HEADER: &str = "# M5 CU bench (pump curve)

Compute units of every curve-path instruction (ARCH §9 M5: T-CU) of the **release** short.win \
(`anchor build --tools-version v1.57 --arch v3`) against the **dumped mainnet** pump, pump-fees and \
PumpSwap programs and coins (`tests/fixtures/`, group `pump_state`), measured with Mollusk 0.15.1 \
on account states built by the LiteSVM harness on the pump fork, with the same short.win, pump, \
pump-fees, SPL Token (p-token), Token-2022 and ATA ELFs; LiteSVM 0.16.0 (mainnet feature set) runs \
the same instruction behind a 1.4 M `SetComputeUnitLimit` as a cross-check (its own `consumed` \
line) and gives its instruction-trace entries, its peak stack height and pump's own CU (its \
outermost frames: its pump-fees, token, System and event CPIs included). Budgets are ARCH §6.1's \
PROPOSED transaction limits; `results.md` names the source of each. Written by `M5_CU_WRITE=1 \
cargo test --manifest-path tests/bench/m5_cu/Cargo.toml --test m5_cu` (README.md); the test \
fails when a Mollusk figure exceeds its budget, the two harnesses disagree, or a case does not \
execute.

| Instruction | Case | Mollusk CU | LiteSVM CU | pump CU | Budget | Budget source | Trace | ARCH §6.2 (curve) | Peak height |
|---|---|---|---|---|---|---|---|---|---|
";

#[test]
fn m5_cu_bench() {
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
    if std::env::var_os("M5_CU_WRITE").is_some() {
        let path = repo_path("tests/bench/m5_cu/results.md");
        std::fs::write(&path, &table).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
