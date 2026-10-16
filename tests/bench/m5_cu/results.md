# M5 CU bench (pump curve)

Compute units of every curve-path instruction (ARCH §9 M5: T-CU) of the **release** short.win (`anchor build --tools-version v1.57 --arch v3`) against the **dumped mainnet** pump, pump-fees and PumpSwap programs and coins (`tests/fixtures/`, group `pump_state`), measured with Mollusk 0.15.1 on account states built by the LiteSVM harness on the pump fork, with the same short.win, pump, pump-fees, SPL Token (p-token), Token-2022 and ATA ELFs; LiteSVM 0.16.0 (mainnet feature set) runs the same instruction behind a 1.4 M `SetComputeUnitLimit` as a cross-check (its own `consumed` line) and gives its instruction-trace entries, its peak stack height and pump's own CU (its outermost frames: its pump-fees, token, System and event CPIs included). Budgets are ARCH §6.1's PROPOSED transaction limits; `results.md` names the source of each. Written by `M5_CU_WRITE=1 cargo test --manifest-path tests/bench/m5_cu/Cargo.toml --test m5_cu` (README.md); the test fails when a Mollusk figure exceeds its budget, the two harnesses disagree, or a case does not execute.

| Instruction | Case | Mollusk CU | LiteSVM CU | pump CU | Budget | Budget source | Trace | ARCH §6.2 (curve) | Peak height |
|---|---|---|---|---|---|---|---|---|---|
| `create_market` | Auto, SPL Token, 115 B curve, no creator (legacy115): `PumpCurve`, the trader's base ATA and pump UVA created | 141493 | 141493 | 17046 | 250000 | §6.1 `create_market` | 15 | — | 3 |
| `create_market` | Auto, Token-2022, 115 B curve (t22_115): `PumpCurve`, the trader's base ATA and pump UVA created | 169916 | 169916 | 15546 | 250000 | §6.1 `create_market` | 15 | — | 3 |
| `create_market` | Auto, Token-2022, 125 B curve (fresh): `PumpCurve`, the trader's base ATA and pump UVA created | 143229 | 143229 | 15546 | 250000 | §6.1 `create_market` | 15 | — | 3 |
| `create_market` | Auto, Token-2022, 151 B curve near completion (near): `PumpCurve`, the trader's base ATA and pump UVA created | 170135 | 170135 | 17046 | 250000 | §6.1 `create_market` | 15 | — | 3 |
| `create_market` | Auto, Token-2022, 151 B curve near completion (near), completed by a third party: `PumpMigrating`, with the init set | 170329 | 170329 | 17046 | 250000 | §6.1 `create_market` | 15 | — | 3 |
| `create_market` | Auto, PumpSwap-class coin (graduated): `PumpSwap` on its canonical pool, the pAMM init set (M6) | 139206 | 139206 | — | 250000 | §6.1 `create_market` | 15 | — | 3 |
| `create_market` | Auto, graduated elsewhere (Fartcoin): list-only `None` (detection set only) | 58075 | 58075 | — | 250000 | §6.1 `create_market` | 7 | — | 2 |
| `open_short` | SPL Token, 115 B curve, no creator (legacy115): first open of a wallet (position + exposure created) | 139107 | 139107 | 46732 | 250000 | §6.1 `open_short` curve | 13 | 12 (≤ 15) | 3 |
| `open_short` | Token-2022, 115 B curve (t22_115): first open of a wallet (position + exposure created) | 145323 | 145323 | 48342 | 250000 | §6.1 `open_short` curve | 13 | 12 (≤ 15) | 3 |
| `open_short` | Token-2022, 125 B curve (fresh): first open of a wallet (position + exposure created) | 144551 | 144551 | 42475 | 250000 | §6.1 `open_short` curve | 13 | 12 (≤ 15) | 3 |
| `open_short` | Token-2022, 151 B curve near completion (near): first open of a wallet (position + exposure created) | 145925 | 145925 | 48919 | 250000 | §6.1 `open_short` curve | 14 | 12 (≤ 15) | 3 |
| `open_short` | SPL Token, 115 B curve, no creator (legacy115): second position of a wallet | 141827 | 141827 | 46865 | 250000 | §6.1 `open_short` curve | 12 | 12 (≤ 15) | 3 |
| `open_short` | SPL Token, 115 B curve, no creator (legacy115): first open, normalising (lamports to the trader, tokens to trader_base_ata) | 145444 | 145444 | 46732 | 250000 | §6.1 `open_short` curve | 16 | 12 (≤ 15) | 3 |
| `open_short` | Token-2022, 115 B curve (t22_115): second position of a wallet | 144683 | 144683 | 48475 | 250000 | §6.1 `open_short` curve | 12 | 12 (≤ 15) | 3 |
| `open_short` | Token-2022, 115 B curve (t22_115): first open, normalising (lamports to the trader, tokens to trader_base_ata) | 153996 | 153996 | 48342 | 250000 | §6.1 `open_short` curve | 16 | 12 (≤ 15) | 3 |
| `open_short` | Token-2022, 151 B curve near completion (near): costliest — first open, normalising, after a creator change into an empty vault, the last buyback recipient | 154567 | 154567 | 49101 | 250000 | §6.1 `open_short` curve | 17 | 12 (≤ 15) | 3 |
| `close_short` | SPL Token, 115 B curve, no creator (legacy115) | 109274 | 109274 | 49097 | 250000 | §6.1 `close_short` curve / PumpSwap | 9 | 12 (≤ 15) | 3 |
| `close_short` | Token-2022, 115 B curve (t22_115) | 113946 | 113946 | 50707 | 250000 | §6.1 `close_short` curve / PumpSwap | 9 | 12 (≤ 15) | 3 |
| `close_short` | Token-2022, 125 B curve (fresh) | 108176 | 108176 | 44707 | 250000 | §6.1 `close_short` curve / PumpSwap | 9 | 12 (≤ 15) | 3 |
| `close_short` | Token-2022, 151 B curve near completion (near) | 112734 | 112734 | 49206 | 250000 | §6.1 `close_short` curve / PumpSwap | 9 | 12 (≤ 15) | 3 |
| `close_short` | SPL Token, 115 B curve, no creator (legacy115): normalising | 116048 | 116048 | 49097 | 250000 | §6.1 `close_short` curve / PumpSwap | 12 | 12 (≤ 15) | 3 |
| `close_short` | Token-2022, 115 B curve (t22_115): normalising | 123056 | 123056 | 50707 | 250000 | §6.1 `close_short` curve / PumpSwap | 12 | 12 (≤ 15) | 3 |
| `close_short` | Token-2022, 115 B curve (t22_115): costliest — normalising, after a creator change into an empty vault, the last buyback recipient | 123252 | 123252 | 50889 | 250000 | §6.1 `close_short` curve / PumpSwap | 12 | 12 (≤ 15) | 3 |
| `close_short` | Token-2022, 151 B curve near completion (near): costliest — normalising, after a creator change into an empty vault, the last buyback recipient | 122040 | 122040 | 49388 | 250000 | §6.1 `close_short` curve / PumpSwap | 12 | 12 (≤ 15) | 3 |
| `close_short` | Token-2022, 151 B curve near completion (near): completing buyback (q = rT; `PumpCurve → PumpMigrating` in the same instruction) | 125458 | 125458 | 54646 | 250000 | §6.1 `close_short` curve / PumpSwap | 11 | 12 (≤ 15) | 3 |
| `close_short` | Token-2022, 151 B curve near completion (near): synthetic past rT (the crowd left rT = q/2: `buy_v3(q)` takes rT on the curve and q − rT from the would-be pool; `PumpCurve → PumpMigrating`) | 135832 | 135832 | 63422 | 250000 | §6.1 `close_short` curve / PumpSwap | 12 | 12 (≤ 15) | 3 |
| `close_short` | Token-2022, 151 B curve near completion (near): synthetic past rT (the crowd left rT = q/2: `buy_v3(q)` takes rT on the curve and q − rT from the would-be pool; `PumpCurve → PumpMigrating`), worst ring (every ring entry past rT) | 135830 | 135830 | 63422 | 250000 | §6.1 `close_short` curve / PumpSwap | 12 | 12 (≤ 15) | 3 |
| `liquidate` | SPL Token, 115 B curve, no creator (legacy115): after a real pump (40 % of vT bought), both legs | 130564 | 130564 | 49095 | 300000 | §6.1 `liquidate` / `expire` curve | 9 | 12 (≤ 15) | 3 |
| `liquidate` | Token-2022, 115 B curve (t22_115): after a real pump (40 % of vT bought), both legs | 134901 | 134901 | 50706 | 300000 | §6.1 `liquidate` / `expire` curve | 9 | 12 (≤ 15) | 3 |
| `liquidate` | Token-2022, 125 B curve (fresh): after a real pump (40 % of vT bought), both legs | 129945 | 129945 | 44707 | 300000 | §6.1 `liquidate` / `expire` curve | 9 | 12 (≤ 15) | 3 |
| `liquidate` | Token-2022, 115 B curve (t22_115): costliest — normalising, creator change into an empty vault, last recipients | 144093 | 144093 | 50888 | 300000 | §6.1 `liquidate` / `expire` curve | 12 | 12 (≤ 15) | 3 |
| `liquidate` | ECON's D@80 % (fresh, Token-2022): synthetic past rT (the crowd left rT = q/2, the price × 2.4), both legs | 151518 | 151518 | 58921 | 300000 | §6.1 `liquidate` / `expire` curve | 12 | 12 (≤ 15) | 3 |
| `liquidate` | ECON's D@80 % (fresh, Token-2022): synthetic past rT (the crowd left rT = q/2, the price × 2.4), both legs, worst ring (every ring entry past rT) | 159242 | 159242 | 58921 | 300000 | §6.1 `liquidate` / `expire` curve | 12 | 12 (≤ 15) | 3 |
| `expire` | SPL Token, 115 B curve, no creator (legacy115): past the deadline, healthy | 126932 | 126932 | 49097 | 300000 | §6.1 `liquidate` / `expire` curve | 9 | 12 (≤ 15) | 3 |
| `expire` | Token-2022, 115 B curve (t22_115): past the deadline, healthy | 131620 | 131620 | 50707 | 300000 | §6.1 `liquidate` / `expire` curve | 9 | 12 (≤ 15) | 3 |
| `expire` | Token-2022, 125 B curve (fresh): past the deadline, healthy | 126319 | 126319 | 44707 | 300000 | §6.1 `liquidate` / `expire` curve | 9 | 12 (≤ 15) | 3 |
| `expire` | Token-2022, 151 B curve near completion (near): past the deadline, healthy | 130487 | 130487 | 49206 | 300000 | §6.1 `liquidate` / `expire` curve | 9 | 12 (≤ 15) | 3 |
| `expire` | Token-2022, 151 B curve near completion (near): costliest — normalising, creator change into an empty vault, last recipients | 139679 | 139679 | 49388 | 300000 | §6.1 `liquidate` / `expire` curve | 12 | 12 (≤ 15) | 3 |
| `expire` | Token-2022, 151 B curve near completion (near): completing buyback (q = rT; `PumpCurve → PumpMigrating`) | 139845 | 139845 | 54646 | 300000 | §6.1 `liquidate` / `expire` curve | 11 | 12 (≤ 15) | 3 |
| `expire` | Token-2022, 151 B curve near completion (near): synthetic past rT (the crowd left rT = q/2 after the deadline; `PumpCurve → PumpMigrating`) | 155067 | 155067 | 63422 | 300000 | §6.1 `liquidate` / `expire` curve | 12 | 12 (≤ 15) | 3 |
| `expire` | Token-2022, 151 B curve near completion (near): synthetic past rT (the crowd left rT = q/2 after the deadline; `PumpCurve → PumpMigrating`), worst ring (every ring entry past rT) | 156628 | 156628 | 63422 | 300000 | §6.1 `liquidate` / `expire` curve | 12 | 12 (≤ 15) | 3 |
| `sol_settle` | SPL Token, 115 B curve, no creator (legacy115): healthy, the no-change call | 43512 | 43512 | — | 150000 | §6.1 `sol_settle` | 1 | 2 (≤ 5) | 1 |
| `sol_settle` | Token-2022, 115 B curve (t22_115): healthy, the no-change call | 43907 | 43907 | — | 150000 | §6.1 `sol_settle` | 1 | 2 (≤ 5) | 1 |
| `sol_settle` | `PumpMigrating` (stuck migration): S3 at migration_seen_at + 70 min, FV on the post-migration pool | 64773 | 64773 | — | 150000 | §6.1 `sol_settle` | 2 | 2 (≤ 5) | 2 |
| `sol_settle` | `PumpMigrating` after a third party's synthetic completion (`pc ≠ 0`): S3 at migration_seen_at + 70 min, FV ρ-scaled on the pool migrate_v2 will create | 69295 | 69295 | — | 150000 | §6.1 `sol_settle` | 2 | 2 (≤ 5) | 2 |
| `sol_settle` | `None` (graduated elsewhere; the detection set as ReadSet): S3 after s3_wait_s (M2-SEC-5) | 44882 | 44882 | — | 150000 | §6.1 `sol_settle` | 2 | 2 (≤ 5) | 2 |
| `crank_price` | SPL Token, 115 B curve, no creator (legacy115): roll + record (first crank of a second) | 32389 | 32389 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | Token-2022, 115 B curve (t22_115): roll + record (first crank of a second) | 32422 | 32422 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | Token-2022, 125 B curve (fresh): roll + record (first crank of a second) | 32652 | 32652 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | Token-2022, 151 B curve near completion (near): roll + record (first crank of a second) | 32650 | 32650 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | Token-2022, 115 B curve (t22_115): record only (same second again) | 27767 | 27767 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | Token-2022, 151 B curve near completion (near): `PumpCurve → PumpMigrating` (completed by a third party), chains start | 37041 | 37041 | — | 60000 | §6.1 `crank_price` | 4 | 2 (≤ 2) | 2 |
| `crank_price` | Token-2022, 151 B curve near completion (near): `PumpCurve → PumpMigrating` after a third party's synthetic completion (`buy_v3` past rT, `pc ≠ 0`), chains start | 37053 | 37053 | — | 60000 | §6.1 `crank_price` | 4 | 2 (≤ 2) | 2 |
| `crank_price` | Token-2022, 151 B curve near completion (near): `PumpMigrating` after short.win's own synthetic close (`pc ≠ 0`), roll + record | 35213 | 35213 | — | 60000 | §6.1 `crank_price` | 3 | 2 (≤ 2) | 2 |
| `crank_price` | `PumpMigrating → None` (migrated elsewhere; `graduated_elsewhere`) | 26378 | 26378 | — | 60000 | §6.1 `crank_price` | 3 | 2 (≤ 2) | 2 |
| `crank_price` | `None` (graduated elsewhere), the detection set, the S3 chain running | 16759 | 16759 | — | 60000 | §6.1 `crank_price` | 2 | 2 (≤ 2) | 2 |
| `crank_price` | Token-2022, 115 B curve (t22_115): `None → PumpCurve` re-detection (curve cache, price start, Tier D, chains and fp_* cleared) | 50726 | 50726 | — | 60000 | §6.1 `crank_price` | 4 | 2 (≤ 2) | 2 |
| `sweep_trader` | SPL Token, 115 B curve, no creator (legacy115): token leg — lamports on the trader, tokens on trader_base_ata | 21974 | 21974 | — | 60000 | no §6.1 row (§3.9); §6.1 `claim_sol` / `cover_deficit` / `sync_lender` class | 4 | — | 2 |
| `sweep_trader` | Token-2022, 115 B curve (t22_115): token leg — lamports on the trader, tokens on trader_base_ata | 24513 | 24513 | — | 60000 | no §6.1 row (§3.9); §6.1 `claim_sol` / `cover_deficit` / `sync_lender` class | 4 | — | 2 |
| `sweep_trader` | Token-2022, 115 B curve (t22_115): nothing to sweep | 18555 | 18555 | — | 60000 | no §6.1 row (§3.9); §6.1 `claim_sol` / `cover_deficit` / `sync_lender` class | 2 | — | 2 |
| `flag_venue_upgraded` | pump, the dumped 10,485,760-byte ProgramData | 6036 | 6036 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
| `flag_venue_upgraded` | pump-fees, the dumped 948,989-byte ProgramData | 6046 | 6046 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
| `flag_venue_upgraded` | PumpSwap, the dumped 10,485,760-byte ProgramData | 6036 | 6036 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
| `revalidate_venue` | pump, the dumped ProgramData | 8506 | 8506 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
| `revalidate_venue` | pump-fees, the dumped ProgramData | 8521 | 8521 | — | 400000 | §6.1 `flag_venue_upgraded` / `revalidate_venue` (U5) | 2 | 2 (≤ 2) | 2 |
