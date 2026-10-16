# M3 stack depth (Raydium)

Peak stack height (1 = top level) of every Raydium-path instruction of the release short.win on the dumped AMM v4 and CPMM programs, called directly and through 1–3 levels of the TEST-ONLY CPI wrapper (`wrapper/`), on LiteSVM 0.16.0 with the mainnet feature set (`MAX_INSTRUCTION_STACK_DEPTH = 5`, ARCH §6.3). Written by `M3_DEPTH_WRITE=1 cargo test --manifest-path tests/bench/m3_cu/Cargo.toml --test t_depth` (README.md); the test fails unless each level reaches exactly `direct + levels`, and a level above 5 fails with `CallDepth`.

| Instruction | Case | Direct | 1 wrapper | 2 wrappers | 3 wrappers |
|---|---|---|---|---|---|
| `open_short` | AMM v4, SPL (Fartcoin): first open, normalising | 3 | 4 | 5 | refused (`CallDepth`) |
| `close_short` | AMM v4, SPL (Fartcoin) | 3 | 4 | 5 | refused (`CallDepth`) |
| `liquidate` | AMM v4, SPL (Fartcoin) | 3 | 4 | 5 | refused (`CallDepth`) |
| `expire` | AMM v4, SPL (Fartcoin) | 3 | 4 | 5 | refused (`CallDepth`) |
| `sol_settle` | AMM v4, SPL (Fartcoin): S3 on a halted pool | 2 | 3 | 4 | 5 |
| `crank_price` | AMM v4, SPL (Fartcoin): pool halted, chains start | 2 | 3 | 4 | 5 |
| `open_short` | CPMM, SPL (USELESS): first open, normalising | 3 | 4 | 5 | refused (`CallDepth`) |
| `close_short` | CPMM, SPL (USELESS) | 3 | 4 | 5 | refused (`CallDepth`) |
| `liquidate` | CPMM, SPL (USELESS) | 3 | 4 | 5 | refused (`CallDepth`) |
| `expire` | CPMM, SPL (USELESS) | 3 | 4 | 5 | refused (`CallDepth`) |
| `sol_settle` | CPMM, SPL (USELESS): S3 on a halted pool | 2 | 3 | 4 | 5 |
| `crank_price` | CPMM, SPL (USELESS): pool halted, chains start | 2 | 3 | 4 | 5 |
| `open_short` | CPMM, Token-2022 base: first open, normalising | 3 | 4 | 5 | refused (`CallDepth`) |
| `close_short` | CPMM, Token-2022 base | 3 | 4 | 5 | refused (`CallDepth`) |
| `liquidate` | CPMM, Token-2022 base | 3 | 4 | 5 | refused (`CallDepth`) |
| `expire` | CPMM, Token-2022 base | 3 | 4 | 5 | refused (`CallDepth`) |
| `sol_settle` | CPMM, Token-2022 base: S3 on a halted pool | 2 | 3 | 4 | 5 |
| `crank_price` | CPMM, Token-2022 base: pool halted, chains start | 2 | 3 | 4 | 5 |
| `create_market` | CPMM, Token-2022 base | 2 | 3 | 4 | 5 |
| `flag_venue_upgraded` | AMM v4, the dumped ProgramData | 2 | 3 | 4 | 5 |
| `propose_pool` | CPMM, Token-2022 base: zero-position adoption | 2 | 3 | 4 | 5 |
| `execute_pool_switch` | AMM v4, SPL (Fartcoin): fast adoption, a position open | 2 | 3 | 4 | 5 |
