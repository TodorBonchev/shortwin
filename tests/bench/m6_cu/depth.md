# M6 stack depth (PumpSwap venue)

Peak stack height (1 = top level) of every PumpSwap-path instruction of the release short.win on the dumped PumpSwap, pump and pump-fees programs, called directly and through 1–3 levels of the TEST-ONLY CPI wrapper (`wrapper/`), on LiteSVM 0.16.0 with the mainnet feature set (`MAX_INSTRUCTION_STACK_DEPTH = 5`, ARCH §6.3). Written by `M6_DEPTH_WRITE=1 cargo test --manifest-path tests/bench/m6_cu/Cargo.toml --test t_depth` (README.md); the test fails unless each level reaches exactly `direct + levels`, and a level above 5 fails with `CallDepth`.

| Instruction | Case | Direct | 1 wrapper | 2 wrappers | 3 wrappers |
|---|---|---|---|---|---|
| `open_short` | Token-2022, 301 B, boost spent (mature): first open, normalising | 3 | 4 | 5 | refused (`CallDepth`) |
| `close_short` | Token-2022, 301 B, boost spent (mature) | 3 | 4 | 5 | refused (`CallDepth`) |
| `liquidate` | Token-2022, 301 B, boost spent (mature) | 3 | 4 | 5 | refused (`CallDepth`) |
| `expire` | Token-2022, 301 B, boost spent (mature) | 3 | 4 | 5 | refused (`CallDepth`) |
| `create_market` | Auto, Token-2022, 301 B, boost spent (mature): PumpSwap, init set | 3 | 4 | 5 | refused (`CallDepth`) |
| `sweep_trader` | Token-2022, 301 B, boost spent (mature): token leg | 2 | 3 | 4 | 5 |
| `open_short` | SPL Token, 261 B (legacy261): first open, normalising | 3 | 4 | 5 | refused (`CallDepth`) |
| `close_short` | SPL Token, 261 B (legacy261) | 3 | 4 | 5 | refused (`CallDepth`) |
| `liquidate` | SPL Token, 261 B (legacy261) | 3 | 4 | 5 | refused (`CallDepth`) |
| `expire` | SPL Token, 261 B (legacy261) | 3 | 4 | 5 | refused (`CallDepth`) |
| `create_market` | Auto, SPL Token, 261 B (legacy261): PumpSwap, init set | 3 | 4 | 5 | refused (`CallDepth`) |
| `sweep_trader` | SPL Token, 261 B (legacy261): token leg | 2 | 3 | 4 | 5 |
| `open_short` | Token-2022, 301 B, boost spent (mature): the costliest open (creator change) | 3 | 4 | 5 | refused (`CallDepth`) |
| `sol_settle` | S3 under the buy halt | 2 | 3 | 4 | 5 |
| `crank_price` | graduation transition: `PumpCurve → PumpSwap` | 2 | 3 | 4 | 5 |
| `flag_venue_upgraded` | PumpSwap, the dumped ProgramData | 2 | 3 | 4 | 5 |
