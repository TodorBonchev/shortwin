# M3 T-TRACE (Raydium)

Instruction-trace entries (every top-level instruction, ComputeBudget included, plus every CPI at any depth; ARCH §6.2) of the worst-case Raydium transactions of the release short.win on the dumped AMM v4 and CPMM programs, measured on LiteSVM 0.16.0 with every invariant hook after every instruction; the limit is `MAX_INSTRUCTION_TRACE_LENGTH = 64`. The peak stack height is the deepest CPI level reached (1 = top level; the mainnet limit is 5, ARCH §6.3). Written by `M3_TRACE_WRITE=1 cargo test --manifest-path tests/bench/m3_cu/Cargo.toml --test t_trace` (README.md); the exact entries of each row are also asserted (their composition is pinned in `tests/litesvm/tests/trace_raydium_it.rs`).

Sizes (T-SIZE preview, ARCH §6.4): the legacy wire size (no lookup table) and the v0 wire size with the Raydium part of the protocol ALT (A28; `shortwin_litesvm_tests::raydium::protocol_alt_addresses`), each with its account count; LiteSVM enforces neither. Every client and keeper transaction must fit v0 + ALT (≤ 1,232 B, ≤ 64 accounts), except the keeper's crank batches, which ARCH §6.4 sends as **v1** transactions (≤ 4,096 B, every key inline, ≤ 64 accounts; the legacy encoding stands in for v1's here). Bold: above 1,232 B.

