# Pump.fun bonding-curve program: mainnet reference for short.win

*Research note, written 2026-09-27. Mainnet data was sampled between slots ~450,798,000 and ~450,806,000 (2026-09-26, 21:40–22:15 UTC). Baseline: `docs/SPEC.md` v0.3.*

**Scope.** This covers the Pump.fun bonding-curve program only: what short.win must CPI into to sell borrowed tokens at open and buy them back at close or liquidation, and what it needs to price trades on-chain. PumpSwap (the AMM that tokens graduate to) is covered here only where graduation needs it.

**How claims are sourced.** Each claim cites one of the following:

- **[DOCS]**: `github.com/pump-fun/pump-public-docs` at commit `81091419e4457566469d4e2a27f64ed84d42419c` (HEAD, 2026-09-14).
- **[IDL]**: `idl/pump.json` in that repo, last changed in commit `e0687ae9b7e064a0f54efc7297c65eecfbba3a8f` (2026-09-12). It is identical to the on-chain Anchor IDL (§1).
- **[TS-SDK]**: npm `@pump-fun/pump-sdk@2.0.0` (published 2026-09-13), files under `src/`.
- **[RS]**: crates.io `pump-rust-client@0.1.13` (2026-09-09; `.cargo_vcs_info.json` sha `bd875a38ee3b4223bd8e07fa93d5f677bca11065`).
- **Transaction signatures and account addresses**: fetched from mainnet RPC (`api.mainnet-beta.solana.com` / `solana-rpc.publicnode.com`, `solana-core 4.3.0`).
- **SIM**: my own `simulateTransaction` runs (`sigVerify:false`, `replaceRecentBlockhash:true`, commitment `processed`) against live mainnet state on 2026-09-26. I built the transactions by hand from the IDL. They were not broadcast. Each SIM result is labelled as such.

Anything I could not verify is marked **UNVERIFIED**.

---

## 0. Summary

1. **Program** `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` is upgradeable. Its upgrade authority is a Squads v4 multisig vault (`7gZufwwAo17y5kg8FMyJy2phgpvv9RSdzWtdXiWHjFr8`). It was upgraded **4 times in Sept 2026**, most recently **2026-09-23 14:45:19 UTC**, which is after the last IDL/doc commit.
2. **All new coins are Token-2022** (`create_v2`): 6 decimals, 1,000,000,000 supply (mayhem coins: 2,000,000,000), mint and freeze authority `None`. The only extensions are `MetadataPointer` and `TokenMetadata`, so they pass the SPEC §3 safety filter. Legacy SPL-Token pump coins still exist, but all 253 curve trades in my sample were Token-2022 (sample: every 6th of ~2,100 successful pump txs on 2026-09-26).
3. **Curve math is Uniswap-v2 over virtual reserves.** I reproduced it bit-exactly against 6 mainnet trades and a live simulation (§4).
   - Current fees: **95 bps protocol + 30 bps creator = 1.25%**. They are read at trade time from the `pump-fees` `FeeConfig` account (one tier today) and are charged **on top of** buys and **out of** sells, each fee rounded **up**.
   - 50% of the protocol fee goes to a "buyback" recipient. This does not change what the trader pays.
