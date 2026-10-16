//! Stack depth on Raydium (ARCHITECTURE §6.3; C1-8; the Raydium part of T-DEPTH): every
//! Raydium-path instruction of the release short.win, called directly and through one, two and
//! three levels of the TEST-ONLY wrapper program (`wrapper/`: it forwards one instruction by CPI,
//! as a Squads vault transaction or an integrator's program would call short.win), on LiteSVM's
//! **mainnet** feature set (`MAX_INSTRUCTION_STACK_DEPTH = 5`; SIMD-0268 inactive). Mollusk is
//! not used here: it enables every feature, SIMD-0268's limit of 9 included.
//!
//! Every trading path peaks at 3 (short.win 1 → venue 2 → token 3), so two wrapper levels fit
//! (peak 5) and a third fails at the venue's token CPI with `CallDepth`; the instructions without
//! a venue CPI peak at 2 and fit three levels. Every run goes through `Env::send`: the invariant
//! hooks see the wrapped instruction's events and accounts.
//!
//! ```sh
//! cargo-build-sbf --tools-version v1.57 --arch v3 \
//!   --manifest-path tests/bench/m3_cu/wrapper/Cargo.toml --sbf-out-dir tests/bench/m3_cu/target/deploy
//! cargo test --manifest-path tests/bench/m3_cu/Cargo.toml --test t_depth -- --nocapture
//! ```
//!
//! `M3_WRAPPER_SO` overrides the wrapper's ELF; `M3_DEPTH_WRITE=1` rewrites `depth.md`.

use std::fmt::Write as _;

use m3_cu_bench::{depth_cases, repo_path, run_wrapped};

/// `MAX_INSTRUCTION_STACK_DEPTH` on mainnet (ARCH §6.3).
const MAX_DEPTH: u8 = 5;

const HEADER: &str = "# M3 stack depth (Raydium)

Peak stack height (1 = top level) of every Raydium-path instruction of the release short.win \
on the dumped AMM v4 and CPMM programs, called directly and through 1–3 levels of the TEST-ONLY \
CPI wrapper (`wrapper/`), on LiteSVM 0.16.0 with the mainnet feature set \
(`MAX_INSTRUCTION_STACK_DEPTH = 5`, ARCH §6.3). Written by `M3_DEPTH_WRITE=1 cargo test \
--manifest-path tests/bench/m3_cu/Cargo.toml --test t_depth` (README.md); the test fails unless \
each level reaches exactly `direct + levels`, and a level above 5 fails with `CallDepth`.

| Instruction | Case | Direct | 1 wrapper | 2 wrappers | 3 wrappers |
|---|---|---|---|---|---|
";

#[test]
fn raydium_paths_keep_two_spare_levels() {
    let mut table = String::from(HEADER);
    let mut failures = Vec::new();
    for case in depth_cases() {
        let mut cells = Vec::new();
        for levels in 0..=3usize {
            let want = case.direct_peak + levels as u8;
            let got = run_wrapped(&case, levels);
            let cell = match (&got, want <= MAX_DEPTH) {
                (Ok(peak), true) if *peak == want => peak.to_string(),
                (Err(e), false) if e.contains("CallDepth") => "refused (`CallDepth`)".to_owned(),
                _ => {
                    failures.push(format!(
                        "{} ({}), {levels} wrapper level(s): expected {}, got {got:?}",
                        case.handler,
                        case.case,
                        if want <= MAX_DEPTH {
                            format!("peak {want}")
                        } else {
                            "CallDepth".to_owned()
                        }
                    ));
                    "*unexpected*".to_owned()
                }
            };
            cells.push(cell);
        }
        writeln!(
            table,
            "| `{}` | {} | {} |",
            case.handler,
            case.case,
            cells.join(" | ")
        )
        .unwrap();
    }
    println!("{table}");
    if std::env::var_os("M3_DEPTH_WRITE").is_some() {
        let path = repo_path("tests/bench/m3_cu/depth.md");
        std::fs::write(&path, &table).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