| Transaction | Entries | Expected | Peak height | CU | Legacy B / accounts | v0 + ALT B / accounts | ARCH §6.2 |
|---|---|---|---|---|---|---|---|
| AMM v4, SPL (Fartcoin): ComputeBudget ×2 + open_short, first open, normalising | 20 | 20 | 3 | 107625 | 1067 / 25 | 824 / 25 | 2 + ≤ 15 (§6.2 Raydium `open_short` max) |
| AMM v4, SPL (Fartcoin): ComputeBudget ×2 + close_short, normalising | 14 | 14 | 3 | 75086 | 969 / 23 | 788 / 23 | 2 + ≤ 11 (§6.2 Raydium exit max) |
| AMM v4, SPL (Fartcoin): ComputeBudget ×2 + [venue swap, open_short normalising, venue swap] | 26 | 26 | 3 | 137275 | **1283** / 28 | 1071 / 28 | no row (atomic sandwich bundle, §8.2) |
| CPMM, SPL (USELESS): ComputeBudget ×2 + open_short, first open, normalising | 20 | 20 | 3 | 123377 | 1134 / 27 | 860 / 27 | 2 + ≤ 15 (§6.2 Raydium `open_short` max) |
| CPMM, SPL (USELESS): ComputeBudget ×2 + close_short, normalising | 14 | 14 | 3 | 86515 | 1036 / 25 | 824 / 25 | 2 + ≤ 11 (§6.2 Raydium exit max) |
| CPMM, SPL (USELESS): ComputeBudget ×2 + [venue swap, open_short normalising, venue swap] | 26 | 26 | 3 | 171999 | **1374** / 30 | 1131 / 30 | no row (atomic sandwich bundle, §8.2) |
| CPMM, Token-2022 base: ComputeBudget ×2 + open_short, first open, normalising | 20 | 20 | 3 | 131630 | 1166 / 28 | 861 / 28 | 2 + ≤ 15 (§6.2 Raydium `open_short` max) |
| CPMM, Token-2022 base: ComputeBudget ×2 + close_short, normalising | 14 | 14 | 3 | 90084 | 1068 / 26 | 825 / 26 | 2 + ≤ 11 (§6.2 Raydium exit max) |
| CPMM, Token-2022 base: ComputeBudget ×2 + [venue swap, open_short normalising, venue swap] | 26 | 26 | 3 | 185589 | **1406** / 31 | 1132 / 31 | no row (atomic sandwich bundle, §8.2) |
| AMM v4, SPL (Fartcoin): ComputeBudget ×2 + four opens of one wallet (the most that fit: 2 + 15 + 3·14) | 59 | 59 | 3 | 398539 | **1406** / 28 | 1163 / 28 | no row |
| CPMM, Token-2022 base: ComputeBudget ×2 + four opens of one wallet (the most that fit: 2 + 15 + 3·14) | 59 | 59 | 3 | 477678 | **1514** / 31 | 1209 / 31 | no row |
| ComputeBudget ×2 + crank_price ×4 on four Raydium markets, every pool just halted (the most a v0 + ALT transaction holds) | 14 | 14 | 2 | 105869 | 1201 / 31 | 1113 / 31 | 2 + 2k = 10 (§6.2, k = 4) |
| ComputeBudget ×2 + crank_price ×6 on six Raydium markets, every pool just halted (a v1 keeper batch) | 20 | 20 | 2 | 158941 | **1661** / 44 | **1542** / 44 | 2 + 2k = 14 (§6.2, k = 6) |
| AMM v4, SPL (Fartcoin): ComputeBudget ×2 + crank_price + liquidate (keeper) | 13 | 13 | 3 | 119434 | 1047 / 25 | 835 / 25 | 2 + 2 + 11 on Raydium (§6.2) |
| AMM v4, SPL (Fartcoin): ComputeBudget ×2 + crank_price + expire (keeper) | 13 | 13 | 3 | 115285 | 1047 / 25 | 835 / 25 | 2 + 2 + 11 on Raydium (§6.2) |
| AMM v4, SPL (Fartcoin): ComputeBudget ×2 + crank_price + sol_settle S3 (keeper, halted pool) | 6 | 6 | 2 | 67822 | 842 / 19 | 754 / 19 | 2 + 2 + 2 (§6.2) |
| CPMM, SPL (USELESS): ComputeBudget ×2 + crank_price + liquidate (keeper) | 13 | 13 | 3 | 136181 | 1115 / 27 | 872 / 27 | 2 + 2 + 11 on Raydium (§6.2) |
| CPMM, SPL (USELESS): ComputeBudget ×2 + crank_price + expire (keeper) | 13 | 13 | 3 | 130335 | 1115 / 27 | 872 / 27 | 2 + 2 + 11 on Raydium (§6.2) |
| CPMM, SPL (USELESS): ComputeBudget ×2 + crank_price + sol_settle S3 (keeper, halted pool) | 6 | 6 | 2 | 73824 | 876 / 20 | 757 / 20 | 2 + 2 + 2 (§6.2) |
| CPMM, Token-2022 base: ComputeBudget ×2 + crank_price + liquidate (keeper) | 13 | 13 | 3 | 140631 | 1147 / 28 | 873 / 28 | 2 + 2 + 11 on Raydium (§6.2) |
| CPMM, Token-2022 base: ComputeBudget ×2 + crank_price + expire (keeper) | 13 | 13 | 3 | 133890 | 1147 / 28 | 873 / 28 | 2 + 2 + 11 on Raydium (§6.2) |
| CPMM, Token-2022 base: ComputeBudget ×2 + crank_price + sol_settle S3 (keeper, halted pool) | 6 | 6 | 2 | 74133 | 876 / 20 | 757 / 20 | 2 + 2 + 2 (§6.2) |
| ComputeBudget ×2 + flag_venue_upgraded ×2 (both Raydium programs, the dumped ProgramData) | 6 | 6 | 2 | 12392 | 378 / 7 | 352 / 7 | 2 + 2 + 2 (§6.2) |
| AMM v4, SPL (Fartcoin): ComputeBudget ×2 + propose_pool (zero-position adoption, ring rewritten) | 4 | 4 | 2 | 42455 | 726 / 18 | 607 / 18 | 2 + 2 (§6.2 `propose_pool`) |
| AMM v4, SPL (Fartcoin): ComputeBudget ×2 + execute_pool_switch (fast adoption, a position open) | 4 | 4 | 2 | 47988 | 725 / 18 | 606 / 18 | 2 + 2 (no row; as `propose_pool`) |
| CPMM, Token-2022 base: ComputeBudget ×2 + propose_pool (zero-position adoption, ring rewritten) | 4 | 4 | 2 | 42828 | 727 / 18 | 608 / 18 | 2 + 2 (§6.2 `propose_pool`) |
| CPMM, Token-2022 base: ComputeBudget ×2 + execute_pool_switch (fast adoption, a position open) | 4 | 4 | 2 | 48948 | 726 / 18 | 607 / 18 | 2 + 2 (no row; as `propose_pool`) |
