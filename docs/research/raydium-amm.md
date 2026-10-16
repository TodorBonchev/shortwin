# Raydium AMM v4 and Raydium CPMM as short.win MVP venues

*Research note for short.win (spec baseline: `docs/SPEC.md` v0.3; owner decision D2, "Raydium AMM v4 and Raydium CPMM are in the MVP as full venues"). Researched 2026-09-26/27, ≈ 23:50–01:15 UTC. Mainnet state and transactions were sampled at slots ≈ 450,804,000 – 450,842,300, i.e. 2026-09-26 22:06Z to 2026-09-27 00:57Z.*

**Scope.** Two constant-product programs: Raydium **AMM v4** (`675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8`, a native program) and Raydium **CPMM** (`CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`, an Anchor program). short.win's per-market trader PDA has to (a) sell borrowed tokens for SOL at open and (b) buy back exactly `Q` tokens at close, liquidation and expiry. The program must also price both trades on-chain, bit-exactly. This note covers what that requires, and ends with design implications.

**How claims are sourced.** Each claim carries a citation in brackets. Anything I could not confirm is marked **UNVERIFIED**.

| Key | Source |
|---|---|
| **AMM-SRC** | `github.com/raydium-io/raydium-amm` @ `d26944bfb76fb5fa8f91e5d440c2050ed358ef81` (HEAD of `master`, committed 2026-09-10 16:39:54Z), files under `program/src/` |
| **CPMM-SRC** | `github.com/raydium-io/raydium-cp-swap` @ `59fb845a9e5bb569c8b2f3415f13b0c0ebcc6b92` (HEAD of `master`, 2026-09-10 06:36:22Z), files under `programs/cp-swap/src/` |
| **CPMM-IDL** | The on-chain IDL in the Program Metadata Program (PMP, `ProgM6JCCvbYkfKqJYHePx4xxSUSqJp7rh8Lyv7nk7S`): canonical `"idl"` account `FHuJXL7cEttfHk4sXdTAXVaRymBdKfVs7ZH6cMgA1ihk`, zlib-compressed JSON. Also `raydium-io/raydium-idl` @ `e7e0c96fe77bcf6a020b84a44c47a722aac8e359` `raydium_cpmm/raydium_cp_swap.json`, which is stale (see §1.3) |
| **RPC** | Live mainnet JSON-RPC (`api.mainnet-beta.solana.com`, fallback `solana-rpc.publicnode.com`, `solana-core 4.3.0`): `getAccountInfo` / `getMultipleAccounts` (base64), `getProgramAccounts`, `getSignaturesForAddress`, `getTransaction` (`maxSupportedTransactionVersion: 1`) |
| **SIM** | My own `simulateTransaction` runs (`sigVerify: false`, `replaceRecentBlockhash: true`, `processed`) against live state. I built the transactions by hand; none were broadcast. Each SIM result is labelled |
| **ELF** | `solana program dump -um` of both programs (Agave CLI 4.2.2), then sha256 over the dump with trailing zeros trimmed |
| **OSEC** | OtterSec verification API `verify.osec.io/status/<program>` and `/status-all/<program>`, queried 2026-09-27 |
| **TOKEN** | `github.com/solana-program/token` @ `8185db13640f0df038266c7cf4306c212d81380a` (p-token under `pinocchio/program/src/`) |
| **AGAVE** | `github.com/anza-xyz/agave` @ `f77165963c709934e8ea8564187c00f081b597d1` |
| **CRIT** | The first-pass review findings (`critics.md`, IDs C0-x / C1-x). Cited only for facts that critic verified and that I did not re-check |

Mainnet transactions used as evidence. Short names are used below.

| Name | Signature | What it is |
|---|---|---|
| **TX-V4-OUT2** | `27EekaMRjqX2ya7owoAwXWUtorEPrnNC6pe5cHWfk1NrA9imsfMBkkrTyvFjjQo8Kc6z2wDoSea9gocoqBsNxsUw` | AMM v4 `swap_base_out_v2` (tag 17), exact-out **buy of POPCAT with SOL**. CPI from Jupiter at stack height 2; the owner is Jupiter PDA `["authority",3]`. Slot 450,827,576 |
| **TX-V4-OUT2-TOP** | `4PvobD1K6qEFSHKCMYpKnT5hwSj2wvNyeLozBk1WkDAX8ieJTi8uoPXGAJydZS8UqGknkFWSqKWRyudqx8zzTnUM` | Top-level `swap_base_out_v2` on the GOAT pool. Slot 450,804,968 |
| **TX-V4-IN2** | `4s6yJp8B25bvpjq1rWB488sRMKnnXECJPBNrQNyZiuTavshd9YEaJUTYi1ayQEjCGPGdqJE2eE8WnMAeZ5sdnSkw` | Top-level `swap_base_in_v2` (tag 16): **sell Fartcoin for SOL**. Slot 450,820,264 |
| **TX-V4-OUT1** | `5Y8sTSnTGVovPNpAPd6z87wnH4LKqVR71P9CpySQj6cJt8cj7jqUhGLrE3he5JqARZktYmi9dAStPKr9gghyVPuD` | Legacy `swap_base_out` (tag 11, 17 accounts) via CPI from `3URuDiCXFYJ95YVeDW8MP3b7CKTnuqArY5GTZA2JnPWQ`. The owner is PDA `A9PzVmoB…`. All eight OpenBook slots contain **the pool address itself**. Slot 450,808,427 |
| **TX-V4-IN1** | `5mXDiWRqyPVkmFu1pdxBRq4jYt8hbSY7aJo7mMUQUcjQ1dUtmhUA6FsEsqAZtznA6iv1W5Fd4QY8KLhv3KxVy1oz` | Legacy top-level `swap_base_in` (tag 9) that still passes the real OpenBook accounts. Slot 450,839,009 |
| **TX-V4-PNL** | `2GxiBzcMMCNiizTzTQRPuMK4D9UdVBwSByQ5v9ssXW6EaKmZgqEcqSJkdy7ysdC5x6rnvHHijWR94Y1CuDMN8RyL` | AMM v4 `WithdrawPnl` (tag 7) on the POPCAT pool by `pnl_owner` `PNLCQcVC…`. Slot 450,837,904 |
| **TX-CP-OUT** | `4dijJfULkDojL6TgxUfkeKehrJh6saUUN1ARLYmqmVw73gwzUinYGw77rXokYLEWz2quyoyhkJb3X9HrsBdruGMv` | CPMM `swap_base_output`, exact-out **buy of USELESS with SOL**, via CPI from program `3s1rAymURnacreXreMy718GfqW6kygQsLNka1xDyW8pC`. Payer is its program-owned PDA `HWGoJ1Ha…`. Transaction format v1. Slot 450,830,346 |
| **TX-CP-OUT-TOP** | `dR9wZQZwZWvULjDkAGPrY4dis5yPiFp1NqwnbpWm9t3PPFDvhXZDfHPmEsZkCCAgcf6mMctSzf4DmgKaNBh9fuh` | Top-level CPMM `swap_base_output`. Slot 450,834,516 |
| **TX-CP-IN-TOP** | `3Pp4LzfNxGgWJjQ6Br3mcWXiMmrAZ8VLYQfPBAfbGmweuLNZ3Bvhcrju7Q4euXKULPyA8szUg6DeFJTYrVyjbBvv` | Top-level CPMM `swap_base_input` on USELESS. Slot 450,833,822 |
| **TX-CP-CFIN** | `qzC7eoYkArnE2kEmyPCPhRmQT3oG95Po6sQYFxR9Jchju4zC5GJxUPf5PpfDaZAz2PAcohRQ41z3rteoxoU6muS` | CPMM buy on a creator-fee pool, creator fee charged **on the SOL input**. Slot 450,826,100 |
| **TX-CP-CFOUT** | `5vjTcH3hzPbX9RXKhdmSyYkNy7LPu4wKSYAa5f6LpK4ExYAzBEvrA5ccmqEBDT1wo4eUTxmVJM9XxjvZEQ7Ejg1j` | CPMM sell on the same pool, creator fee charged **on the SOL output**. Slot 450,832,100 |
| **TX-UP-V4 / TX-UP-CP** | `3PGo54fAhX437hUSyx3H7Fr3pbZMKPf3TZ2qDNfS66x27Fjj5Qc5StCownmSHjpFaDVmmhEAC7u6FYaKPvAPaz7y` / `5LfBmTxPEhRZrttMwKYr49KxKJpiuYzFocGPk2JqMSkez7bhHSHgqcJV4n6P89qWhfPvYZyVh6uFQL3iQKFtKDoj` | The most recent upgrades. Both are Squads v4 `VaultTransactionExecute` → "Upgraded program …" |

---

## 0. Summary

1. **Both programs are upgradeable by one Squads v4 multisig: 3 of 4 signers, `time_lock = 0`.** The vault is `FytDrVzDybM1TwFQPGb8qaxZR7dBCzNeqT3vtQsceZQK`.
   - Both were last upgraded on **2026-09-10**: AMM v4 at 01:52:56Z (slot 445,763,204), CPMM at 01:54:31Z (slot 445,763,504).
   - Since 2025-01-01 I count 6 AMM v4 upgrades and 10 CPMM upgrades (§1.2).
   - **Neither binary is a verified build.** OSEC reports "not verified" for the current on-chain hashes. The deployed ELFs contain strings that exist only at the current repo HEADs, which ties them to those commits behaviorally, though not byte-for-byte (§1.3).
2. **Pool-level admin powers belong to a second key**, `GThUX1Atko4tqhN2NaiTazWSeFWMuiUvfFnyJyUghFMJ`. It is a Squads **v3** vault (2 of 3, no timelock).
   - It can disable swaps on any pool: AMM v4 `status`; CPMM `status` bit 2.
   - It can raise fees to almost 100%: AMM v4 per pool; CPMM per `AmmConfig`, which moves every pool on that config at once.
   - **No instruction lets any admin move pool principal.** Fee and PnL collection only take amounts already excluded from the swap reserves (§5).
   - Today **no pool of either program is swap-disabled.** Of 705,975 AMM v4 pool accounts: 705,790 have status 6, 170 have status 7, 15 have status 0, and none have status 1–5. All 236,265 CPMM pools have `status == 0` [RPC `getProgramAccounts`].
3. **AMM v4 no longer touches OpenBook.**
   - The 2026-07-22 upgrade (commit `27f461d` "Remove openbook dependency") deleted the orderbook code.
   - Swap reserves are now `vault.amount − need_take_pnl`. The eight market accounts in the legacy 17/18-account swaps are read but **ignored**: TX-V4-OUT1 fills all of them with the pool address.
   - **Swap v2 instructions (tags 16 and 17) take only 8 accounts** and are the majority of the swaps in my mainnet samples (235 of 388). The exact-out `swap_base_out_v2` is used on mainnet, including by Jupiter via CPI with a PDA owner (TX-V4-OUT2).
4. **CPMM swap reserves are `vault.amount − (protocol_fees + fund_fees + creator_fees)` for each token.**
   - The fee counters can be large. The creator-fee pool `FQd8z3…` holds 792.6 SOL in its WSOL vault, but only **547.1 SOL of real reserves**, because 245.5 SOL of creator fees are uncollected. Raw vault balances therefore overstate liquidity.
5. **The swap math is reproduced to the raw unit.**
   - **AMM v4:** 377 exact-in and 11 exact-out mainnet swaps, across ≥ 6 pools and all 4 swap variants (tags 9, 11, 16, 17), plus 3 SIMs.
   - **CPMM:** 280 exact-in and 5 exact-out mainnet swaps, across 38 pools and 7 configs. The sample covers creator fees charged on the input and on the output, and a Token-2022 pool. There are 5 SIMs on top, including exact-out on a creator-fee pool.
   - Formulas are in §3.3 and worked examples in §3.5. Because the venue reads the same accounts in the same instruction, short.win can pass `max_in = quote` exactly. No slippage tolerance is needed.
6. **CPI from a PDA works on both, and Raydium is simpler than pump.**
   - Mainnet swaps have off-curve PDAs as owner or payer that are not transaction signers: Jupiter PDAs, a 9-byte program-owned PDA of program `T1TANpTe…`, and a zero-data program-owned PDA (§4.1).
   - Neither program debits lamports from the user, creates accounts, or charges rent.
   - **Neither checks who owns the destination token account.** Bought tokens can go straight into short.win's lending vault. The sell source can be the lending vault itself, with the trader PDA as a delegate limited to `Q`.
   - Both need an **SPL-Token WSOL account** for the SOL side. Neither pays native SOL.
7. **Cost per swap.**
   - AMM v4 v2: ≈ 15.2–15.5k CU (median 15.4k, n = 93).
   - CPMM: ≈ 24–34k CU (median 25.1k, n = 244), and ≈ 28.1k when one side is Token-2022.
   - Each swap is **3 instruction-trace entries** (the swap plus 2 Token CPIs), against pump's ≈ 8.
   - Stack depth is short.win 1 → venue 2 → Token 3, which leaves 2 spare levels.
8. **Many pools per mint.**
   - Fartcoin has 2 AMM v4 and 7 CPMM SOL pools; one of them holds 99.99% of the SOL.
   - Every target coin's main pool (Fartcoin, WIF, PNUT, POPCAT, GOAT, MOODENG on AMM v4; USELESS on CPMM) has **92–99.8% of its LP permanently burned**. The burned share can be measured on-chain as `1 − lp_mint.supply / internal_lp_counter`.
   - I propose a permissionless reference-pool rule built on this **locked-SOL** metric, with a slow-switch hysteresis (§6).
