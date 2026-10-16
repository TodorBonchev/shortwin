//! T-CU-MATH (ARCHITECTURE §6.1, §8.1, §9 M0b): the compute units of every `shortwin-math`
//! primitive of ARCH §6.1 (and the v0.4.1 and pump-final additions) on SBF v3, measured with
//! Mollusk, must stay within the §6.1 measured value + 20 % (new primitives: their first
//! measurement + 20 %).
//!
//! Needs the bench program built for SBF first (README.md):
//!
//! ```sh
//! cargo-build-sbf --tools-version v1.57 --arch v3 \
//!     --manifest-path tests/bench/cu_math/program/Cargo.toml --sbf-out-dir tests/bench/cu_math/target/deploy
//! cargo test --manifest-path tests/bench/cu_math/Cargo.toml --release
//! ```
//!
//! `CU_MATH_SO` overrides the program path; `CU_MATH_WRITE=1` rewrites `results.md`.

use std::fmt::Write as _;
use std::path::PathBuf;

use cu_math_bench::id;
use mollusk_svm::Mollusk;
use mollusk_svm::program::loader_keys::LOADER_V3;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

/// Iterations per measurement (as V18).
const N: u32 = 50;

/// A benched primitive: its ARCH §6.1 measured value (V18, the M0 crate) where §6.1 lists
/// it, and its budget.
struct Row {
    id: u8,
    name: &'static str,
    /// ARCH §6.1 measurement (V18); `None` for primitives new in v0.4.1.
    arch: Option<u32>,
    /// CU budget per call: the reference + 20 %, rounded up.
    budget: u32,
}

/// `⌈1.2·cu⌉`.
const fn plus_20(cu: u32) -> u32 {
    (cu * 12).div_ceil(10)
}

const fn arch(id: u8, name: &'static str, cu: u32) -> Row {
    Row {
        id,
        name,
        arch: Some(cu),
        budget: plus_20(cu),
    }
}

/// A v0.4.1 primitive §6.1 does not list: budget from the M0b measurement.
const fn new(id: u8, name: &'static str, measured: u32) -> Row {
    Row {
        id,
        name,
        arch: None,
        budget: plus_20(measured),
    }
}

const ROWS: &[Row] = &[
    arch(id::MUL_DIV, "`mul_div` u128 fast path", 691),
    arch(
        id::MUL_DIV_U256,
        "`mul_div` 256-bit fallback (Knuth D)",
        783,
    ),
    arch(id::MUL_DIV_U64, "`mul_div_u64`", 267),
    arch(
        id::NATIVE_U64,
        "native u64 `a·b` then `div_ceil` (reference)",
        76,
    ),
    arch(
        id::QUOTE_PUMPSWAP,
        "PumpSwap `buy_exact_out` (3 fee components)",
        1_452,
    ),
    arch(id::QUOTE_CPMM, "CPMM `buy_exact_out`", 1_872),
    arch(id::QUOTE_RAY_V4, "AMM v4 `buy_exact_out`", 959),
    arch(id::REPRICE, "`reprice(L, P)`", 843),
    arch(id::LIQUIDITY, "`liquidity` = `isqrt(X·Y)` u128", 734),
    arch(
        id::SQRT_U256,
        "`sqrt_u256` (off the on-chain paths since v0.4)",
        624,
    ),
    arch(id::RENT_DUE, "`rent::due`", 1_287),
    arch(id::RENT_CREDITED_FOR, "`rent::credited_for`", 2_637),
    arch(id::RENT_ACCRUE, "`rent::accrue`", 1_109),
    // §6.1's 2,990 is v0.3's roll; v0.4 adds the cached reprojection, one `reprice`
    // (§6.1 "prologue with a roll": roll + reprice, `cu_budget.py` ROLL + RP).
    arch(
        id::EMA_ROLL,
        "`record` + `integrate` (one EMA roll incl. the cached `reprice`)",
        2_990 + 843,
    ),
    // §6.1: "v0.4.1 sorts 4 × 8 bucket extremes ≈ 3.2 k, by scaling" `obs_median`'s 1,604.
    arch(
        id::RING_MEDIANS,
        "`ring_medians` (4 × 8 bucket extremes)",
        2 * 1_604,
    ),
    arch(id::SIGMA_XY, "`sigma_xy`", 1_473),
    arch(
        id::LEG_CACHED,
        "EMA-leg entry, v0.4 (cached `(x, y)` + quote + CR)",
        1_468,
    ),
    arch(
        id::LEG_REPRICED,
        "repriced quote + CR (v0.3 EMA-leg entry; a box corner)",
        2_289,
    ),
    new(id::BOX_OF, "`settle::box_of` (full ring)", 3_589),
    new(
        id::LIQ_EMA_LEG,
        "`persist::liq_ema_leg(3)`, every entry below `L` (PumpSwap quote + CR each)",
        2_908,
    ),
    new(
        id::SUSTAINED_GROWTH,
        "`sustained_growth(3)` (full ring, a pump)",
        1_475,
    ),
    new(
        id::CAL_ADVANCE,
        "`pool::cal_advance` (one day elapsed)",
        261,
    ),
    // pump-final §5.2–§5.4 (P1), measured at P3 (2026-10-15).
    new(
        id::CURVE_SYNTHETIC,
        "`quote::curve_synthetic` past `rT` (`B̂_v3`: the curve leg and the would-be pool's part; pump-final §5.2)",
        2_429,
    ),
    new(
        id::MIGRATING_FV,
        "`quote::migrating_fv` after a synthetic pool part (ρ-scaled, at the pool's PumpSwap tier; pump-final §5.4)",
        4_743,
    ),
];