4. **An oversized buy reverts.** If a buy asks for more than `real_token_reserves`, it fails with `6021 NotEnoughTokensToBuy`. There is no cap and no partial fill; SIM confirms this for `buy`, `buy_v2` and `buy_exact_quote_in_v2`. After `complete == true`, every buy and sell fails with `6005 BondingCurveComplete`.
5. **CPI from a PDA works, with one requirement.**
   - `buy` pays via `system_program::transfer` **from `user`**, so `user` must be a **system-owned, zero-data** account (a PDA is fine; pump's own mayhem program does exactly this).
   - `sell` credits proceeds **directly to `user`'s lamports** (the curve account is debited in-program).
   - The first buy auto-creates a 137-byte `UserVolumeAccumulator` PDA, **paid by `user`**.
   - Mainnet shows a `sell_v2` by a non-whitelisted, 0-lamport PDA `user` under CPI (§6.1). Buys by a PDA `user` are seen only for the whitelisted, fee-exempt mayhem PDA; the non-whitelisted buy path rests on SIM (§6.1, §10).
6. **Graduation is fast and permissionless.** `migrate_v2` has been called by many different signers and runs **0–6 slots** after the completing buy: frequently in the same slot, but always as a separate transaction in my sample. The canonical PumpSwap pool is `PDA(pump_amm, ["pool", 0u16 LE, PDA(pump, ["pool-authority", mint]), mint, WSOL])`.
   - **New (undocumented):** normal-coin pools migrated on 2026-09-23 and 09-26 carry `virtual_quote_reserves` ≈ 17.58 SOL, with that amount of the curve's real SOL routed to a "boost" vault. The amount is **`floor(migrated quote × 2069/10000)`** (20.69%). That matched all 130 non-mayhem migrations checked on 2026-09-27; mayhem coins get none (§7.4).
7. **Admins can change a lot without notice.** They can change fees (up to 10,000 bps total), fee-recipient lists, a curve's `creator`, mayhem-coin virtual reserves (via the mayhem program) and the program code itself. There is **no trading pause flag** in `Global`.

---

## 1. Program ID, IDL sources, change history

| Item | Value | Source |
|---|---|---|
| Program ID | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` (same on devnet) | [DOCS] `docs/PUMP_PROGRAM_README.md`; mainnet `getAccountInfo` → owner `BPFLoaderUpgradeab1e11111111111111111111111`, executable |
| ProgramData | `B5MvUwXdiW1NMM6QFFD3ssPKBujD4zMohncbM73Z2BQu` (10,485,760 bytes) | mainnet `getAccountInfo(6EF8…)` bytes 4..36 |
| Upgrade authority | `7gZufwwAo17y5kg8FMyJy2phgpvv9RSdzWtdXiWHjFr8`: system-owned and off-curve, i.e. a Squads v4 vault. Upgrades are executed through `SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf` `VaultTransactionExecute`. | ProgramData header; tx `4WRMFG9YnYqbouLzayMGUjJTN3rKdEcApaxdekf5mAxBsx4NdfgKVXTuQZhmWvEa5uLJCVQ9DcSarEmvfQF133kq` (log: "Upgraded program 6EF8…") |
| Last deploy | slot 449,734,335 = **2026-09-23 14:45:19 UTC** | ProgramData `slot` field; same tx |
| Upgrade cadence | Successful upgrades touching ProgramData on 2026-09-23, 09-15, 09-12, 09-09, 08-14, 07-15, 05-25, 05-21, 05-20, 05-19, 05-14, 05-07, 04-30, 04-28, 04-20, … (about 2–4 per month) | `getSignaturesForAddress(B5MvU…)` |
| Authoritative IDL (GitHub) | `idl/pump.json` (+ `pump.ts`), last changed in commit `e0687ae9b7e0…` "refresh pump, pump_amm and pump_fees IDLs", **2026-09-12** | [DOCS] `git log -- idl/pump.json` |
| On-chain Anchor IDL | account `AYgC53tU5BbP2NAnv5nConJxAdpQZctvmZK88pu69xRs` (`createWithSeed(PDA([],program), "anchor:idl", program)`), IDL authority `92gWFZMpGhWzVq5tjBnJxF9VnFwE3ydB2idJp9z2D9Nr`, last written **2026-09-12 18:02:23 UTC** (tx `4BAVCvQjCzoXwpray1Ghrnkfr29JKXxmMNa55Vyx2GM5c6Rir7epyLmDtj5sGsq8m9JhiM8BCSSPT91xSqGM4MtS`). After zlib-decompression, its 47 instructions, all types and 94 errors are **identical** to GitHub `idl/pump.json` (docs strings excluded). | mainnet RPC + local diff |
| Other official sources | TS SDK `@pump-fun/pump-sdk@2.0.0` (2026-09-13); Rust `pump-rust-client@0.1.13` (2026-09-09). The Rust crate ships `artifacts/pump.so`, which predates the 09-12/09-15/09-23 upgrades. | npm registry, crates.io |

**Announced changes** ([DOCS] `README.md`):

- **Adopt the v2 trade instructions.** Pump "urge[s] all integrations to switch" to `buy_v2`, `sell_v2` and `buy_exact_quote_in_v2`, which have no optional accounts and one account order for all coins. The legacy instructions "will continue to work". USDC-paired coins **cannot** be traded with the legacy instructions.
- **USDC and custom quote mints are live.** `Global.whitelisted_quote_mints = [USDC]`, and custom-pair (`quote-control`) mints also exist. My sample included `buy_v2` / `sell_v2` with quote mints `EPjF…Dt1v` (USDC), `A7bdiYdS…`, `PEPEqnuu…` and `pumpCmXq…` (PUMP).
- **2026-09-12:** holder-rewards coins added and cashback deprecated. The docs say there are "no trade interface changes".
- **2026-04-28 (already past):** a breaking upgrade added `buyback_fee_recipient` as a required trailing account after `bonding_curve_v2` ([DOCS] `docs/BREAKING_FEE_RECIPIENT.md`). The notice was committed on 2026-04-22 21:06 UTC (commit `7de0b959fa`) for an upgrade at 2026-04-28 16:00 UTC, i.e. about 6 days' warning. `bonding_curve_v2` itself was already required before that date (§3.2).
- **No future-dated breaking change is announced** in the repo as of HEAD.
- **The 09-15 and 09-23 upgrades are undocumented.** Their content is **UNVERIFIED**. The pool-boost behavior at migration (§7.4) is also undocumented, and was already live before the 09-23 upgrade.

---

## 2. Account layouts

### 2.1 `BondingCurve`: PDA `["bonding-curve", mint]` under pump

Discriminator `17b7f83760d8ac60` = `sha256("account:BondingCurve")[..8]` [IDL]. Fields come from [IDL] `types.BondingCurve`, and the offsets were checked by decoding live accounts:

| Offset | Size | Field | Type | Notes |
|---:|---:|---|---|---|
| 0 | 8 | discriminator | `[u8;8]` | `17 b7 f8 37 60 d8 ac 60` |
| 8 | 8 | `virtual_token_reserves` | u64 | |
| 16 | 8 | `virtual_quote_reserves` | u64 | Formerly `virtual_sol_reserves` (renamed, same slot) [DOCS README] |
| 24 | 8 | `real_token_reserves` | u64 | Tokens still sellable by the curve |
| 32 | 8 | `real_quote_reserves` | u64 | Formerly `real_sol_reserves`. For SOL curves, account lamports = this + rent. |
| 40 | 8 | `token_total_supply` | u64 | 1e15 for normal coins |
| 48 | 1 | `complete` | bool | |
| 49 | 32 | `creator` | Pubkey | Creator-fee recipient key. `creator_vault` is derived from it. **Admin-mutable.** |
| 81 | 1 | `is_mayhem_mode` | bool | |
| 82 | 1 | `is_cashback_coin` | bool | Cashback is deprecated for new coins |
| 83 | 32 | `quote_mint` | Pubkey | `Pubkey::default()` (all zeros) = SOL |
| 115 | 8 | `creator_fee_bps` | u64 | Only for custom-pair coins; 0 = use schedule |
| 123 | 1 | `can_edit_creator_fee` | bool | Reserved, always false |
| 124 | 1 | `is_holder_reward` | bool | |
| 125.. | – | zero padding | | Allocation is **151 bytes** (new coins) or **150 bytes** (pre-2025 coins extended for the creator-fee upgrade) |

The docs say accounts written before a field existed are shorter, and missing trailing fields should be read as 0/false/default ([DOCS] `PUMP_PROGRAM_README.md`). `buy_v2`/`sell_v2` "top up rent to make the account data length 115 bytes if it is smaller" ([DOCS] `instructions/BUY.md` row 11). The legacy path requires `extend_account` when the account is shorter than 150 bytes ([DOCS] `PUMP_CREATOR_FEE_README.md`).

**Live, un-graduated example** (decoded 2026-09-26):

- Account `8xXtX7wc2RjCHGQdzQB5akLFCu96PMj95jZAxpkCF5TB` (mint `HcvPkKKuP1o8qDHbPw5yypJzp7WAcMofqVQzhvEtRQxm`, Token-2022).
- 151 bytes, owner pump, lamports 38,354,443,065.
- `vT` = 470,937,460,709,830; `vQ` = 68,353,025,745; `rT` = 191,037,460,709,830; `rQ` = 38,353,025,745; supply 1e15; `complete` = false; `creator` = `GaJtj7V9HnvSE5MKgW2Td2uzwEpypJTG4VKW9wYQbjwW`; all flags false; `quote_mint` = default; trailing 26 bytes zero.
- lamports − `rQ` = 1,417,320 = `getMinimumBalanceForRentExemption(151)`.
- This coin graduated within minutes of the snapshot (a later SIM returned 6005).

**Completed and migrated example:**

- Account `C63Y7RiGGyXkqEeVyn9umYvUmrdwcPU66YUSWYwuz8WJ` (mint `G6vNKu5xhB2RN7tK6HXRPTG5pYgy5nc6CEnWqPuG2edn`), 151 bytes, lamports 1,417,320 (rent only).
- All four reserves are **0**, `complete` = true, `token_total_supply` = 1e15, `creator` = `4uPAsYhKdWs5MYPeFVADnDQ1D9V3LjocoywFyUy8vK1B`.
- Just before migration (post-state of the completing buy `SfdLRmJp1663rgig9YMK9UgsyYyVWcDfS51oyeU3Pf6j3Ms66e2E75v6pkeWGwiwZf3Rc2NZNEN6JMUvK5WruHd`): `rT` = 0, `vT` = 279,900,000,000,000, `vQ` = 115,005,359,346, `rQ` = 85,005,359,346.
- So after `migrate`, both virtual reserves are zeroed. The SDKs treat `virtual_token_reserves == 0` as "migrated" ([RS] `math/bonding_curve.rs`).

**Legacy SPL-Token example:** `Coyj3LtKn1BNSgWc9HsGK5SKoGfEoDaymig4wrN6pump` (mint owner `Tokenkeg…`) has a **150-byte** curve, `complete` = true.

### 2.2 `Global`: PDA `["global"]` = `4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf`

1,087 bytes, discriminator `a7e8e8b1c86c727f`. This is a live decode (2026-09-26) of the fields that matter for fees and limits [IDL `types.Global`]:

| Off | Field | Live value | Meaning |
|---:|---|---|---|
| 9 | `authority` | `FFWtrEQ4B4PKQoVuHYzZq8FabGkVatYzDpEVHsK5rrhF` | Can call `set_params` and the toggles |
| 41 | `fee_recipient` | `62qc2CNX…NgV` | Normal fee recipient #0 |
| 73 | `initial_virtual_token_reserves` | 1,073,000,000,000,000 | |
| 81 | `initial_virtual_sol_reserves` | 30,000,000,000 (30 SOL) | Not used for mayhem coins (see §8) |
| 89 | `initial_real_token_reserves` | 793,100,000,000,000 | 793.1M tokens sold on the curve |
| 97 | `token_total_supply` | 1,000,000,000,000,000 | |
| 105 | `fee_basis_points` | **95** | Fallback only; the real fee comes from `FeeConfig` (§2.3) |
| 113 | `withdraw_authority` | `39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg` | Passed (non-signer) to `migrate_v2` |
| 145 | `enable_migrate` | true | IDL doc: "Unused" |
| 146 | `pool_migration_fee` | 15,000,001 | Lamports kept from the curve at migration |
| 154 | `creator_fee_basis_points` | 5 | Fallback only |
| 162 | `fee_recipients[7]` | `7VtfL8…`, `7hTckg…`, `9rPYyA…`, `AVmoTt…`, `CebN5W…`, `FWsW1x…`, `G5UZAV…` | With `fee_recipient`, the 8 normal recipients |
| 386 | `set_creator_authority` | `39azU…` | Can rewrite `BondingCurve.creator` |
| 418 | `admin_set_creator_authority` | `UqN2p5bAzBqYdHXcgB6WLtuVrdvmy9JSAtgqZb3CMKw` | Community takeover (`admin_cto`) |
| 450 | `create_v2_enabled` | true | |
| 451 | `whitelist_pda` | `BwWK17cbHxwWBKZkUYvzxLcNQ1YVyaFezduWbtm2de6s` | = mayhem program `["sol-vault"]` PDA. Trades by it are fee-free (§8). |
| 483 / 516 | `reserved_fee_recipient` / `reserved_fee_recipients[7]` | `GesfTA3X…` + 7 | Must be used as `fee_recipient` for mayhem coins |
| 515 | `mayhem_mode_enabled` | true | |
| 740 | `is_cashback_enabled` | true | (`create_v2` still rejects new cashback coins, per the docs) |
| 741 | `buyback_fee_recipients[8]` | `5YxQFd…`, `9M4giF…`, `GXPFM2…`, `3BpXnf…`, `5cjcW9…`, `EHAAiT…`, `5eHhjP…`, `A7hAgC…` | |
| 997 | `buyback_basis_points` | **5000** | Share of the protocol fee routed to the buyback recipient |
| 1005 | `initial_virtual_quote_reserves` | 4,292,000,000 | For USDC curves (6-decimal USDC) |
| 1013 | `whitelisted_quote_mints[1]` | USDC `EPjFWdd5…Dt1v` | |
| 1045 | `creator_fee_configurable` | true | Gate for per-coin `creator_fee_bps` |
| 1046 | `max_configurable_creator_fee_bps` | 300 | |
| 1054 / 1086 | `holder_reward_claim_authority` / `is_holder_reward_enabled` | `HckQ93X…` / true | |

The docs' fee-recipient lists ([DOCS] `docs/FEE_RECIPIENTS.md`) match these live values.

### 2.3 `FeeConfig`: PDA `["fee_config", pump_program_id]` under `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ` = `8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt`

The account is 4,097 bytes and owned by the pump-fees program. Discriminator: `8f3492bbdb7b4c9b`.

Layout: `bump u8`, `admin Pubkey`, `flat_fees Fees`, `fee_tiers Vec<FeeTier>`, `stable_fee_tiers Vec<FeeTier>`, `exotic_flat_fees Fees`. `FeeTier` = `{market_cap_lamports_threshold: u128, fees: Fees}`, and `Fees` = `{lp_fee_bps, protocol_fee_bps, creator_fee_bps}` (all u64) [IDL].

Live decode (2026-09-26):

- `admin` = `FFWtrEQ4…` (the same key as `Global.authority`).
- `flat_fees` = `exotic_flat_fees` = {0, 95, 30}.
- `fee_tiers` = **one tier**: threshold 0 → {lp 0, protocol 95, creator 30}.
- `stable_fee_tiers` = the same single tier.

**Market-cap tiers are a live mechanism with one tier today.** [DOCS] `docs/FEE_PROGRAM_README.md` (the 2025-09-01 change) introduced them. Pump states that "if you implement the fee logic correctly, any future change to the fee tiers structure above should not affect your code". The admin can replace the tiers at any time with `update_fee_config` / `upsert_fee_tiers`, subject to "Fee total must not exceed 10_000bps" and tiers sorted ascending (pump-fees IDL errors 6005/6006) [IDL `idl/pump_fees.json`].

### 2.4 Other accounts

- **`UserVolumeAccumulator`**: PDA `["user_volume_accumulator", user]` under pump, 137 bytes, discriminator `56ff700e66359afa`. It is auto-created by the first buy (legacy `buy` and all v2 instructions). **`user` pays**: 1,346,200 lamports at today's rent. The docs' "0.0018444 SOL" figure is stale: `getMinimumBalanceForRentExemption(137)` returned 1,346,200 and `(0)` returned 650,240. Evidence: SIM of a legacy buy from a fresh user shows `createAccount{source: user, space: 137, owner: pump}`. The account can be closed by `user` with `close_user_volume_accumulator` [IDL].
- **`GlobalVolumeAccumulator`**: `["global_volume_accumulator"]` = `Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y`, 600 bytes.
- **`bonding_curve_v2`**: PDA `["bonding-curve-v2", mint]` under pump. Legacy `buy`/`sell` require it as a remaining account (error 6074 `InvalidBondingCurveV2`) [IDL]. On mainnet it is **uninitialized**: `getAccountInfo` returns null for `HFHUPvDs…`, `7TEHtusE…` and `3hm9wuR4…`, so it is only seed-checked. Its purpose is **UNVERIFIED**.
- **`creator_vault`**: PDA `["creator-vault", bonding_curve.creator]` under pump. It is a system-owned, zero-data account that accumulates creator-fee lamports; for example `gU8aVSBRSbi8oDEJgdCsmwcVRrCkzNvbhpKTR7ZDiZV` holds 1.53 SOL.
- **`sharing_config`**: PDA `["sharing-config", mint]` under pump-fees. It may be uninitialized; handlers check `data_is_empty()` [IDL docs on `buy_v2.sharing_config`]. When a creator has migrated to fee sharing, `BondingCurve.creator` **is** the sharing-config PDA. Example: in `sell_v2` tx `535njpqhYVXzwZmZLNCmKqiqNKfpAUYBndXoibtBBm7ZrNxF6ieXNAoHYaEhtsRnLjK3i73pB2bcGHnqeMoyKrjJ`, the `TradeEvent.creator` is `3No4qq4t…`, which equals the account passed as `sharing_config`.

---

## 3. Instructions short.win needs

All discriminators are `sha256("global:<name>")[..8]`; I checked each against [IDL]. All integers are little-endian.

- `OptionBool` / `OptionU64` are **newtype structs** (1-byte bool / 8-byte u64), not `Option<T>`. A mainnet `buy` with `track_volume` has 25 bytes of data; older clients omit the trailing byte (24 bytes) and the call still succeeds.
- `Buy ⇄ Sell` gotcha: in the legacy instructions, `creator_vault` and `token_program` **swap positions** (buy: 8 = token_program, 9 = creator_vault; sell: 8 = creator_vault, 9 = token_program).

### 3.1 PDA and fixed-address table

| Account | Derivation | Mainnet address |
|---|---|---|
| global | pump `["global"]` | `4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf` |
| event_authority | pump `["__event_authority"]` | `Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1` |
| global_volume_accumulator | pump `["global_volume_accumulator"]` | `Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y` |
| fee_config | pump-fees `["fee_config", 6EF8…F6P]` | `8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt` |
| fee_program | fixed | `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ` |
| bonding_curve | pump `["bonding-curve", mint]` | per mint |
| bonding_curve_v2 | pump `["bonding-curve-v2", mint]` | per mint (legacy remaining account) |
| associated_bonding_curve / associated_base_bonding_curve | ATA(owner = bonding_curve, token_program = mint's program, mint) | per mint |
| associated_quote_bonding_curve | ATA(bonding_curve, quote_token_program, quote_mint); for SOL coins quote_mint = WSOL | seed-checked only for SOL coins |
| creator_vault | pump `["creator-vault", bonding_curve.creator]` | read `creator` from the curve **at execution time** |
| associated_creator_vault | ATA(creator_vault, quote_token_program, quote_mint) | |
| user_volume_accumulator | pump `["user_volume_accumulator", user]` | init-if-needed, payer = user |
| associated_user_volume_accumulator | ATA(user_volume_accumulator, quote_token_program, quote_mint) | |
| sharing_config | pump-fees `["sharing-config", mint]` | |
| associated_quote_fee_recipient / associated_quote_buyback_fee_recipient | ATA(recipient, quote_token_program, quote_mint) | |
| mint_authority (create only) | pump `["mint-authority"]` | `TSLvdd1pWpHVjahSpsvCXUbgwsL3JAcvokwaKt1eokM` |
| pool_authority (migration) | pump `["pool-authority", mint]` | |

These derivations come from [IDL] `pda` seeds and [TS-SDK] `src/pda.ts`, and I recomputed them locally against the addresses seen in the transactions below.

**Recipient rules.**

- `fee_recipient` must be one of the 8 normal recipients (`Global.fee_recipient` plus `fee_recipients[7]`). For mayhem coins it must be one of the 8 reserved recipients instead. The wrong kind fails with `6000 NotAuthorized` at `programs/pump/src/fee_recipient.rs:19` (SIM, mayhem mint `FjwDVKUVaAYXy2DUMPPT1rKmEhowCgiSU6eMhZGLpump`).
- `buyback_fee_recipient` must be one of `Global.buyback_fee_recipients[8]` (error 6057) [DOCS `FEE_RECIPIENTS.md`, IDL].

### 3.2 Legacy `buy`: exact tokens out, capped SOL in

- Discriminator `66063d1201daebea`.
- Args: `amount: u64` (base units out), `max_sol_cost: u64` (includes fees), `track_volume: OptionBool`.

| # | Account | Signer | Writable | Notes |
|---:|---|:-:|:-:|---|
| 0 | global | | | |
| 1 | fee_recipient | | W | normal/reserved list |
| 2 | mint | | | |
| 3 | bonding_curve | | W | |
| 4 | associated_bonding_curve | | W | |
| 5 | associated_user | | W | receives tokens; must already exist (not created by pump) |
| 6 | user | **S** | W | pays SOL via System transfers |
| 7 | system_program | | | |
| 8 | token_program | | | Tokenkeg or Token-2022 (the mint's owner) |
| 9 | creator_vault | | W | |
| 10 | event_authority | | | |
| 11 | program | | | pump |
| 12 | global_volume_accumulator | | | (clients often mark it W) |
| 13 | user_volume_accumulator | | W | created if missing |
| 14 | fee_config | | | |
| 15 | fee_program | | | |
| 16 | *remaining:* bonding_curve_v2 | | | required; **pre-dates 2026-04-28** ([DOCS] `BREAKING_FEE_RECIPIENT.md`: "All accounts till bonding-curve-v2 and pool-v2 remain the same"). It first appears as a buy/sell remaining account in [TS-SDK] `@pump-fun/pump-sdk@1.29.0` (2026-02-28); when the program began to require it is **UNVERIFIED** |
| 17 | *remaining:* buyback_fee_recipient | | W | required since 2026-04-28 (added "AFTER the bonding-curve-v2 account") |

Confirmed on mainnet in:

- Top-level tx `5BLMnSVa2AiQLphAWyUevpkx5t1AiGLcjNuQW97txZ3skPTbT4SK8qFBZEMbFQeJdQP4pDV3hmNfqeYwBKXufXY` (18 accounts in exactly this order, 25-byte data).
- CPI tx `5hRKzXWt67F1xT2GS6UVexZpZXeiKTt4M7hW72LN2jVXCA7WxqmoMr83XmWrBYMPwGeJHJR6jGomAiU5Cwdp9eYT` (called from program `FAdo9NCw1ssek6Z6yeWzWjhLVsr8uiCwcWNUnKgzTnHe`).

Order sources: [IDL]; [RS] `sdk/pump_legacy.rs::buy_instruction`; [TS-SDK] `getBuyInstructionInternal`; [DOCS] `BREAKING_FEE_RECIPIENT.md` ("Buy instructions should have 18 accounts in total").

### 3.3 Legacy `sell`: exact tokens in, floor on SOL out

- Discriminator `33e685a4017f83ad`.
- Args: `amount: u64`, `min_sol_output: u64` (net of fees).

| # | Account | S | W |
|---:|---|:-:|:-:|
| 0 | global | | |
| 1 | fee_recipient | | W |
| 2 | mint | | |
| 3 | bonding_curve | | W |
| 4 | associated_bonding_curve | | W |
| 5 | associated_user (token source; `user` is the transfer authority) | | W |
| 6 | user | **S** | W |
| 7 | system_program | | |
| 8 | **creator_vault** | | W |
| 9 | **token_program** | | |
| 10 | event_authority | | |
| 11 | program | | |
| 12 | fee_config | | |
| 13 | fee_program | | |
| 14 | *remaining (cashback coins only):* user_volume_accumulator | | W |
| 14/15 | *remaining:* bonding_curve_v2 | | |
| 15/16 | *remaining:* buyback_fee_recipient | | W |

- Mainnet: top-level tx `cHHjJZFF6jtA5H8gpGBDXaFabRbKhK9qxmgZMP4c28wtR9rXapjthV4hia7PFvg4xKpakyaPFxv3Fi8NRL2FXhc` (16 accounts); CPI from the mayhem program in tx `BhxQktaHEpmw4dkiubTXVvUqpxhHNfTKLiEwzCDb9hVJA8aMEd6kdqWRdzYX8ySWwzCSo9CvnmeaFrBTkbgctE1`.
- Cashback coins: [IDL] `sell` docs say that if the UVA is omitted, the creator fee "falls back to … creator_vault". There is also an error `6072 MissingCashbackAccounts`, so to be safe, always pass it when `is_cashback_coin` is set.

### 3.4 Legacy `buy_exact_sol_in`: exact SOL in, floor on tokens out

- Discriminator `38fc74089edfcd5f`.
- Args: `spendable_sol_in: u64` (fees included), `min_tokens_out: u64`, `track_volume: OptionBool`.
- Accounts: **identical to `buy`** (16 + the same 2 remaining).
- Mainnet: top-level tx `3pu4FB7rvXEhsZP3fSKfFHFkaF2LapShP2t3GpSehm6u3jHNvRpxrrH4hwUqvG2WuFsp3KrvYoByWk5E6FxHPFYs`.
- Rent is **separate**: "make sure the instruction's payer has enough SOL to cover rent for creator_vault … user_volume_accumulator" [IDL docs].

### 3.5 `buy_v2` / `buy_exact_quote_in_v2` / `sell_v2`: unified interface, recommended by Pump

- Discriminators:
  - `buy_v2` `b817ee6167c5d33d`, args `(amount u64, max_sol_cost u64)`
  - `buy_exact_quote_in_v2` `c2ab1c46684d5b2f`, args `(spendable_quote_in u64, min_tokens_out u64)`
  - `sell_v2` `5df6823ce7e940b2`, args `(amount u64, min_sol_output u64)`
- None of them takes remaining accounts.
- For SOL coins, pass `quote_mint` = WSOL `So11111111111111111111111111111111111111112` and `quote_token_program` = Tokenkeg. All the quote ATAs are then only seed-checked and need not exist; trading still uses native lamports [DOCS README "SOL-Paired Meme Coins"].

| # | buy_v2 / buy_exact_quote_in_v2 | sell_v2 | S | W |
|---:|---|---|:-:|:-:|
| 0 | global | global | | |
| 1 | base_mint | base_mint | | |
| 2 | quote_mint | quote_mint | | |
| 3 | base_token_program | base_token_program | | |
| 4 | quote_token_program | quote_token_program | | |
| 5 | associated_token_program | associated_token_program | | |
| 6 | fee_recipient | fee_recipient | | W |
| 7 | associated_quote_fee_recipient | associated_quote_fee_recipient | | W |
| 8 | buyback_fee_recipient | buyback_fee_recipient | | W |
| 9 | associated_quote_buyback_fee_recipient | associated_quote_buyback_fee_recipient | | W |
| 10 | bonding_curve | bonding_curve | | W |
| 11 | associated_base_bonding_curve | associated_base_bonding_curve | | W |
| 12 | associated_quote_bonding_curve | associated_quote_bonding_curve | | W |
| 13 | user | user | **S** | W |
| 14 | associated_base_user | associated_base_user | | W |
| 15 | associated_quote_user | associated_quote_user | | W |
| 16 | creator_vault | creator_vault | | W |
| 17 | associated_creator_vault | associated_creator_vault | | W |
| 18 | sharing_config | sharing_config | | |
| 19 | **global_volume_accumulator** | user_volume_accumulator | | –/W |
| 20 | user_volume_accumulator | associated_user_volume_accumulator | | W |
| 21 | associated_user_volume_accumulator | fee_config | | W/– |
| 22 | fee_config | fee_program | | |
| 23 | fee_program | system_program | | |
| 24 | system_program | event_authority | | |
| 25 | event_authority | program | | |
| 26 | program | – | | |

Both buy variants have 27 accounts; `sell_v2` has 26, because it has no `global_volume_accumulator`. Sources: [IDL]; [DOCS] `instructions/BUY.md`, `instructions/SELL.md`; [RS] `sdk/pump_v2.rs`.

Mainnet confirmations:

- `buy_v2` (SOL coin, top-level, 27 accounts in exactly the order above, 89,448 CU): `5zNHaL6fDfeFCJEPZWHqt6pjCT7XTY7cBwFq1KvXsE9fx9krKYRZK3epJtvseBwidyXUb7zsoEdrtRyPCLpDeSdx`.
- `sell_v2` (SOL coin, top-level): `535njpqhYVXzwZmZLNCmKqiqNKfpAUYBndXoibtBBm7ZrNxF6ieXNAoHYaEhtsRnLjK3i73pB2bcGHnqeMoyKrjJ`.
- `buy_v2` (USDC-paired coin, top-level, in the same tx as its `create_v2`): `2XaZGFPQTTaVfH5uVC5yzoo2xr98td8AbyCGBfcR3ReH3e26YY7puDxJwzEoHNRtbYewsLqphQCodSjbN3ScpACZ`.
- `buy_exact_quote_in_v2` (SOL coin, CPI): `3QHQnT3SP38mNgWYsVZh9h387HkXsqiAQzhE6GEqv3HJXXaoSe8WuWmdwD3oQyqNFoDA4Uc5txcycgXubVUY8Afz`.
- SIM: `buy_v2` and `sell_v2` with SOL quote succeed with exactly this order (§6).

`TradeEvent.ix_name` reports `"buy"`, `"sell"`, `"buy_exact_sol_in"` or `"buy_exact_quote_in"` (the v2 names are not reported).

### 3.6 Instructions used around graduation

- **`migrate_v2`** (`bbcb121fceedfe29`, no args): 27 IDL accounts plus 2 remaining.
  - The signer is `user` (index 7, W). `withdraw_authority` (index 1) must equal `Global.withdraw_authority` but is **not** a signer.
  - `pool` = pump-amm `["pool", [0,0], pool_authority, base_mint, quote_mint]`; `pool_authority` = pump `["pool-authority", mint]`; `lp_mint` = pump-amm `["pool_lp_mint", pool]`. The rest are listed in [IDL].
  - Remaining accounts observed in `3u74vnKGX9XmZRViv3qCvhfDQUUY7SEZtyrozhCfPUBaHVk1ZNr62vVnPAVTW8sFzVMrz7e1zco5piqDRdHp7yUd`: (27) pump-amm `["boost_vault", pool]` PDA and (28, W) its WSOL ATA.
  - The older `migrate` (`9beae792ec9ea21e`, 25 accounts) is still accepted: tx `2ZABEL3WdVacvjARcf6UusX1DcTUz8FLdDzvspFuXpscJndMkk67Kcr5yHEoQqgQvaXQL5E5cbMrmtPE1nLA72nD`.
- **`extend_account`** (`ea66c2cb96483ee5`): `[account W, user S W, system_program, event_authority, program]`.
- **`init_user_volume_accumulator`** (`5e06ca73ff60e8b7`): `[payer S W, user, user_volume_accumulator W, system_program, event_authority, program]`. The payer can differ from the user, so the UVA can be pre-created.

---

## 4. Curve math: what the program does, bit-exact

Notation, all u64 values from `BondingCurve`: `vT` = `virtual_token_reserves`, `vQ` = `virtual_quote_reserves`, `rT` = `real_token_reserves`. Arithmetic is integer (u128 intermediates), and `ceil(a/b) = (a+b−1)/b`.

**Fee rates at trade time.** Pump CPIs `pump-fees::get_fees_with_quote_mint`; the log line "Instruction: GetFeesWithQuoteMint" appears in every trade.

1. `mcap = vQ * S / vT`. `S` = **1e15 (fixed) for non-mayhem coins**, and the mint supply for mayhem coins ([TS-SDK] `fees.ts::getFee`, [RS] `math/bonding_curve.rs::fee_for_quote`).
2. Select the schedule by `quote_mint`:
   - SOL-like (default / WSOL / Token-2022 native): `fee_tiers`
   - USDC: `stable_fee_tiers`
   - anything else: `exotic_flat_fees`, or `flat_fees` if the exotic fees are all zero
3. Tier choice (`calculate_fee_tier`): if `mcap < tiers[0].threshold`, use tier 0; otherwise use the **last** tier with `threshold ≤ mcap`.
4. `creator_bps = schedule.creator_fee_bps`, **unless** `Global.creator_fee_configurable && curve.creator_fee_bps != 0`, in which case use `curve.creator_fee_bps`. The per-coin override applies to custom pairs only.
5. **If `curve.creator == Pubkey::default()` the creator fee is 0.**

Sources: [RS] `math/fees.rs`, [TS-SDK] `fees.ts`, [DOCS] `FEE_PROGRAM_README.md`. Today this gives `protocol_bps = 95` and `creator_bps = 30` for every SOL coin, and every `TradeEvent` observed shows `fee_basis_points: 95` and `creator_fee_basis_points: 30`.

**Buy exact out: `buy` / `buy_v2(amount)`.**

```
require 0 < amount <= rT            (else 6020 BuyZeroAmount / 6021 NotEnoughTokensToBuy: REVERT, no cap)
sol       = floor(amount * vQ / (vT - amount)) + 1          // rounds against buyer
protocol  = ceil(sol * protocol_bps / 10_000)
creator   = ceil(sol * creator_bps  / 10_000)               // 0 if creator == default
total     = sol + protocol + creator     ; require total <= max_sol_cost (else 6002 TooMuchSolRequired)
state:  vQ += sol; rQ += sol; vT -= amount; rT -= amount; if rT == 0 -> complete = true (CompleteEvent)
```

**Sell exact in: `sell` / `sell_v2(amount)`.**

```
sol_gross = floor(amount * vQ / (vT + amount))              // rounds against seller
protocol  = ceil(sol_gross * protocol_bps / 10_000)
creator   = ceil(sol_gross * creator_bps  / 10_000)
net       = sol_gross - protocol - creator ; require net >= min_sol_output (else 6003 TooLittleSolReceived)
state:  vQ -= sol_gross; rQ -= sol_gross; vT += amount; rT += amount
```

**Exact SOL in: `buy_exact_sol_in` / `buy_exact_quote_in_v2(spendable)`.** From [IDL] docs, verified against mainnet:

```
net  = floor(spendable * 10_000 / (10_000 + protocol_bps + creator_bps))
fees = ceil(net*protocol_bps/1e4) + ceil(net*creator_bps/1e4)
if net + fees > spendable: net -= (net + fees - spendable)
tokens = floor((net - 1) * vT / (vQ + net - 1))            // curve credited with `net`, priced on net-1
require tokens >= min_tokens_out (6042) and tokens <= rT (6021: REVERT, see §7)
```

**Where the fees go.**

- `buyback_fee = floor(protocol * buyback_basis_points / 10_000)` goes to `buyback_fee_recipient`; `fee_recipient` gets `protocol − buyback_fee`.
- The trader's cost is unchanged by the split.
- Observed: protocol fee 1,970,359 splits into buyback 985,179 and fee recipient 985,180 (SIM); 143,747 splits into 71,873 and 71,874 (tx `535njpqh…`, recipient lamport deltas).
- For mayhem coins, `buyback_fee` is 0 (SIM on `FjwDVKUV…pump`: protocol 95 + creator 30 bps, buyback_fee 0). The rounding direction of the split is inferred from these data points; there is no source code.

### 4.1 Verification against mainnet trades

For each trade below I back-computed the pre-trade state from the `TradeEvent` post-state (post reserves minus or plus the deltas). Every row reproduced exactly.

| Tx (ix) | Inputs | Computed | Event |
|---|---|---|---|
| `5hRKzXWt…p9eYT` (`buy`) | amount 2,528,780,625,270; vQ 41,131,160,096; vT 782,618,335,346,345 | sol 133,333,001; protocol 1,266,664; creator 400,000 | identical |
| `3pu4FB7r…xHPFYs` (`buy_exact_sol_in`) | spendable 340,302,691 | net 336,101,423 → 336,101,422 after the step-3 adjustment; tokens 3,092,473,240,142; total paid = 340,302,691 exactly | identical |
| `3QHQnT3S…VUY8Afz` (`buy_exact_quote_in_v2`) | spendable 87,775,543 | net 86,691,894; tokens 230,647,741,584 | identical |
| `38PUmRjYaproDGMyAjxRWvnWmrb4gqJ3heVG1cdCFug25ERF4h5Rkuj84ALDBptERvvstkKUvAn2h7p6jnvBsX5N` (`buy_exact_quote_in_v2`) | spendable 85,936,276 | net 84,875,333; tokens 226,167,326,976 | identical |
| `cHHjJZFF…RL2FXhc` (`sell`) | amount 580,183,108,894 | gross 217,992,813; protocol 2,070,932; creator 653,979 | identical; curve lamports −217,992,813 |
| `535njpqh…MoyKrjJ` (`sell_v2`) | amount 293,336,418,455 | gross 15,131,225; net 14,942,084 | identical; user +14,942,084 incl. the 85,000 tx fee |

### 4.2 Worked example on a live account (slot 450,803,590, `processed`)

Curve `28WmHtX9opx4M6z1TG6QpAV69gxjmd1yyhMZqyY9bj9a` (mint `BpZgKbs2NfMd6Cbra8ccTyzASs1cMRYVsMNL7A4Xpump`, Token-2022, not mayhem, SOL-quoted):

- `vT` = 824,130,602,106,172, `vQ` = 39,059,343,535, `rT` = 544,230,602,106,172, `rQ` = 9,059,343,535.
- Spot = `vQ/vT` = 0.047395 SOL per 1M tokens. `mcap` = 39,059,343,535 × 1e15 / 824,130,602,106,172 = **47,394,604,004 lamports** (≈ 47.39 SOL). That lands in tier 0: 95 / 30 bps.

`Q = 10,000,000 tokens = 10,000,000,000,000 base units`:

| | Buy Q (close/liquidation) | Sell Q (open) |
|---|---:|---:|
| curve SOL | 479,767,539 | 468,264,123 (gross) |
| protocol fee (95 bps, ceil) | 4,557,792 (of which buyback 2,278,896) | 4,448,510 |
| creator fee (30 bps, ceil) | 1,439,303 | 1,404,793 |
| **trader pays / receives** | **485,764,634** | **462,410,820** |
| avg price vs spot | +1.228% impact | – |

- **An immediate sell-then-buy of Q on the same curve costs ≈ 2.53% of the sale proceeds** (fees only; the price impact cancels). The sale nets 462,410,820. The buyback of Q is then priced on the *post-sell* state (`vT + Q`, `vQ − 468,264,123`): curve SOL 468,264,124, fees 4,448,510 + 1,404,793, total **474,117,427**, i.e. 11,706,607 more than the proceeds = 2.53%. The buy returns the curve to its original reserves (1 lamport of rounding).
- **5.05% is a different quantity:** the gap between buying and selling Q from the *same* pre-trade state (485,764,634 vs 462,410,820), i.e. the bid/ask spread for size Q. That is what a short pays if other flow restores the curve to its pre-open state before the buyback.
- (Both recomputed with the §4 formulas: buy `floor(q·vQ/(vT−q)) + 1`, sell `floor`, fees `ceil`, which match [TS-SDK] `src/bondingCurve.ts` and `src/fees.ts`.)
- **SIM at the same moment:** legacy `buy(amount=Q)` emitted `TradeEvent{sol_amount: 479,767,539, fee: 4,557,792, creator_fee: 1,439,303, buyback_fee: 2,278,896}`, identical to the computed values.

---

## 5. Token programs and mint state

- **`create_v2` makes Token-2022 mints.** This is the only create path used in my sample; I observed `create_v2` inside txs such as `2XaZGFPQ…ScpACZ`, and every mint in the trade sample was owned by `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`.
  - [DOCS] `instructions/COIN_CREATION.md`: "New Token-2022 mint account … `decimals = 6` … metadata pointer set to `mint`".
  - The legacy `create` (SPL Token + Metaplex) is still in the IDL and marked `#[deprecated]` in [RS] `pump_legacy.rs`. **UNVERIFIED** whether it is still enabled; I saw no call in the sample.
- **Both token programs are in circulation.** Older pump coins are SPL Token, e.g. `Coyj3LtKn1BNSgWc9HsGK5SKoGfEoDaymig4wrN6pump` (mint owner `Tokenkeg…`, no extensions, 150-byte curve, complete). Live curves in the sample were **100% Token-2022**: all 253 decoded curve trades (sample: every 6th of ~2,100 recent successful pump txs) used a Token-2022 base mint, and none used Tokenkeg. The legacy instructions take `token_program` explicitly; v2 takes `base_token_program`.
- **Decoded Token-2022 mints (2026-09-26)**, all with decimals **6**, mint authority **None** (revoked at create; the curve was still live for the first), freeze authority **None**, and extensions **only `MetadataPointer` (self) + `TokenMetadata` (update authority None)**:
  - `HcvPkKKuP1o8qDHbPw5yypJzp7WAcMofqVQzhvEtRQxm`
  - `H4hRz96AMoMLZznhak5Lq2VEE5BHcKg2VBvsPcB1pump`
  - `G6vNKu5xhB2RN7tK6HXRPTG5pYgy5nc6CEnWqPuG2edn`
  - `9usueSYim16JMGk6vVzRzX9jCBDW79byNBsNj37Fpump`
  - `BpZgKbs2NfMd6Cbra8ccTyzASs1cMRYVsMNL7A4Xpump`
- **Supply** is 1,000,000,000,000,000 (1B × 1e6) at creation, but it can **decrease** through burns. Examples: `H4hRz96…` has 999,993,947,790,104, and graduated `G6vNKu5…` has 990,473,535,545,849 (PumpSwap "boost_buy_and_burn", §7.4). **Mayhem coins have 2,000,000,000,000,000**, e.g. `3LkzKcLozAumKvodqFSJz5qudnREsPGg8FT1kUHopump` and `FjwDVKUV…pump`.
- **SPEC §3 filter:** there is no freeze authority, no permanent delegate, no transfer hook and no transfer fee, so standard pump coins pass. short.win's pool vaults and ATAs must use **Token-2022** (`transfer_checked`, 170-byte ATAs costing 1,513,840 lamports at current rent) for these mints, and Tokenkeg for legacy ones.

---

## 6. CPI constraints

1. **`user` may be a PDA signing via `invoke_signed`.** Nothing checks that the account is on-curve, is the fee payer, or is at top level.
   - **Mainnet proof:** the mayhem program `MAyhSmzXzV1pTf7LsNkrNwkWKTo4ougAJ1PPg47MD4e` CPIs `buy` and `sell` with `user` = its PDA `["sol-vault"]` = `BwWK17cbHxwWBKZkUYvzxLcNQ1YVyaFezduWbtm2de6s` (off-curve, system-owned, 0 data). See `2KAAwkwk7x8LTYexDmeRDturS4sd3mtFhFMa33pVT1E1HnozCxae8HVFVkndjTMbhZan4Sgukc3faci7JMkF6mfp` (buy) and `BhxQktaH…gctE1` (sell). That PDA is whitelisted and fee-exempt, so it is a special case.
   - **Mainnet proof for sells by a non-whitelisted PDA** (re-checked 2026-09-27 with `getTransaction`, `maxSupportedTransactionVersion: 1`): tx `zsGhkQcrD6a1kmFaQtQ5ypE9fyZcYJ6puxWmnZceVJ7DKpkmJBaGcVMhiYThrh3zLu2fbzSesQKUBqKiq4vpfHB` (slot 450,802,207). Program `6Vo3245eszAb5wuqEMw8mGdbfRUdKbHhDHP5LcaGuTAB` (h1) → Jupiter `route_v2` (h2) → pump **`sell_v2` (h3)** with `user` (#13) = `EMopL9kdFtQ3AYmsJqYVJYr4bNsgZm44LDvhGEEcSe8K`. That account is off-curve, is not a transaction signer (its signature comes from `invoke_signed` up the chain), is not the whitelisted mayhem PDA, and held **0 lamports before and after** the tx.
     - `sell_v2` credited the proceeds straight to its lamports. Jupiter then moved all 180,512,910 lamports out with a System transfer signed by the same PDA (h3).
     - `sell_v2` also **initialized that PDA's UVA** `FpiRuGNkFCGUPKDUJpDz2GAbjh8cXfPqLwjdiaDxExpz` = PDA(pump, `["user_volume_accumulator", EMopL9…]`) with System `Allocate(137)` + `Assign` at h4. Jupiter had pre-funded the address with exactly 1,346,200 lamports from the fee payer, so nothing was debited from the 0-lamport user. The docs say the UVA is "Initialized if needed, paid by `user`" ([DOCS] `instructions/SELL.md` row 20), so a `sell_v2` with a missing UVA and an unfunded PDA user would need that rent. Pre-create the UVA (§9.2).
     - The quote-side accounts (`associated_quote_user`, `associated_creator_vault`, `associated_user_volume_accumulator`) had 0 lamports before and after, i.e. `sell_v2` did not create them for this SOL coin.
   - **PumpSwap `sell` with a program-owned PDA user** is also on mainnet (see `graduated-venues.md` §3.1 and `jupiter-cpi.md` §1.3).
   - **Still not observed on mainnet: a buy by a non-whitelisted PDA user**, i.e. the System debits from `user` to the curve, fee recipients and `creator_vault`, plus UVA creation paid by `user`, under CPI. What exists:
     - the whitelisted mayhem PDA buy above, where pump System-transfers 64,134,766 lamports from `BwWK17…` to the curve at h3 (tx `2KAAwkwk…`). That PDA pays no fees, so only the curve transfer is exercised.
     - SIM of the non-whitelisted path (next bullet), with a top-level off-curve signer.
     - A 2026-09-27 scan of ~130 recent txs of the CPI callers `FLASHX8D…`, `GMgnVFR8…`, `4DvQwk6W…` and `FAdo9NCw…`, plus 59 of `6Vo3245e…` and 57 of `FqGg2Y1F…`, found no buy with an off-curve, non-signer `user` (the only PDA-user calls were PumpSwap sells).

     The mechanism is the same System transfer as in the mayhem case. Keep the fork test for the buy path.
   - **SIM of the non-whitelisted path:** legacy `buy`, `buy_v2`, `buy_exact_quote_in_v2` and `buy_v2` with an oversized amount, with `user` = `9rPYyANsfQZw3DnDmKE3YCQF5E8oD89UXoHn9JFEhJUz` (an off-curve, system-owned Squads vault holding 7,787 SOL). The valid buys succeeded with normal fees, and the oversized ones failed only with 6021.
   - **Other CPI callers seen in the sample:** `FAdo9NCw…`, `FLASHX8DrLbgeR8FcfNV1F5krxYcYMUdBkrP1EPBtxB9`, `GMgnVFR8…`, `4DvQwk6W…`, `6Vo3245e…`. None of them is blocked.
2. **Buys move SOL with `system_program::transfer` from `user`.** SIM inner instructions of a legacy buy show four transfers, all with `source = user`: 207,161,836 lamports to `bonding_curve`, 621,486 to `creator_vault`, and 984,019 each to `fee_recipient` and `buyback_fee_recipient`. They also show `createAccount(source=user)` for the UVA. The mainnet buy `3pu4FB7r…` shows the same four System invokes.
   - **So `user` must be system-owned with empty data**, because the System Program refuses to debit an account that carries data.
   - After the trade it must be **either 0 lamports or ≥ the rent-exempt minimum** (650,240 lamports today; read the Rent sysvar).
3. **Sell proceeds are credited directly to `user`'s lamports.** The program debits `bonding_curve` and credits `user`, `fee_recipient`, `buyback_fee_recipient` and `creator_vault`; there is no System transfer (tx `cHHjJZFF…`: curve −217,992,813; user +215,267,902 before the tx fee and a tip). The only CPI is Token `TransferChecked` from `associated_user` to `associated_bonding_curve` with authority `user`. For SOL coins, `sell_v2` also pays native lamports, and the WSOL `associated_quote_user` is ignored (tx `535njpqh…` token balances).
4. **Account creation, and who pays:**
   - The UVA is created on the first buy, or the first `sell_v2`, of a user, paid by `user` (1,346,200 lamports). It can be pre-created by anyone with `init_user_volume_accumulator`.
   - `creator_vault` gets a "rent-exempt top-up if needed" ([DOCS] BUY/SELL tables), paid by `user` per the `buy_exact_sol_in` docs. **UNVERIFIED** for sells. The `sell_v2` table ([DOCS] `instructions/SELL.md`) also lists a `bonding_curve` "Rent top-up to make the account data length 115 bytes if it is smaller", the UVA "Initialized if needed, paid by `user`", and "ATA rent if missing" for the quote ATAs (not created for SOL coins in tx `zsGhkQ…`, §6.1). Whether any of these debits `user` by a System transfer *before* the proceeds are credited, which would make a 0-lamport PDA seller fail, is **UNVERIFIED**. So fund the trader with a rent buffer before sells too, or pre-create everything.
   - The user's **token ATA is not created by pump**; clients prepend `create_associated_token_account_idempotent`.
   - v2 SOL coins need no WSOL ATAs [DOCS README].
5. **The token destination is not owner-checked (SIM).** Legacy `buy` and `buy_v2` both succeeded with `associated_user` / `associated_base_user` set to *another wallet's* ATA. That makes delivering bought tokens straight into short.win's pool vault technically possible. This is **undocumented behavior**: the docs say "User's associated token account". Do not rely on it.
6. **Compute units (pump instruction only):**

| Instruction | Mainnet | SIM |
|---|---|---|
| legacy `buy` | 66,366 (`5hRKz…`), 70,584 total tx (`5BLMnSVa…`), 77,886 on the completing buy (`SfdLRmJp…`) | 76,612 incl. UVA creation |
| `buy_exact_sol_in` | 73,666 | – |
| legacy `sell` | 54,240 | 55,743 |
| `buy_v2` | 89,448 (`5zNHaL6f…`) | **96,871** incl. UVA creation |
| `buy_exact_quote_in_v2` | 90,114 | – |
| `sell_v2` | 84,498 | 81,628 |
| `migrate_v2` | 322,371 for the whole tx `3u74vn…` | – |

   Pump's FAQ recommends a static 100k CU limit for legacy buy/sell ([DOCS] `FAQ.md`). CU varies with PDA bump search and log sizes.
7. **CPI depth.** Pump itself CPIs pump-fees, Token and System, and self-CPIs for `emit_cpi` events. With short.win top-level (pump at invoke depth 2), all nested calls are at depth 3, the same shape as the mayhem program's calls above.
   - In every SOL-coin trade traced for this note, pump's own CPIs sit **exactly one level below pump**:
     - top-level `buy_v2` `5zNHaL6f…`: inner max height 2;
     - mayhem `buy` at h2 in `2KAAwkwk…`: inner h3;
     - `sell_v2` at h3 in `zsGhkQ…`: inner h4, including the UVA `Allocate`/`Assign`.
   - No ATA-program call was seen, since SOL coins need no quote ATAs.
   - So a curve leg from a top-level short.win instruction peaks at height 3, and at 4 if short.win is itself invoked once by CPI, under the mainnet maximum of 5. That covers the observed shapes only; non-SOL quotes (ATA creation) are excluded from the MVP.
8. **Transaction size.** A legacy buy (18 accounts) plus an ATA create is 807 bytes as a legacy-format tx; `buy_v2` is 1,070 bytes (SIM builds). With short.win's own accounts added, use v0 transactions with an ALT. Pump publishes `Hyif6eWb8x88RVrvjPfabsgRYnwkVnyByEXTVTXbUcyP` as its mainnet ALT ([RS] `constants.rs`).

9. **Instruction trace: at most 64 entries per transaction.** This counts every top-level instruction (ComputeBudget included) plus every CPI (`MAX_INSTRUCTION_TRACE_LENGTH = 64`, checked in `TransactionContext::push`, `anza-xyz/agave@f77165963c709934e8ea8564187c00f081b597d1` `transaction-context/src/lib.rs` L26 and `src/transaction.rs` L493–494; `execution_budget.rs` L78 sets `max_instruction_trace_length` to it). Counts measured with `getTransaction` (1 + inner instructions):

   | Instruction | Trace entries | Tx |
   |---|---:|---|
   | `migrate_v2` | **51** (1 + 50 CPIs; whole tx 54 with 2 ComputeBudget + 1 transfer) | `3u74vn…` |
   | `migrate_v2` | 51 (whole tx 53) | `4e9yHC…` (graduated-venues TX-MIG) |
   | `buy_v2` | 8 | `5zNHaL6f…` |
   | legacy `buy` that **completes** the curve (extra `CompleteEvent` self-CPI) | 9 | `SfdLRmJp…` (called by `FLASHX8D…`) |
   | `sell_v2` | 4 | `535njpqh…` |
   | PumpSwap `sell` | 8 | `2oSvEbDT…` (graduated-venues TX-SELL) |
   | PumpSwap `buy` | 8 | `udGBnNtp…` (TX-BUY) |
   | ATA `CreateIdempotent` that creates | 5 | `5zNHaL6f…`, `udGBnNtp…` |

   `migrate_v2` alone uses ~80% of a transaction's trace budget. See §9.6 for what this rules out.

---

## 7. Graduation

### 7.1 `complete`

A buy sets `complete = true` when it takes `real_token_reserves` to 0, and emits `CompleteEvent`. Example: tx `SfdLRmJp…` bought exactly the remaining 10,056,414,279,600 base units. Its post-state is `rT = 0`, `vT = 279.9e12` (= 1.073e15 − 793.1e12), `vQ` ≈ 115.005 SOL, `rQ` ≈ 85.005 SOL. Only buys can complete a curve.

### 7.2 Trading after `complete`

**Every** buy and sell fails with `6005 BondingCurveComplete` ("…migrated to raydium", a stale message):

- **Before migration:** txs `2Xff7pFtV3eyuenAm4NNVcjaie19yru3gZictZCvadQ3kvmDvSUiVAnbXyxijkq4ypYzTdepBtzcea4BV89S5MZ` and `2LZ8EUbV4rc784w9R3dm3D4PHbspPYDVBgpWT1bvDc3txCxotTxx8YLp1f3ykPCaVwisqW5AhgLbjKjcv8pmTYQD` (log: `programs/pump/src/lib.rs:503`).
- **After migration:** `jaY2AFRRXDdHyFr272A92EsaxVCTfHQ2tPeA83StgxNQd9AMbH9WkEoMsv2WAe5tArX8z37AM9Yd23K5pmtdxAq`, and `buy_exact_quote_in_v2` `32dpqTYUqZitGubAzeBd2aR9G1frP32jTH3TMVcTzpZExSJYjgoEED1UF2VZoKuAbN3syYz6Lvx61TiWradzvZwy` (`buy_v2.rs:733`).
- **Sells (SIM on the graduated `G6vNKu5…`):** legacy `sell` fails with 6005 at `lib.rs:861`, `sell_v2` at `sell_v2.rs:242`.

### 7.3 Oversized buys revert

A buy for more than `real_token_reserves` **reverts** (SIM on the live curve for mint `9usueSYim16JMGk6vVzRzX9jCBDW79byNBsNj37Fpump`, `rT` ≈ 114.4e12):

| Instruction | Request | Result |
|---|---|---|
| legacy `buy` | `amount = rT + 5e12` | `6021 NotEnoughTokensToBuy` (`lib.rs:514`) |
| `buy_v2` | same | 6021 (`buy_v2.rs:355`) |
| `buy_exact_quote_in_v2` | 200 SOL, more than needed to finish | 6021 (`buy_v2.rs:911`) |

There is **no capping and no partial fill**, and the SDK comments that claim "caps at real_token_reserves like the on-chain program" ([RS] `math/bonding_curve.rs`) do not match current mainnet behavior. [DOCS] `instructions/BUY.md` agrees with the revert ("cannot exceed the bonding curve's real token reserves"). Completing bots therefore request exactly `rT`: all 13 completing buys I traced did.

### 7.4 Migration

**Mechanics:**

- `migrate` / `migrate_v2` is **permissionless**: any signer as `user`. `withdraw_authority` must be passed but not signed ([DOCS] `PUMP_PROGRAM_README.md`; [IDL]).
- 13 traced migrations were signed by 10 different keys, including the coin creator itself in `3NyrUcBP…` (creator `8jn4mUq2…`).
- The migrator pays about 1.49M lamports of rent for a created account plus the tx fee (tx `3u74vn…`). Pool accounts are funded from `pool_migration_fee` = 15,000,001 lamports kept from the curve ([DOCS]). The migration moved `mint_amount` 206,900,000,000,000 and `sol_amount` 84,990,359,345 (= `rQ` − 15,000,001).

**Timing: completion → migration, 13 of 14 traced cases** (the completing tx of the 14th was not found):

| Slot gap | Count |
|---:|---:|
| 0 (same slot, separate tx; likely a bundle) | 6 |
| 2 | 3 |
| 3 | 3 |
| 6 | 1 |

That is at most ~2 s wall-clock. Examples: completing buy `SfdLRmJp…` at slot 450,800,852 → `migrate_v2` `3u74vnKGX9XmZRViv3qCvhfDQUUY7SEZtyrozhCfPUBaHVk1ZNr62vVnPAVTW8sFzVMrz7e1zco5piqDRdHp7yUd` at 450,800,858; completing buy `3mCALe3Z…` and `migrate_v2` `oRzHTUR3SwS3H4vfGcpuQMVuuDpuxn3BsRm2spKVpBRQHVLYSuJAX58hHxtgms64zUcn4k8gy1fbtkNTNuUV2cj`, both at slot 450,801,340. Redundant `migrate_v2` calls after the first succeed as no-ops (e.g. `2jqeu424…`), matching the docs' "idempotent". The migration is **not atomic with the completing buy**, but in practice it follows within seconds.

**Canonical pool.** `pool = PDA(pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA, ["pool", 0u16.to_le_bytes(), pool_authority, mint, quote_mint])`, where `pool_authority = PDA(pump, ["pool-authority", mint])` and `quote_mint` = WSOL for SOL coins.

- Verified: `G6vNKu5…` → pool_authority `AgN4VRcthWBzLYwWo9wjtfYgQbL5ymN9JfCThVqMzLZE` → pool `8DY32U41qDx6rGj1zDkPzwL2FZi34j6wQANc27N75mKb` (301-byte `Pool`, `index` 0, `creator` = pool_authority).
- Sources: [TS-SDK] `pda.ts::canonicalPumpPoolPda`; [DOCS] `PUMP_SWAP_README.md` ("`CANONONICAL_POOL_INDEX == 0`").

**Boost: new and undocumented, `migrate_v2` today.** In `3u74vn…` the curve's 85.005 SOL was split:

- 67,407,342,437 lamports went to the pool's WSOL vault.
- **17,585,993,788 (~20.7%)** went to `A4xZKuxsAbxcGgQ2awP8LQyPULXufg6ZF7rvJehk2bdd`, the WSOL ATA of the pump-amm `["boost_vault", pool]` PDA `6sJR2sjBQ4qYCfHDjsCazVU9c41JiyFn8M99E1ADrENp`.
- The new pool has **`virtual_quote_reserves = 17,584,505,348`**, so the effective price is continuous (67.41 + 17.58 ≈ 84.99 SOL).

This contradicts the docs' "`virtual_quote_reserves` is `0` on all pools today" ([DOCS] README / `PUMP_SWAP_README.md`, written 2026-07-15).

- **It predates the latest upgrade.** Normal-coin migration `UVeVUKbyq5Vm5iWenK5rS9RAS851W6vJPL2MbXe38wCDQAzu134CTFGg16iwn7xF7BtecABgfwqLdPxktvPmpWw` (2026-09-23 09:57 UTC, before the 14:45 upgrade) shows the same split: 67,407,343,880 lamports to the pool and `virtual_quote_reserves` = 17,584,505,725.
- **The boost is 20.69% of the migrated quote, not a fixed amount** (review, 2026-09-27).
  - `VQR = floor(sol_amount × 2069 / 10000)` holds to the lamport for all 130 non-mayhem migrations decoded between 2026-09-25 03:08 and 2026-09-27 00:04 UTC, including USDC- and custom-quote coins. Details: `graduated-venues.md` §1.4.
  - Here: 84,990,359,345 → 17,584,505,348 = the pool's VQR.
  - For normal SOL coins this is always ≈ 17.58 SOL, because every normal curve completes with ≈ 85.005 SOL. It is proportionally smaller for coins that graduate with less SOL.
  - The factor equals the migrated token share (206.9M of 1B). The source is closed, so this is inferred from data.
- **Mayhem-coin migrations in the same window had `virtual_quote_reserves = 0`:** `2XwZRmZW…` (3.61 SOL) and `zFcZfr3C…` (0.68 SOL). None of 14 further mayhem migrations on 2026-09-26/27 emitted an `InitBoostEvent`.
- The pump-amm IDL says effective reserves are "pricing-only; payout is capped at real_vault" (error `BOOST…`), and it has a `boost_buy_and_burn` instruction; the boost vault is swept by recurring txs.
- When this started is **UNVERIFIED**.

It matters to short.win's post-graduation (Tier C) pricing, and belongs in the PumpSwap research note.

---

## 8. Other features and admin risks

- **Mayhem mode** (`is_mayhem_mode`; `create_v2(is_mayhem_mode=true)`; `Global.mayhem_mode_enabled = true`):
  - The mint has **2B supply**. The mayhem program's token vault (ATA of `BwWK17…`) held **1,295,188,807,240,642** base units (1.295B tokens) of `FjwDVKUV…pump`.
  - An agent (the mayhem program) trades the curve **fee-free**: TradeEvents show `fee_basis_points: 0`, `creator_fee_basis_points: 0`.
  - Normal users pay 95+30 bps and must pass a *reserved* fee recipient.
  - Mayhem curves' virtual SOL offsets differ from 30 SOL and between coins (`vQ − rQ` = 17.82 SOL for `FjwDVKUV…`, 21.05 SOL for `3LkzKc…`).
  - The instruction `set_mayhem_virtual_params` (signer `sol_vault_authority`, writes `bonding_curve`) can change them, and `UpdateMayhemVirtualParamsEvent` records `virtual_*` → `new_virtual_*`. In the 200 most recent successful mayhem-program txs I sampled, its pump CPIs were only `buy` (110), `sell` (84), `create_v2` (6), `buy_v2` (5) and `sell_v2` (1), with **no** `set_mayhem_virtual_params`. **How often it fires and what it does is UNVERIFIED.**
  - **Mayhem coins can graduate with almost no real SOL.** Mayhem coin `4BmkuYFT98HEMn6Fea4Lv6adKGM7Sp1LaNrSXwjKpump` migrated on 2026-09-19 with `sol_amount` = **63,791,787 lamports (0.064 SOL)** against 206.9M tokens. Tx `3mDCmoAvZ2Ej9Vv3NnfKUnoHokSJTjcHz353TzhSgRRep7oUrMgLM5u81QESnJZuShegNcRzwaTCCevFHAqGM6zW`; the mint supply is now 1,987,911,326,029,965. A normal coin reaches about 85 SOL (§7.1).
  - Fee-tier market cap uses the live mint supply for mayhem coins (§4).
- **Cashback coins** are deprecated: `create_v2` rejects them (error 6082), but existing ones still trade. The legacy `sell` needs the UVA as a remaining account, and cashback accrues in the UVA's lamports ([DOCS] `PUMP_CASHBACK_README.md`). The v2 instructions handle it automatically.
- **Holder-rewards coins** (added 2026-09-12): `creator` = pump PDA `["holder-rewards", mint]`, and trading is unchanged ([DOCS] `HOLDER_REWARDS_README.md`).
- **Custom quote mints / USDC coins** (`quote_mint != default`) exist and trade. Curves and proceeds are then denominated in that token, and they require the v2 instructions.
- **Token incentives / volume tracking:** the volume accumulators, `track_volume` and `claim_token_incentives` pay PUMP rewards. They have no effect on price.
- **Pause flags:** `Global` has **no trading-pause field**; its flags gate creation modes (`create_v2_enabled`, `mayhem_mode_enabled`, `is_holder_reward_enabled`, …). A halt would require a program upgrade.
- **Admin powers that can hurt a short.win position:**
  1. **Program upgrade** by the Squads multisig, observed about weekly with no public changelog for 09-15 and 09-23.
  2. **Fees:** the `FeeConfig` admin can reset tiers up to 10,000 bps total at any time. `Global.authority` can change fallbacks, fee-recipient lists and `buyback_basis_points`.
  3. **Creator changes:** `set_creator` (by `Global.set_creator_authority` = `39azU…`) and `admin_cto` can change `BondingCurve.creator`, which changes the required `creator_vault` account. Always derive it from the curve at execution time.
  4. **Mayhem virtual-parameter changes** (mayhem coins only).
  5. **Migration parameters** (`pool_migration_fee`) and the boost split at graduation.

  I found **no instruction that lets an admin withdraw a live curve's SOL or tokens**; the old `withdraw` is gone from the IDL. **UNVERIFIED** beyond the IDL.
- **Devnet differs:** devnet curves start with 1 SOL virtual reserves instead of 30 ([DOCS] issue #33, open). Do not calibrate risk parameters on devnet.

---

## 9. Design implications for short.win

1. **Use the v2 instructions (`sell_v2` at open, `buy_v2` at close/liquidation) in a versioned venue adapter, and keep a legacy path as a fallback.** v2 is Pump's stated long-term interface and handles cashback, holder-rewards and fee-sharing coins without special-casing. It costs about 20–25k more CU and 9 more accounts. Budget **≥ 150k CU for the pump CPI** inside an open or close, plus short.win's own logic, and use v0 transactions with an ALT.
2. **Make the `user` a system-owned, zero-data PDA that short.win signs for** (for example `["pump_trader", market]`). Program-owned data accounts cannot pay for buys.
   - Keep its balance at 0 or at least the rent-exempt minimum (read `Rent`; 650,240 lamports today).
   - *Review note (2026-09-27):* **third parties can change the trader's state.** Two cheap moves exist:
     - A System transfer only checks `from` (it must carry no data and hold enough lamports; `to` is unrestricted, `anza-xyz/agave@f77165963c…` `programs/system/src/system_processor.rs` L222–227). So anyone can donate lamports to the trader PDA; ≥ 650,240 makes it a rent-exempt system account.
     - Anyone can create `ATA(trader, WSOL)` or any other ATA of it. The ATA program checks only the address derivation, not a wallet signature (`solana-program/associated-token-account@4e58a109…` `program/src/processor.rs`).
     - So the adapter must normalise pre-existing trader state at the start of each leg: sweep all trader lamports, and close a stray WSOL ATA, which the PDA can sign for. Measure `S`/`B` against the post-sweep baseline, and never assert that an account does not exist.
   - **Pre-create its UVA** with `init_user_volume_accumulator` at market creation, so no rent debit lands inside an open or close. Also budget a possible `creator_vault` rent top-up on buys.
   - Give it a Token-2022 ATA (or a Tokenkeg one for legacy mints).
   - Per-position trader PDAs would each need their own UVA (1,346,200 lamports, reclaimable with `close_user_volume_accumulator`). A per-market trader with SOL swept to and from the position vault inside the same instruction is cheaper.
3. **Open (§2.1 SPEC):**
   - Move `Q` from the pool vault to the trader ATA, then CPI `sell_v2(amount=Q, min_sol_output=user bound)`.
   - Measure `S` as the trader's lamport delta and sweep it into the position vault. The sell credits lamports directly, so no WSOL is needed.
   - Assert that `S` equals the on-chain formula as a sanity check.
4. **Close/liquidation:**
   - Move at most `budget` lamports into the trader PDA and CPI `buy_v2(amount=Q, max_sol_cost=budget)`. Measure the actual spend, return the leftovers, and move `Q` tokens to the pool vault. Do not depend on the unchecked `associated_base_user` destination (§6.5).
   - `buy_exact_*` variants are unsuitable here, because the protocol owes exactly `Q`.
5. **Executable-cost pricing on-chain (SPEC §4, §9):**
   - Implement §4 exactly: u128 intermediates, the `+1` / `ceil` rounding, fees from `FeeConfig` selected by market cap with 1e15 supply for non-mayhem coins, and zero creator fee if `creator == default`.
   - Read `BondingCurve`, `FeeConfig` and `Global.creator_fee_configurable` **at execution time**; never cache fee bps.
   - Include possible rent top-ups in the S1 buyback-cost check.
   - Buyback cost for CR = `sol + protocol + creator`, and it is only defined for `Q ≤ rT`.
6. **`Q > real_token_reserves` is a first-class state, because a buy reverts (6021).**
   - If `Q_position > rT` at close or liquidation, the program must either:
     - (a) straddle the graduation with a **Jito bundle of at least 3 transactions**. This can **never be one transaction**: the binding limit is the 64-entry instruction trace (§6.9), not CU or account locks. The completing buy (8–9) + `migrate_v2` (51) + 2 ComputeBudget = 61–62 entries, and 66–67 with a keeper ATA create, before any short.win instruction or PumpSwap buy (≥ 8 more, plus short.win's own CPIs). The three transactions:
       - **tx1:** buy exactly `rT` on the curve with `buy_v2`, which completes it. The buyer's token ATA must already exist. The buyer is either the keeper, trading on its own account, or short.win in a first partial step; the architecture decides who holds these tokens and how the CR pricing below maps onto it.
       - **tx2:** `migrate_v2` as a top-level instruction with only ComputeBudget beside it (≈ 53–54 entries), including the 2 remaining boost accounts (§3.6). A CPI from short.win would reach stack depth 5 (Corrections 3).
       - **tx3:** short.win's price crank plus the close or liquidation, which now buys on the canonical PumpSwap pool.

       Jito bundles hold at most 5 transactions and execute them "sequentially and atomically meaning all-or-nothing", with a tip required ([docs.jito.wtf/lowlatencytxnsend](https://docs.jito.wtf/lowlatencytxnsend/), fetched 2026-09-27). Someone else may complete or migrate the curve first; `migrate_v2` is idempotent (§7.4), but tx1 then fails with 6005, so the keeper must rebuild the bundle. The bundle has **not been simulated** (CU, size and the per-transaction trace with short.win's own CPIs are **UNVERIFIED**). Or:
     - (b) treat it as "no executable route" and apply the SPEC S2/S3 rules.
   - Pricing for CR must handle it too, as cost(rT on the curve) + cost(remainder on the post-migration pool).
   - Consider a Tier D cap so that total outstanding `Q` for a market stays well below `rT`.
7. **Graduation window.** Migration runs 0–6 slots after completion and anyone can trigger it. short.win's keeper can call `migrate_v2` itself (about 0.0015 SOL, about 320k CU), so the SPEC §5.3 "graduation in progress" pause can be short. The trigger is: `complete == true` and the canonical pool account does not exist. Detect graduation from `BondingCurve.complete` and not from `real_token_reserves`, since `vT == 0` means already migrated.
8. **Market eligibility rules to add at create time and on every interaction:**
   - `BondingCurve.quote_mint == default` (SOL); reject USDC or custom quotes, or give them a separate venue path.
   - Reject or special-case **mayhem coins**: they have a fee-free agent holding about 1.3B tokens and admin-mutable virtual reserves. Recommend rejecting them in MVP; this needs an owner decision under SPEC §15.
   - Require account length ≥ 115 bytes; otherwise call `extend_account` or reject.
   - The mint's owner must match the chosen token program, and the SPEC §3 extension filter must pass.
9. **Pump can upgrade at any time.** Mitigate SPEC R5 on-chain: store the expected pump ProgramData `last_deploy_slot` (ProgramData bytes 4..12 of `B5MvU…`) in `GlobalConfig`, and **pause new opens automatically** when it changes until an admin re-validates. Closes and liquidations keep running, as SPEC §11 requires. Also check the `BondingCurve` discriminator and length, and the `FeeConfig` discriminator.
10. **Pass the fee recipient and buyback recipient in as accounts.** The client or keeper picks one at random from the live `Global` lists (reserved list for mayhem), and pump validates it. Picking at random spreads write-lock contention across the 8 hot accounts. Do not hardcode the lists; they changed on 2026-04-28.
11. **Testing:** a static pump `.so` can drift from mainnet; the crate's `pump.so` predates three upgrades. For LiteSVM/Bankrun, dump the current mainnet program and the `Global`, `FeeConfig` and curve accounts. Do not rely on devnet's parameters.
12. **MEV (SPEC R4):** every curve trade is a plain constant-product swap with a user-set bound (`min_sol_output` / `max_sol_cost`). Use tight bounds derived from the on-chain quote plus bundles, as the SPEC already plans.

---

## 10. UNVERIFIED items and open questions

**UNVERIFIED:**

- Whether pump rejects a **program-owned (non-System) `user`** on *sells*, which would otherwise work with direct lamport credits. The design above avoids this case.
- Who pays the `creator_vault` rent top-up on sells, and exactly when it triggers. I have docs only, no simulation.
- The rounding of the buyback split (floor to buyback) is inferred from 4 data points; there is no source code.
- The purpose of `bonding_curve_v2` (uninitialized PDA, seed-checked only).
- What changed in the 2026-09-15 and 2026-09-23 upgrades.
- When pool `virtual_quote_reserves` / boost at migration started (it was live by 2026-09-23 09:57 UTC). *(Resolved 2026-09-27: the boost is 20.69% of the migrated quote, §7.4. What makes VQR drift upward on some pools after the boost is **UNVERIFIED**, see `graduated-venues.md` §1.4.)*
- How often `set_mayhem_virtual_params` runs, and what exactly it changes.
- Whether the legacy `create` (SPL Token) is still enabled.
- The ≥ 3-transaction graduation-straddle bundle in §9.6(a): CU, size and the per-transaction instruction trace were not simulated. A single transaction is ruled out by the 64-entry trace limit (§6.9).
- ~~The CPI-depth headroom if short.win is itself called via CPI.~~ Resolved for SOL-coin curve legs (§6.7): pump's CPIs are one level below pump, so a top-level short.win leg peaks at height 3, or 4 if short.win is CPI'd once.
- Buys by a **non-whitelisted PDA `user`** under CPI (System debits from `user`, UVA creation paid by `user`) have not been seen on mainnet. Sells by such PDAs have (§6.1). Keep the fork test for the buy path.
- Whether any of the `sell_v2` rent items (creator-vault top-up, curve top-up to 115 bytes, UVA init) debits `user` before the proceeds are credited (§6.4).
- The mayhem fee-tier supply choice: [TS-SDK]/[RS] bonding-curve code uses the live mint supply for mayhem coins, but a [RS] AMM comment says the reverse. It makes no difference today because only one tier exists.

**Open questions for the owner and other agents:**

- Should mayhem coins, custom-quote coins and legacy SPL coins be allowed in MVP?
- Which approach should close/liquidation use when `Q > rT`: the graduation-straddling bundle of ≥ 3 transactions (§9.6a) or SOL settlement?
- Is an automatic pause of new opens on a pump program upgrade acceptable? It could pause opens roughly weekly.

---

## Corrections

*Added 2026-09-27 by the architecture pass (`docs/design/ARCHITECTURE.md` §2.5, §5.4, §13.2).*

1. **§9.7: the graduation-window trigger needs a reserves test.** "`complete == true` and the canonical pool account does not exist" also matches pump coins that graduated to Raydium before PumpSwap.
   - Example: Fartcoin's curve `TBHe5tJnuT4CQbHorJ1uVdfUoaYGPKgfCpiv2jgesVN` is complete with all four reserves 0, and there is no canonical PumpSwap pool (`EVtHHoEb2V35V1fpzUZcaASgFn6N4yGWoaZ1twNDVmjW` does not exist; mainnet RPC, slot 450,811,173).
   - Use `complete && virtual_token_reserves > 0 && no canonical pool` for "migrating". Treat `vT == 0` as already migrated (to PumpSwap if the canonical pool exists, elsewhere otherwise).
2. **§6.2: when the "0 lamports or ≥ rent-exempt" rule applies.** Agave checks rent-state transitions once per transaction, comparing each account's state before and after the whole message. It does not check after each instruction or CPI (`anza-xyz/agave@f77165963c709934e8ea8564187c00f081b597d1` `svm/src/transaction_processor.rs` L1144–1205: `TransactionAccountStateInfo::new_pre_exec` → `process_message` → `new_post_exec` + `verify_changes`). A trader PDA may therefore hold a sub-rent balance inside the calling instruction, as long as it ends the transaction at 0 or above the minimum.
3. **§9.6(a): calling `migrate_v2` by CPI from short.win has no depth headroom.** The top-level `migrate_v2` transaction `3u74vnKGX9XmZRViv3qCvhfDQUUY7SEZtyrozhCfPUBaHVk1ZNr62vVnPAVTW8sFzVMrz7e1zco5piqDRdHp7yUd` reaches inner stack height 4 (pump → pump-amm `create_pool` → ATA program → Token-2022/System; `getTransaction`, `stackHeight`). Invoking it from short.win adds one level and reaches 5, which is the mainnet maximum (`MAX_INSTRUCTION_STACK_DEPTH`, see `jupiter-cpi.md` §3.1). Prefer `migrate_v2` as a separate top-level instruction. *(Amended by the 2026-09-27 review: put it in its **own transaction** of the bundle, not "in the same transaction". Its 51 instruction-trace entries leave no room for the completing buy plus a PumpSwap buy under the 64-entry limit; see §6.9 and §9.6.)*

---

## Review corrections (2026-09-27)

These apply the first-pass review findings that target this note. Each was re-checked against primary sources: GitHub at pinned commits, npm tarballs, and mainnet `getTransaction` / `getAccountInfo` on 2026-09-27.

1. **C1-1: the instruction trace limits the graduation straddle** (new §6.9; §9.6(a); §10; the earlier Corrections item 3).
   - **Limit:** `MAX_INSTRUCTION_TRACE_LENGTH = 64` (`anza-xyz/agave@f77165963c…` `transaction-context/src/lib.rs` L26). It is enforced in `TransactionContext::push` (`src/transaction.rs` L493–494) and applied through `execution_budget.rs` L78.
   - **Measured:** `migrate_v2` alone is 51 entries (1 + 50 CPIs; whole txs 54 and 53); `buy_v2` 8; the completing `buy` 9; PumpSwap `buy`/`sell` 8; an ATA create 5.
   - **Consequence:** the one-transaction "complete + migrate + PumpSwap buy" of §9.6(a) is impossible (61–67 entries before any short.win instruction). It is replaced by a Jito bundle of ≥ 3 transactions: at most 5 per bundle, sequential, all-or-nothing, tip required, per [docs.jito.wtf](https://docs.jito.wtf/lowlatencytxnsend/). The earlier Corrections item 3 now says "own transaction", not "same transaction".
2. **C1-6: round-trip cost (§4.2).**
   - An immediate sell-then-buy of Q costs **2.53%** of proceeds (462,410,820 → 474,117,427; fees only, impact cancels), not 5.05%.
   - 5.05% is re-labelled as the same-state bid/ask spread for size Q. Recomputed with the §4 formulas.
3. **C1-7: PDA `user` on mainnet (§0 item 5, §6.1, §10).**
   - Added mainnet evidence of a **non-whitelisted PDA `sell_v2`** under CPI (`zsGhkQ…`, `user` `EMopL9…` at h3, 0 lamports before and after). In it, `sell_v2` initialised that PDA's UVA from rent the caller pre-funded.
   - PumpSwap `sell` with program-owned PDA users is covered in `graduated-venues.md` §3.1.
   - The remaining unverified item is narrowed to the **buy path** with a non-whitelisted PDA: System debits from `user` and UVA creation. A scan of 245 recent txs of six CPI-caller programs found no such buy. The only on-chain PDA buy remains the whitelisted mayhem PDA (`2KAAwkwk…`, System transfer from the PDA at h3).
4. **C1-10: legacy remaining-account history (§1, §3.2 rows 16–17).**
   - `bonding_curve_v2` **pre-dates** 2026-04-28. `BREAKING_FEE_RECIPIENT.md`: "All accounts till bonding-curve-v2 and pool-v2 remain the same … added AFTER the bonding-curve-v2 account".
   - It first appears as a trade remaining account in `@pump-fun/pump-sdk@1.29.0` (2026-02-28; `pool-v2` in `@pump-fun/pump-swap-sdk@1.14.1`, same day). When the program started requiring it is **UNVERIFIED**.
   - Only `buyback_fee_recipient` dates from 2026-04-28. The notice gave about 6 days (commit `7de0b959fa`, 2026-04-22 21:06 UTC).
5. **C1-13: boost amount (§0 item 6, §7.4, §10).** *Confirmed and extended.*
   - `VQR = floor(migrated quote × 2069/10000)` for **all 130** non-mayhem migrations decoded between 2026-09-25 03:08 and 2026-09-27 00:04 UTC, from the `create_pool` args and `InitBoostEvent`. That covers 115 SOL, 9 USDC and 6 custom-quote pools, with the quote ranging from 8.2e7 to 4.8e12 atoms.
   - None of 14 mayhem migrations was boosted.
   - **New caveat:** VQR can rise later. 2 older pools drifted upward after their boost ended; the cause is **UNVERIFIED** (`graduated-venues.md` §1.4).
6. **Research part of C1-4: rent on sell legs (§6.4, §10).** Listed the `SELL.md` rent items: creator-vault top-up, curve top-up to 115 B, UVA init paid by `user`, and quote ATAs. Whether any is debited before the proceeds are credited stays **UNVERIFIED**. Recommend a rent buffer before sells as well.
7. **Research part of C1-8: CPI depth (§6.7, §10).** The curve-leg depth is resolved for SOL coins. Pump's CPIs sit one level below pump (`5zNHaL6f…`, `2KAAwkwk…`, `zsGhkQ…`), so a leg peaks at height 3 from a top-level short.win instruction, and at 4 if short.win is CPI'd once.
8. **Research part of C0-3 / C1-3: trader-state griefing (§9.2).** Anyone can credit the trader PDA (System transfer checks only `from`; agave `system_processor.rs` L222–227) and create ATAs for it (the ATA program checks only seeds). Adapters must normalise that state before measuring.

No review finding against this note was rejected.

---

## Update 2026-09-29 (M5 re-verification)

*Re-verified for milestone M5 against mainnet on 2026-09-29 (slots ≈ 451,665,000–451,673,000, 14:10–14:50 UTC) and against the sources listed below. The sections above are not edited; where an item here supersedes a statement above, it says so. SIM runs are as in the header (`sigVerify:false`, `replaceRecentBlockhash:true`, commitment `processed`), built as v0 transactions over pump's ALT `Hyif6eWb…`. The throwaway scripts and raw data live outside the repo, in `~/.cache/shortwin-work/m5/research/`.*

### U1. Program deploys: none since the research

| Program | ProgramData | Last deploy (slot, UTC) | Upgrade tx | Changed since 2026-09-27? |
|---|---|---|---|---|
| pump | `B5MvUwXdiW1NMM6QFFD3ssPKBujD4zMohncbM73Z2BQu` | 449,734,335, 2026-09-23 14:45:19 | `4WRMFG9YnYqbouLzayMGUjJTN3rKdEcApaxdekf5mAxBsx4NdfgKVXTuQZhmWvEa5uLJCVQ9DcSarEmvfQF133kq` | no |
| PumpSwap | `6naEzKeUuFh1Jeeu51NXQgr5qkXgXtc9WKNct4xynVJc` | 446,462,733, 2026-09-12 15:23:56 | `3kTQq7ETwHfaq6mfRAYy2NZmiD9Qm7EJ6moPcoU2Wt18tTX9zUFYZ55daC1okHBueQWXqNx9gZxwYGom3N3SQvTD` | no |
| pump-fees | `75Uu23mqWBb8LM8vDppqC1mQAnCcBuLXhVaDezVMQLRw` | 446,465,969, 2026-09-12 15:41:00 | `647tW2A7fgG6PMvg2VuQJjXMgVeQgKvrEKnELpyMV9vQeU4KKs1LwFbosjLRsdJ3e5uVCr7PcewAwtbjg5doFcv9` | no |

- **Slots.** The deploy slots come from the ProgramData headers read at slot ≈ 451,665,300.
- **September upgrade list.** Every September upgrade is a Squads `VaultTransactionExecute` whose log reads "Upgraded program …":
  - pump: 09-09 19:30:47 (`5bD2JZEjvh8BhrJWbQy6QwhSSDRVJvqkeJNxajCdK84o35bnKL8G7KDkAJo7ahFGSdzrpUvdFFUdzhPZeL9fnjZv`), 09-12 15:24:04 (`SgEwKjomLvZgYWZVHkqLZAzB7LHtyQo7hWu5VgYKoVb82jqtz3fum7sAaCB7yzL2GNks5BwRB5wvpuyy4CKFbQV`), 09-15 10:34:32 (`5mfXpjh9SFxVq61V6z31nv3Mvp6UopTpjHdNvqimcWioFHQKrhW5apJxn5UFNSjBRAEixdi3bjotprVAgEPhYGmY`) and 09-23 14:45:19 (above);
  - PumpSwap and pump-fees: only 09-12 (above).
- **A fourth program shares the authority.** On 09-12 15:24:13 the same authority also upgraded `6Vo3245eszAb5wuqEMw8mGdbfRUdKbHhDHP5LcaGuTAB` (`B9oiWctojE3cSfJaNVYQ6vWUyFp1Q5f1shbNWyr5RPLDXjRX24jnNtmo48VMQjjyDhhM1u2N62fNrbg7ANMXDvB`). That program is one of the §6.1 CPI callers.
- **The 09-28 activity on the PumpSwap and pump-fees ProgramData accounts is not an upgrade.** Their signature lists show dozens of transactions on 09-28. These are MEV-bot transactions that list the ProgramData accounts read-only, e.g. `4yFJZCpFrJoKYJ3zKPaVCVGVMMyU8fPEhDdE2GUfdu8retvWNJLhMwAe5AYNHia2qmYNoULrrC7Po7onNbPgdhoh` (`MEVteoCR…` "ExecuteTwoLegFixed") and `62j8WMuhE48GAZhJ1sagNmtwGxRXtyYac2sTM5KPqvhQVHoKYq5w3xrLhdaUWxX5k58S6LarvZscxRfrY3GVdwWc` (`Prism8hs…`). The deploy slots are unchanged.
- **The upgrade authority is now identified** (§1 said "a Squads v4 multisig vault"; `graduated-venues.md` §9 had it UNVERIFIED).
  - `7gZuf…` = PDA(`SQDS4ep6…`, `["multisig", 2yMoQqQrtbhq3nQ3wFoQQawWS65qcqUXcwHEYha4rshW, "vault", 0u8]`). The derivation was reproduced (bump 254).
  - The multisig account (495 B) holds threshold **3 of 4 members, time_lock 0 s**, transaction_index 140 and stale_transaction_index 96.
  - The last transaction on the multisig is the 09-23 upgrade (checked at 14:13 UTC today). No vault transaction has been created since, so nothing was pending on-chain then.
  - Creating, approving and executing a proposal takes minutes. On 09-23, for example, the multisig saw activity at 07:15 and 14:35 UTC and executed the upgrade at 14:45.

### U2. Sources since `81091419`

**pump-public-docs: one new commit.**
- `cb188ce08b5069196eef1f3e4a0c43b70099793b`, 2026-09-29 11:50:38 UTC, "docs: negative virtual quote reserves on PumpSwap pools". It adds a README banner and a new `docs/NEGATIVE_VIRTUAL_QUOTE_RESERVES.md`.
- **From "September 30", PumpSwap `Pool::virtual_quote_reserves` can be negative.** Pump "guarantee[s] that `pool_quote_token_account.amount + virtual_quote_reserves` will never overflow and will never be negative", and says the sum fits a u64. The pricing formula is unchanged.
- The commit touches no IDL file, no pump-curve instruction and no account list. There is no other breaking-change notice. Details and design impact are in `graduated-venues.md`, Update 2026-09-29.
- Two open third-party PRs change nothing here: #47 (webpack/CodeQL) and #49 (GitHub social-fee wording).

**IDLs.**
- `idl/*.json` is unchanged since `e0687ae9`.
- The on-chain pump IDL `AYgC53tU…` (last written 2026-09-12 18:02:23) is still identical to GitHub `idl/pump.json`: 47 instructions, 7 accounts, 42 types, 94 errors and 28 events (docs strings excluded).
- **The on-chain PumpSwap IDL `5fLnXNNoZcZt9Qku6HARM3un3Ttm2cGsR7gN9Zp1R7h3` and pump-fees IDL `6hgWp61YgGzJ9QmvxyFtLnGfA8MYgx93Hby6fdq8gG31` are stale.** Both were last written 2026-05-14.
  - The PumpSwap one lacks `init_boost`, `boost_buy_and_burn`, `toggle_boost`, `set_boost_authority`, `admin_cto_pool` and `update_creator_fee_config`, and 19 errors, among them `InsufficientRealQuoteReserves` and `PoolCannotBoost`.
  - It also carries old `Pool`, `BuyEvent`, `SellEvent` and `GlobalConfig` layouts.
  - Use GitHub for both programs.

**SDKs:** no new release. pump-sdk is still 2.0.0 (2026-09-13), pump-swap-sdk 1.20.0 (2026-09-10) and pump-rust-client 0.1.13 (2026-09-09).

### U3. Live configuration: unchanged

- **`Global`** (slot 451,665,934): every §2.2 value is unchanged. That includes:
  - fees: `fee_basis_points` 95, `creator_fee_basis_points` 5, `buyback_basis_points` 5000;
  - migration: `pool_migration_fee` 15,000,001, `withdraw_authority` `39azU…`;
  - all recipient lists;
  - flags: `create_v2_enabled`, `mayhem_mode_enabled`, `is_cashback_enabled` and `is_holder_reward_enabled` are all true; `creator_fee_configurable` true with a maximum of 300 bps;
  - `whitelisted_quote_mints = [USDC]`.
- **pump `FeeConfig` `8Wf5Ti…`** (slot 451,665,971): flat, stable and exotic fees are all {0, 95, 30}. `fee_tiers` and `stable_fee_tiers` each hold one tier, threshold 0 → {0, 95, 30}.
- **Rent** (`getMinimumBalanceForRentExemption`):

  | Size | Lamports | Size | Lamports |
  |---:|---:|---:|---:|
  | 0 B | 650,240 | 151 B | 1,417,320 |
  | 125 B | 1,285,240 | 165 B | 1,488,440 |
  | 137 B | 1,346,200 | 170 B | 1,513,840 |

### U4. Trades reproduced to the lamport

**Sample.** I took 12 rounds of `getSignaturesForAddress(pump)` and fetched every 2nd–4th successful transaction: **1,485 transactions with 1,488 trade instructions**, slots 451,666,275–451,668,225 (2026-09-29 14:17:23–14:26:07 UTC). The pre-trade state is the `TradeEvent` post-state minus or plus the trade. The whitelisted mayhem agent's 294 trades are excluded here and covered below.

| Instruction | Reproduced / non-whitelisted | SOL-quoted: non-mayhem top-level / non-mayhem CPI / mayhem | Examples (SOL, not mayhem) |
|---|---|---|---|
| `buy_v2` | 55 / 55 | 42 (32 / 2 / 8); 13 non-SOL | `5uUka8Lbn3m6TMur4SgxCxeTdyARJUQu4MYrtsPfVEiPQMNsrh7xD1ggv7KeXU2gNTFS54HfZRrEGL5YTTVQ9N2e` (with its `create_v2`), `4g4VQkAELQotSRiYQA6wob8Tm5Q7zM2BuCywKYzMFswMVP3S5bXB65oLAyYF5VLDfDeTTkUPjARkLQRCcDHSXGd7`, `mtBsHC52cU5h5v4uiZgKULhCs4y5uzNwuJvxq371HmpPrhFFG9mDojDb3vdhT6dUgGg4Py7higfu4FYMgSHcnuJ` |
| `sell_v2` | 246 / 246 | 203 (49 / 129 / 25); 43 non-SOL | `5n9GTsdnKVZE27NoeNZWsBdH2gsU8ga33Gmc5iB3ZTCwJFaVVycDc5BkELaWShPW1s19F4t2zNLJUZ42yBveNQRk`, `Karixm2B3wWMyAM26zh81gQzqMCzFsBqVEy51ufEAwDhiHE79HPe8kbayLaoen4Pdwe9sYGXMX8FdaWhbPCthVe`, `2JdYuYRPG8ZdfLXtwxoey2rMjMyr3n8Jn2kpFJHxLzJUiPg8efM56BcBrC3pZS4Wv3pJ56NQqz11FGNBRbnWJXte` |
| `buy_exact_quote_in_v2` | 223 / 223 | 201 (54 / 118 / 29); 22 non-SOL | `4cd3Ziy6by2ULWNQc3fofUK6SaN4u7f2eAgAQe7LBasdMfSC3jecWLmigDw59QzTw9j5wD1ezDeHPc3fobcUqDbT`, `Fz3BytiEtwqvae8E3ju5EhcC7WK1ij4X8VCud4SXs4iigAspXHQjbtJkixsQWB6PdLmaySSnMaxzkraKd6xqskr`, `5uYGKb5TNmqsfps7qbs4PjjJw5zJ6RtdMfUUxCWHnGaVTyT9R7n1rovsePoUABc2WAkmpoJyhHbJ2KNE9yhse6nu` |
| `buy` | 233 / 233 | 233 (66 / 148 / 19) | `5t4yqb7VTngFMFT6w14ssPHwcZ8jpU91nhVquU9NH1ygVUJ2tvo923utBkkkRDFSJqma9LiaE1B5vSuC5owhY1wv`, `2BWaCCph3MAKui8mfBUwPLnXaVXuLKbG2ywvE3J7RU1f6NfjaKjbbyGGZirbu1CxAG1C7j4PZdbUsBQoYRZctgNf`, `5sTCR5pF7o6YDecW5gYoH2X55F1QungRyeEw7AnGNa2BTKLmFVhvpXoae9EkJEDp3whGEkrEaLvYsM2ebeh9s87Y` |
| `sell` | 314 / 314 | 314 (134 / 156 / 24) | `67TmdLT1PtqCiHHJvtaP8nRdFNNfcEiz3Wk5km2eesu8JAXcs8FXttd9zjt8x9AP6wXZhgyXnfr97Fys8foh9dC2`, `2hdqxNASDZfQPDsYwS9Y4FNrk59q3h7sMTnpPQDEdRX2g5aAcs82mRYqBynhUcyJ7kndCANwqJg5PgB7H3BB1W3e`, `4RBKHrHd26RcGhArdQuVobrWKYboBgMJQ3eCyyx98NArQnAokoLxVUuAPwHYi4P4bbTvCAFiaTbvG5AXu8BttXap` |
| `buy_exact_sol_in` | 123 / 123 | 123 (87 / 13 / 23) | `2vihxd7yFgstkUBXDb4nVELcmm4L8vimESZG2SbqRJKiRwc4VhCPh1vAARgrtrLAP9qqjbg7vFEroNtiZ3SheUd`, `GdAUNE78LMEpzsfe8HTHMeEqA8xHGSKRDi3h92pkMKbqRhicUo85pXHZqKvtZ8ZFDVPzFovLqPP8fRQCZPDbV4a`, `kDcZD3bU1hfFu7aNeoSy9JiMQkajqm9Z3xw3K2BKQHHX8LfrxRVh3wFDxY4Zj7MTeQ3gV8u86UqRtMko8mDCNZX` |

**Reproduced with shortwin-math itself.**
- Method: a throwaway crate linked by path to the repo at `eb647fc`, using `fees::pump_curve` on the live tier plus `quote::sol_leg::curve_charge_exact_out` / `sell_exact_in`.
- Input: all **792 SOL exact-out buys and exact-in sells**, i.e. `buy` 233, `buy_v2` 42, `sell` 314 and `sell_v2` 203.
- **791 are identical** in `n`, cost or proceeds, and post-trade reserves.
- The one other trade is a legacy cashback coin (`4fKRy4yC6Xg1RomSPBYTejvb6mx7eCZGoCmTnoPZirh6UWpmjb2vPz9hWV7gZhYTta2AfgcEjSY6TnQgLM696zAF`). Its 30 bps go to the seller's UVA as `cashback` (UVA +329,238), not to a creator. The total deducted from the seller equals shortwin-math's 95 + 30 bps quote; only the event field differs.
- The exact-in instructions were reproduced with the §4 formula. shortwin-math has no exact-in quote because short.win does not use one.

**Rules re-confirmed on this sample:**
- **Default creator.** `creator == Pubkey::default()` means a creator fee of 0, although the event still says 30 bps. On exact-in buys the program then divides by 10,000 + protocol bps only. Six trades show it, e.g. `5HKeKS5VDnJ8m3D8qEA25jkZ8nrPz68nhbW3vkNPGcDDLxDGfo32qRuMS1MymwSdZDf6YmoHYFHb66ANH4fpx3jt` and `r1JFdMb1KFvYfbQxU3WVBTRRFvNRyjJTWAT555KVSMeG2Sxcw2QkrXjsXJuP3H2ADPo9CqthxRK9rCdXMuyhKQ4`.
- **Exact-in fee base.** Exact-in fees are computed on the net *before* the §4 step-3 adjustment. Example `4EQcs1DvrcxbnwXfsvwsRGqrxgmsV9ZF1gvF3LDCjGm38U1xDsMiuAw35N92WwJQ2GnA3HYYstNnzz4wpCEkCQzN`:
  - fee 388,648 = ⌈40,910,211·95/10⁴⌉, while the curve is credited 40,910,210;
  - the total paid equals `spendable` exactly.
- **Holder-reward coins** (178 trades) use the same math. The `TradeEvent` repeats the creator fee as `holder_rewards`.
- **Per-coin creator fees** of 100, 125, 200, 250 and 300 bps on custom-quote curves apply as in §4 step 4.
- **Mayhem coins traded by normal users** (95/30 bps, reserved recipients) follow the formula: 128 trades.

**New: the whitelisted mayhem agent trades off the curve.**
- All 294 trades by `BwWK17…` (`buy` 125, `sell` 164, `sell_v2` 5) are fee-free. Each executes at exactly the caller's bound: `sol_amount == max_sol_cost` for buys, `== min_sol_output` for sells (294 of 294).
- Only 6 of them match the constant-product price implied by the event post-state. The median deviation is 16 %; the largest is 87 % (`2o55E7wyQPudQGMrbQDt9SFJb6CXB114wNQ6mha1gk6LKH2Eiz31byo9NXyjZ69UdDKWqULxXfSw6zGutmphgZAB`).
- So pump's agent sets mayhem-coin prices directly. This adds a reason for Q6's rejection of mayhem coins; nothing needs to be built.

**Account lists.**
- **The v2 lists are unchanged.** Every `buy_v2` and `buy_exact_quote_in_v2` in the sample has at least 27 accounts and every `sell_v2` at least 26. The fixed accounts sit at their §3.5 positions in all 524 v2 instructions: global, ATA program, GVA, `fee_config`, `fee_program`, System, event authority and program.
- **Pump accepts unexpected trailing accounts** (U24 / A41, pump side). 34 successful trade instructions carry 1–7 accounts beyond their list:
  - `buy_exact_quote_in_v2` with 28 accounts (13) or 29 (1), `sell_v2` with 27 (12), `buy_v2` with 28 (4);
  - legacy `buy`/`sell` with `jitodontfront111111111111111111nopainnogain` appended: `3EYS4zN5b8aLvocZ5MiCA3sZS2HsVgeKpqUXeXN1TbsMvRXhVAM3ERkNKCkyGFUDKJZEfSrGXKNMf5LYNMbDQRxY`, `2ENw2GpvL7Sq49TcqAjRL1thbhzsVZTW478Ru56dJqgQxKTmLT4jZqb3AHQMS3GCCQZ7oBo5BwCxZdSkbnf94mXM`;
  - a legacy `sell` with the 7 other buyback recipients appended: `3YMkWEPFM2Nmob8T68MR8jJtdju293GuytP7DtMpaVHvgqsnvhXXz4bL2pVR3EezisMHdYoAwgTZ4zBVVJFWdQdz`.

**Instruction data.** Buy instructions accept one undocumented trailing byte; see U7.

**Compute units.** The mainnet figures are for top-level calls and cover the whole pump instruction including its CPIs, read from the logs:

| Instruction | Median CU | Range | Sample |
|---|---:|---|---:|
| `buy_v2` | 97,155 | 79,038–170,959 | 40 |
| `sell_v2` | 75,628 | 66,935–137,817 | 57 |
| `buy_exact_quote_in_v2` | 91,063 | 77,549–141,015 | 56 |
| legacy `buy` | 69,493 | 60,894–100,852 | 71 |
| legacy `sell` | 53,762 | 43,125–67,804 | 149 |
| `buy_exact_sol_in` | 72,147 | 61,776–103,023 | 105 |

- The upper tails include create or extension work in the same instruction.
- SIM: `buy_v2` 83,447 CU when the UVA exists, 86,371 when the buy creates it, and 93,506 for a completing buy.

### U5. Coin creation (create_v2, Token-2022, mayhem, cashback, holder rewards)

- **`create_v2` allocates a 125-byte `BondingCurve`** (createAccount space 125, 1,285,240 lamports, owner pump). This holds for all 27 creates in the fetched transactions, e.g. `45hW4FoAJLheqL98hRB1mNEyzJtZj12qYiCz4mw4GGmoR81Xkkv8KdwbkZFBUBsxQcYqfBBtWp7PGzTh6Knzppwk` (curve of `GB1gk9vU…`), `2hWAQWSjag8yoXKPdn3yVSQYWtFKWXmjnUAwc3uBsNmri4TovwZAkE799cxfBzQQme1quZ6m1FKQHAeHhVwTCuW8`, and the mayhem create `VonMspwLnixrMLWLcyBeSTV4eiwMBtgMXNpkmvPCAwZxyP8sTFGAVwJbD94THcVtAXoXoaqEGWwwz1zaD6FGp7r`.
  - 125 bytes ends exactly after `is_holder_reward` (@124); the layout is unchanged.
  - 151-byte curves are extended ones. Clients prepend `extend_account` (125 → 151), e.g. FLASHX's `47PMcc17XhEgQt8FCTFizjAP6crbb6EAThLqHWc9KtPYQbWGqcpSgXcEtSX8Qac3f3AUsjMdX44G5pLD1yurwaB`.
  - **Corrects §2.1** "Allocation is 151 bytes (new coins)".
- **Both v2 and legacy trade 125-byte curves without extension.**
  - SIM: `buy_v2` on `AP6aC5om…` and `GB1gk9vU…`.
  - SIM: legacy `buy` on `GB1gk9vU…` succeeds both with and without a prepended `extend_account`. Without it the curve stays at 125 bytes.
  - So §2.1's "the legacy path requires `extend_account` when the account is shorter than 150 bytes" no longer holds for 125-byte curves.
- **Mints (29 fresh creates).** All are Token-2022 with decimals 6 and mint and freeze authority None. Their only extensions are MetadataPointer (18) and TokenMetadata (19). Supply is 10¹⁵ for the 21 normal coins and 2·10¹⁵ for the 8 mayhem coins. The SPEC §3 filter result is unchanged.
- **Create flags** (27 creates):
  - none set `is_cashback_enabled`, so cashback remains legacy-only;
  - holder-reward and mayhem creates occur;
  - custom-quote creates with `creator_fee_bps` 100 or 300 occur;
  - the trailing optional args (`creator_fee_bps`, `is_holder_reward`) are often omitted.

### U6. T-PDA-USER and T-RENT on the curve

**Mainnet, new:**

- **Buys by a non-whitelisted, off-curve, system-owned PDA under CPI now appear on mainnet.**
  - The OKX DEX router `proVF4pMXVaYqmy4NjniPh4pqKNfMmsihgd4wdkCX3u` uses PDA `ARu4n5mFdZogZAravu7CcizaojWnS6oqka37gdLT5SZn` as `user` of legacy `buy_exact_sol_in` at stack height 2. The PDA is off-curve, System-owned, has 0 data, holds a buffer of about 0.869 SOL and is not a transaction signer.
  - Examples: `kDcZD3bU1hfFu7aNeoSy9JiMQkajqm9Z3xw3K2BKQHHX8LfrxRVh3wFDxY4Zj7MTeQ3gV8u86UqRtMko8mDCNZX`, `4WSTpmMb6enyup9EUC3tbPUzhfXvBYuLYSwxtdKwDd6U3MWwAei1CPc1cb4NYer8yF9S8RrGyGQoNjJ6v8CQ4EdA`, `5EkZvuhSkrxSdMaXfzeEDodDLb1hJSW7obq1QnR675Ld1jnmuwjRXp41kq7yWjLpvaaxLZimqWnkrCE1C9jdRqaM`, `3msnKdQs4WuEjSVhTGPkRGPGvm4rs1tbhG3SaKAkTBghS9A6qpd8pxbGEEquaDMohMuZjPS2Uf69fTLJC4M7T5BA`, `42FtxVj9QDT158GCENTff9et5iY8NFCpir2FZsZcuRK5KK7DWZcFGviZbegjuwAEQLabMxiD2y7d9SsH7ZycrNQ9`, `5BWDKvBCbFDRFBHGpRKDTB3dSCJnSA4uFpJ6xwHGxjpEXNKAk8dNeuRVUtxFcGrRgnqJPreeCJj498BEwoxo7kC`.
  - Pump System-transfers from the PDA at height 3. In `kDcZD3bU…`: 137,580,246 to the curve, 412,741 to the creator vault, 653,507 to the fee recipient and 653,506 to the buyback recipient.
  - The router funds the PDA before the buy and sweeps it afterwards (pre = post = 868,910,230).
  - The same PDA also calls `buy_exact_quote_in_v2` and `sell_v2` on non-SOL coins (`3AWoyYGuxQqoCrwJeGGeqrtuDd27BsxQw1fSwftqhXvxio6iW8uZHPVJSw3ULEgxcLeQQm2e2wgsLFVx5ee4KGei`).
  - **Still not observed:** a SOL-coin `buy_v2` by a PDA, and UVA creation paid by a CPI PDA. This PDA's UVA `2DYcto5G…` already existed.
- **A program-owned PDA can be the `sell_v2` user.** This resolves the first §10 UNVERIFIED item.
  - In `5Hs2eAs1JtYXX4wUMgnLAuWrFcVF7nYyqLntFNQ97XHQg2yv79GpgGrGoteVep4sAw5dFxzHFDKEzaeNaJFNUfox` the `user` `D5YqVMoS…` is owned by `T1TANpTe…`, carries 9 bytes of data and sits at height 2.
  - Pump credited the proceeds to it, and the caller then moved them on.
- **A 0-lamport PDA seller at height 3:** `2NcTDgnghCL3sBdyeHn6vCuR3hbYBg8gY9pSpa6ffsvgh61CDGV4bFDbYZfCQJ6tKYqADfGJ77oRxcpcKMqPYj4x` (user `6zQ6Ee7T…`, which does not exist before or after; called via `6Vo3245e…`).

**SIM setup.** The trader is PDA(short.win, `["trader", mint]`), which is off-curve and does not exist beforehand. The funder `9rPYy…` pays and signs (sigVerify is off). Tokens for sells come from a same-transaction buy or from a holder's transfer.

**SIM, funded creator vault** (`G5yt7iw9…`, 151 B; `AP6aC5om…`, 125 B):

| # | Case | Result |
|---|---|---|
| A | `buy_v2`, fresh PDA with no UVA, funded B̂ + 2,000,000 | ok. The UVA is created with the PDA as payer; the PDA ends at 653,800 = 2,000,000 − 1,346,200 |
| B | UVA pre-created (`init_user_volume_accumulator`, payer = funder), funded exactly B̂ | ok; the PDA ends at 0 |
| C | as B, funded B̂ − 1 | System error 0x1, "insufficient lamports 5905350, need 5905351" (the last fee transfer) |
| C2 | as B, `max_sol_cost` = B̂ − 1 | 6002 `TooMuchSolRequired` (`buy_v2.rs:463`): the bound covers the fees |
| F | `amount` = rT + 1 | 6021 (`buy_v2.rs:355`) |
| R | one tx: B (the PDA ends at 0), then `sell_v2` of the same Q from the 0-lamport PDA with `min_sol_output` = the exact §4 proceeds | ok; the PDA ends with exactly the proceeds, 1,227,691,446 |
| E | `sell_v2` from a 0-lamport PDA with no UVA | fails before any other step: "insufficient lamports 0, need 1346200". `sell_v2` creates the UVA from `user` before crediting proceeds |

**SIM, empty creator vault:**

| # | Case | Result |
|---|---|---|
| V1 | `buy_v2` on `GB1gk9vU…`, whose creator vault `9deZCXjK…` does not exist, funded exactly B̂ | fails. Pump System-transfers the creator fee **plus 650,240** (rent of a 0-byte account) from `user` to the vault before the curve transfer ("insufficient lamports 27600694, need 27985075"). Legacy `buy` does the same |
| V2 | as V1, fresh PDA funded B̂ + 2,000,000 | `InsufficientFundsForRent` on the PDA, which is left with 3,560 after UVA 1,346,200 and top-up 650,240 |
| V3 | `sell_v2` from a 0-lamport PDA (UVA pre-created), right after `create_fee_sharing_config` in the same tx moved the curve's creator to a sharing-config PDA whose vault is empty (`4JsVFApe…`) | fails, "insufficient lamports 0, need 650240": the top-up is taken from `user` **before** the proceeds are credited |
| V4 | as V3, PDA pre-funded with 650,240 | ok. The PDA ends at exactly the control run's proceeds (858,221,919), so the venue-side rent r = 650,240 |

**Mainnet counterpart of V3.**
- `sell_v2` `42oStfUiWwAW5UJjjiGZn4TYxGp86SYEpUqky1nTyZRkPEZRp6bFQMbGDpVu5GZQ2dQW6RpQ3is2HFFSztLwuuJw` transfers 650,240 from the seller to creator vault `3mm1brw2…` at height 2.
- It was the first trade after fee-sharing migration `376EbQQcK3USikDTyDtMqSHWLirLu95PLxcPdRQ1Vy8FaxNSqZHeXKkvvXRaxkyHU9Pptxr13Jw83Zzvt5ftt7fm` left that vault at 0.

**When is a creator vault empty?**
- After a fee-sharing migration, and for creators who were never paid.
- `collect_creator_fee(_v2)` and `distribute_creator_fees` leave the vault at 650,240 in 51 of 53 sampled calls. One `distribute_creator_fees` left 0.
- All 72 creator vaults of the live curves in the sample were funded.

**Consequence.** Once the trader's UVA exists, venue-side rent on a curve leg is at most **650,240 per leg** (a creator-vault top-up), on buys and on sells alike. That holds for curves ≥ 115 bytes; every `create_v2` in today's sample allocated 125 bytes. A missing UVA adds 1,346,200. `exec_rent_tol` = 6,500,000 covers both.

**Stack depth.** Under the OKX router, pump sits at height 2 and its System and Token CPIs at height 3. So pump's CPIs remain one level below pump.

### U7. Oversized buys and the undocumented cap flag

**Default behaviour is unchanged.** An amount above `real_token_reserves` reverts with 6021 on every buy instruction. SIM confirmed this for `buy_v2`, `buy_exact_quote_in_v2` and `buy_exact_sol_in`, at 1.1× to 100× the needed SOL and with `rT` from 0.26·10¹² to 79·10¹².

**New: a trailing bool caps the fill at `rT`.** One extra byte after the documented args (on legacy `buy` / `buy_exact_sol_in`, after `track_volume`) changes the behaviour:

- value **1**: pump fills **exactly `rT` at the exact-out price** and completes the curve;
- value 0, or no byte: 6021, as before;
- value 2: 102 `InstructionDidNotDeserialize`, so the byte is parsed as a bool.

SIM on `8xSiWmXU…` (rT 78.58·10¹²) completed the curve with the byte set to 1 in all four buys:

- `buy_v2(rT + 10¹², …, 0x01)`;
- `buy_exact_quote_in_v2(3·n, 1, 0x01)`;
- legacy `buy(rT + 10¹², …, 0x00, 0x01)`;
- `buy_exact_sol_in(3·n, 1, 0x00, 0x01)`.

In each case the curve's vQ rose by 25,209,732,182 = ⌊rT·vQ/(vT − rT)⌋ + 1.

The flag is in neither the IDL nor the docs.

**Mainnet use.**
- 2 of 20 sampled graduating buys used it. The first is `2FaaS3BXh5D7gF75Z9e4LnrbE13cim5akRvi4vCgquJZeBWNTrpGLc2aVZz8db9TuUxnhdpPH5RnGLwwK5vLC1m6` via Jupiter: it offered 1.218 SOL, and the curve took 105,830,393 lamports for the last 257,807,207,879 atoms. The second is `26WGe8F2En3unBm5f51ST4gXWiHHUPJuJEHsJjzvLECPxJ7GX9ALUtVv35iy14vsTPpZFSuHMafnCcfmQMtFaoAP`: it offered 11.09 SOL and the curve took 9.555 SOL.
- In the U4 trade sample the byte is set on 48 of 223 `buy_exact_quote_in_v2`, 9 legacy `buy` (`0x01 0x01`) and 1 `buy_v2` (`0x01 0x01`).

**Corrects §0 item 4 and §7.3** ("no cap, no partial fill") for callers that set the byte.

### U8. Graduation

**Migrations since the research.**
- Of the 4,028 successful transactions on `39azU…` since 2026-09-27 00:08 UTC, I decoded 2,942. They span 2026-09-27 18:13:09 to 09-29 14:27:49.
- They hold 2,826 migrate instructions: 2,102 create a pool (2,102 distinct mints, about 48 an hour) and 724 are no-ops.

**The boost rule holds on every fresh pool.**
- All 1,610 non-mayhem migrations (1,455 SOL-, 94 USDC- and 61 custom-quote) emitted an `InitBoostEvent` with VQR = ⌊`quote_amount_in`·2069/10⁴⌋ exactly: 1,610 of 1,610.
- None of the 492 mayhem migrations (473 SOL, 19 USDC) was boosted.
- `base_amount_in` = 206,900,000,000,000 in all 2,102.

**Instruction forms:**
- `migrate_v2` with 29 accounts (27 + boost-vault PDA + its WSOL ATA): 2,502;
- `migrate_v2` with 27 accounts: 35, all mayhem or no-op;
- legacy `migrate` with 25 accounts: 288, of which 150 created mayhem pools;
- legacy `migrate` with the 2 boost accounts: 1, which was boosted (`5DGxpWSf3zBmDofyjw3oeKL4J32gzedz5iXb9Mms9x8pN73AFe9tzzymey42r6qbCzUgJZ2fU2cjhkjdePQrYC6B`).

**Permissionless, and CPI works at exactly depth 5.**
- 117 pool-creating `migrate_v2` calls ran at height 2 from six programs: `AdBMsmwr…` 54, `4RoVsR9z…` 39, `QECRnxRg…` 18, `AJHKxaUH…` 9, `5kZmvKba…` 3, `J7iAnje8…` 1. Example: `2dLArKYWP17mKkTkRdBnwKZNWcVtx56H72aRsKrUQpZrv1Sjwb6DggxjHXTZykoaUmRAcEXAdEdsxG8QkcPCs5TT`.
- All 117 reach stack height 5; all 204 top-level migrations checked reach 4.
- So a CPI'd `migrate_v2` hits the maximum of 5 and succeeds. **Refines Corrections item 3**: the call is possible but leaves no headroom, and the 64-entry trace remains the binding constraint.

**Completion → migration**, on 20 random boosted migrations:

| Slot gap | Count |
|---:|---:|
| 0 | 9 |
| 1 | 2 |
| 2 | 2 |
| 3 | 6 |
| 14 | 1 |

The completing instruction was legacy `buy` in 12 cases, `buy_v2` in 6 and `buy_exact_quote_in_v2` in 2 (both with the U7 flag).

**SIM of the straddle core.**
- Setup: `G5yt7iw9…` (rT 75.88·10¹²), one transaction of ComputeBudget, transfer, `buy_v2(rT)` by a wallet that has its ATA and UVA, and `migrate_v2` with 27 + 2 accounts.
- Result: ok, with 62 trace entries, 423,227 CU (the completing `buy_v2` 93,506) and a 1,202-byte transaction (v0 over pump's ALT).
- **The new pool matches shortwin-math:**
  - It is 271 bytes.
  - The base vault holds 206,900,000,000,000 = curve base ATA − rT.
  - Quote vault 67,405,854,966 + VQR 17,584,505,601 = 84,990,360,567 = rQ + n(rT) − 15,000,001, exactly.
  - VQR = ⌊0.2069 × 84,990,360,567⌋, and the boost ATA holds VQR.
  - shortwin-math `quote::migrated_pool` (linked Rust) gives X_p = 206.9·10¹² and Y_p = 84,990,360,568. That is +1 lamport, from its intended ⌈⌉ + 1.

**New pools are 271 bytes on mainnet too.**
- `5QvPCCGK5rEPPtt3MqU2RwGiFji9sQCveKo5Wb5nYxGZSm22sx6ucxoqHAFspuzUJhFUJJ15h3Sh92TvkXVF8yjs` creates its pool with space 271.
- The first trade on it, `47G9kQKc33RbRviKTiuCArcShJH8dT2rhkBfPFeXjdtHRgMuSXgaSSAuuWr3phqhFz4GJ2L5S1wCGFzqi28utWH1` (same slot), ran on 271 bytes.
- Seven slots later a client prepended PumpSwap `extend_account` (`2BoLeZtrcjtDNMMjKGLe97dbrdzAavWGb6dqsoKPcqzj4MfP658465aC3xVArchvi4Ggm4U5XpHcmvZ9EdJgQhnB`), taking it to 301 bytes.
- All 150 sampled boosted pools are now 301 bytes.
- Details are in `graduated-venues.md`, Update 2026-09-29.

### U9. Still UNVERIFIED after this pass

- A SOL-coin `buy_v2` by a PDA under CPI on mainnet (only legacy `buy_exact_sol_in` is observed), and UVA creation paid by a CPI PDA (SIM covers it only at top level). The fork test stays (T-PDA-USER).
- The name and intended semantics of the trailing cap flag, and whether pump keeps it: it is undocumented.
- How the whitelisted mayhem agent's price is set (U4).
- What pump will ship on "September 30" for negative VQR, and whether it touches migration or the boost (`graduated-venues.md` Update 2026-09-29).
- The Jito straddle bundle itself is still not simulated. Its tx2 core (completing buy plus `migrate_v2`) now is (U8).

### U10. Found by the M5 build on the dumped ELFs (2026-09-29)

*What the M5 build found by executing the **dumped** mainnet programs in LiteSVM (`tests/fixtures/`, group `pump_programs` at slot 451,672,911: pump deploy slot 449,734,335, pump-fees 446,465,969, PumpSwap 446,462,733 — the U1 slots; coins and statics from group `pump_state` at slot 451,675,432). Each item names the test that pins it; none needs a mainnet transaction.*

- **The curve's fee tier uses the mint's live supply.** The program resolves the curve's `fee_tiers` at `mcap = ⌊vQ·mint.supply/vT⌋` with the base mint's **live** supply, not a fixed 10¹⁵. **Corrects §4 step 1.** The two differ only for a coin whose supply was burned (the `near` fixture: 999,978,034,493,483) and only when a tier threshold lies in `[mcap_live, mcap_10¹⁵)`; with today's single tier (threshold 0, U3) no quote differs. `tests/litesvm/tests/pump_math_it.rs` `tripwire_the_curve_tier_market_cap_uses_the_live_mint_supply` (a threshold at the fixed-supply mcap is not selected by the ELF).
- **A curve shorter than 125 bytes grows to 125 on its first v2 trade** (`buy_v2`, `sell_v2`, `buy_exact_quote_in_v2`), not to 115; `user` tops the curve's rent reserve (`lamports − rQ`) up to `Rent.minimum_balance(125)` = 1,285,240. That was 53,320 lamports on the 2024 49-byte fixture (reserve 1,231,920) and 0 on every 115-byte fixture (reserve 1,691,280). **Corrects §2.1**'s "115" top-up. `pump_math_it` `legacy_spl_49_byte_curve_grows_to_125`, `curve_growth`.
- **The creator-vault top-up (U6) applies to every buy form**, `buy_exact_quote_in_v2` included (there on top of `spendable`), as well as to `sell_v2` (before the proceeds are credited): `pump_math_it` `an_empty_creator_vault_is_topped_up_by_the_user`; through short.win's legs (T-RENT) `tests/litesvm/tests/pump_it/rent.rs`.
- **Fee rules confirmed on the ELF** (`pump_math_it`): the buyback share is `⌊protocol·buyback_basis_points/10⁴⌋` (§4 only inferred it); an LP component in a curve tier is ignored; the per-coin `creator_fee_bps` applies to SOL curves while `Global.creator_fee_configurable` is set; a default creator pays no creator fee; exact-in fees are charged on the pre-adjustment net (as U4).
- **Recipient checks:** a reserved (mayhem) or unlisted `fee_recipient` fails with 6000, a wrong `buyback_fee_recipient` with 6057 (`pump_math_it`).
- **The cap byte (U7) on the ELF:** a trailing `1` fills exactly `rT` at the exact-out charge; `0` or no byte fails 6021; `2` fails 102 (`pump_math_it`).
- **Accounts:** pump's eight buyback recipients are 208-byte accounts owned by pump-fees, not System accounts; neither pump's nor PumpSwap's `event_authority` has an account on mainnet; pump's fee-recipient list includes `CebN5WGQ…`, PumpSwap's `JCRGumoE…` (`scripts/dump-fixtures.py`, `tests/fixtures/manifest.json`).
- **Trace under CPI** (short.win at height 1, pump at 2; `tests/litesvm/tests/trace_pump_it.rs`):
  - `sell_v2`: pump, pump-fees, the token transfer `user → curve_base_ata`, pump's event = **4** entries; an empty creator vault adds pump's System top-up from `user` after the token transfer (**5**). The proceeds are direct lamport moves (no entry).
  - `buy_v2`: pump, pump-fees, the token transfer `curve_base_ata → user`, three System transfers from `user` (curve, protocol fee, buyback fee) plus one for the creator fee on a curve with a creator, pump's event = **7** (no creator) or **8**; the top-up adds 1, a completing buy adds pump's completion event (1).
  - Every pump CPI (pump-fees, Token, System, its event) sits exactly one level below pump.
- **Compute units on the ELF** (pump's outermost frames, its CPIs included; `tests/bench/m5_cu/results.md`): `sell_v2` 69.6–85.4 k, `buy_v2` 80.4–97.9 k under short.win (the upper ends after a creator change into an empty vault).
- **Graduation on the ELF** (`tests/litesvm/tests/straddle_it.rs`, `pump_it/graduation.rs`, `tests/bench/m5_cu/trace.md`):
  - `migrate_v2` top-level with the two boost accounts: 329.3–329.8 k CU, 50 inner instructions (51 entries; 53 with ComputeBudget ×2), peak stack height 4; the pool it creates is **271 bytes** and PumpSwap `extend_account` takes it to **301** (not 300);
  - on a curve that is not complete it fails with 6006; on a curve that has already migrated it is a no-op (4 entries, 56,725 CU);
  - a completing `buy_v2(rT)` under a CPI helper (`complete_curve`, A66) is 9 entries (pump's completion event included), 12 with the helper and ComputeBudget ×2.

---

## Update 2026-09-30 (M6 re-verification)

*Re-verified for milestone M6 on 2026-09-29 22:14 UTC – 2026-09-30 00:07 UTC (slots ≈ 451,773,000–451,798,083). The sections above are not edited. The PumpSwap side — T-DISABLE, negative VQR, cashback, pool sizes, the fixtures added — is in `graduated-venues.md`, Update 2026-09-30 (H1–H12); this part covers the curve program and graduation. "Candidate" means the unreleased ELFs inside `pump-rust-client` 0.2.0 (K2), run in LiteSVM in place of the deployed ones; the throwaway crate and raw outputs are in `~/.cache/shortwin-work/m6/research/`.*

### K1. Deploys and configuration: unchanged

- **No pump, pump-fees or PumpSwap upgrade** up to 2026-09-30 00:07 UTC: deploy slots 449,734,335 / 446,465,969 / 446,462,733, as in U1; the Squads multisig's `transaction_index` is still 140 (the 09-23 upgrade), so nothing is queued (`graduated-venues.md` H1).
- **`Global` and pump's `FeeConfig` are byte-identical** to the `pump_state` fixtures (read at slot 451,785,107): fees 95 + 30 bps in one tier, `pool_migration_fee` 15,000,001, the recipient lists.
- **Sources.** PPD: no commit after `cb188ce0`. The on-chain pump IDL is still the 2026-09-12 one. npm `@pump-fun/pump-sdk` is still 2.0.0. **`pump-rust-client` 0.2.0** (crates.io, 2026-09-29 16:36:53 UTC) is new: K2.

### K2. `pump-rust-client` 0.2.0: the next pump release, visible before it ships (UNVERIFIED as the deployment)

The client that PPD's `CPI_README.md` recommends for CPI now ships IDLs, math and ELF artifacts (`pump.so` sha256 `36dfc0a0…`, 1,769,160 B; the deployed ELF is `742f3c81…`, 1,664,408 B) of a pump program with, compared with PPD's `idl/pump.json`:

- **The U7 cap byte has a name: `partial_fill: OptionBool`**, appended to `buy`, `buy_v2`, `buy_exact_quote_in_v2` and `buy_exact_sol_in`. The client's README: "A buy that would take more than a curve's remaining tokens fills only the remainder and completes the curve (the v2 and v3 builders request `partial_fill`)". Default (absent / false): no partial fill.
- **v3 trades with fee buckets:** `buy_v3` (`07051dc4…`), `sell_v3` (`1c92de77…`), `buy_exact_quote_in_v3`, 17 accounts each (`global`, both mints, both token programs, the curve and its two ATAs, `user` S, the user's two ATAs, UVA, `fee_config`, `buyback_fee_recipient` W, System, event authority, program: no fee recipient, no creator vault, no pump-fees CPI). The buyback slice is paid in the trade; the rest of the protocol fee and the creator fee stay in the curve, counted in **`BondingCurve.creator_fee` @125 and `protocol_fees` @133** (u64 each; the account grows to 141 bytes). Cashback coins stay on v2. Permissionless `sweep_creator_fee` / `sweep_protocol_fee` (13 accounts) pay them out; `distribute_creator_fees*`, `update_fee_shares*` and `admin_cto` fail 6095 `CreatorFeesNotSwept` until the creator bucket is swept (client README).
- **Also new:** `set_quote_control_mint_reserves` / `set_quote_control_reserves_admin` (a quote-control admin sets a custom quote mint's initial virtual reserves) and errors 6094–6097. `migrate_v2`'s 27 accounts are unchanged.
- The PumpSwap half of the same release (v2 pool trades, fee buckets in the pool, negative VQR as the bucket offset, 287-byte pools) is `graduated-venues.md` H2.

### K3. The candidate curve in LiteSVM (with the deployed and with the candidate PumpSwap)

On the `near` fixture (`tcand2.txt`, `tcand4.txt`):

- **short.win's `buy_v2` is unaffected:** with 24 bytes of data (no `partial_fill`) a buy of `rT + 1` still fails 6021; with the byte set to 1 it fills exactly `rT` and completes the curve (U7, now under its name). After a v2 completion the curve holds exactly its rent above `rQ` (1,417,320 on 151 bytes): v2 still pays every fee out in the trade.
- **v3 leaves fees in the curve:** a `buy_v3` of 10¹² tokens left `creator_fee` 1,161,764 and `protocol_fees` 1,839,459 in the curve (3,001,223 lamports above `rQ` + rent); the 151-byte curve did not grow (it already holds @125–@141).
- **Graduation is unaffected by buckets:** a completing `buy_v2(rT)` and `migrate_v2` after that v3 trade succeed (329.7 k CU; 330.1 k with the candidate PumpSwap); the pool gets the same reserves as without it — quote vault 67,405,853,922, VQR 17,584,505,328, boost vault 17,584,505,328, as on the deployed programs (M5 U8) — so `B̂_str` is unchanged. The buckets stay in the migrated curve account (4,418,543 lamports = rent + buckets) for a later sweep. With the candidate PumpSwap the new pool is **287 bytes** (271 with the deployed one).
- The migrated quote does not depend on the curve's path before completion (the curve's reserves at `rT = 0` are fixed by `k`), so v3 trades before a completion cannot move `B̂_str` either.

### K4. Graduation today

- **Census, 2026-09-29 20:30–22:47 UTC** (slots 451,749,681–451,780,329; the last 250 successful transactions on `39azU…`): **195 pool-creating migrations**, all creating a **271-byte** pool with `base_amount_in` 206.9·10¹²; **147 non-mayhem** (142 SOL, 1 USDC, 4 custom quote) got VQR = ⌊`quote_amount_in`·2069/10⁴⌋ exactly; **48 mayhem** got no boost. U8's rule holds.
- **A PumpSwap `disable_flags` create_pool bit halts graduation.** With bit 0 set, `migrate_v2` fails 6017 `DisabledCreatePool` (from PumpSwap's `create_pool`, `create_pool.rs:193`), so a completed curve stays migrating until the flag is cleared; bits 1–5, each set alone, do not stop it (bit 3 leaves `init_boost` working and halts only the later `boost_buy_and_burn`) (`tmigrate.txt`; `graduated-venues.md` H4).
- **The pool `migrate_v2` builds on the ELF** (unchanged): 271 B, VQR 17,584,505,328, quote vault 67,405,853,922, base vault 206.9·10¹², boost ATA 17,584,505,328 on the `near` fixture; 329,458 CU.

### K5. What this changes for the curve adapter (M5, as built)

- Nothing breaks: `buy_v2` / `sell_v2` keep their accounts and data, 24-byte buys still revert above `rT`, fees are still paid out in v2 trades, and `migrate_v2` builds the pool `B̂_str` prices.
- The curve account's lamports can now exceed `rQ` + rent by the v3 fee buckets; short.win reads `rQ` from the curve data and never the curve's lamports, so no reader changes. How the candidate's rent-reserve top-up (U10) treats the buckets was not tested.

### K6. Still UNVERIFIED after this pass

- Whether and when the K2 programs are deployed, and whether they equal the 0.2.0 artifacts.
- Whether `partial_fill` stays an optional trailing byte on `buy_v2` in the deployed release (its default is "no partial fill" in the candidate).

---

## Update 2026-10-03 (M10)

- **The three pump programs were redeployed on 2026-10-02.** `scripts/dump-fixtures.py check --live`
  at slot 452,907,481 reads deploy slots 452,654,932 (pump), 452,654,882 (PumpSwap) and 452,655,002
  (pump-fees), 2026-10-02 15:47:07Z–15:47:39Z, against the fixtures' and
  `VENUE_GUARD_SLOTS_2026_10_02`'s 449,734,335 / 446,462,733 / 446,465,969. Whether these are the
  K2 programs (K6's first open question) is **UNVERIFIED**: nothing was dumped or compared yet, and
  the repository's curve and PumpSwap tests still run the 2026-09 ELFs (`graduated-venues.md`,
  Update 2026-10-03, I3). *Resolved below.*
- **K6 resolved (M10 pump revalidation).** They are the `pump-rust-client` 0.2.0 release, not
  byte-identical to its artifacts (the same discriminators, `.rodata` and error sites; `.text` +312
  B on pump; `graduated-venues.md` I4 has the method and the full instruction diff). ELF to ELF,
  pump adds only `buy_v3`, `sell_v3`, `buy_exact_quote_in_v3`, `sweep_protocol_fee` and
  `sweep_creator_fee`; `set_quote_control_*` predate the redeploy (the 2026-09 ELF dispatches
  them; they are new only against the on-chain IDL).
- **Curves grow to 141 B** on their first v2/v3 trade since the redeploy (the layout through the
  fee buckets `creator_fee` @125 and `protocol_fees` @133), which corrects U10's 125 for the new
  build: 81,280 lamports from a 125-B curve at minimum rent, 132,080 from 115 B, taken from the
  trader (PUMP-2026-10-2, disclosed; an open whose tolerance cannot cover it fails
  `ProceedsBelowMin`).
- **v3 trades** keep their fees in the curve's buckets (outside `rQ`), and v3 buys credit `n(q)`
  without ε₁ (1 lamport below the v2 quote); after v3 trades the migrated pool is seeded at `Y_p`,
  not `Y_p − 1`, never above, so `B̂_str` still covers the bundle but its 1-lamport margin can
  vanish (PUMP-2026-10-3). `migrate_v2` moves `rQ` and leaves the buckets in the curve.
  short.win's curve adapter reads `rQ` as the curve's real SOL, so the buckets need no change
  there (`pump_it::buckets`).
- Every curve test runs the redeploy (`pump_programs_2026_10`); T-LATCH replays the real upgrade.

## Update 2026-10-13 (the pump-final re-pin; short.win 1.0.1)

The design `pump-final` (2026-10-13) and its phases P0 … P8; the figures are the re-pin's gates G0
and G0b on the dumped ELFs (`tests/fixtures/README.md`, `pump_math_it`). Where this conflicts with
the sections above, this update and ARCH win.

- **Deploys.** pump was redeployed on **2026-10-08** (slot 454,596,459, trimmed ELF `6956c2f8…bf6f`)
  and again on **2026-10-13** at 15:40 UTC (slot 456,494,698, `e2020969…52be`; the same upgrade
  authority); PumpSwap on 2026-10-08 (454,596,406) and pump-fees on 2026-10-12 (456,132,171), see
  `graduated-venues.md`'s update. The 10-13 build changes only `create_v2`'s quote chain (a quote
  chain of pump coins, depth capped at 5, pump's new error 6109 `QuoteChainAccountsRequired`); every
  path short.win uses runs the same code (G0b: 180 transactions on both builds, equal logs, accounts
  and CU). Mainnet's `Global.max_curve_depth` is **2** since slot 456,495,021 (`set_max_curve_depth`).
  Fixture groups `pump_programs_2026_10_13`, `pump_programs_2026_10_13b`, `pump_state_2026_10_13`;
  baseline `VENUE_GUARD_SLOTS_2026_10_13`.
- **Layouts.** `BondingCurve` is **166 bytes** (`create_v2` allocates it; every v2 or v3 trade grows a
  shorter curve to it): `creator_fee` @125 and `protocol_fees` @133 (the fee buckets of v3 trades,
  outside `rQ`), `depth` @141 (u8; 0 for a SOL curve), `initial_virtual_quote_reserves` @142 (`vQ −
  rQ` of a `create_v2` curve, `ivqr`; 30e9 on a SOL curve), `post_complete_base_out` @150 (`pcB`) and
  `post_complete_quote_in` @158 (`pcQ`). `Global` is **1,088 bytes** (`max_curve_depth` @1087). The
  growth tops the curve's rent reserve up from `user`, the reserve counted as `lamports − rQ −` the
  buckets: 76,200 lamports from 151 B, 127,000 from 141, and **261,600** from the 49-byte legacy
  curve (its reserve holds the old rent; not 594,360).
- **v3 trades** (`buy_v3` `07051dc4…`, `sell_v3` `1c92de77…`, `buy_exact_quote_in_v3`; 17 accounts,
  24 data bytes plus an optional `partial_fill` byte ignored on non-mayhem coins): a buy charges
  `⌊q·vQ/(vT − q)⌋ + 1` — `⌈⌉` off an exact division; on every one of the 400 exact divisions G0
  constructed pump took `⌊⌋ + 1`, v2's charge —, a sale pays `⌊⌋`, each fee component `⌈⌉`; the
  buyback slice is paid out and the rest of the fees stays in the buckets; `vQ − rQ` is unchanged.
  `TradeEvent.ix_name` reads `buy` / `sell` / `buy_exact_quote_in`, not the `*_v3` names. A cashback
  coin is refused (6094 `CashbackCoinNotSupported`) on each v3 trade.
- **Synthetic migration.** `buy_v3(rT + δ)` buys `rT` on the curve, which completes, and `δ` from the
  would-be pool `(X_s, Y_s)` = (the live `curve_base_ata − rT`, donations counted; `rQ + n₁ − fee`)
  at `⌈Y_s·δ/(X_s − δ)⌉` (`⌈⌉` on an exact division too), both parts at the curve tier of its
  **pre-trade market cap on the mint's live supply** (G0's tier tripwire (e)); `max_sol_cost` covers
  both; `NotEnoughTokensToBuy` if `δ ≥ X_s`. pump writes `pcB = δ`, `pcQ` = the pool part's quote;
  `rQ` excludes `pcQ`, and `vQ − rQ = ivqr` still holds. Events: `TradeEvent`, `CompleteEvent`,
  `PostCompleteBuyEvent` (`6fb06d8b…`). `buy_exact_quote_in_v3` past `rT` spends the rest by the v2
  exact-quote-in rule, `⌊(in − 1)·X_s/(Y_s + in − 1)⌋`. On the 2026-10-02 build a buy past `rT`
  reverts (6021). Mainnet examples: 98u8RJ's completion `2vVRr9eg…` (pool part 6,238,850,918,425
  tokens for 2,642,475,552 lamports), the stuck depth-1 completion on `E1bYRCNq…`; 11 of 11 sampled
  synthetic completions follow the rule.
- **`migrate_v2` after a synthetic completion** seeds the pool with `(X₁, Y₁)` = (the live
  `curve_base_ata`, `rQ + pcQ − fee`), donations before or after the completion included, and books
  the **boost** `VQR = ⌊X₁·Y₁/10¹⁵⌋` (17,669,496,403 lamports with 1e12 donated before, 35,169,012,080
  with 206.9e12 donated, 17,852,426,396 with 3e12 donated after, against 17,584,506,040 by the proportional
  `⌊(Y₁ − pcQ)·2069/10⁴⌋`, which holds only without a donation); `vault + VQR = Y₁` each time, so the
  boost moves no price. 330,859 CU top-level, as after an exact completion.
- **Multi-hop.** `multi_hop_curve_swap` (`e19a7516…`) and `set_max_curve_depth` are new; short.win
  uses neither (exact-in only, one more CPI level, a mandatory WSOL ATA, and only for coins quoted in
  pump coins, which short.win refuses: ADOPTED Q6).
- **What short.win does with it (1.0.1; ARCH §2.4.1, §4.3, §5.4):** every curve leg trades v3 (10
  TradeExtras, no pump-fees CPI, no creator vault); a buyback past `rT` is one `buy_v3` priced
  `B̂_v3` at the live supply's fee (known issues KI-77); a `PumpMigrating` market prices the pool
  `migrate_v2` creates, ρ-scaled (PUMP-2026-10-13-1); a live cashback curve is refused
  `CurveCashbackCoin` (6109); U7's cap byte (`partial_fill`) is never sent. The straddle bundle of
  §7 / U8 and `complete_curve` are retired.