9. **Pool age.**
   - CPMM `open_time` is clamped to at least `creation_time + 1` since commit `a7330e3` (2024-05-16) and can never be changed afterwards. So `now − max(open_time, T_clamp)` is a valid lower bound on pool age.
   - AMM v4 `pool_open_time` is unusable: it is not clamped, the admin can reset it, and it is `0` on pump's graduation pools (§7).

---

## 1. Programs, authorities, sources, IDLs

### 1.1 Addresses

| Item | AMM v4 | CPMM | Source |
|---|---|---|---|
| Program ID | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` | `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C` | RPC; owner `BPFLoaderUpgradeab1e…`, executable |
| ProgramData | `A7ZG7ByDi8DpzT9Ab7CiXhvgYTJQmaDPJkMDoPitaCQV` (1,406,429 B) | `DMawCQzbgNTmbzaESc7o6pvL1KAeetY8zA7jNpzntHhU` (793,869 B) | RPC; the AMM v4 PDA derivation was re-checked locally |
| Last deploy slot / time | 445,763,204 = **2026-09-10 01:52:56Z** | 445,763,504 = **2026-09-10 01:54:31Z** | ProgramData bytes 4..12; `getBlockTime`; TX-UP-V4 / TX-UP-CP |
| Upgrade authority | `FytDrVzDybM1TwFQPGb8qaxZR7dBCzNeqT3vtQsceZQK` | same | ProgramData bytes 13..45 |
| That authority is | Squads v4 vault #0 of multisig `tr8rgazUrZzgdkfc6Q622nVJHMMzh29trdBE2uBHb4u`: **threshold 3 of 4 members, `time_lock` 0, no config authority** (autonomous), tx index 51. The members all have permission 7 and are listed below the table. The vault PDA `["multisig", ms, "vault", 0]` was re-derived and matches | same | RPC decode of the Squads v4 `Multisig` account; TX-UP-* are `VaultTransactionExecute`. Squads v4 is immutable [CRIT C1 verified-OK list] |
| Pool admin (`admin::ID` / `amm_owner`) | `GThUX1Atko4tqhN2NaiTazWSeFWMuiUvfFnyJyUghFMJ` (hard-coded) | same (hard-coded) | AMM-SRC `processor.rs` `config_feature::amm_owner`; CPMM-SRC `lib.rs` `admin::ID` |
| That admin is | Squads **v3** (`SMPLecH534NA9acpos4G6x7uf3LWbCAwZQE9e8ZekMu`, immutable: ProgramData `Q1xCTDDf…` has no authority, deploy slot 178,977,035). It is authority index 1 of multisig `EXZY7FPccNuEvgHZMCMpww2Fen8oLWBSJzdgCsX3Djwm`: **threshold 2 of 3** (`6KTbnuxC…`, `8aSRiwaj…`, `Cth8ZMqN…`). Squads v3 has no timelock | | RPC decode; the PDA `["squad", ms, 1u32 LE, "authority"]` re-derived to `GThUX1…`; admin transactions such as `31t2UwiT…` run `ExecuteTransaction` |
| Other hard-coded keys | `pnl_owner` in AmmConfig = `PNLCQcVCD26aC7ZWgRyr5ptfaR7bBrWdTFgRWwu2tvF`; `collect_lamports` = `Rayyxz4cjBTQo7XTT1fhVNqFWK21TjiWcrtPgC3czbL`; create-pool fee destination `7YttLkHD…` (0.15 SOL) | `protocol_owner` `ProCXqRcXJjoUd1RNoo28bSizAA6EEqt9wURZYPDc5u`, `fund_owner` `FUNDduJTA7XcckKHKfAoEnnhuSud2JUCUZv6opWEjrBU`, `collect_lamports` `RayGkhY93thaTgCv98sx1pNLgBHhJDxWUeZXp4bjmnp`, create-pool fee receiver `DNXgeM9E…` (0.15 SOL per config) | AMM-SRC / CPMM-SRC `lib.rs`; RPC (AMM v4 AmmConfig `9DCxsMizn3H1hprZ7xWe6LDzeUeZBksYFpBWBtSf1PQX`, 544 B) |
| Vault / LP authority | `5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1` = `create_program_address(["amm authority", 254])`. `initialize2` enforces the canonical nonce. The only other valid bump in 246–255 is 246, and no pool uses it; `getProgramAccounts` also found none at the invalid values 250–253 and 255 | `GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL` = PDA `["vault_and_lp_mint_auth_seed"]` (bump 253) | AMM-SRC `processor.rs` `process_initialize2` (`find_program_address(&[AUTHORITY_AMM])`); RPC `getProgramAccounts` memcmp on offset 8 |

The four members of the Squads v4 multisig are `4aky1VxQ…`, `7W4mr6bH…`, `AZEvpc6R…` and `AaUu9okT…`.

### 1.2 Upgrade history (ProgramData transactions whose logs contain "Upgraded program", 2025-01-01 → 2026-09-27)

| AMM v4 | CPMM |
|---|---|
| 2026-09-10 01:52 (Squads v4) · 2026-07-22 13:29 (v4) · 2026-06-18 14:45 (v4) · 2025-09-22 11:59 (Squads v3) · 2025-08-26 08:30 (v3) · 2025-05-14 08:19 (v3) | 2026-09-10 01:54 (v4) · 2026-08-17 12:04 (v4) · 2026-06-11 16:39 (v4) · 2025-12-29 07:21 (v3) · 2025-10-31 15:59 and 07:27 (v3) · 2025-08-18 08:57 (v3) · 2025-06-26 13:51 and 10:17 (v3) · 2025-05-13 12:26 (v3) |

- Source: `getSignaturesForAddress(ProgramData)`, then `getTransaction` on each [RPC].
- The upgrade authority moved from a Squads v3 to a Squads v4 multisig sometime between 2025-12-29 and 2026-06-11.
- Two older AMM v4 ProgramData transactions (2025-02-22 `4JcNbtKJ…` and 2025-03-11 `3SwSLAZx…`) could not be fetched from either RPC. Whether they were upgrades is **UNVERIFIED**.
- Later non-upgrade writes to the ProgramData accounts (2026-09-17/18/19/20/21) leave the deploy slot unchanged.
- The upgrades line up with repo commits:
  - AMM v4: swap v2 (`3b087ad`, 2025-09-22) → deploy 2025-09-22; "Remove openbook dependency" (`27f461d`, 2026-07-23 12:18Z) → deploy 2026-07-22; "Upgrade version" (`d26944b`) → deploy 2026-09-10.
  - CPMM: "permissionless collect creator fee" (`244e124`, 2026-08-19) → deploy 2026-08-17; "Chore/upgrade anchor" (`59fb845`) → deploy 2026-09-10.
  - Raydium tends to push the commit a few hours to a day after deploying. **There is no public changelog or advance notice.**

### 1.3 Source repo, commit and build verification

| | AMM v4 | CPMM |
|---|---|---|
| Repo / HEAD | `raydium-io/raydium-amm` @ `d26944bfb7` ("Upgrade version (#70)": solana libs 3.0, adds `WithdrawExcessLamports` tag 18) | `raydium-io/raydium-cp-swap` @ `59fb845a9e` ("Chore/upgrade anchor (#77)": anchor-lang `=1.0.2`, adds `collect_excess_lamports`) |
| On-chain hash (ELF, trailing zeros trimmed) | `d4cee0cd4f78f69d05f5d645fc6d120dff830f231162d35cf2b2dae1df7b2c9f` (308,601 B) | `bdaff73e7ed9fc75f091d732987d36476d7378d32c47495fe9eef0a930dbf8cc` (689,009 B) |
| OSEC verified build | `is_verified: false`. The last attempts, dated 2026-06-18, targeted commits `d10a8e9` and `2cda268`, which are older than the current binary | `is_verified: false`. The last attempts, dated 2026-06-11, targeted `cfdb70a`, `609780f` and `34a208a` |
| Behavioral tie to HEAD | The ELF contains `"Lamports calculate error."`, an error added only in `d26944b`, plus the `swap_base_out_v2` log strings. Mainnet logs show `"Program log: Program ID: …"` and `"Number of accounts: N"`, which exactly match the `entrypoint.rs` added in `27f461d` | The ELF contains `CollectExcessLamports` (only in `59fb845`), `CollectCreatorFeePermissionless` (`244e124`) and `CreateSupportMintAssociated` (`78f254e`) |
| security.txt | `source_code: https://github.com/raydium-io/raydium-amm`, auditor MadShield Q2 2023; no `source_revision` | `…/raydium-cp-swap`, MadShield Q1 2024; no `source_revision` |

- **UNVERIFIED:** a byte-exact reproducible build of either HEAD. I did not attempt Docker `solana-verify` builds.
- An unmerged CPMM branch `feat/creator-fee-share` (last commit `bdeee23`, 2026-09-15) contains "Fix: update for verify build". It also adds a creator-fee split into the protocol fee.
  - The split fits inside existing padding (`AmmConfig.creator_fee_share_rate`, sizes unchanged) and does **not** change the swap math or the reserve formula.
  - Expect another CPMM upgrade. (*It came: CPMM was redeployed at slot 452,376,460, 2026-10-01, from that branch; §13.*)

**IDLs.**

- **CPMM (Anchor 1.0.2).**
  - There is no legacy `anchor:idl` account (the derived `3HD1FNEKoNh5aYfvw3VrNWy6WwrEtS6JYx1RmFTE7DMC` does not exist).
  - The IDL lives in the **Program Metadata Program**: account `FHuJXL7c…` (owner PMP, 7,615 B, authority `idLqqjj3SJmeTLobCXVdcb6RoigjfhsFYjMXan755Ej`). It was written 2026-09-11 11:35:55Z in tx `2JtypQL9YGPxWs7KXwrD3rNQUekAnnaoxmr1PRcH8T98KjyhEE8X12e4pjoSsBDcYTfAh8SP1ruiKJCuiegvCNz7`.
  - It decompresses to 82,883 B of JSON: `raydium_cp_swap` 0.2.0, spec 0.1.0, 18 instructions, 16 errors.
  - The `raydium-idl` repo copy has only 14 instructions: it lacks `collect_creator_fee_permissionless`, `collect_excess_lamports` and the two `*_support_mint_associated`. Its `swap_base_input` and `swap_base_output` entries are identical to the on-chain ones [CPMM-IDL].
- **AMM v4 (native)** has no IDL. The legacy `anchor:idl` address `C2Di4S9J…` does not exist, and `raydium-idl` has no AMM v4 file. `instruction.rs`, `state.rs` and `processor.rs` [AMM-SRC] are the reference.

---

## 2. Pool state layouts and reserves

### 2.1 AMM v4 `AmmInfo` (752 bytes, `#[repr(C, packed)]`, **no discriminator**)

- Offsets come from AMM-SRC `state.rs`.
- The value column is the Fartcoin pool `Bzc9NZfMqkXR6fz1DBph7BDf9BroyEf6pnzESP7v5iiw` at slot 450,828,527 [RPC].
- The program's own loader only checks `owner == program && data_len == size_of::<AmmInfo>() == 752 && status != 0` (`load_mut_checked`).

| Field | Off | Type | Fartcoin | Used by swap? |
|---|---|---|---|---|
| `status` | 0 | u64 | **6** (SwapOnly) | yes (permission) |
| `nonce` | 8 | u64 | 254 | yes (authority) |
| `order_num`, `depth` | 16, 24 | u64 | 7, 3 | no (legacy orderbook) |
| `coin_decimals` | 32 | u64 | 9 | no (only for PnL normalisation) |
| `pc_decimals` | 40 | u64 | 6 | no |
| `state`, `reset_flag`, `min_size`, `vol_max_cut_ratio`, `amount_wave`, `coin_lot_size`, `pc_lot_size`, `min/max_price_multiplier` | 48–119 | u64 | 1, 0, 10^7, 500, … | no |
| `sys_decimal_value` | 120 | u64 | 10^9 | PnL only |
| `fees.min_separate_{num,den}` | 128, 136 | u64 | 5 / 10,000 | no |
| `fees.trade_fee_{num,den}` | 144, 152 | u64 | 25 / 10,000 | **no** (legacy) |
| `fees.pnl_{num,den}` | 160, 168 | u64 | **12 / 100** | protocol share of fee growth (§2.3) |
| `fees.swap_fee_{num,den}` | **176, 184** | u64 | **25 / 10,000** | **yes: the swap fee** |
| `state_data.need_take_pnl_coin` | **192** | u64 | 0 | **yes (reserves)** |
| `state_data.need_take_pnl_pc` | **200** | u64 | 0 | **yes (reserves)** |
| `total_pnl_pc`, `total_pnl_coin` | 208, 216 | u64 | stale | deprecated, not updated |
| `state_data.pool_open_time` | **224** | u64 | **0** | only when `status == 7` |
| padding, `orderbook_to_init_time`, `swap_*` statistics | 232–335 | | stale | deprecated, "no longer in use or updated" |
| `coin_vault` | **336** | Pubkey | `F6iWqisguZYprVwp916BgGR7d5ahP6Ev5E213k8y3MEb` | yes |
| `pc_vault` | **368** | Pubkey | `7bxbfwXi1CY7zWUXW35PBMZjhPD27SarVuHaehMzR2Fn` | yes |
| `coin_vault_mint` | **400** | Pubkey | **`So11111111111111111111111111111111111111112`** (WSOL is the *coin* here) | yes (direction) |
| `pc_vault_mint` | **432** | Pubkey | `9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump` | yes |
| `lp_mint` | **464** | Pubkey | `6JDPHdZEgQ5YtercMxNiSvVtVTkvWADvxA2x6xR3XHC8` | no |
| `open_orders`, `market`, `market_program` | 496, 528, 560 | Pubkey | `GVQjiMGV…` (OpenBook-owned, all balances 0), `HLBdJzA7…`, `srmqPvym…` | **no** (see §2.2) |
| `target_orders` | 592 | Pubkey | `GRQD5dMo4RuSN5UzMFraWz1fGQwDzu3uX79PEx8sWmHJ` (2,208 B, owner AMM) | no (deposit, withdraw and PnL only) |
| `amm_owner` | 688 | Pubkey | `GThUX1…` | no |
| `lp_amount` | **720** | u64 | 4,051,966,758,784 | no (the *internal* LP counter; used for §6 locked liquidity) |
| `client_order_id`, `recent_epoch`, `padding2` | 728, 736, 744 | u64 | 0, 1043, 0 | `recent_epoch` is written on every swap |