fn program() -> (Mollusk, Pubkey) {
    let path = std::env::var_os("CU_MATH_SO").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/deploy/cu_math_bench.so"),
        PathBuf::from,
    );
    let elf = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} ({e}): build it with cargo-build-sbf first (README.md)",
            path.display()
        )
    });
    let id = Pubkey::new_unique();
    let mut mollusk = Mollusk::default();
    mollusk.add_program_with_loader_and_elf(&id, &LOADER_V3, &elf);
    (mollusk, id)
}

fn cu(mollusk: &Mollusk, program: &Pubkey, bench: u8, n: u32) -> u64 {
    let mut data = vec![bench];
    data.extend_from_slice(&n.to_le_bytes());
    let ix = Instruction::new_with_bytes(*program, &data, vec![]);
    let r = mollusk.process_instruction(&ix, &[]);
    assert!(
        r.raw_result.is_ok(),
        "bench {bench} × {n} failed: {:?}",
        r.raw_result
    );
    r.compute_units_consumed
}

/// Per-call CU of `bench`: `(CU(N) − CU(0))/N` minus the loop overhead.
fn per_call(mollusk: &Mollusk, program: &Pubkey, bench: u8, loop_cu: f64) -> f64 {
    let c0 = cu(mollusk, program, bench, 0);
    let cn = cu(mollusk, program, bench, N);
    (cn - c0) as f64 / f64::from(N) - loop_cu
}

#[test]
fn per_primitive_budgets() {
    let (mollusk, program) = program();
    let base = cu(&mollusk, &program, id::LOOP, 0);
    let loop_cu = (cu(&mollusk, &program, id::LOOP, N) - base) as f64 / f64::from(N);
    assert_eq!(
        ROWS.len(),
        usize::from(id::LAST),
        "every bench id has a row"
    );
    let mut table = String::new();
    writeln!(
        table,
        "| Primitive | ARCH §6.1 (V18) | M0b measured | Budget |\n|---|---|---|---|"
    )
    .unwrap();
    let mut over = Vec::new();
    for row in ROWS {
        let got = per_call(&mollusk, &program, row.id, loop_cu).round() as u32;
        let arch = row.arch.map_or("—".into(), |a| a.to_string());
        writeln!(table, "| {} | {arch} | {got} | {} |", row.name, row.budget).unwrap();
        if got > row.budget {
            over.push(format!("{}: {got} CU > budget {}", row.name, row.budget));
        }
    }
    println!("empty call {base} CU; loop overhead {loop_cu:.1} CU/iteration\n{table}");
    if std::env::var_os("CU_MATH_WRITE").is_some() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../results.md");
        let text = format!(
            "# T-CU-MATH results\n\nPer-call compute units of `shortwin-math` on SBF v3 \
             (platform-tools v1.57, Mollusk 0.15.1; {N} iterations, loop overhead \
             subtracted). Written by `CU_MATH_WRITE=1 cargo test --release` in \
             `tests/bench/cu_math` (README.md); CI fails when a value exceeds its budget.\n\n{table}"
        );
        std::fs::write(&path, text).unwrap();
    }
    assert!(
        over.is_empty(),
        "T-CU-MATH budget exceeded:\n{}",
        over.join("\n")
    );
}
