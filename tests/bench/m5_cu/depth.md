# M5 stack depth (pump curve)

Peak stack height (1 = top level) of every curve-path instruction of the release short.win on the dumped pump, pump-fees and PumpSwap programs, called directly and through 1–3 levels of the TEST-ONLY CPI wrapper (`wrapper/`), on LiteSVM 0.16.0 with the mainnet feature set (`MAX_INSTRUCTION_STACK_DEPTH = 5`, ARCH §6.3). Written by `M5_DEPTH_WRITE=1 cargo test --manifest-path tests/bench/m5_cu/Cargo.toml --test t_depth` (README.md); the test fails unless each level reaches exactly `direct + levels`, and a level above 5 fails with `CallDepth`.

| Instruction | Case | Direct | 1 wrapper | 2 wrappers | 3 wrappers |
|---|---|---|---|---|---|
| `open_short` | SPL Token, 115 B curve, no creator (legacy115): first open, normalising | 3 | 4 | 5 | refused (`CallDepth`) |
| `close_short` | SPL Token, 115 B curve, no creator (legacy115) | 3 | 4 | 5 | refused (`CallDepth`) |
| `liquidate` | SPL Token, 115 B curve, no creator (legacy115) | 3 | 4 | 5 | refused (`CallDepth`) |
| `expire` | SPL Token, 115 B curve, no creator (legacy115) | 3 | 4 | 5 | refused (`CallDepth`) |
| `create_market` | Auto, SPL Token, 115 B curve, no creator (legacy115): `PumpCurve`, init set | 3 | 4 | 5 | refused (`CallDepth`) |
| `sweep_trader` | SPL Token, 115 B curve, no creator (legacy115): token leg | 2 | 3 | 4 | 5 |
| `open_short` | Token-2022, 115 B curve (t22_115): first open, normalising | 3 | 4 | 5 | refused (`CallDepth`) |
| `close_short` | Token-2022, 115 B curve (t22_115) | 3 | 4 | 5 | refused (`CallDepth`) |
| `liquidate` | Token-2022, 115 B curve (t22_115) | 3 | 4 | 5 | refused (`CallDepth`) |
| `expire` | Token-2022, 115 B curve (t22_115) | 3 | 4 | 5 | refused (`CallDepth`) |
| `create_market` | Auto, Token-2022, 115 B curve (t22_115): `PumpCurve`, init set | 3 | 4 | 5 | refused (`CallDepth`) |
| `sweep_trader` | Token-2022, 115 B curve (t22_115): token leg | 2 | 3 | 4 | 5 |
| `open_short` | Token-2022, 151 B curve near completion (near): the costliest open (normalising, growth, the last buyback recipient) | 3 | 4 | 5 | refused (`CallDepth`) |
| `close_short` | Token-2022, 151 B curve near completion (near): completing buyback (`→ PumpMigrating`) | 3 | 4 | 5 | refused (`CallDepth`) |
| `close_short` | Token-2022, 151 B curve near completion (near): synthetic past rT (`buy_v3(q)`, `→ PumpMigrating`) | 3 | 4 | 5 | refused (`CallDepth`) |
| `liquidate` | ECON's D@80 %: synthetic past rT, worst ring | 3 | 4 | 5 | refused (`CallDepth`) |
| `expire` | Token-2022, 151 B curve near completion (near): synthetic past rT, worst ring | 3 | 4 | 5 | refused (`CallDepth`) |
| `sol_settle` | `PumpMigrating`: S3 at 70 min | 2 | 3 | 4 | 5 |
| `sol_settle` | `PumpMigrating` after a synthetic completion (`pc ≠ 0`): S3 at 70 min | 2 | 3 | 4 | 5 |
| `sol_settle` | `None` (graduated elsewhere): S3 | 2 | 3 | 4 | 5 |
| `crank_price` | Token-2022, 151 B curve near completion (near): `PumpCurve → PumpMigrating` | 2 | 3 | 4 | 5 |
| `crank_price` | Token-2022, 151 B curve near completion (near): `PumpCurve → PumpMigrating` after a synthetic completion (`pc ≠ 0`) | 2 | 3 | 4 | 5 |
| `crank_price` | Token-2022, 115 B curve (t22_115): `None → PumpCurve` re-detection | 2 | 3 | 4 | 5 |
| `flag_venue_upgraded` | pump, the dumped ProgramData | 2 | 3 | 4 | 5 |