- **Vault and mint facts** (same slot):
  - Both vaults are owned by Tokenkeg with authority `5Q544f…`. The WSOL vault holds 38,155,515,042,581 and the Fartcoin vault 24,145,801,124,347.
  - LP mint supply is 8,906,225,417 at 9 decimals.
- **Token program:** AMM v4 is **SPL Token only.** Every swap requires `token_program == spl_token::id()` and unpacks the vaults and user accounts as exactly 165-byte `spl_token::state::Account`s [AMM-SRC `process_swap_base_in*`, `unpack_token_account`]. Token-2022 mints (all new pump.fun coins) cannot have AMM v4 pools.
- **Base/quote order is arbitrary.**
  - Fartcoin, PNUT, GOAT and MOODENG have WSOL as the *coin*.
  - WIF and POPCAT have WSOL as the *pc*.
  - Fartcoin also has a second, tiny AMM v4 pool `9GWMrDgHzWqT5wLZSwKbsFud63q93vEuA5TE6Qt9txVs` with the order reversed [RPC `getProgramAccounts` memcmp @400/@432].
- **Pool addresses are not derivable from the mint.** `initialize2` creates the pool at `PDA([program, market, "amm_associated_seed"])`, and "`market` … Just a seed for AMM account. Can be any account" [AMM-SRC `process_initialize2`]. So anyone can create any number of AMM v4 pools per pair; the fee is 0.15 SOL (AmmConfig `create_pool_fee` = 150,000,000 [RPC]).

### 2.2 AMM v4 reserves, and OpenBook today

```
total_pc_without_take_pnl   = pc_vault.amount   − need_take_pnl_pc     (CheckedSubOverflow ⇒ swap reverts)
total_coin_without_take_pnl = coin_vault.amount − need_take_pnl_coin
```

- Source: AMM-SRC `math.rs` `calc_total_without_take_pnl_no_orderbook`. Every swap variant calls it.
- **No OpenBook interaction remains.** Commit `27f461d` (2026-07-23, deployed 2026-07-22) deleted the orderbook code. The legacy swaps still *consume* 7 market accounts plus `open_orders`, but bind them to `_`-prefixed names and never read them [AMM-SRC `process_swap_base_in`/`process_swap_base_out`].
- Before that commit, swaps with `status ∈ {1 Initialized, 5 OrderBookOnly}` cancelled and settled OpenBook orders and counted open-orders balances [`processor.rs` @ `3b087ad`, `process_swap_base_in`, the `enable_orderbook` branch]. That no longer happens, and no pool has status 1 or 5 today [RPC].
- The Fartcoin pool's legacy `OpenOrders` account holds `coin/pc free/total = 0` [RPC decode]. Nothing is stranded there.
- Mainnet proof: TX-V4-OUT1 passes the pool address in all eight OpenBook positions and swaps normally. TX-V4-IN1 still passes real OpenBook accounts, which only adds write locks.

**`need_take_pnl` dynamics.**

- Swaps never change `need_take_pnl`.
- It is recomputed (it only grows) in `deposit`, `withdraw` and `withdrawpnl` through `calc_take_pnl`. That function takes `pnl_numerator/pnl_denominator = 12/100` of the constant-product growth since the snapshot `target_orders.calc_pnl_{x,y}` (TargetOrders offsets 1024 and 1040).
- `withdrawpnl` then transfers `need_take_pnl_{coin,pc}` to the PnL owner and zeroes it [AMM-SRC `process_withdrawpnl`].
- TX-V4-PNL (POPCAT): `delta_x = 41,301,912` lamports and `delta_y = 88,506,204,436` raw POPCAT were withdrawn from reserves of 17,579.9 SOL, i.e. 0.0002%, price-neutral.
- On all six target pools in §6.4, `need_take_pnl` is currently 0. So in practice reserves equal the vault amounts. Swaps between two PnL withdrawals still price on reserves that include the protocol's not-yet-realised 12% share, as the formula above shows.

### 2.3 AMM v4 fee split

- The trader pays only `swap_fee_numerator/denominator`, 25/10,000 on every pool I sampled, and it is read per pool.
- The whole fee stays in the vault, so it counts toward reserves.
- The protocol later extracts 12% of the resulting constant-product growth through `withdrawpnl`, i.e. ≈ 0.03% of volume. LPs keep ≈ 0.22%.
- There is no creator or fund fee, and no fee recipient account in the swap.

### 2.4 CPMM `PoolState` (637 bytes incl. 8-byte discriminator `f7ede3f5d7c3de46` = `sha256("account:PoolState")[..8]`, `#[repr(C, packed)]`, zero-copy)

- Offsets come from CPMM-SRC `states/pool.rs` (`LEN = 637`) and match the on-chain IDL.
- The value column is USELESS `Q2sPHPdUWFMg7M7wwrQKLrn619cAucfRsmhVJffodSp` at slot 450,833,690 [RPC].

| Field | Off | Type | USELESS |
|---|---|---|---|
| `amm_config` | 8 | Pubkey | `D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2` (index 0) |
| `pool_creator` | 40 | Pubkey | `WLHv2UAZ…` |
| `token_0_vault` | **72** | Pubkey | `HgNPDD8bpbSrGyHegiCT5xrYxHTfwLfZydwGkjNCJRKA` |
| `token_1_vault` | **104** | Pubkey | `9xsCiNwYQXM3ZeHFSVj9JQdP1vREJREpN23f6wvxA1ty` |
| `lp_mint` | 136 | Pubkey | `5rASbyrU…` |
| `token_0_mint` | **168** | Pubkey | WSOL |
| `token_1_mint` | **200** | Pubkey | `Dz9mQ9NzkBcCsuGPFJ3r1bS4wgqKMHBPiVuniW8Mbonk` |
| `token_0_program`, `token_1_program` | 232, 264 | Pubkey | Tokenkeg, Tokenkeg |
| `observation_key` | 296 | Pubkey | `4UdSz2kMddtX4woMmdgkWg75fdBP8FgYwqfkh4ri7mnD` (4,075 B) |
| `auth_bump` | 328 | u8 | 253 |
| **`status`** | **329** | u8 | 0 (bit 0 disables deposit, bit 1 withdraw, **bit 2 swap**) |
| `lp_mint_decimals`, `mint_0_decimals`, `mint_1_decimals` | 330, 331, 332 | u8 | 9, 9, 6 |
| `lp_supply` | 333 | u64 | 3,646,542,711,017 (internal counter, includes locked and burned LP) |
| `protocol_fees_token_0/1` | **341, 349** | u64 | 78,740 / 4,218,909 |
| `fund_fees_token_0/1` | **357, 365** | u64 | 43,811,337 / 20,243,797 |
| **`open_time`** | **373** | u64 | 1,746,886,997 (2025-05-10 14:23:17Z) |
| `recent_epoch` | 381 | u64 | 1043 |
| `creator_fee_on` | **389** | u8 | 0 (0 = BothToken, 1 = OnlyToken0, 2 = OnlyToken1) |
| `enable_creator_fee` | **390** | bool | 0 |
| `padding1` | 391 | [u8; 6] | 0 |
| `creator_fees_token_0/1` | **397, 405** | u64 | 0 / 0 |
| `padding` | 413 | [u64; 28] | all 0 |

- **Reserves** [CPMM-SRC `PoolState::vault_amount_without_fee`]:

  ```
  r0 = vault0.amount − (protocol_fees_token_0 + fund_fees_token_0 + creator_fees_token_0)   (InsufficientVault 6012 if negative)
  r1 = vault1.amount − (protocol_fees_token_1 + fund_fees_token_1 + creator_fees_token_1)
  ```

  For USELESS: r0 = 24,202,211,659,543 − 43,890,077 = 24,202,167,769,466 lamports; r1 = 10,208,030,224,011.
  The worst case I saw is the creator-fee pool `FQd8z3Dy6NRxZgx152SUmYL9AyyqAbYCPtSmy4spEgnH`: vault 792.6 SOL, `creator_fees_token_0` 245.45 SOL, real reserve **547.1 SOL** [RPC slot ≈ 450,838,200].
- **Collection does not change reserves.** `collect_protocol_fee`, `collect_fund_fee`, `collect_creator_fee` and the permissionless creator variant each transfer at most the counter and decrement it by the same amount [CPMM-SRC `instructions/admin/collect_*.rs`, `collect_creator_fee_permissionless.rs`].
- **Pool addresses.** A pool is either the PDA `["pool", amm_config, token_0_mint, token_1_mint]` or, when the creator signs, **any random keypair address** [CPMM-SRC `initialize.rs` `create_pool`].
  - So there can be many pools per pair and config. `token_0_mint < token_1_mint` bytewise is enforced.
  - WSOL's key starts with `0x06`, so WSOL is almost always `token_0`. It is `token_1` when the other mint's key sorts lower (e.g. pool `GaPTvXuog95vxJii7qwvfNVNdpgAcEmjFqYdZnxxgzLV`, whose token 0 is `9NiHcbT8…`).
  - Vaults are `["pool_vault", pool, mint]`, the LP mint `["pool_lp_mint", pool]`, the observation `["observation", pool]`. All of these were re-derived for USELESS and match.
- **Token-2022.**
  - `is_supported_mint` accepts any Tokenkeg mint, and Token-2022 mints whose only extensions are `TransferFeeConfig`, `MetadataPointer`, `TokenMetadata`, `InterestBearingConfig` or `ScaledUiAmount`.
  - It also accepts any mint the admin has whitelisted via a `SupportMintAssociated` PDA [CPMM-SRC `utils/token.rs`].
  - Pump.fun's Token-2022 mints (MetadataPointer + TokenMetadata) qualify.
  - On mainnet there are 30,869 CPMM pools with WSOL as token 0 and a Token-2022 token 1. The largest, `7ZRtFcgf…`, holds 3,130 SOL [RPC].

### 2.5 CPMM `AmmConfig` (236 bytes; discriminator `daf42168cbcb2b6f`) and all 21 live configs

- Offsets: `bump` 8, `disable_create_pool` 9, `index` u16 10, `trade_fee_rate` 12, `protocol_fee_rate` 20, `fund_fee_rate` 28, `create_pool_fee` 36, `protocol_owner` 44, `fund_owner` 76, `creator_fee_rate` 108, `padding` 116.
- Rates are in parts per 10^6. The PDA is `["amm_config", index as u16 **big-endian**]`, which re-derives to all 21 [CPMM-SRC `create_config.rs`; RPC `getProgramAccounts` dataSize 236].
- All 21 have `protocol_fee_rate` 120,000 (12% of the trade fee), `fund_fee_rate` 40,000 (4%), `create_pool_fee` 0.15 SOL, and `disable_create_pool` false.

| index | trade fee | creator fee* | | index | trade fee | creator fee* |
|---|---|---|---|---|---|---|
| 0 `D4FPEruK…` | 0.25% | 0.05% | | 11 `CRRS5ieQ…` | 0.25% | 1.00% |
| 1 `G95xxie3…` | 1% | 0.05% | | 12 `61GwTFRp…` | 0.30% | 1.20% |
| 2 `2fGXL8uh…` | 2% | 0.05% | | 13 `9UQpPzgj…` | 0.03% | 0.97% |
| 3 `C7Cx2pML…` | 4% | 0.05% | | 14 `52MZUkNs…` | 0.75% | 0.50% |
| 4 `BhH6HphjB…` | 0.50% | 0.05% | | 15 `GtdipqAc…` | 0.50% | 0.75% |
| 5 `BgxH5ife…` | 0.30% | 0.05% | | 16 `FMmwToGq…` | 0.005% | 1.495% |
| 6 `B5u5x9S5…` | 1.5% | 0.05% | | 17 `2vYYfNHJ…` | 0.40% | 0.85% |
| 7 `ESLj2Rzm…` | 2.5% | 0.05% | | 18 `EwJc3KbD…` | 0.70% | 0.80% |
| 8 `EUZHCdd8…` | 0.25% | 0.55% | | 19 `GwHmAEmo…` | 0.35% | 0.65% |
| 9 `LNmHRmMv…` | 0.25% | 0.75% | | 20 `627Jx6eN…` | 0.25% | 1.25% |
| 10 `Bq9YmSkZ…` | 0.50% | 0.40% | | | | |

\* The creator fee applies **only if `pool.enable_creator_fee`**.

- The permissionless `initialize` always sets `enable_creator_fee = false`.
- Only `initialize_with_permission` enables it and chooses `creator_fee_on`. It needs an admin-created `Permission` PDA for the payer, e.g. launchpads such as LaunchLab [CPMM-SRC `initialize.rs` L344–359, `initialize_with_permission.rs`].
- On mainnet, 1,667 pools have WSOL as token 0 and the creator fee enabled; 1,644 of those charge it in WSOL (`creator_fee_on = 1`) [RPC].

### 2.6 CPMM observation account (an optional TWAP)

