# M2 T-TRACE

Instruction-trace entries (every top-level instruction, ComputeBudget included, plus every CPI at any depth; ARCH §6.2) of the worst-case M2 transactions on the mock venue, measured on LiteSVM 0.16.0 with every invariant hook after every instruction. The limit is `MAX_INSTRUCTION_TRACE_LENGTH = 64`. Written by `M2_TRACE_WRITE=1 cargo test --manifest-path tests/bench/m2_cu/Cargo.toml --test t_trace` (README.md).

The legacy size is the transaction's wire size without an address lookup table, the figure the 1,232-byte packet limit applies to (LiteSVM does not enforce it; T-SIZE, M3–M7, measures the real transactions with the protocol ALT of A28).

| Transaction | Entries | CU | Legacy size (B) | ARCH §6.2 |
|---|---|---|---|---|
| ComputeBudget ×2 + open_short with normalisation (first open) | 20 | 114303 | 1033 | 2 + ≤ 15 (§6.2 Raydium `open_short` max) |
| ComputeBudget ×2 + close_short with normalisation | 14 | 76340 | 935 | 2 + ≤ 11 (§6.2 Raydium exit max) |
| ComputeBudget ×2 + [front-run swap, open_short with normalisation, back-run swap] | 26 | 153411 | 1269 (> 1,232: v0 + ALT or v1) | no row (atomic sandwich bundle, §8.2) |
| ComputeBudget ×2 + four opens of one wallet (the most that fit: 2 + 15 + 3·14) | 59 | 398479 | 1366 (> 1,232: v0 + ALT or v1) | no row |
| ComputeBudget ×2 + crank_price ×6, every crank starting a route chain | 20 | 144996 | 1394 (> 1,232: v0 + ALT or v1) | 2 + 2k = 14 (§6.2, k = 6) |
| ComputeBudget ×2 + add_margin ×20 | 62 | 269480 | 942 | no row (3 per add_margin) |
| ComputeBudget ×2 + crank_price + liquidate (keeper) | 13 | 119789 | 1012 | ≤ 2 + 2 + 19 = 23 (§6.2; 2 + 2 + 11 on Raydium) |
| ComputeBudget ×2 + crank_price + expire (keeper) | 13 | 115298 | 1012 | ≤ 2 + 2 + 11 on Raydium (§6.2) |
| ComputeBudget ×2 + crank_price + sol_settle S3 (keeper) | 6 | 77045 | 808 | 2 + 2 + 2 (+3 with a ShareEpoch record) (§6.2) |
