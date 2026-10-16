# cu_math — T-CU-MATH per-primitive CU budgets

The M0b gate of ARCHITECTURE §6.1 and §8.1: a Mollusk bench of every `shortwin-math`
primitive that §6.1 lists (V18), with a budget of the §6.1 measured value + 20 %, plus the
v0.4.1 primitives §6.1 does not list (`settle::box_of`, `persist::liq_ema_leg`,
`sustained_growth`, `pool::cal_advance`), budgeted at their M0b measurement + 20 %. A
regression fails CI. T-CU then measures whole instructions (M1 onwards).

It ports `docs/design/arch/cu_math` (the `solana-test-validator` micro-benchmark behind V18)
to Mollusk and to the v0.4.1 API, with the same Tier A PumpSwap inputs, `n = 50` iterations
and the loop overhead subtracted, so the numbers compare with §6.1's.

- `program/` — `cu-math-bench`, an SBF program with a raw entrypoint: instruction data
  `[bench id u8][n u32 LE]`, no accounts. Each bench is an `#[inline(never)]` function (one
  ring or vault per 4 KiB frame). On the host it is a plain library whose unit test checks
  that every bench computes a value on these inputs.
- `runner/` — `cu-math-runner`, whose test `tests/cu_math.rs` loads the ELF into Mollusk,
  measures `(CU(n) − CU(0))/n − loop overhead` per bench and asserts the budget table.
- `results.md` — the last measurement (written with `CU_MATH_WRITE=1`).

```sh
# from the repository root: build the program (never a bare cargo build-sbf default)
cargo-build-sbf --tools-version v1.57 --arch v3 \
  --manifest-path tests/bench/cu_math/program/Cargo.toml --sbf-out-dir tests/bench/cu_math/target/deploy
# run the budgets (CU_MATH_SO=<path> overrides the ELF; CU_MATH_WRITE=1 rewrites results.md)
cargo test --manifest-path tests/bench/cu_math/Cargo.toml --release
```

## Workspace and pins

This is a standalone workspace with its own `Cargo.lock`: Mollusk's Agave host crates must
not enter the on-chain workspace's lockfile before M1 pins the one Agave line LiteSVM and
Mollusk share (ARCH §1.1, C1-14). Findings for that pin:

- Mollusk **0.15.1** declares Agave `^4.2.0`; resolver 3 picks the **4.3.0** crates, against
  which Mollusk 0.15.1 does not compile (`InvokeContext::{prepare_top_level_instructions,
  process_precompile, process_instruction}` became private). The runner therefore holds the
  Agave runtime crates at `~4.2.2` (its `Cargo.toml`), which ARCH §1.1's "UNVERIFIED which of
  4.2.x / 4.3.0 resolver 3 picks" now settles: 4.3.0, and 4.2.x must be pinned.
- The program is built by platform-tools v1.57 (rustc 1.95), SBPF v3, like the program.

## Findings (M0b, see `results.md`)

- Every §6.1 primitive is within its budget. `reprice` is the tightest (985 CU against 843:
  the v0.4.1 `isqrt⁺(P)` costs one extra square and compare); the EMA roll now includes the
  cached reprojection (§6.1's roll + `reprice`).
- `mul_div_u64` takes a native u64 path when `a·b` fits `u64` (ARCH §9 M0b's optional item,
  bit-identical, proptested in `crates/shortwin-math/tests/num_muldiv.rs`): 278 → 96 CU, which
  takes the PumpSwap quote from 1,458 to 944 CU and an EMA-leg entry from 1,465 to 951 CU.
- `rent::credited_for` keeps the dot product in u128 and takes one 256-bit `mul_div` for the
  lender share (a typical position's `Σ q·coef·ΔJ·(10⁴ − p)` exceeds 2¹²⁸ only at the last
  factor): 3,239 → 2,991 CU (§6.1: 2,637, budget 3,165).
- pump-final (P3, 2026-10-15) adds the two quotes pump's synthetic migration brought, on a
  near-complete curve (2 M tokens left, a position of 4 M, pump's 0 / 95 / 30 bps):
  `quote::curve_synthetic` past `rT` (`B̂_v3`, the curve leg plus the would-be pool's part)
  2,429 CU, about two PumpSwap quotes and a `migrated_pool`; `quote::migrating_fv` after a
  synthetic pool part (the pool `migrate_v2` will create, ρ-scaled, at its PumpSwap tier)
  4,743 CU. Every quote a curve exit takes on a state past `rT` (the EMA leg, the box corners,
  `B̂_ref`) is a `B̂_v3`: the M5 bench's worst-ring rows measure what that adds to `liquidate`
  and `expire`. The other figures are unchanged by pump-final's program change.
