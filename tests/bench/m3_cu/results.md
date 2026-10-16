# M3 CU bench (Raydium)

Compute units of every Raydium-path instruction (ARCH §9 M3: T-RAY-CU, T-CU; M4: `propose_pool` and `execute_pool_switch`) of the **release** short.win (`anchor build --tools-version v1.57 --arch v3`) against the **dumped mainnet** Raydium AMM v4 and CPMM programs and pools (`tests/fixtures/`), measured with Mollusk 0.15.1 on account states built by the LiteSVM harness on the mainnet fork, with the same short.win, Raydium, SPL Token (p-token) and Token-2022 ELFs; LiteSVM 0.16.0 (mainnet feature set) runs the same instruction as a cross-check and gives its instruction-trace entries, its peak stack height and the Raydium program's own CU (its token CPIs included). AMM v4 is SPL Token only, so the Token-2022 rows are CPMM's. Written by `M3_CU_WRITE=1 cargo test --manifest-path tests/bench/m3_cu/Cargo.toml --test m3_cu` (README.md); the test fails when a Mollusk figure exceeds its budget, the two harnesses disagree, or a case does not execute.

| Instruction | Case | Mollusk CU | LiteSVM CU | Venue CPI CU | Budget | Budget source | Trace | ARCH §6.2 (Raydium) | Peak height |
|---|---|---|---|---|---|---|---|---|---|
| `create_market` | AMM v4, SPL (Fartcoin) | 67892 | 67892 | — | 250000 | §6.1 `create_market` | 7 | — | 2 |
| `create_market` | CPMM, SPL (USELESS) | 65652 | 65652 | — | 250000 | §6.1 `create_market` | 7 | — | 2 |
| `create_market` | CPMM, Token-2022 base | 76913 | 76913 | — | 250000 | §6.1 `create_market` | 7 | — | 2 |
| `create_market` | CPMM, creator fee on the SOL side | 73162 | 73162 | — | 250000 | §6.1 `create_market` | 7 | — | 2 |
| `create_market` | CPMM, WSOL as token 1 | 69734 | 69734 | — | 250000 | §6.1 `create_market` | 7 | — | 2 |
| `open_short` | AMM v4, SPL (Fartcoin): first open of a wallet (position + exposure created) | 101357 | 101357 | 15420 | 150000 | §6.1 `open_short` RaydiumV4 / CPMM | 15 | 13 (≤ 15) | 3 |
| `open_short` | CPMM, SPL (USELESS): first open of a wallet (position + exposure created) | 117109 | 117109 | 25091 | 150000 | §6.1 `open_short` RaydiumV4 / CPMM | 15 | 13 (≤ 15) | 3 |
| `open_short` | CPMM, Token-2022 base: first open of a wallet (position + exposure created) | 125362 | 125362 | 28041 | 150000 | §6.1 `open_short` RaydiumV4 / CPMM | 15 | 13 (≤ 15) | 3 |
| `open_short` | CPMM, creator fee on the SOL side: first open of a wallet (position + exposure created) | 116865 | 116865 | 25110 | 150000 | §6.1 `open_short` RaydiumV4 / CPMM | 15 | 13 (≤ 15) | 3 |
| `open_short` | CPMM, WSOL as token 1: first open of a wallet (position + exposure created) | 119402 | 119402 | 25093 | 150000 | §6.1 `open_short` RaydiumV4 / CPMM | 15 | 13 (≤ 15) | 3 |
| `open_short` | AMM v4, SPL (Fartcoin): second position of a wallet | 99277 | 99277 | 15420 | 150000 | §6.1 `open_short` RaydiumV4 / CPMM | 14 | 13 (≤ 15) | 3 |
| `open_short` | AMM v4, SPL (Fartcoin): first open, normalising (trader and trader_wsol donated to) | 107325 | 107325 | 15420 | 150000 | §6.1 `open_short` RaydiumV4 / CPMM | 18 | 13 (≤ 15) | 3 |
| `open_short` | CPMM, Token-2022 base: second position of a wallet | 120013 | 120013 | 28035 | 150000 | §6.1 `open_short` RaydiumV4 / CPMM | 14 | 13 (≤ 15) | 3 |
| `open_short` | CPMM, Token-2022 base: first open, normalising (trader and trader_wsol donated to) | 131330 | 131330 | 28041 | 150000 | §6.1 `open_short` RaydiumV4 / CPMM | 18 | 13 (≤ 15) | 3 |
| `close_short` | AMM v4, SPL (Fartcoin) | 68381 | 68381 | 15900 | 200000 | no Raydium `close_short` row; §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `close_short` | CPMM, SPL (USELESS) | 79810 | 79810 | 25183 | 200000 | no Raydium `close_short` row; §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `close_short` | CPMM, Token-2022 base | 83379 | 83379 | 28094 | 200000 | no Raydium `close_short` row; §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `close_short` | CPMM, creator fee on the SOL side | 79861 | 79861 | 25184 | 200000 | no Raydium `close_short` row; §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `close_short` | CPMM, WSOL as token 1 | 79691 | 79691 | 25187 | 200000 | no Raydium `close_short` row; §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `close_short` | AMM v4, SPL (Fartcoin): normalising | 74786 | 74786 | 15900 | 200000 | no Raydium `close_short` row; §6.1 `liquidate` / `expire` Raydium | 12 | 9 (≤ 11) | 3 |
| `close_short` | CPMM, Token-2022 base: normalising | 89784 | 89784 | 28094 | 200000 | no Raydium `close_short` row; §6.1 `liquidate` / `expire` Raydium | 12 | 9 (≤ 11) | 3 |
| `liquidate` | AMM v4, SPL (Fartcoin): after a real ≈ 2.2× pump, both legs, within the fixed bound | 91159 | 91159 | 16032 | 200000 | §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `liquidate` | CPMM, SPL (USELESS): after a real ≈ 2.2× pump, both legs, within the fixed bound | 107839 | 107839 | 25236 | 200000 | §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `liquidate` | CPMM, Token-2022 base: after a real ≈ 2.2× pump, both legs, within the fixed bound | 111849 | 111849 | 28161 | 200000 | §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `expire` | AMM v4, SPL (Fartcoin): past the deadline, healthy | 87036 | 87036 | 15900 | 200000 | §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `expire` | CPMM, SPL (USELESS): past the deadline, healthy | 102021 | 102021 | 25189 | 200000 | §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `expire` | CPMM, Token-2022 base: past the deadline, healthy | 105564 | 105564 | 28100 | 200000 | §6.1 `liquidate` / `expire` Raydium | 9 | 9 (≤ 11) | 3 |
| `sol_settle` | AMM v4, SPL (Fartcoin): healthy, the no-change call | 39294 | 39294 | — | 150000 | §6.1 `sol_settle` | 1 | 2 (≤ 5) | 1 |
| `sol_settle` | AMM v4, SPL (Fartcoin): S3 on a halted pool at the deadline, chain run s3_wait_s | 47491 | 47491 | — | 150000 | §6.1 `sol_settle` | 2 | 2 (≤ 5) | 2 |
| `sol_settle` | CPMM, SPL (USELESS): healthy, the no-change call | 41678 | 41678 | — | 150000 | §6.1 `sol_settle` | 1 | 2 (≤ 5) | 1 |
| `sol_settle` | CPMM, SPL (USELESS): S3 on a halted pool at the deadline, chain run s3_wait_s | 52838 | 52838 | — | 150000 | §6.1 `sol_settle` | 2 | 2 (≤ 5) | 2 |
| `sol_settle` | CPMM, Token-2022 base: healthy, the no-change call | 42030 | 42030 | — | 150000 | §6.1 `sol_settle` | 1 | 2 (≤ 5) | 1 |
| `sol_settle` | CPMM, Token-2022 base: S3 on a halted pool at the deadline, chain run s3_wait_s | 53167 | 53167 | — | 150000 | §6.1 `sol_settle` | 2 | 2 (≤ 5) | 2 |
| `crank_price` | AMM v4, SPL (Fartcoin): roll + record (first crank of a second) | 27955 | 27955 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | AMM v4, SPL (Fartcoin): pool halted by its admin: roll, no record, both chains start | 26719 | 26719 | — | 60000 | §6.1 `crank_price` | 3 | 2 (≤ 2) | 2 |
| `crank_price` | CPMM, SPL (USELESS): roll + record (first crank of a second) | 28014 | 28014 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | CPMM, SPL (USELESS): pool halted by its admin: roll, no record, both chains start | 27375 | 27375 | — | 60000 | §6.1 `crank_price` | 3 | 2 (≤ 2) | 2 |
| `crank_price` | CPMM, Token-2022 base: roll + record (first crank of a second) | 28026 | 28026 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | CPMM, Token-2022 base: pool halted by its admin: roll, no record, both chains start | 27382 | 27382 | — | 60000 | §6.1 `crank_price` | 3 | 2 (≤ 2) | 2 |
| `crank_price` | AMM v4, SPL (Fartcoin): record only (same second again) | 23075 | 23075 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `flag_venue_upgraded` | AMM v4, the dumped 1,406,429-byte ProgramData | 6046 | 6046 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
| `flag_venue_upgraded` | CPMM, the dumped 793,869-byte ProgramData | 6046 | 6046 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
| `flag_venue_upgraded` | AMM v4, a second upgrade while flagged (refresh) | 6028 | 6028 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
| `flag_venue_upgraded` | a 10,485,760-byte ProgramData (pump's size, synthetic): U5 | 6036 | 6036 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
| `revalidate_venue` | AMM v4, the dumped ProgramData | 8521 | 8521 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
| `revalidate_venue` | CPMM, the dumped ProgramData | 8521 | 8521 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | AMM v4, SPL (Fartcoin): list-only market adopts it (Fartcoin's real curve: D1 detection) | 27042 | 27042 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | CPMM, SPL (USELESS): list-only market adopts it | 27250 | 27250 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | AMM v4: listing-window replacement of the thin reversed Fartcoin pool by the main pool, ring rewritten | 45399 | 45399 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | AMM v4, SPL (Fartcoin): zero-position adoption of a planted CPMM pool 2× as deep, ring rewritten | 42155 | 42155 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | CPMM, SPL (USELESS): zero-position adoption of a planted CPMM pool 2× as deep, ring rewritten | 42482 | 42482 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | CPMM, Token-2022 base: zero-position adoption of a planted CPMM pool 2× as deep, ring rewritten | 42528 | 42528 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | AMM v4, SPL (Fartcoin): halted incumbent, a position open: fast-path window opened (CPMM candidate at 40 % of D) | 39339 | 39339 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | AMM v4, SPL (Fartcoin): fast-path window improved (60 % replaces 40 %) | 38956 | 38956 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | CPMM, SPL (USELESS): halted incumbent, a position open: fast-path window opened (CPMM candidate at 40 % of D) | 40041 | 40041 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | CPMM, SPL (USELESS): fast-path window improved (60 % replaces 40 %) | 39829 | 39829 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | CPMM, Token-2022 base: halted incumbent, a position open: fast-path window opened (CPMM candidate at 40 % of D) | 39681 | 39681 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `propose_pool` | CPMM, Token-2022 base: fast-path window improved (60 % replaces 40 %) | 39932 | 39932 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `execute_pool_switch` | AMM v4, SPL (Fartcoin): fast adoption after the window, a position open, ring rewritten | 47688 | 47688 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `execute_pool_switch` | AMM v4, SPL (Fartcoin): window cancelled (the incumbent resumed) | 29864 | 29864 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `execute_pool_switch` | CPMM, SPL (USELESS): fast adoption after the window, a position open, ring rewritten | 48567 | 48567 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `execute_pool_switch` | CPMM, SPL (USELESS): window cancelled (the incumbent resumed) | 29739 | 29739 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `execute_pool_switch` | CPMM, Token-2022 base: fast adoption after the window, a position open, ring rewritten | 48648 | 48648 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `execute_pool_switch` | CPMM, Token-2022 base: window cancelled (the incumbent resumed) | 29724 | 29724 | — | 100000 | §6.1 `propose_pool` / `execute_pool_switch` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | AMM v4, SPL (Fartcoin): the last post-change warm-up integration (seed over 8 entries, guard cleared; M4R) | 27952 | 27952 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | CPMM, SPL (USELESS): the last post-change warm-up integration (seed over 8 entries, guard cleared; M4R) | 27952 | 27952 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | CPMM, Token-2022 base: the last post-change warm-up integration (seed over 8 entries, guard cleared; M4R) | 27961 | 27961 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