- `ObservationState` (4,075 B) keeps 100 `{block_timestamp, cumulative_token_{0,1}_price_x32}` entries, at least 15 s apart. Swaps update it with the **pre-swap** price times the elapsed seconds [CPMM-SRC `states/oracle.rs` `update`].
- Because the elapsed time is 0 within one transaction, an atomic [pump → read → dump] bundle cannot move this accumulator.
- It is a manipulation-resistant cross-check for CPMM only. AMM v4 has no equivalent. It is optional for short.win (§9.3, §10).

---

## 3. Swap instructions and exact math

### 3.1 AMM v4 (native; the first data byte is a tag, followed by `u64` LE arguments)

| Instruction | Tag | Data | Accounts | Mainnet use in my sample (n = 388 AMM v4 swaps) |
|---|---|---|---|---|
| `SwapBaseIn` | **9** | `amount_in u64, minimum_amount_out u64` | **exactly 17 or 18** (18 inserts the ignored `target_orders` at #4) | common (legacy clients, some routers) |
| `SwapBaseOut` | **11** | `max_amount_in u64, amount_out u64` | exactly 17 or 18 | rare |
| `SwapBaseInV2` | **16** | `amount_in, minimum_amount_out` | **8**; extra trailing accounts are tolerated (no length check) | the majority: 227 of 388 (tag 9: 150, tag 11: 3, tag 17: 8) |
| `SwapBaseOutV2` | **17** | `max_amount_in, amount_out` | **8**, extras tolerated | TX-V4-OUT2 (Jupiter CPI, 7 in the POPCAT sample), TX-V4-OUT2-TOP |

- Tags 0, 2, 5, 8, 10, 12 and 13 hit `unimplemented!()`. Tag 18 is `WithdrawExcessLamports`.
- Sources: AMM-SRC `instruction.rs` `AmmInstruction::unpack`; the account-count checks are in `process_swap_base_in` (`ACCOUNT_LEN = 17`, 17 or 18) and `process_swap_base_out`.
- The v2 variants were added in `3b087ad` "add swap v2 instructions to reduce useless accounts" and deployed 2025-09-22.

**v2 account list** (the `swap_base_in_v2` / `swap_base_out_v2` helpers in AMM-SRC `instruction.rs`). The flags were confirmed on TX-V4-IN2 and TX-V4-OUT2-TOP.

| # | Account | S/W | Program check |
|---|---|---|---|
| 0 | SPL Token program | – | `== spl_token::id()` |
| 1 | AMM pool | W | owner = program, len 752, status ≠ 0, `coin_vault_mint ≠ pc_vault_mint` |
| 2 | AMM authority `5Q544f…` | – | `create_program_address(["amm authority", pool.nonce])` |
| 3 | coin vault | W | `== pool.coin_vault` |
| 4 | pc vault | W | `== pool.pc_vault` |
| 5 | user source token account | W | ≠ either vault; SPL account; `amount ≥ amount_in` |
| 6 | user destination token account | W | ≠ either vault; SPL account. **Its owner is not checked.** The (source mint, destination mint) pair fixes the direction |
| 7 | user source owner | **S** | `is_signer` only; the Token program then checks it is the source's owner **or delegate** |

- The **v1 list** is `[Token, pool W, authority, open_orders W, (target_orders W), coin_vault W, pc_vault W, market_program, market W, bids W, asks W, event_queue W, market coin vault W, market pc vault W, vault_signer, user_source W, user_dest W, owner S]`.
- The ignored positions are 3 and 6–13 in the 17-account form, and 3, 4 and 7–14 in the 18-account form. Any keys work there (TX-V4-OUT1). v1 buys nothing over v2 and costs write locks. **Use v2.**
- Swaps emit a `ray_log`: base64 of bincode `SwapBaseInLog {log_type 3, amount_in, minimum_out, direction, user_source, pool_coin, pool_pc, out_amount}` or `SwapBaseOutLog {log_type 4, max_in, amount_out, direction, user_source, pool_coin, pool_pc, deduct_in}`.
  - `pool_coin` and `pool_pc` are the reserves the math used [AMM-SRC `log.rs`].
  - `direction` is 1 = PC2Coin (input pc) or 2 = Coin2PC (input coin) [`math.rs` `SwapDirection`].
- Key errors, as `AmmError` discriminants in source order: 0x16 `InvalidStatus`, 0x1d `InvalidInput`, 0x1e `ExceededSlippage`, 0x20 `CheckedSubOverflow`, 0x28 `InsufficientFunds`, 0x2a `InvalidUserToken`, 0x18 `WrongAccountsNumber`, 0x15 `InvalidSignAccount` [AMM-SRC `error.rs`].

### 3.2 CPMM (Anchor; 8-byte discriminator = `sha256("global:<name>")[..8]`)

| Instruction | Discriminator | Args | Mainnet |
|---|---|---|---|
| `swap_base_input` | `8fbe5adac41e33de` `[143,190,90,218,196,30,51,222]` | `amount_in u64, minimum_amount_out u64` | TX-CP-IN-TOP; the majority of CPMM swaps |
| `swap_base_output` | `37d96256a34ab4ad` `[55,217,98,86,163,74,180,173]` | `max_amount_in u64, amount_out u64` | TX-CP-OUT (CPI with a PDA payer), TX-CP-OUT-TOP |

- Discriminators and argument order match the on-chain IDL [CPMM-IDL].
- Both instructions share the `Swap` accounts struct [CPMM-SRC `instructions/swap_base_input.rs`]. The flags were confirmed on TX-CP-IN-TOP and TX-CP-OUT-TOP.

| # | Account | S/W | Constraint |
|---|---|---|---|
| 0 | `payer` | **S** (not W) | none beyond signer; it is the authority of the input transfer (owner or delegate) |
| 1 | `authority` `GpMZb…` | – | seeds `["vault_and_lp_mint_auth_seed"]` |
| 2 | `amm_config` | – | `== pool_state.amm_config` |
| 3 | `pool_state` | W | Anchor owner and discriminator check |
| 4 | `input_token_account` | W | a token account; no authority constraint |
| 5 | `output_token_account` | W | a token account; **no owner constraint** |
| 6 | `input_vault` | W | `== token_0_vault ∨ token_1_vault` |
| 7 | `output_vault` | W | same (the direction comes from the (input, output) vault pair) |
| 8 | `input_token_program` | – | Tokenkeg or Token-2022 |
| 9 | `output_token_program` | – | Tokenkeg or Token-2022 |
| 10 | `input_token_mint` | – | `== input_vault.mint` |
| 11 | `output_token_mint` | – | `== output_vault.mint` |
| 12 | `observation_state` | W | `== pool_state.observation_key` |

- Transfers are `transfer_checked`, which is why the mints are required.
- Events use `emit!` (`sol_log_data`), not `emit_cpi!`, so **they add no trace entry**.
- `SwapEvent` carries `{pool_id, input_vault_before, output_vault_before, input_amount, output_amount, input_transfer_fee, output_transfer_fee, base_input, input_mint, output_mint, trade_fee, creator_fee, creator_fee_on_input}`. The two `*_vault_before` fields are **the post-fee-exclusion reserves**. Its discriminator is `40c6cde8260871e2`. Other programs emit an event with the same name, and therefore the same discriminator: my sample included Raydium CLMM `CAMMCzo5…` pools and two other programs. Filter by the emitting program.
- Errors [CPMM-IDL]: 6000 `NotApproved` (swap bit set or before `open_time`), 6005 `ExceededSlippage`, 6006 `ZeroTradingTokens`, 6008 `InvalidVault`, 6012 `InsufficientVault`.

### 3.3 Exact math (both verified bit-exactly)

Notation:

- `X` = reserve of the input token and `Y` = reserve of the output token, both **after** the §2.2 / §2.4 exclusions.
- `⌈a/b⌉ = (a + b − 1) / b`. All intermediates are u128. The AMM v4 program uses a `uint` U128 type, which gives identical results for these widths.

**AMM v4** [AMM-SRC `processor.rs` `process_swap_base_{in,out}{,_v2}`, `math.rs` `swap_token_amount_base_{in,out}`, `CheckedCeilDiv` (plain ceiling since `78c6a81`, 2025-08-26)]; `n/d = swap_fee_numerator/denominator`:

```
exact in  (tags 9/16):  fee  = ⌈amount_in·n / d⌉
                         net  = amount_in − fee
                         out  = ⌊Y·net / (X + net)⌋
                         require out ≥ minimum_amount_out (ExceededSlippage), out > 0 and amount_in > 0 (InvalidInput),
                                 out < Y (InsufficientFunds), user_source.amount ≥ amount_in
                         vaults: X += amount_in (the fee stays in the pool), Y −= out

exact out (tags 11/17): require 0 < Q < Y   (Q ≥ Y ⇒ underflow / divide-by-zero panic ⇒ revert)
                         pre  = ⌈X·Q / (Y − Q)⌉
                         cost = ⌈pre·d / (d − n)⌉              // "swap_in_after_add_fee"
                         require cost ≤ max_amount_in (ExceededSlippage), user_source.amount ≥ cost (InsufficientFunds)
                         vaults: X += cost, Y −= Q
```

**Status gate** (evaluated before the math):

- `swap_permission(status)` is true only for status 1 (Initialized), 6 (SwapOnly) and 7 (WaitingTrade).
- With status 7, the swap requires `clock.unix_timestamp ≥ pool_open_time`, and then **writes `status = 6`**.
- Status values above 7 hit `unreachable!()` in `AmmStatus::from_u64` and revert [AMM-SRC `state.rs`].

**CPMM** [CPMM-SRC `curve/calculator.rs`, `curve/fees.rs`, `curve/constant_product.rs`, `instructions/swap_base_*.rs`, `states/pool.rs`]:

- `t = amm_config.trade_fee_rate`.
- `c = amm_config.creator_fee_rate` if `pool.enable_creator_fee`, else `0`.
- `p`, `f` = the protocol and fund rates. D = 10^6.
- `fee_on_input = (creator_fee_on == 0) ∨ (creator_fee_on == 1 ∧ input is token0) ∨ (creator_fee_on == 2 ∧ input is token1)`.
- `pre(a, r) = ⌈a·D / (D − r)⌉`, and `pre(a, 0) = a`.

```
exact in (swap_base_input), amount S (Token-2022 transfer fee already subtracted: 0 for short.win's allowed mints):
  if fee_on_input:  total = ⌈S·(t+c)/D⌉ ; creator = ⌊total·c/(t+c)⌋ ; trade = total − creator ; net = S − total
                    out = ⌊net·Y / (X + net)⌋
  else:             trade = ⌈S·t/D⌉ ; net = S − trade ; sw = ⌊net·Y / (X + net)⌋
                    creator = ⌈sw·c/D⌉ ; out = sw − creator
  protocol = ⌊trade·p/D⌋ ; fund = ⌊trade·f/D⌋
  require out > 0, out ≥ minimum_amount_out ; k check (x'·y' ≥ x·y) always holds
  counters: protocol_fees_in += protocol, fund_fees_in += fund, creator_fees_{in|out} += creator

exact out (swap_base_output), amount Q:
  actual = fee_on_input ? Q : pre(Q, c)                 (creator fee on output is "grossed up")
  require actual < Y
  sw     = ⌈X·actual / (Y − actual)⌉
  if fee_on_input: cost = pre(sw, t+c) ; total = cost − sw ; creator = ⌊total·c/(t+c)⌋ ; trade = total − creator
  else:            cost = pre(sw, t)   ; trade = cost − sw ; creator = actual − Q
  protocol = ⌊trade·p/D⌋ ; fund = ⌊trade·f/D⌋
  require cost ≤ max_amount_in (ExceededSlippage 6005)
```

Gate: `status & 0b100 == 0 ∧ clock.unix_timestamp ≥ open_time`, otherwise `NotApproved` (6000).

**Post-trade reserves**, for multi-step quoting:

| | Input reserve | Output reserve |
|---|---|---|
| AMM v4 | `X' = X + amount_paid` | `Y' = Y − amount_out` |
| CPMM | `X' = X + paid − protocol − fund − (fee_on_input ? creator : 0)` | `Y' = Y − actual` (exact out) or `Y' = Y − sw` (exact in) |

**Spot price for the EMA** (short.win's convention: SOL per raw token) is `P = X_sol / Y_tok`, taken on the excluded reserves. Neither program has virtual reserves: real equals effective.

### 3.4 Verification coverage

| Program / path | Mainnet trades reproduced exactly | Pools / configs | SIM |
|---|---|---|---|
| AMM v4 exact in (tags 9, 16) | 377 | Fartcoin, WIF, PNUT, POPCAT, GOAT, MOODENG and others from a program-wide sample; both `direction`s | tag 16 sell ✓ |
| AMM v4 exact out (tags 11, 17) | 11 (both directions, incl. Jupiter CPI and a PDA-owner CPI) | same | tag 17 buy of 100,000 FART ✓; tag 11 with 17 dummy-market accounts ✓ |
| CPMM exact in | 280 | 38 pools; configs 0, 1, 2, 5, 11, 14, 15; `creator_fee_on` 0/1/2 with the fee on input **and** on output; 1 Token-2022 pool (`7ZRtFcgf…`, 39 trades) | ✓ |
| CPMM exact out | 5 | incl. TX-CP-OUT | 10,000 USELESS ✓; creator-fee pool `FQd8z3…` for Q = 10^15 and Q = 123,456,789,012,345 (creator fee on input) ✓ |

- **Method:**
  - AMM v4: recompute `out` or `deduct_in` from `ray_log.pool_coin/pool_pc` with the §3.3 formulas.
  - CPMM: recompute `output_amount`/`input_amount`, `trade_fee` and `creator_fee` from `SwapEvent.*_vault_before`, using the pool's live config rates.
  - For the worked examples I also checked vault deltas and pre-trade vault balances against `pre/postTokenBalances` [RPC].
- **Not covered by a mainnet sample:** exact-out with the creator fee charged on the **output** side (`creator_fee_on` naming the token being bought). Its formula comes from source only.

### 3.5 Worked examples (raw units)

**(A) AMM v4 exact-out buy with SOL: TX-V4-OUT2** (POPCAT pool `FRhB8L7Y9Qq41qZXYLtC2nw8An1RJfLLxRF2x9RwLLMo`: coin = POPCAT (9 dp), pc = WSOL; tag 17 via Jupiter, owner `6U91aKa8…` = Jupiter PDA `["authority", 3]`)

- Pre-trade reserves. Both equal the pre-trade vault balances, with `need_take_pnl` 0:
  - `X` = pc vault `n6CwMY77…` = 17,595,123,332,536 lamports;
  - `Y` = coin vault `4Vc6N76U…` = 37,639,277,914,829,371.
- `Q` = 235,932,179,000 (`direction` 1 = PC2Coin).
- `pre = ⌈17,595,123,332,536 × 235,932,179,000 / (37,639,277,914,829,371 − 235,932,179,000)⌉ = 110,291,219`.
- `cost = ⌈110,291,219 × 10,000 / 9,975⌉ = 110,567,639`, i.e. fee 276,420.
- Logged `deduct_in = 110,567,639` (and `max_in = 110,567,639`, passed exactly).
- Vault deltas: WSOL +110,567,639; POPCAT −235,932,179,000.

**(B) AMM v4 exact-in sell of the token for SOL: TX-V4-IN2** (Fartcoin pool; coin = WSOL, pc = FART; tag 16, top level)

- `X` = 24,152,226,230,990 FART, `Y` = 38,145,208,954,870 lamports. `amount_in` = 1,864,750,682.
- `fee = ⌈1,864,750,682 × 25 / 10,000⌉ = 4,661,877`; `net = 1,860,088,805`.
- `out = ⌊38,145,208,954,870 × 1,860,088,805 / (24,152,226,230,990 + 1,860,088,805)⌋ = 2,937,535,090` lamports. This equals the logged value and the WSOL vault delta; the FART vault delta is +1,864,750,682.

**(C) AMM v4 exact-out via a PDA with dummy market accounts: TX-V4-OUT1**

- `X` = 24,133,461,116,333 FART and `Y` = 38,174,727,180,804 lamports, both equal to the pre-trade vaults. `Q` = 1,493,590,000 lamports.
- `pre = 944,261,013`; `cost = ⌈944,261,013 × 10,000 / 9,975⌉ = 946,627,582`, which is the logged value and the token deltas.

**(D) CPMM exact-out buy with SOL: TX-CP-OUT** (USELESS, config 0: `t` = 2,500, creator fee off, `p` = 120,000, `f` = 40,000)

- `X = input_vault_before` = 24,271,071,490,644. That is the pre-trade WSOL vault 24,271,198,365,737 minus fee counters of 126,875,093.
- `Y` = 10,178,955,637,762 (the pre-trade vault 10,179,007,183,525 minus 51,545,763).
- `Q` = 409,010,196.
- `sw = ⌈X·Q / (Y − Q)⌉ = 975,297,955`; `cost = ⌈975,297,955 × 10^6 / 997,500⌉ = 977,742,311`.
- trade fee 2,444,356; protocol 293,322; fund 97,774; LP 2,053,260.
- The event shows `input_amount` 977,742,311 and `trade_fee` 2,444,356. The vault deltas are +977,742,311 WSOL and −409,010,196 USELESS.
- `max_amount_in` was `u64::MAX`, and `payer` was the program-owned PDA `HWGoJ1Ha…` (not a signer of the transaction).

**(E) CPMM creator fee.**

- **On the SOL input (TX-CP-CFIN; pool `FQd8z3…`, config 0, `c` = 500, `creator_fee_on` = 1 with WSOL = token 0).**
  - `S` = 164,842,583 lamports.
  - `total = ⌈S × 3,000/10^6⌉ = 494,528`; `creator = ⌊494,528 × 500/3,000⌋ = 82,421`; `trade = 412,107`; `net = 164,348,055`.
  - `out = ⌊net × 30,996,027,477,657,999 / (547,003,359,834 + net)⌋ = 9,310,009,993,705`, which matches the event.
- **On the SOL output (TX-CP-CFOUT, the same pool, selling the token).**
  - `S` = 2,150,833,510,816.
  - `trade = ⌈S × 2,500/10^6⌉ = 5,377,083,778`; `sw = 37,882,164`; `creator = ⌈sw × 500/10^6⌉ = 18,942`; `out = 37,863,222`, which matches the event.

**(F) SIMs at slot ≈ 450,837,0xx (not broadcast).**

- **AMM v4 tag 17, 100,000,000,000 raw FART bought with WSOL.**
  - Reserves: `pool_coin` 38,133,419,249,319 and `pool_pc` 24,159,957,563,306.
  - `pre = 158,493,293,885`; `cost = 158,890,520,186`, which equals `deduct_in`.
  - 16,055 CU for the AMM (16,205 for the whole transaction); the transaction is 458 B with 10 accounts.
  - The same trade with the destination set to **a third party's** FART account (`BuWZbmZe…`, owner `31rAZaBK…`) also succeeded.
- **CPMM `swap_base_output`, 10,000,000,000 raw USELESS.**
  - Input 23,661,141,365, trade fee 59,152,854, both equal to the formula; 25,257 CU for CPMM; the transaction is 694 B with 15 accounts.
  - Delivering to a third party's USELESS account (`68akEwyq…`) also succeeded.

---

## 4. CPI constraints

### 4.1 A PDA as `owner` / `payer` via `invoke_signed`: yes, with mainnet evidence

| Program | Tx | Caller (height) | Signer role | Signer account |
|---|---|---|---|---|
| AMM v4 tag 17 | TX-V4-OUT2 | Jupiter `JUP6…` (2) | owner | `6U91aKa8…` = PDA(JUP6, `["authority",3]`), system-owned, 0 data, off-curve, not a transaction signer |
| AMM v4 tag 16 | `4LwBVDFKPWvGPjgz1d5oWgw8JoWeTT5VmFWgf97faGTWPtNfJiB6yvLBkCdsZ8itrykWEvPdxbaHx7xD4gsffaCM` | Jupiter (2) | owner | `6LXutJvK…` = PDA(JUP6, `["authority",7]`) |
| AMM v4 tag 16 | `xFwpXtWbdHnAa5wxJZGtxzXmzGbiohv14sKq54nmpqS8rn1RNGe4ucXc6sHChs8uhvp537UEihiyd25VX2GCG65` | program `T1TANpTeScyeqVzzgNViGDNrkQ6qHz9KrSBS4aNXvGT` (2); I did not confirm who operates it | owner | `D5YqVMoS…`: off-curve, **owned by that program, 9 bytes of data** |
| AMM v4 tag 11 | TX-V4-OUT1 | `3URuDi…` (2) | owner | `A9PzVmoB…`: off-curve, system-owned |
| CPMM `swap_base_output` | TX-CP-OUT | `3s1rAym…` (2) | payer | `HWGoJ1Ha…`: off-curve, **owned by `3s1rAym…`, 0 data** |
| CPMM `swap_base_input` | `4ig3joy8cRbK2CAPv1PpfVr7n5pB77Yktng6EZ8qx6i31Ff4PhNpox8YDfS6Diqy5QqrMkP998GawtTLFEVFmG2x`, `53bCpF9jfyFoQuQmUprxedDty896LE5xhKLD63D5nfJCZPzC6d4kPNrHpmepC7iaj88DR6JGgkfRZWm9XV7Lpar2` | Jupiter (2) | payer | `6U91aKa8…`, `6LXutJvK…` |

Sources: `getTransaction` inner instructions, header signer lists and `getAccountInfo` of each signer [RPC]; the on-curve checks were done locally.

- Neither program runs a System transfer from, or creates any account for, the owner/payer. Any account that can sign is enough: system-owned or program-owned, with or without data. The only requirement is `is_signer` [AMM-SRC; CPMM-SRC `Swap.payer: Signer`].
- This is a real simplification compared with pump (C1-4 / C1-9 venue rent, C1-8 depth):
  - no `exec_rent_tolerance` is needed on Raydium legs;
  - no user-volume accumulator (UVA) or fee ATAs;
  - no fee-recipient accounts chosen by the keeper (C0-14 does not apply).

### 4.2 WSOL and token accounts

- **The SOL side must be an SPL-Token WSOL account on both venues.** AMM v4 insists on Tokenkeg for every account. CPMM's WSOL vault program is Tokenkeg; the Token-2022 native mint is not WSOL and must be rejected.
- Neither venue pays native SOL, so every Raydium trade needs a wrap/unwrap step.
- Useful token-program facts, from the p-token source that mainnet Tokenkeg runs [CRIT C1 verified-OK list]:
  - `InitializeAccount3` on the native mint sets `amount = lamports − rent_exempt_minimum` [TOKEN `processor/shared/initialize_account.rs` L57–92]. Crediting the lamports before init therefore needs no `SyncNative`.
  - Transfers by a delegate decrement `delegated_amount` and **clear the delegate when it reaches 0** [TOKEN `processor/shared/transfer.rs` L130–145].
  - `CloseAccount` on a native account sends all lamports to the destination, and any writable account can be credited.
- **Destination ownership is not checked** by either venue (§3.1, §3.2; SIM-verified). Direct delivery of the bought `Q` into the market's lending vault is therefore possible.
- The input account must be owned by the signer, or delegated to it.
- **Griefing surfaces:**
  - `ATA(trader, WSOL)` can be created by anyone (C0-3).
  - A PDA address can be **pre-funded** by anyone. `System::create_account` then fails with `AccountAlreadyInUse`, because it rejects `to.lamports > 0` [AGAVE `programs/system/src/system_processor.rs` L150–170].
  - `allocate` plus `assign` only require empty data and the System owner, so they still work on a pre-funded PDA [same file, L75–135].

### 4.3 Compute units, trace entries, depth, size

| | AMM v4 v2 (8 accts) | AMM v4 v1 (17) | AMM v4 v1 (18) | CPMM (SPL/SPL) | CPMM (one side Token-2022) |
|---|---|---|---|---|---|
| CU, venue invocation only (mainnet, min / median / max) | 15,204 / 15,404 / 15,491 (n = 93) | 16,266 / 16,271 / 18,331 (n = 64) | 16,177 / 18,567 / 19,193 (n = 6) | 24,147 / 25,079 / 33,713 (n = 244); exact-out 24,723–25,171 (n = 5) | 28,095 / 28,103 / 28,244 (n = 39) |
| Trace entries per swap | 3 (swap + 2 × Token `Transfer`) | 3 | 3 | 3 (swap + 2 × `TransferChecked`; `emit!` is a log) | 3 |
| Stack depth when short.win is top level | short.win 1 → venue 2 → Token 3 | | | same | same |

- CU comes from the "consumed" lines of each invocation's logs [RPC] and SIM (§3.5 F).
- The two Token CPIs inside a swap cost 76–112 CU each, since Tokenkeg is p-token.
- Limits: `MAX_INSTRUCTION_TRACE_LENGTH = 64` [AGAVE `transaction-context/src/lib.rs` L26]; `MAX_INSTRUCTION_STACK_DEPTH = 5` [AGAVE `program-runtime/src/execution_budget.rs` L8]. The SIMD-0268 depth increase is not active [CRIT C1-8].
- Neither venue creates ATAs, so **two spare levels always remain**: short.win can be called by one more program and still fit.
- **Transaction size:** a bare swap plus ComputeBudget is 458 B / 10 keys (AMM v4 v2) or 694 B / 15 keys (CPMM), as legacy transactions (SIM).
- **Unique venue keys added to a short.win instruction:**
  - AMM v4: 5 (program, authority, pool, 2 vaults).
  - CPMM: 8–9 (program, authority, config, pool, 2 vaults, observation, the WSOL mint, and Token-2022 if needed).
  - Plus the venue ProgramData for the open guard (§9.5).

---

## 5. Admin powers that can affect open positions

### 5.1 AMM v4 (signer `amm_owner` = `GThUX1…`, Squads v3 2-of-3, unless noted)

| Power | Instruction | Effect on short.win | Bounds in code |
|---|---|---|---|
| Change status | `SetParams` tag 6, param 0 | Status 2 (Disabled), 3 (WithdrawOnly), 4 (LiquidityOnly) or 5 (OrderBookOnly) **disables swaps** on that pool. Status 7 plus a future `pool_open_time` disables them until then | `valid_status`: 1..=7 |
| Change fees | `SetParams` param 2 | Sets `swap_fee`, `trade_fee`, `pnl` and `min_separate` **per pool, immediately**. `swap_fee` can be anything `< 1`: `validate_fraction` only requires `num < den`, so up to 99.99…% | numerator < denominator |
| Reset open time | `SetParams` param 3 | Any value; matters only while `status == 7` | none |
| Change orderbook state | `SetParams` param 1 | No effect on swaps | 0..=6 |
| Withdraw PnL | `WithdrawPnl` tag 7 (signer `amm_owner` or `AmmConfig.pnl_owner` `PNLCQcVC…`) | Transfers `need_take_pnl` plus `pnl_num/pnl_den` of the constant-product growth since the snapshot. It reduces reserves at an unchanged price. If `pnl_num` were raised towards `pnl_den`, it could take ≈ 100% of the growth since the last snapshot (fee income), but not principal | TX-V4-PNL: 0.0002% of reserves |
| Collect excess lamports | `WithdrawExcessLamports` tag 18 (signer `collect_lamports` `Rayyxz4c…`) | Only lamports above rent and **unsynced** WSOL lamports: it runs SyncNative, UnwrapLamports of the excess, and asserts `amount` is unchanged. **Token amounts are untouched** | [AMM-SRC `process_withdraw_excess_lamports`] |
| Update config | `UpdateConfigAccount` tag 15 | `pnl_owner`, `cancel_owner`, `create_pool_fee` | – |
| Program upgrade | Squads v4 3-of-4, **no timelock** | Anything | – |

There is **no admin instruction that transfers vault principal, closes a pool, or freezes a vault.** WSOL has no freeze authority, and freezable memecoin mints are rejected by short.win's mint filter.

### 5.2 CPMM (signer `admin::ID` = `GThUX1…` unless noted)

| Power | Instruction | Effect | Bounds |
|---|---|---|---|
| Pool status | `update_pool_status(status: u8)` | Bit 2 set ⇒ **swaps disabled** (`NotApproved`). Bits 0/1 disable deposit/withdraw (harmless). Any `u8` is accepted | none (`require_gte!(255, status)`) |
| Config fees | `update_amm_config(param, value)`: 0 trade, 7 creator, 1 protocol, 2 fund | `trade_fee_rate` and `creator_fee_rate` change **for every pool on that config at once**. `t + c < 10^6`, so up to 99.9999%. Protocol and fund rates change only the split: the trader's cost is unchanged, but reserves shift because those counters are excluded | `t + c < D`; `p + f ≤ D` |
| Owners, creation fee, disable creation | params 3, 4, 5, 6 | None on existing pools | – |
| Collect fees | `collect_protocol_fee` (protocol_owner or admin), `collect_fund_fee` (fund_owner or admin), `collect_creator_fee` (pool creator), `collect_creator_fee_permissionless` (anyone; pays the creator) | **Reserve-neutral** (§2.4) | ≤ counters |
| Collect excess lamports | `collect_excess_lamports` (`collect_lamports` or admin) | Same semantics as AMM v4 (native amount asserted unchanged) | [CPMM-SRC `utils/token.rs` `withdraw_excess_lamports_from_token`] |
| `open_time` | none | **No instruction can change `open_time` after initialisation** | – |
| Program upgrade | Squads v4 3-of-4, no timelock | Anything | – |

CPMM has no pool-close instruction and no principal-withdraw instruction, so a pool can never be re-initialised and `open_time` is immutable.

### 5.3 On-chain "swap disabled" predicate (for SPEC S3 "no executable route")

```
AmmV4:  route_ok = status ∈ {1, 6}  ∨  (status == 7 ∧ now ≥ pool_open_time)
        and coin_vault.amount ≥ need_take_pnl_coin ∧ pc_vault.amount ≥ need_take_pnl_pc
CPMM:   route_ok = (status & 0b100) == 0 ∧ now ≥ open_time
        and vault_i.amount ≥ protocol_i + fund_i + creator_i   (i = 0, 1)
Both:   (v0.3 correction, §12.1: the two lines below are NOT part of the venue predicate. In the design they only
        feed short.win's S3 "no route" evidence, never quotes or exits.)
        and the venue fee is ≤ a compiled hard cap (§9.4), and the market has no Emergency "route broken" flag (C0-4)
Size:   B̂(Q) is defined only for Q < token reserve (else revert) — treat as S1/S2 territory, not S3.
```

- Every bit and value above comes from source [AMM-SRC `state.rs` `swap_permission`; CPMM-SRC `pool.rs` `get_status_by_bit`], not guessed.
- This settles C0-10's concern for these two venues: unlike PumpSwap's `disable_flags`, the semantics are fully specified.
- Unknown AMM v4 status values (above 7) revert, so treat them as `route_ok = false`.
- **Current mainnet state:** 0 of 705,975 AMM v4 pools and 0 of 236,265 CPMM pools are swap-disabled [RPC, §0.2].
  - I found no evidence that Raydium disables individual pools routinely. Whether it has historically done so is **UNVERIFIED**; I did not scan the admin history.

---

## 6. Reference and execution pool selection

### 6.1 The problem

- A mint can have many SOL pools on both programs, and anyone can create one: 0.15 SOL on either, with CPMM also allowing random pool addresses. Fartcoin, for example:

  | Pool | Type | Real SOL | LP burned |
  |---|---|---|---|
  | `Bzc9NZfM…` | AMM v4 | 38,133 | 99.78% |
  | `9GWMrDgH…` | AMM v4 | 1.76 | 0.3% |
  | 7 CPMM pools | CPMM | 0.00001–5.17 | – |

  Source: RPC slot 450,839,026.
- **Neither program lets you derive a pool from the mint.**
- **The design has to prevent three things:**
  - adopting a thin pool (which makes manipulation cheap);
  - adopting a deep pool whose liquidity is then withdrawn;
  - misreading which side of the pool is SOL.

### 6.2 How the program verifies a genuine pool (all checks on-chain, all accounts mandatory and key-checked, per C0-2)

**AMM v4 candidate `P`:**

1. `P.owner == 675kPX9…` and `P.data_len == 752` (only `AmmInfo` has that size; `TargetOrders` is 2,208 and `AmmConfig` 544).
2. `status ∈ {1, 6, 7}`, which excludes 0 (uninitialised; 15 exist), and `route_ok` holds (§5.3).
3. `nonce == 254`. Then `authority = 5Q544f…`, a compile-time constant.
4. `{coin_vault_mint, pc_vault_mint} == {WSOL, market.mint}`, recording `sol_is_coin`. WSOL must be exactly `So111…112`.
5. The passed vaults equal `P.coin_vault` and `P.pc_vault`. Each is owned by Tokenkeg, has `mint` equal to the stored mint, and `owner` (authority) equal to `5Q544f…`.
6. `swap_fee_numerator/denominator ≤ fee_cap`.
7. `market.mint` is owned by Tokenkeg.

**CPMM candidate `P`:**

1. `P.owner == CPMMoo8…`, length 637, discriminator `f7ede3f5d7c3de46`.
2. `{token_0_mint, token_1_mint} == {WSOL, market.mint}`, recording `sol_is_token0`. The WSOL side's program is Tokenkeg; the token side's program equals the mint's owner.
3. The vaults equal `P.token_{0,1}_vault`, with the mint and authority `GpMZb…` checked.
4. `amm_config == P.amm_config`, owner CPMM, discriminator `daf42168cbcb2b6f`. Its `t + (enable_creator_fee ? c : 0) ≤ fee_cap`.
5. `observation == P.observation_key`.
6. `status & 4 == 0` and `now ≥ open_time`.

For **both**, read the LP mint (`P.lp_mint`, Tokenkeg, supply at offset 36) to compute the locked share in §6.3.

### 6.3 Proposed rule (PROPOSED, for ARCH/ECON)

**Metrics per eligible pool**, in lamports:

- `R` = real SOL reserve (§8).
- `λ` = permanently locked LP share:
  - AMM v4: `(lp_amount − lp_mint.supply) / lp_amount` (`lp_amount` at offset 720).
  - CPMM: `(lp_supply − lp_mint.supply) / lp_supply` (`lp_supply` at offset 333).
- `L = R · λ`, the **locked SOL**.

**Why the numerator measures permanently locked LP.** The internal counters only change inside the program: they grow on mint and shrink on withdraw.

- AMM v4 withholds `10^lp_decimals` LP at initialisation [AMM-SRC `process_initialize2`].
- CPMM withholds 100 LP [CPMM-SRC `initialize.rs` `lock_lp_amount`].
- LP burned outside the program also stays in the counter.
- So `counter − circulating supply` is LP that can never be redeemed.

**Why `L` resists manipulation.**

- Adding or removing *unlocked* liquidity does not change `L`, because `R` and `λ` move inversely.
- Raising `L` requires burning LP, which permanently donates both sides of the liquidity — or, for the constant-product form `D = λ·√(X·Y)` that the architecture adopted, trading volume at fee cost, because both programs keep the LP part of the fee in the reserves (§12.2).
- LP held in lockers (e.g. Raydium "Burn & Earn") still counts as circulating, so the metric is conservative — and it makes locker-launched pools ineligible (§12.3).
- The target set scores high:

  | Token | Venue | λ | L (SOL) |
  |---|---|---|---|
  | Fartcoin | AMM v4 | 99.78% | ≈ 38,000 |
  | WIF | AMM v4 | 99.75% | ≈ 28,000 |
  | PNUT | AMM v4 | 99.68% | ≈ 15,500 |
  | POPCAT | AMM v4 | 99.25% | ≈ 17,450 |
  | GOAT | AMM v4 | 99.75% | ≈ 7,560 |
  | MOODENG | AMM v4 | 99.67% | ≈ 12,950 |
  | USELESS | CPMM | 92.00% | ≈ 22,150 |

  Source: RPC slot 450,842,238, §6.4.

**Rule.**

1. **Eligibility.** The pool passes §6.2 and `route_ok` (§5.3). Its total fee is ≤ `fee_cap` (proposed 150 bps; compiled bound 300 bps). It has `R ≥ R_min` (the smallest tier floor). If `λ < λ_min` (proposed 0.5), most of its LP can be withdrawn at any time. Such a pool is either ineligible, or admitted in a capped mode where tier liquidity comes from `L` instead of `R` (owner/ECON choice).
2. **Listing.** `create_market` names the initial pool. It must be eligible, and it must be *not dominated*: a keeper may, within `T_list` (e.g. 1 h), propose a pool with `L ≥ (1 + h)·L_inc` to replace it before opens are enabled. Opens stay disabled until the pool-age and observation window (Q18) has passed anyway.
3. **Switching** (permissionless `propose_pool(candidate)` plus cranks):
   - Record the candidate, `t0`, and `min L_cand / L_inc` over the crank observations.
   - Execute the switch only after `T_switch` (proposed 6 h), with ≥ N observations (e.g. 36) at least 5 min apart in distinct slots, **each** satisfying `L_cand ≥ (1 + h)·L_inc` (proposed `h = 25%`).
   - Keep one pending slot per market. A new proposal replaces it only if its `L` is higher, so a weaker proposal cannot reset a genuine pending switch.
   - On switch:
     - carry the price EMA across (the same asset, arbitraged);
     - reset the liquidity EMA to `min(R_new, liq_ema_old)`;
     - positions keep their locked parameters (D27/Q9).
4. **Fast path** (incumbent failure only).
   - Trigger: the incumbent's `route_ok` has been false for ≥ `T_fast` (proposed 120 s, i.e. shorter than the 10-minute S3 window), or its `R` has fallen below 50% of its own slow EMA.
   - Then the best eligible candidate may be adopted immediately if `L_cand ≥ max(L_min, 25%·liq_ema_inc)`.
   - Only an admin action (§5) or genuine LP removal can trigger this, never an attacker. Opens stay blocked for `T_list` afterwards. (Architecture v0.3: the trigger is venue-side only — never short.win's own route-broken flag — and the best candidate seen during a 60 s window is adopted, §12.1.)
5. **Tier liquidity** uses the adopted pool's `min(R_now, R_slow_EMA)` (Q19). Optionally cap it at `L/λ_min` so that unlocked liquidity cannot inflate tiers (for ECON).

**Attack analysis.**

| Attack | Outcome under the rule |
|---|---|
| A fake deep pool with self-supplied liquidity, pulled after capture | Unlocked liquidity adds nothing to `L`. To pass, the attacker must burn LP worth `(1+h)·L_inc` of SOL plus tokens, which permanently deepens the market: an honest outcome. Even without the `λ` factor, capture needs the depth held at every observation for 6 h, and a later pull triggers the tier drop through `min(R_now, EMA)` and blocks opens |
| Thin pools used for price manipulation | Never referenced. The market prices and executes only on the adopted pool, whose keys are stored and re-checked on every instruction |
| The mint on either side, or a fake quote | `sol_is_coin` / `sol_is_token0` is derived from the verified mint fields. The quote must be exactly `So111…112` (Tokenkeg), never the Token-2022 native mint or a look-alike |
| Proposal spam | 0.15 SOL per fake pool, and a weaker proposal cannot displace the pending one |
| Liquidity withdrawn from the incumbent by unlocked LPs | `R_now` drops, so tier and size caps drop at once. Opens are blocked if `R < R_min`. Existing positions exit on the thinner pool with the S2 rules |
| Pushing the candidate's price up to inflate `R_cand` | Arbitrage undoes it within slots; the min-over-window with N distinct-slot samples makes holding it cost ≈ a 6-hour price attack |

**Relation to D1.**

- A pump coin with a live curve or a canonical PumpSwap pool uses those venues. `create_market` must derive the curve and canonical-pool PDAs and require their keys (C0-15).
- The Raydium adapter applies when the curve is absent, or complete with zeroed reserves ("migrated elsewhere"), **and** no canonical PumpSwap pool exists.
- Whether a coin that has both a PumpSwap pool and a deeper Raydium pool should ever use Raydium is an owner question. D1 says PumpSwap.

### 6.4 Pools of the D2 target coins (slot 450,842,238)

| Token | Venue | Pool | SOL side | status | `open_time` field | fee | Real SOL | LP burned |
|---|---|---|---|---|---|---|---|---|
| Fartcoin | AMM v4 | `Bzc9NZfM…` | coin | 6 | 0 | 25/10,000 | 38,100.5 | 99.78% |
| WIF | AMM v4 | `EP2ib6dY…` | pc | 6 | 1,700,510,048 | 25/10,000 | 28,078.3 | 99.75% |
| PNUT | AMM v4 | `4AZRPNEf…` | coin | 6 | 0 | 25/10,000 | 15,587.2 | 99.68% |
| POPCAT | AMM v4 | `FRhB8L7Y…` | pc | 6 | 1,702,415,025 | 25/10,000 | 17,582.9 | 99.25% |
| GOAT | AMM v4 | `9Tb2ohu5…` | coin | 6 | 0 | 25/10,000 | 7,582.8 | 99.75% |
| MOODENG | AMM v4 | `22WrmyTj…` | coin | 6 | 0 | 25/10,000 | 12,990.0 | 99.67% |
| USELESS | CPMM | `Q2sPHPdU…` | token0 | 0 | 1,746,886,997 | 0.25% (cfg 0) | 24,072.4 | 92.00% |

Pool addresses are the deepest pools DexScreener lists (`token-pairs/v1`, fetched 2026-09-27); all fields were decoded from RPC.

---

## 7. Pool age

- **CPMM.**
  - Since commit `a7330e3` "fix swap in opentime" (2024-05-16 04:41:29Z), both initialisers do `if open_time <= now { open_time = now + 1 }` [CPMM-SRC `initialize.rs` L206–207, `initialize_with_permission.rs` L219–220].
  - No instruction writes `open_time` afterwards, and pools cannot be closed (§5.2). So for every pool created after that code was deployed, `creation_time < open_time`, and `now − open_time` is a **valid lower bound on age**.
  - Pools created before the clamp may carry any `open_time`, including 0.
  - The first CPMM upgrade after the commit was 2024-05-16 06:31Z (slot 266,073,199), followed by 05-30 and 05-31. Whether the 05-16 deploy contained the clamp is **UNVERIFIED**.
  - Hence use the conservative compiled constant `T_clamp = 1,717,200,000` (2024-06-01T00:00Z) and

  ```
  age_lower_bound = now − max(open_time, T_clamp)
  ```

  This is correct for pre-clamp pools, because they were created before `T_clamp`. It is also correct for post-clamp pools, because then `open_time ≥ creation + 1 > T_clamp`.
  - It measures the *pool's* age. A LaunchLab-migrated token is older than its CPMM pool, so the bound stays conservative.
  - A future upgrade could change this; Q13's venue guard covers that.
- **AMM v4.** `pool_open_time` is stored raw at `initialize2`, is **not clamped**, and the admin can reset it with `SetParams` param 3. It is 0 on pump.fun graduation pools (Fartcoin, PNUT, GOAT, MOODENG) and a 2023 timestamp on others [AMM-SRC `process_initialize2`, `process_set_params`; RPC]. **Unusable.** Use first on-chain observation or listing time (Q18).

---

## 8. Liquidity for tiers: real SOL reserves

| Venue | `R` (real SOL reserve, lamports) | Notes |
|---|---|---|
| AMM v4 | `sol_vault.amount − need_take_pnl_sol` (`sol_vault = sol_is_coin ? coin_vault : pc_vault`) | This is exactly what the swap math uses. It still includes the protocol's unrealised 12% share of fee growth, which `withdrawpnl` later removes (negligible, §2.2) |
| CPMM | `sol_vault.amount − protocol_fees_sol − fund_fees_sol − creator_fees_sol` | Must subtract the counters: pool `FQd8z3…` has a 792.6 SOL vault but `R` = 547.1 SOL. Aggregators that show vault balances (e.g. DexScreener "liquidity") overstate such pools |

- Q19 liquidity = `2·min(R_now, R_slow_EMA(1 h))` on the adopted pool only. Liquidity elsewhere (CLMM, DLMM, other pools) is ignored, which is conservative and consistent with D2.
- Direct WSOL lamport donations do not count until someone calls `SyncNative`. Syncing is permissionless and is equivalent to a donation, which is costly for the donor. Raydium's `*_excess_lamports` instructions only take unsynced lamports.

---

## 9. Design implications for short.win

### 9.1 Venue model and market state

- Add `Venue::RaydiumV4` and `Venue::RaydiumCpmm` beside `PumpCurve` and `PumpSwap`.
- Store in `Market`: `pool`, `sol_side` (a bit), `vault_sol`, `vault_tok`, and for CPMM also `amm_config` and `observation`. Also `lp_mint`, `liq_ema`, and the pending-switch fields (§6.3).
- **Program IDs, authorities and `T_clamp` are compile-time constants** (C0-8): `675kPX9…`, `5Q544f…`, `CPMMoo8…`, `GpMZb…`, WSOL, Tokenkeg, Token-2022.
- Re-check every stored key against the passed accounts on every instruction. All accounts are mandatory (C0-2).

### 9.2 Adapter account sets

The order is binding (S = signer, W = writable).

**`RaydiumV4`**

- **ReadSet**, used by `crank_price`, open, close, liquidate, expire and `sol_settle`:
  - R0 `pool` (W in trades)
  - R1 `coin_vault` (W)
  - R2 `pc_vault` (W)
  - pool-switch cranks also take R3 `lp_mint`.
- **TradeExtras:**
  - X0 AMM v4 program
  - X1 authority `5Q544f…`
  - X2 Tokenkeg
  - X3 WSOL mint
  - X4 `trader` PDA (signs via `invoke_signed`; not a transaction signer)
  - X5 `trader_wsol`, an ephemeral PDA-address token account (W)
  - X6 System program
  - The lending vault (already in short.win's set) is the sell source and the buy destination.
- **CPI** (8 accounts): `[Tokenkeg, pool W, authority, coin_vault W, pc_vault W, source W, destination W, trader S]`, data `[16 | amount_in | min_out]` or `[17 | max_in | Q]`.
- **OpenTail:** AMM v4 ProgramData `A7ZG7By…` (1,406,429 B) for the Q13 venue guard.

**`RaydiumCpmm`**

- **ReadSet:**
  - R0 `pool` (W)
  - R1 `amm_config`
  - R2 `token_0_vault` (W)
  - R3 `token_1_vault` (W)
  - plus R4 `lp_mint` for switch cranks.
- **TradeExtras:**
  - X0 CPMM program
  - X1 authority `GpMZb…`
  - X2 `observation` (W)
  - X3 Tokenkeg
  - X4 the token's program (Tokenkeg or Token-2022, equal to `pool.token_x_program`)
  - X5 WSOL mint
  - X6 token mint (already present)
  - X7 `trader` PDA
  - X8 `trader_wsol` (W)
  - X9 System program
- **CPI** (13 accounts): `[trader S, authority, amm_config, pool W, in_acct W, out_acct W, in_vault W, out_vault W, in_prog, out_prog, in_mint, out_mint, observation W]` with discriminator `8fbe5adac41e33de` or `37d96256a34ab4ad`.
- **OpenTail:** CPMM ProgramData `DMawCQz…` (793,869 B).

### 9.3 Leg flows

These answer C0-3 and C1-3 (trader normalisation) without ATAs.

**Sell at open** (exact in):

1. `Approve(lending_vault → trader, Q)`, signed by the market PDA.
2. Create `trader_wsol`:
   - credit `rent(165)` lamports from the position or payer to the PDA address;
   - `allocate(165)` and `assign(Tokenkeg)` via `invoke_signed` (these work even if a griefer pre-funded the address; `create_account` would not);
   - `InitializeAccount3(native mint, owner = trader)`.
3. Swap exact-in with `source = lending_vault` (the trader acts as delegate for exactly `Q`; the delegate clears automatically at 0) and `destination = trader_wsol`. Pass `min_out` = the on-chain quote, bounded below by the EMA floor.
4. `CloseAccount(trader_wsol → position)`.
5. Post-conditions: `S` = `trader_wsol.amount` read after the swap and before the close; the vault decreased by exactly `Q`; `trader_wsol` is closed; the delegate is cleared.
6. Trace: short.win 1 + Approve 1 + allocate 1 + assign 1 + init 1 + swap 3 + close 1 = **9**. Add one for an explicit `Revoke` if desired.

**Buy at close, liquidate or expire** (exact out):

1. Credit `rent + max_in` lamports from the position (program-owned, debited directly) to the `trader_wsol` address.
2. `allocate`, `assign`, `InitializeAccount3`. The amount becomes `max_in` without `SyncNative`.
3. Swap exact-out with `source = trader_wsol` and `destination = lending_vault` (the owner is not checked, §4.2). Pass `max_in` = the on-chain quote, which can be the exact figure.
4. `CloseAccount(trader_wsol → position)`.
5. `B = max_in − trader_wsol.amount` read after the swap, and the vault received exactly `Q`.
6. Trace: short.win 1 + allocate 1 + assign 1 + init 1 + swap 3 + close 1 = **8**.

If `emit_cpi!` events are kept (ARCH §3.10), each adds 1 trace entry. Even with a `crank_price` in the same transaction the total stays around 12–15 of 64.

**Other points:**

- **ε = 0 is sound** (D20 / `exec_tolerance`): the quote and the venue read identical accounts inside one instruction, and all 673 reproduced trades match exactly. Keep ε only as a post-upgrade guard, and bound it (C0-8).
- **Exit bounds** (C0-1): `max_in = min(quote_spot(Q), quote_ema(Q)·(1+δ), v − R_due − K_s)`.
  - `quote_ema` uses the same §3.3 formula on EMA reserves: store `R_sol_ema` and `P_ema`, and set `R_tok_ema = R_sol_ema / P_ema`.
  - Fees are read live from the pool or config.
  - The CPMM observation TWAP (§2.6) is an optional second bound that atomic bundles cannot move (C0-5); AMM v4 has none.
- **The quoting code is small:** two `u128` multiply-divides plus fee arithmetic, so a few thousand CU. There are no fee tiers and no external fee program, unlike pump and PumpSwap.

### 9.4 Fees and admin risk (for ECON)

- **Round trips:**
  - AMM v4: 2 × 0.25% in fees, and impact cancels on an immediate reversal (cf. C1-6).
  - CPMM: 2 × (`t` + `c`), from 0.25% (config 0) up to 4% (config 3) or 1.25% (config 11 with the creator fee on).
  - Atomic [pump → crank → dump] manipulation is **cheaper** on AMM v4 than on PumpSwap (0.25% versus 0.30–1.25%). C0-5's cost model must use the Raydium fee levels.
- **Fee caps.** Enforce `fee_cap` for opens: proposed 150 bps total, compiled bound 300 bps.
  - A fee raised after open (AMM v4 per pool, or CPMM per config across all its pools) raises every exit's executable cost.
  - Proposal: compute D3's FV (S2/S3 cap) with `min(current fee, fee_at_open)`. Treat `fee > FEE_HARD_CAP` (e.g. 500 bps) as `route_ok = false`, i.e. S3-eligible with the Emergency flag. **This is an owner/ECON question**, because an admin fee hike otherwise moves value from the borrower to lenders. (Architecture v0.3 adopted a narrower form: a fee above the hard cap is S3 evidence only, and every risk quote — CR legs, S1/S2 legs, exit references, FV — caps the fee at the hard cap, so the hike cannot make positions liquidatable; §12.1.)
- **Creator fees** (CPMM `initialize_with_permission` pools): the pool creator earns 0.05–1.5% on short.win's open and close legs, so R1 self-dealing applies here too (C0-18). There are no cashback or reward accounts to strand.
- **Trust assumptions to disclose** (§5):
  - The upgrade authority is a Squads v4 3-of-4 with **no timelock**, and there are no verified builds.
  - Pool admin is a Squads v3 2-of-3 with no timelock, able to disable swaps and raise fees per pool or config.

### 9.5 Budgets

| | AMM v4 leg | CPMM leg |
|---|---|---|
| Venue CU | ≈ 15.5k | ≈ 25k (SPL) / 28k (Token-2022 token side) |
| WSOL lifecycle + Approve (p-token and System) | ≈ 2–4k (**UNVERIFIED**, LiteSVM) | same |
| Quote math | ≈ 2–5k (**UNVERIFIED**) | same |
| Trace entries per leg | 9 (sell) / 8 (buy) | 9 (sell) / 8 (buy) |
| Venue keys added | 5 + ProgramData on opens | 8–9 + ProgramData on opens |

- The whole `close_short` or `liquidate` is estimated at 60–120k CU, including Anchor overhead. That compares with the ≈ 150–250k estimated for PumpSwap (`graduated-venues.md` §8.6). **UNVERIFIED until the T-CU benchmark.**
- **C1-15:** the venue-guard ProgramData is 1.4 MB (AMM v4) or 0.79 MB (CPMM), against pump's 10 MiB. The input-region copy still costs CU. Prefer the keeper-mirrored deploy slot (`sync_venue_slot`) that the C1-15 fix proposes.
- **Size:** use one global ALT holding both program IDs, both authorities, the 21 CPMM configs, WSOL, Tokenkeg, Token-2022, System and ATA. Per-market pool and vault keys go in the market's ALT or inline. v0 transactions fit easily; v1 (4 KB) is active [CRIT C1 verified list].

### 9.6 Risks and open points

1. **Admin halts or fee hikes** (§5). Detection is exact (§5.3). The S3 path exists for *liquidatable* positions; the fast pool switch (§6.3) can restore a route when another eligible pool exists. Healthy positions on a halted pool can neither close nor settle until the route returns or their deadline (§12.4).
2. **Unannounced upgrades** (three per program since June 2026, one every 1–2 months) and unverified builds.
   - The Q13 guard blocks opens after any ProgramData slot change.
   - Exits keep running on the compiled adapter.
   - Raydium's instruction layouts have been additive so far: v2 swaps were added rather than v1 being changed, and the unmerged creator-fee-share branch keeps sizes. A future change to account lists would still freeze exits (C0-4).
   - AMM v4 v2 and Anchor CPMM both **tolerate trailing extra accounts**, so C0-4's "bounded trailing extras" remedy works for these venues. AMM v4 v1 does not (exact 17/18). Venues tend to add *derived* accounts (per mint or per pool), which the architecture's extras now express as derivation templates (§12.5).
3. **Unlocked-LP pools** can be drained by LPs. Mitigated by the `L` metric and `min(R_now, EMA)`.
4. **Uncollected CPMM fee counters** make vault balances misleading. Always use §8.
5. **Token-2022 on CPMM:** transfer fees are handled by the venue, but short.win's mint filter rejects `TransferFeeConfig` anyway. InterestBearing and ScaledUiAmount are allowed by CPMM but must stay rejected by short.win (R-MINT).
6. **Delegate-based sells** leave an `Approve` on the lending vault only within the instruction: it clears at 0 after exact-in consumes `Q`. Add an invariant check `delegate == None` at the end of each instruction.
7. **Pre-funded PDA addresses** block `create_account`. The allocate/assign path avoids that; add a test.
8. **Test gates to add:**
   - **T-RAY-MATH:** differential tests of the §3.3 functions against every signature listed in this note, plus random fuzz against the dumped ELFs in LiteSVM (hashes in §1.3).
   - **T-RAY-PDA:** the trader PDA as owner/payer, the lending vault as delegated source and as destination.
   - **T-RAY-DISABLE:** fork the pool with AMM v4 status 2–5, 7 with a future open time, and 8; CPMM bit 2 and a future `open_time`.
   - **T-RAY-FEE:** a config fee change with positions open.
   - **T-RAY-WSOL:** pre-funded `trader_wsol` address, WSOL donations, a pre-created `ATA(trader, WSOL)` (must be irrelevant).
   - **T-RAY-TRACE / T-RAY-CU:** count trace entries and CU per instruction.
   - **T-RAY-SWITCH:** the fake-pool, LP-pull and proposal-spam scenarios from §6.3.

---

## 10. UNVERIFIED items and open questions

**UNVERIFIED:**

- Byte-exact reproducible builds of `raydium-amm@d26944b` and `raydium-cp-swap@59fb845`. I only have a behavioral match via ELF strings and mainnet logs. (*M10:* the CPMM half is superseded — the deploy mainnet runs since 452,376,460 is `raydium-cp-swap@bdeee23`, rebuilt byte for byte, §13. AMM v4 is still a behavioural match to `d26944b` only.)
- Whether the AMM v4 ProgramData transactions of 2025-02-22 and 2025-03-11 were upgrades (the RPC did not return them).
- Whether the CPMM deploy of 2024-05-16 06:31Z contained the `open_time` clamp; hence the conservative `T_clamp` of 2024-06-01.
- Whether Raydium's admin has ever used `SetParams` status, `update_pool_status` or fee changes on individual pools. Current state: none disabled.
- CU of the full short.win Raydium instructions, including the WSOL lifecycle and Approve. Only the venue CU is measured.
- Exact-out with the CPMM creator fee charged on the output side (derived from source only).
- Token-2022 delegate auto-clear. Verified in p-token (Tokenkeg); for Token-2022 it is inferred.

**Open questions (owner, ECON, ARCH):**

- The pool-selection parameters (`h`, `T_switch`, `λ_min`, `fee_cap`, `T_fast`), and whether tiers should be capped by locked liquidity `L`.
- Whether FV (D3) should use `min(current fee, fee at open)` after an admin fee hike, and whether `fee > FEE_HARD_CAP` counts as "no route".
- Precedence when a pump coin has both a canonical PumpSwap pool and a deeper Raydium pool (D1 says PumpSwap).
- Whether to use the CPMM observation TWAP as an additional EMA sanity bound for CPMM markets.

---

## 11. Cross-reference to review findings

| Finding | What this note establishes for the Raydium venues |
|---|---|
| C0-1, C0-5 | The exit bound can be exact (ε = 0) and priced on EMA reserves with the same formula (§9.3). Manipulation is cheaper at the 0.25% fee (§9.4). The CPMM TWAP resists atomic bundles (§2.6) |
| C0-2 | All ReadSet and TradeExtras accounts are mandatory and key-checked. None is optional (§9.2) |
| C0-3, C1-3 | No ATA is needed. The WSOL account is ephemeral, lives at a PDA address, and is created with the grief-proof allocate/assign path. The sell source is a delegated lending vault; the buy destination is the lending vault (§9.3) |
| C0-4 | v2 and Anchor tolerate trailing extras; v1 does not (§9.6.2) |
| C0-8 | Program IDs, authorities and `T_clamp` are constants (§9.1) |
| C0-10 | The disable semantics are exact and come from source (§5.3) |
| C0-14, C1-4, C1-9 | No fee recipients, no venue-side rent, no account creation by the venue (§4.1) |
| C0-15 | Detection precedence and re-detection (§6.3, "Relation to D1") |
| C0-18 | No venue rewards. The CPMM creator fee feeds R1 (§9.4) |
| C1-1 | Per-leg trace, counted from the CPI plan: 9 entries (sell), 8 (buy) (§9.3). Venue swap = 3 entries, measured on mainnet |
| C1-7 | PDA signer evidence for both programs (§4.1) |
| C1-8 | Two spare stack levels always, because there is no ATA creation (§4.3) |
| C1-15 | ProgramData sizes of 1.4 MB and 0.79 MB (§9.5) |

---

## 12. Review corrections — round 2 (2026-09-27)

Second design review (findings `V1-x` / `V2-x`, 2026-09-27). Items that correct or extend statements in this note; the design consequences live in `ARCHITECTURE.md` v0.3 (Appendix A, round 2).

### 12.1 One predicate, two jobs (V2-1, V1-7)

§5.3 wrote a single `route_ok` that also contained the fee hard cap and short.win's Emergency "route broken" flag. Used as the "no route" of the exit predicates, that let the no-timelock flag, or a Raydium admin fee change, pause closes, make positions liquidatable on the EMA leg alone and arm the fast pool switch. The venue-side facts in §5.3 are unchanged; the design now uses them as `venue_executable` (quotes, closes, liquidations, the fast path) and adds the hard cap and the flag only to S3 evidence. Verified at the pinned commit: CPMM `update_trade_fee_rate` / `update_creator_fee_rate` only `assert!(trade_fee_rate + creator_fee_rate < FEE_RATE_DENOMINATOR_VALUE)` (10⁶ ppm), so a config fee of 5.01 % — or 99 % — is accepted and applies to every pool on the config at once [CPMM-SRC `instructions/admin/update_config.rs` L47–60, `gh api` 2026-09-27].

### 12.2 Locked depth moves with fees (V2-7)

`√(X·Y)` is invariant under a fee-free swap, not under a real one. AMM v4 `process_swap_base_in_v2` transfers the whole `amount_in`, fee included, into the pool vault (the protocol's 12 % share is later carved out through `need_take_pnl`) [AMM-SRC `processor.rs`, `swap_base_in_v2`]; CPMM keeps the LP part of the trade fee in the vault, outside the protocol/fund/creator counters [CPMM-SRC `curve/calculator.rs`]. A trade of `A` lamports on fee `f` therefore grows `√(X·Y)` by ≈ `f·A/(2Y)`, and wash trading raises the locked depth `D = λ·√(X·Y)` by a factor `(1 + x)` for ≈ `2·Y·ln(1 + x)` of fees (+25 %: ≈ 0.45·Y), of which at least the locked share stays with burned LP. "Cannot be pushed by trading" should read "can be pushed only at fee cost"; the architecture adds a 5 % margin to the listing-window comparison.

### 12.3 Lockers: a coverage gap, not only conservatism (V2-11)

Counting LP held by locker programs as circulating is conservative, but it also makes pools launched with locked (not burned) LP permanently ineligible, so their tokens are list-only despite D2. Live survey, 2026-09-27 (re-run of the reviewer's `getProgramAccounts` script): 1,668 CPMM pools with `enable_creator_fee` and WSOL as token 0; 311 hold ≥ 25 SOL of real reserve; 244 of those have λ ≥ 50 % and 67 do not — e.g. `AtqUb7DhVjiWgUhkrE7CnLGD9aDDedhSNrf15iUwHFGk` (2,024.6 SOL, λ 0.0000), `CheJUxVQrmpeb4UFWHZ72afQPTKM7AEb8giyzNR7cPvH` (608.6 SOL, λ 0.0000), `7H59ffyZoM4QjU7WP6dyAmM9LtjhtSSqChWSmZbDR4u8` (465.5 SOL, λ 0.0000). Pools whose LP is burned pass, e.g. `FQd8z3Dy6NRxZgx152SUmYL9AyyqAbYCPtSmy4spEgnH` (547.2 SOL, λ 0.998) and `koZZXrKJctd7AtVHdx62iYLPv5PqvQ3RrFyUr3GMBfm` (428.9 SOL, λ 1.000). Which locker programs hold the LP of the λ ≈ 0 pools was not examined (**UNVERIFIED**); counting key-checked, provably permanent locker vaults as locked is a v1.1 option (ARCH §10 O4).

### 12.4 Halts freeze healthy positions (V2-5)

An admin halt of the only eligible pool leaves no fast-path target. Live check, 2026-09-27: Fartcoin (`9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump`) has 9 Raydium SOL pools — 2 AMM v4 (one per side ordering) and 7 CPMM — and only `Bzc9NZfMqkXR6fz1DBph7BDf9BroyEf6pnzESP7v5iiw` (AMM v4, status 6, 38,176 SOL real, λ 0.9978) is eligible; every other pool holds ≤ 5.2 SOL or has λ < 0.01. The AMM v4 admin that can halt it is the mainnet build's hard-coded `config_feature::amm_owner` `GThUX1Atko4tqhN2NaiTazWSeFWMuiUvfFnyJyUghFMJ` (Squads v3, 2-of-3, §5.1) [AMM-SRC `processor.rs`, `#[cfg(not(any(feature = "testnet", feature = "devnet")))]`]. Such a halt freezes every healthy Fartcoin position until the route returns or its deadline; the design discloses it and offers the owner options (ARCH §5.3, §10 O14).

### 12.5 Venue account changes are usually derived accounts (V2-6)

§9.6 item 2 is right that AMM v4 v2 and CPMM ignore trailing accounts, so extras cannot break Raydium exits. The accounts venues add in practice are per-mint or per-pool PDAs (pump's `bonding_curve_v2`, PumpSwap's `pool-v2`), which a list of fixed global keys cannot express; the architecture's extras are now derivation templates with two selectable sets.

### 12.6 Rule 5 adopted (V2-8)

§6.3 rule 5's optional cap ("cap tier liquidity at `L/λ_min` so that unlocked liquidity cannot inflate tiers") is now the design's rule for opens, replacing the v0.2 open-time `λ_now ≥ λ_min` check, which anyone could trip by adding unlocked liquidity (λ' = λ/(1 + f)).

---

## 13. M10 update (2026-10-03): the CPMM redeploy

Re-read for M10. Mainnet reads: the ProgramData headers at slot 452,485,424 (2026-10-02) and again
by `scripts/dump-fixtures.py check --live` at slot 452,907,481 (2026-10-03); the fixtures are in
`tests/fixtures` (group `raydium_cpmm_2026_10`), whose README has the full interface diff and the
reproduction. §1–§12 above are not edited except where they point here.

- **Redeploy.** CPMM (`CPMMoo8L…`, ProgramData `DMawCQzb…`) was upgraded at slot **452,376,460**
  (block time 2026-10-01 19:05:48Z) by the same authority, the Squads v4 vault `FytDrVz…`. The
  ProgramData's lamports went from 5,028,383,001 to 4,033,504,760 (excess lamports collected); its
  size is unchanged (793,869 B). AMM v4 is unchanged (445,763,204).
- **Source: reproduced.** The trimmed ELF (722,617 B, sha256 `93cad245…a0d2`) is the build of
  `raydium-io/raydium-cp-swap` branch `feat/creator-fee-share` at `bdeee23` (2026-09-15; 5 commits
  ahead of `59fb845`): solana-verify 0.5.2 in its (3,1,10) image (`sha256:f71be5ca…`, which the
  branch selects with `[workspace.metadata.cli] solana = "3.1.10"`), run through the repository's
  bubblewrap shim, outputs a byte-identical file. verify.osec.io still lists the program as not
  verified (registered commit `cfdb70a8…`, on-chain hash `93cad245…`; checked 2026-10-03).
- **Interface.** The 18 instructions of `59fb845` plus `create_creator_fee_share` and
  `close_creator_fee_share`; a new `CreatorFeeShare` account; `SwapEvent` unchanged.
  **The swap path is unchanged**: `swap_base_input` / `swap_base_output`, the curve and fee math at
  swap time and `vault_amount_without_fee` (the pricing reserves short.win reads) are
  byte-identical in source. `AmmConfig` gains `creator_fee_share_rate: u64` at offset 116, carved
  from the padding (still 236 B); `PoolState` is unchanged (637 B). All 21 configs hold 0 there
  (they equal the fixture bytes at slot 452,488,420).
- **Creator fees (§9.4, ECON R1).** When a pool's accrued creator fee is collected,
  `collect_creator_fee` (which now takes a trailing `creator_fee_share` account) moves
  ⌊fee·share_rate/10⁶⌋ of it to the protocol's counter and pays the creator the rest. With every
  rate at 0 today, nothing changes; a non-zero rate would divert part of a pool creator's fee to
  Raydium. Neither short.win's costs nor its pricing move (both counters are excluded from the
  pricing reserves), and short.win never calls the collect instructions.
- **short.win: no adapter change.** Every CPMM test, bench and the web E2E validator run the
  redeploy (`fork::CpmmDeploy::CURRENT`); each CPMM swap costs 5 CU more inside the venue
  (`tests/bench/m3_cu`). T-LATCH replays the real 445,763,504 → 452,376,460 upgrade
  (`latch_raydium_it`). The program's `VENUE_GUARD_SLOTS_2026_10_02` records 452,376,460 as the test
  baseline; `initialize_config` takes the live slots.
- **Jupiter** was also redeployed (451,957,263, read at slot 452,485,424); no fixture.

