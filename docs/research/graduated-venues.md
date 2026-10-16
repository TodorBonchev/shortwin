# Graduated venues: where graduated tokens trade, and how short.win can price and trade them on-chain

*Research note for short.win (spec baseline: `docs/SPEC.md` v0.3). Researched 2026-09-26/27 (UTC). Written by the venues research agent.*

**Scope:** PumpSwap (the pump.fun AMM) in depth; older graduates and non-pump tokens (Raydium, Meteora, Orca); pool age; USD liquidity thresholds and oracles; admin risk. Ends with design implications.

**How claims are sourced.** Every fact has a citation in brackets. Short keys:

| Key | Source |
|---|---|
| **PPD** | `github.com/pump-fun/pump-public-docs` @ `81091419e4457566469d4e2a27f64ed84d42419c` (HEAD, 2026-09-14), the official pump.fun docs and IDLs |
| **SDK** | npm `@pump-fun/pump-swap-sdk@1.20.0` (published 2026-09-10), `package/src/...` (the official TS SDK, which ships its source) |
| **SDK119** | npm `@pump-fun/pump-swap-sdk@1.19.0` (2026-07-15) and `1.18.0` (2026-06-23), for version history |
| **RPC** | Live Solana mainnet JSON-RPC (`api.mainnet-beta.solana.com` / `solana-rpc.publicnode.com`), queried 2026-09-26 21:40–22:20 UTC, decoded with the PPD IDLs |
| **TX-MIG** | `4e9yHCpwCtavYfP9dGyF8iV4HXYKMiq6y5sgsdwC1gaFwk2Hj3vLKuWDYH6CgMKYG3MFUFdLxGVGXzk39uZRQf82`: `migrate_v2` of mint `F4awKX7vwGP6KnCysp2dUsAudPUeGTDcWx17jXsgX8fx`, 2026-09-26T21:41:02Z |
| **TX-BUY** | `udGBnNtpvjTfHfSsyTJBQPsnzij272pNWoReZUk9gSHoFeVR42wodzmSrRFxJCNZWh1gHXUeGujFXDuKrcaJEL7`: top-level PumpSwap `buy` on that pool, 2026-09-26T21:53:05Z |
| **TX-BUYQ** | `41XkmCLxxBtpofnPqxzocnoW3W6nxpY2ufA71ho3Qi9hdK9qW2eY6VFcXQsHUR2tS6egaBT5dWpU5FS6VpHRNmL4`: top-level `buy_exact_quote_in`, 21:53:06Z |
| **TX-SELL** | `2oSvEbDT5rSgRKbcFU3cFgCX2P2i8YednSzNfba9q45PkDJhkLESXamhKCBV5xQs96vXSvPBbbjKf8LVzGbVZetE`: top-level `sell`, 21:53:15Z |
| **TX-JUP** | `5twfLQzdMxszoQs4wUU61N2Zd9qUtCik4Z2e6ardyhgTT4JfSD2WAC6ayMfLQXZjt9RuUfwwAJxAsN2H6tuQsWTG`: Jupiter `RouteV2` CPI into PumpSwap `sell` on the same pool |
| **TX-BOOST1 / TX-BOOSTN** | `2qnnD91UCc8K4esB8jwPLhQJ9wL532smNfs3PZDS5N2KneYFjQQz8nbx6vD5pmSYKDdsU7rosPMowx9Aywv1F1Kd` (first) and `2kU7TczVoEpPMPCXmDCdMdrHrxtwejgFHdQKw2k7NchqA24EpxE4MysNUvZyuxp3aFczWELLzSKVGE4PcNeAXs9M` (last): `boost_buy_and_burn` on the same pool |

Anything marked **UNVERIFIED** could not be confirmed from a primary source or from on-chain data.

---

## 0. TL;DR

1. **Graduated pump.fun tokens trade on PumpSwap.** The program is `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`, a constant-product AMM [PPD `docs/PUMP_SWAP_README.md`].
   - All 29 successful migrations among the last 60 transactions touching pump's `withdraw_authority` (2026-09-26 21:15–21:43Z) created a PumpSwap pool through `migrate_v2` [RPC, `getSignaturesForAddress(39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg)`].
   - The **canonical pool can be derived from the mint alone**. It cannot be spoofed and its LP is 100% burned, so it is the right on-chain price reference.
2. **New since July 2026: "boost" and virtual reserves.** The docs still say `virtual_quote_reserves` is 0 everywhere. On-chain, that is false for new graduates.
   - At migration, `init_boost` moves **exactly `floor(quote_amount_in × 2069 / 10000)`** (20.69% of the migrated quote; ≈ **17.58 SOL** for a normal ~84.99 SOL graduation) of the pool's real quote into a boost vault, and sets `Pool.virtual_quote_reserves` to that amount. The rule matched all 130 non-mayhem migrations checked on 2026-09-27, including USDC- and custom-quote pools. Mayhem migrations get no boost (§1.4).
   - A pump.fun-controlled key then **buys and burns** the token with that SOL. In the observed pool the vault was fully spent **within 6 minutes** of graduation [TX-MIG, TX-BOOST1, TX-BOOSTN].
   - Consequences for us:
     - Pricing must use **effective quote reserves = vault balance + `virtual_quote_reserves`**.
     - Sells are capped by *real* reserves.
     - Every new non-mayhem graduate gets a mechanical buy of 20.69% of its migrated quote (about 17.6 SOL for SOL coins) right after migration.
3. **Swap math verified to the lamport.** We checked the exact buy (exact-out), buy (exact-in) and sell math, including ceiling rounding and market-cap-tiered fees (1.25% total at graduation), against real transactions (§2.4). short.win can compute the executable buyback cost on-chain from 4–5 account reads.
4. **A PDA can be the `user`.**
   - It needs WSOL and Token-2022 token accounts.
   - It needs a pre-created `user_volume_accumulator`.
   - Ideally it is a **data-less, system-owned PDA**, so it can pay the rent of any fee ATA the AMM creates on the fly.
   - Cost: about 75–85k CU per PumpSwap swap, 24–26 accounts [TX-BUY, TX-SELL].
   - Mainnet already shows PumpSwap `sell` with a program-owned PDA `user` under CPI. A `buy` with a PDA `user` has not been observed (§3.1).
5. **No creation timestamp exists on-chain** for PumpSwap pools, Raydium AMM v4 pools (`pool_open_time` is 0 on pump's old migration pools), or Orca pools. Raydium CPMM's `open_time` is a valid *lower bound* on age.
   - short.win has to record graduation time itself, by crank.
   - For tokens listed after they graduated, it must assume they are "young" at first.
6. **USD thresholds:** use a hybrid.
   - Store thresholds in USD and convert with a cached SOL/USD price.
   - Refresh the cache permissionlessly from Pyth's sponsored SOL/USD `PriceUpdateV2` account `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE`.
   - *Corrected 2026-09-27:* it is **not** updated every ~1–4 s. Sponsored shard-0 feeds are pushed on a heartbeat (Pyth docs: 55 s default) or on a 0.5% price move. Measured `publish_time` steps were 52–54 s, and staleness reached ~45 s against block time (§5). Size `oracle_max_age_s` with margin above the heartbeat, and have keepers retry.
   - Switchboard reportedly ended all support on 2026-09-25.
7. **Coverage.** Among 10 sampled established memecoins that graduated before PumpSwap or were never on pump.fun:
   - 7 keep **82–99.6% of their liquidity in a Raydium AMM v4 constant-product pool**;
   - USELESS keeps 57% in Raydium CPMM;
   - BONK and TRUMP live mostly in CLMM/DLMM.
   - Sensible MVP: PumpSwap canonical pools.
   - v1.1: add Raydium AMM v4 and CPMM as reference pools.
   - Defer: CLMM/DLMM/Whirlpool pricing.
   - *Superseded by owner decision (SPEC v0.3 §0, §14 #16, 2026-09-27):* **Raydium AMM v4 and CPMM are MVP venues**, traded directly, alongside the curve and PumpSwap. Jupiter moves to v1.1. Tokens only on concentrated-liquidity venues can be listed and lent, but not shorted, in the MVP. See also §6.3, §8.9 and §8.10.
8. **Admin risk.** PumpSwap has one admin key (`FFWtrEQ4B4PKQoVuHYzZq8FabGkVatYzDpEVHsK5rrhF`) that can:
   - globally disable buy or sell;
   - change fees (they live in the separate `pfee` program);
   - toggle boost.

   All three pump programs share one upgrade authority and ship breaking upgrades several times a year.

---

## 1. PumpSwap program, IDL, Pool layout, canonical pool

### 1.1 Addresses and versions

| Item | Value | Source |
|---|---|---|
| PumpSwap (pump AMM) program | `pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA` | PPD `docs/PUMP_SWAP_README.md`; RPC (owner of every pool decoded) |
| Pump bonding-curve program | `6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P` | SDK `src/sdk/pda.ts` |
| Pump fee program (`pump_fees`) | `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ` | PPD `idl/pump_fees.json`; SDK `pda.ts` |
| PumpSwap `GlobalConfig` | `ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw` = PDA `["global_config"]` | PPD README; RPC (949 bytes) |
| PumpSwap `FeeConfig` (in `pfee`) | `5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx` = PDA `["fee_config", pAMM…]` under `pfee` | RPC (4097 bytes); derivation verified |
| `event_authority` | `GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR` = PDA `["__event_authority"]` | TX-BUY; derivation verified |
| `global_volume_accumulator` | `C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw` = PDA `["global_volume_accumulator"]` | TX-BUY; derivation verified |
| Pump `withdraw_authority` (= `Global.withdraw_authority`; writable in every `migrate_v2`, so useful for enumerating migrations) | `39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg` | TX-MIG account #1; PPD `idl/pump.json` `migrate_v2` |
| Migration tx signer / `user` (pump's migration bot; whether `migrate_v2` is callable by anyone is **UNVERIFIED**) | `9C4nRvhhVquCKATjDCx5FKvNS9PNgNqgyWy9AcoDjYv5` | TX-MIG (sole signer) |
| Upgrade authority, all three pump programs | `7gZufwwAo17y5kg8FMyJy2phgpvv9RSdzWtdXiWHjFr8` (no account data; likely a multisig vault PDA, **UNVERIFIED**) | RPC: programdata `6naEzKeUuFh1Jeeu51NXQgr5qkXgXtc9WKNct4xynVJc` (pAMM), `B5MvUwXdiW1NMM6QFFD3ssPKBujD4zMohncbM73Z2BQu` (pump), `75Uu23mqWBb8LM8vDppqC1mQAnCcBuLXhVaDezVMQLRw` (pfee) |
| Last deploys | pAMM slot 446,462,733 = **2026-09-12T15:23:56Z**; pump slot 449,734,335 = 2026-09-23T14:45:19Z; pfee slot 446,465,969 (~2026-09-12) | RPC `getBlockTime` |

**IDL sources**, most authoritative first:

1. **PPD `idl/pump_amm.json`**, refreshed 2026-09-12 in commit `e0687ae9b7`. It is the newest and includes `Pool.is_holder_reward` and `holder_rewards*` event fields.
2. **SDK 1.20.0 `src/idl/pump_amm.json`**. It is two days older and lacks `is_holder_reward`.
3. Rust crate `pump-rust-client` 0.1.13 (crates.io, 2026-09-09). PPD `docs/CPI_README.md` recommends it for CPI because its `AccountWrapper` zero-pads short legacy accounts. Its dependencies include heavy client crates (`solana-client`, `solana-rpc`, `solana-test-validator` as normal deps) [crates.io API `pump-rust-client/0.1.13/dependencies`]. For an on-chain program, a hand-built `Instruction` plus `invoke_signed` may be lighter.

### 1.2 `Pool` account layout, verified on a fresh mainnet pool

Pool `32UVHz1WQ44Azr26EJTJZbGUp2Eb6QjWKy21BrGf3k4e` belongs to mint `F4awKX7vwGP6KnCysp2dUsAudPUeGTDcWx17jXsgX8fx` and was created by TX-MIG. It is 301 bytes, owned by pAMM. Anchor discriminator `f19a6d0411b16dbc` = `sha256("account:Pool")[..8]` [RPC, PPD IDL `accounts[Pool]`].

| Offset | Size | Field | Decoded value (32UVHz…) | Notes |
|---|---|---|---|---|
| 0 | 8 | discriminator | `f19a6d0411b16dbc` | |
| 8 | 1 | `pool_bump` | 254 | |
| 9 | 2 | `index` (u16 LE) | 0 | Canonical pools use index 0 (`CANONICAL_POOL_INDEX`) [PPD README; SDK `pda.ts`] |
| 11 | 32 | `creator` | `BWNyceQn9VUdU77QBoorpqyExuyqCLgjMujB2vw44YQY` | = pump `pool-authority` PDA of the mint, which makes the pool canonical |
| 43 | 32 | `base_mint` | `F4aw…X8fx` | Token-2022 mint in this case |
| 75 | 32 | `quote_mint` | `So11111111111111111111111111111111111111112` | WSOL (legacy SPL Token) |
| 107 | 32 | `lp_mint` | `FeqnZr5QJmZ3hwJ7jAxLXEM7ENoH3ykQn3Kb98qsETAr` | = PDA `["pool_lp_mint", pool]` (verified); a Token-2022 mint |
| 139 | 32 | `pool_base_token_account` | `Dfg7J15DcjE18u9B5wtZvzdQRHA47tEXMjVyMnsVRhYv` | = ATA(pool, base_mint, Token-2022) (verified) |
| 171 | 32 | `pool_quote_token_account` | `BVDGVPY23xoUj76j78rGhYiirbyoR74CKnGpSZDJ1CTY` | = ATA(pool, WSOL, Token) (verified) |
| 203 | 8 | `lp_supply` | 4,193,388,287,908 | Supply "without burns"; the LP mint's live supply is **0**, so 100% of LP is burned |
| 211 | 32 | `coin_creator` | `D5kH3vXm6Kxg3t4jYDFkfaJfneyYFBvxgEqqcFeWSfdG` | `Pubkey::default()` on non-canonical or pre-creator-fee pools |
| 243 | 1 | `is_mayhem_mode` | 0 | |
| 244 | 1 | `is_cashback_coin` | 0 | |
| 245 | 16 | `virtual_quote_reserves` (**i128**) | **17,584,505,333** (17.58 SOL) | See §1.4 |
| 261 | 8 | `creator_fee_bps` | 0 | Per-pool creator fee override (§2.3) |
| 269 | 1 | `can_edit_creator_fee` | 0 | |
| 270 | 1 | `is_holder_reward` | 0 | Appended 2026-09-12 [PPD `HOLDER_REWARDS_README.md`] |
| 271… | — | padding up to allocated size (300/301) | | Pools shorter than 300 bytes need a permissionless `extend_account` before trading [PPD `PUMP_SWAP_CREATOR_FEE_README.md`; SDK `offlinePumpAmm.ts` `POOL_ACCOUNT_NEW_SIZE = 300`] |

**Older pools are shorter.** Per SDK `offlinePumpAmm.ts`, pools come in 211, 243, 244, 245 or 261 bytes. Missing trailing fields must be read as 0/false [SDK; PPD README "Pools written before an appended field existed…"]. Example: the docs' sample pool `GseMAnNDvntR5uFePZ51yZBXzNSn7GdFPkfHwfr6d77J` has VQR = 0 [RPC].

### 1.3 Canonical pool derivation (mint to pool)

```text
pool_authority = PDA(["pool-authority", base_mint], PUMP_PROGRAM 6EF8rr…F6P)
pool           = PDA(["pool", u16_le(0), pool_authority, base_mint, quote_mint], PUMP_AMM pAMMBay…EA)
quote_mint     = WSOL (So111…112) for SOL-paired coins; the bonding curve stores Pubkey::default()
                 for SOL, which maps to WSOL for the pool
```

- Sources: PPD `PUMP_SWAP_CREATOR_FEE_README.md` (`pump_pool_authority_pda`); SDK `pda.ts` (`canonicalPumpPoolPda`, `canonicalPoolQuoteMint`).
- **Verified:** the derivation reproduces pools `54wRtK…`, `32UVHz…`, `6DVgJX…`, `4JAnKF…` and `4bKe1w…` exactly [RPC].
- **"Canonical" test:** `pool.creator == pool_authority(base_mint)`. This same test selects the market-cap fee tiers [PPD `FEE_PROGRAM_README.md` `isPumpPool`; SDK `util.ts`].
- **Anyone can create non-canonical pools for any pair.**
  - Example: Fartcoin has a non-canonical PumpSwap pool `AmmpSnW5xVeKHTAU9fMjyKEMPgrzmUj3ah5vgvHhAB5J` with `creator` = a user wallet and `coin_creator` = default [RPC].
  - Several of the top-24h-volume PumpSwap pools (tens of $M "volume") show about $0 liquidity [GeckoTerminal `/networks/solana/dexes/pumpswap/pools`, DexScreener]. Example: pool `4xbNQWaGRFEGpiutdmEqoHtJee88giWtsiLACrcBSDcM` is non-canonical, holds 1 atom of base, and was reported with $61.9M of 24h volume [RPC + DexScreener].
  - **Never accept an arbitrary PumpSwap pool as a price reference.**
- **Quote is WSOL for SOL-paired coins, but not always.**
  - USDC-paired coins exist [PPD commit `91db6800e5` "USDC paired coins"; README "New Bonding Curve Trade Instructions"].
  - So do custom-pair ("exotic") coins. Example: canonical pool `4bKe1wsi1o682BdBxgFYvneagLdvF7SzUyDcGktjpiKK` is quoted in `98kfF7rmsg1QDUEoCqNE7g7M1FdrTt92TEp2CLzypump` [RPC].
  - In the 29 migrations sampled, 28 were SOL-quoted and 1 was a custom pair [RPC].

### 1.4 Where reserves live; virtual reserves and "boost" (new, important)

- **Real reserves:**
  - Base = `pool_base_token_account.amount` (SPL/Token-2022 amount at offset 64).
  - Quote = `pool_quote_token_account.amount`.
  - The LP fee stays in the quote vault. Protocol, creator and buyback fees leave the pool [TX-BUY balance diffs: the quote vault grew by exactly `quote_amount_in + lp_fee` = 706,095,586].
- **Effective quote reserves.** All buys and sells are priced on:

  ```text
  effective_quote_reserves = pool_quote_token_account.amount + Pool.virtual_quote_reserves
  ```

  [PPD README "Quoting: effective quote reserves"; SDK `buy.ts`/`sell.ts`; verified in §2.4].

  The docs say "`virtual_quote_reserves` is `0` on all pools today". **That is outdated.** Every non-mayhem SOL graduation sampled on 2026-09-25/26 has VQR ≈ 17.58–17.60 SOL:
  - `32UVHz…`: 17,584,505,333;
  - `6DVgJX…`: 17,584,505,711;
  - `4JAnKF…`: 17,597,859,231 (live value; its `InitBoostEvent` set 17,584,505,362, see the drift note below).

  The sampled mayhem-mode pool had VQR = 0 [RPC].

  **Boost rule (review, 2026-09-27): proportional, not a fixed ~17.58 SOL.**
  - For each migration, `create_pool`'s `quote_amount_in` arg was decoded along with the `InitBoostEvent` (pAMM self-CPI event, fields `pool`, `virtual_quote_reserves`, `real_quote_reserves_after`).
  - Sample: 144 distinct migrations from `getSignaturesForAddress(39azU…)` between 2026-09-25 03:08 and 2026-09-27 00:04 UTC.
  - **All 130 non-mayhem migrations satisfy `VQR = floor(quote_amount_in × 2069 / 10000)` to the lamport/atom.** That is 115 SOL-, 9 USDC- and 6 custom-quote pools (SPCX, PUMP, `8J69…pump`, `DJTu…`, `SQQQ…`), with `quote_amount_in` from 82,211,037 to 4,796,948,316,916 atoms. Examples:
    - SOL: 84,990,359,413 → 17,584,505,362 (`4JAnKF…`, tx `5CMjgZ18…`);
    - USDC: 12,161,433,370 → 2,516,200,564;
    - SPCX: 82,211,098 → 17,009,476.
  - **None of the 14 mayhem migrations emitted an `InitBoostEvent`** (no boost, VQR 0).
  - 2069/10000 = 206.9M / 1B is also the migrated token share (`base_amount_in` = 206,900,000,000,000 of a 1e15 supply in every sampled migration). The data cannot tell a fixed factor from `quote × base_in / supply`. Source is closed, so the rule is inferred from data only.
  - **VQR can change after the boost.** It stayed equal to the `InitBoostEvent` value during the boost, in every event traced for `4JAnKF…`.
    - But 2 pools now show a larger live VQR. Live VQR was compared with the initial VQR for 132 of the sampled pools, of which 34 were older than one day, and both drifted pools are among those 34: `CxakjwVvLftY4i6nYhdkdF23cWok88C89x7kGSPrmwKD` +617,275,524 and `4JAnKF…` +13,353,869.
    - Their boost vaults had already closed (`CxakjwVv…` boost ended 2026-09-25 15:00 UTC).
    - What changes it (possibly trade-path logic: `BuyEvent`/`SellEvent` carry `virtual_quote_reserves` and a `can_boost` flag, and `buy`/`sell` write `pool`) is **UNVERIFIED**. (*M10:* that the events carry VQR is now confirmed, Update 2026-10-03 below; what moves it is still open.)
  - ⇒ Read VQR live on every quote, never cache it, and treat it as proportional to migrated quote.
- **Mechanism, from TX-MIG's inner instructions:**
  1. `create_pool` deposits 84.990 SOL + 206.9M tokens.
  2. In the same tx, `init_boost` moves SOL from the pool's quote vault into a **boost vault**: the WSOL ATA of PDA `["boost_vault", pool]` = `35q3duB5kbVZmbLksjm1SfK5SNGbEjRoAtcFyYGqTixk` for this pool. It sets `virtual_quote_reserves = 17,584,505,333`, leaving `real_quote_reserves_after = 67,405,853,938`, so the price is unchanged.
  3. The boost authority `HTVZVEQMBsNanubDPTs3CxDAEGNFQHJY8c1441iy2S5r` (from `GlobalConfig.boost_authority`) then calls `boost_buy_and_burn` repeatedly. Each call buys base from the pool with boost-vault SOL at **zero fee** and **burns** it. VQR stays constant during the boost, while real quote rises and base falls (but see the drift note below: VQR can change later).
  4. For `32UVHz…` the first boost ran 1 s after migration and the vault hit 0 about 6 minutes later (~40 calls of ~0.39–0.71 SOL) [TX-BOOST1, TX-BOOSTN, `getSignaturesForAddress(35q3du…)`]. Across all pools the boost authority transacts every few seconds [RPC `getSignaturesForAddress(HTVZ…)`].
  5. **Afterwards the boost vault is closed.** For both pools checked, the boost-vault PDA and its WSOL ATA no longer exist (`getMultipleAccounts` at slot 450,831,815: `7GBdtALN…`/`35q3duB5…` for `32UVHz…`, and `6sJR2sjB…`/`A4xZKuxs…` for `8DY32U41…` all return null).
     - Anyone can **re-create** that ATA and donate WSOL to it. The ATA program checks only the address derivation: no wallet signature and no on-curve check, and any funder may pay (`solana-program/associated-token-account@4e58a1098c8ce9bb256c0aa8d768e34d690918e9` `program/src/processor.rs` `process_create_associated_token_account`).
     - Only the boost authority can spend it: `boost_buy_and_burn` requires the signer `authority` = `GlobalConfig.boost_authority` [PPD IDL].
     - Whether pump's boost bot sweeps donated dust is **UNVERIFIED**. So "boost ATA missing or empty" is **not** a reliable end-of-boost signal (§8.4).
- **Consequences:**
  - **Sells are capped by real reserves.** Error `InsufficientRealQuoteReserves`: "effective = real + virtual is pricing-only; payout is capped at real_vault" [PPD IDL `errors`; SDK `sell.ts`]. When real SOL drains, a token has a pricing floor of `VQR / base_reserves` that sellers cannot get through.
  - **Virtual can dominate.** In `32UVHz…`, after the dump, real quote was 15.2 SOL against 17.58 virtual [TX-BOOSTN].
  - **Graduation now comes with a deterministic buy of ≈17.6 SOL within minutes.** On a pool whose effective reserve is ≈85 SOL, that alone moves price by `(1 + 17.58/84.99)² ≈ 1.46×`, before any organic flow (constant-product arithmetic on the numbers above).
  - Boost pools reject deposit/withdraw (`BoostPoolLiquidityUnsupported`) [PPD IDL errors]. Combined with 100% burned LP, **canonical boost-pool liquidity cannot be pulled; it only changes through trading.**
  - Base-mint **supply shrinks** with each burn: 817.9M vs the 1B initial for F4aw [RPC mint]. This matters because fee tiers use live supply (§2.3).
- **Interface history.** VQR and boost instructions first appear in SDK 1.19.0 (2026-07-15) and are absent in 1.18.0 [SDK119]. When boosting was first switched on on-chain is **UNVERIFIED**.

### 1.5 Other pool flavours to know about

- **Mayhem mode** (`is_mayhem_mode`): 7 of 29 sampled migrations.
  - Several migrated with almost no SOL (0.03–0.9 SOL), e.g. `9iksx5fDGrtQLgdxuzpuGcAH8WpuophJGdWZ7YF2pump` → 0.032 SOL [RPC, tx `43KUNrFVpUeePzc2xPru2WaPr8ZFvnZs8vWs8XSdgSMDBReAUpTbeDgSjNACysMbTD6VwU1qFo2zP3oAe6GRozvz`].
  - Supply around 2B (1,995,179,201,179,066 atoms) [RPC].
  - Market cap for fees uses a fixed 1e15 supply [SDK `util.ts` `PUMP_AMM_TOTAL_TOKEN_SUPPLY`].
  - Trades must use the *reserved* fee recipients [PPD `docs/FEE_RECIPIENTS.md`; SDK `fees.ts getFeeRecipient`].
  - A separate Mayhem program `MAyhSmzXzV1pTf7LsNkrNwkWKTo4ougAJ1PPg47MD4e` holds a token vault [PPD `docs/instructions/COIN_CREATION.md`].
- **Cashback coins** (deprecated for new coins; existing ones still trade) and **holder-reward coins** only change *who receives* the creator fee, not the price math [PPD README, `HOLDER_REWARDS_README.md`, `PUMP_CASHBACK_README.md`].
- **Pump mints are now usually Token-2022** with only the MetadataPointer and TokenMetadata extensions, and no mint or freeze authority [RPC mint `F4aw…`, TLV types 18 and 19]. Vanity `…pump` suffixes are no longer universal: 12 of the 29 sampled mints lack them [RPC].

---

## 2. `buy` / `sell` / `buy_exact_quote_in`

### 2.1 Instructions

Source: PPD `idl/pump_amm.json`. The discriminator is `sha256("global:<name>")[..8]`.

| Instruction | Discriminator (hex) | Args |
|---|---|---|
| `buy` (**exact base out**) | `66063d1201daebea` | `base_amount_out: u64`, `max_quote_amount_in: u64`, `track_volume: OptionBool` |
| `buy_exact_quote_in` (exact quote in) | `c62e1552b4d9e870` | `spendable_quote_in: u64`, `min_base_amount_out: u64`, `track_volume: OptionBool` |
| `sell` (exact base in) | `33e685a4017f83ad` | `base_amount_in: u64`, `min_quote_amount_out: u64` |

- There is **no exact-quote-out sell instruction**. The SDK's `sellQuoteInput` just computes a base amount for `sell` [PPD README "Mapping PumpSwap SDK methods"].
- For short.win, **`buy` is exactly the buyback primitive**: buy exactly `Q` for at most X SOL.
- `OptionBool` is a 1-byte tuple struct (`[true]`/`[false]`). Trailing optional args may be omitted: TX-BUYQ omits `track_volume` [TX-BUYQ data].

### 2.2 Ordered account list

For `buy` and `buy_exact_quote_in` (identical), confirmed by TX-BUY (26 accounts) and TX-BUYQ [PPD IDL; TX-BUY]:

| # | Account | Flags (IDL) | Constraint / derivation |
|---|---|---|---|
| 0 | `pool` | W | Pool PDA (§1.3) |
| 1 | `user` | **W, S** | Trader; authority of accounts 5–6 |
| 2 | `global_config` | – | `ADyA8h…` |
| 3 | `base_mint` | – | = `pool.base_mint` |
| 4 | `quote_mint` | – | = `pool.quote_mint` |
| 5 | `user_base_token_account` | W | Any base token account owned by `user`. No PDA constraint in the IDL; the observed tx used the ATA (Token-2022) |
| 6 | `user_quote_token_account` | W | Any **WSOL** token account owned by `user` |
| 7 | `pool_base_token_account` | W | = `pool.pool_base_token_account` |
| 8 | `pool_quote_token_account` | W | = `pool.pool_quote_token_account` |
| 9 | `protocol_fee_recipient` | – | One of `GlobalConfig.protocol_fee_recipients` (mayhem pools: reserved recipients) |
| 10 | `protocol_fee_recipient_token_account` | W | ATA(recipient, quote_mint, quote_token_program) |
| 11 | `base_token_program` | – | Token-2022 for new pump mints |
| 12 | `quote_token_program` | – | SPL Token for WSOL |
| 13 | `system_program` | – | `111…1` |
| 14 | `associated_token_program` | – | `ATokenGP…` |
| 15 | `event_authority` | – | `["__event_authority"]` |
| 16 | `program` | – | pAMM |
| 17 | `coin_creator_vault_ata` | W | ATA(`coin_creator_vault_authority`, quote_mint, quote_token_program) |
| 18 | `coin_creator_vault_authority` | – | PDA `["creator_vault", pool.coin_creator]` |
| 19 | `global_volume_accumulator` | – (clients often mark W) | `["global_volume_accumulator"]` |
| 20 | `user_volume_accumulator` | W | PDA `["user_volume_accumulator", user]`; created if missing, paid by `user` (IDL docs on `buy_exact_quote_in`) |
| 21 | `fee_config` | – | PDA `["fee_config", pAMM]` under `pfee` |
| 22 | `fee_program` | – | `pfee…VZ` |
| 23+ | **remaining accounts**, in order below | | |

**Remaining accounts**, in the order used by SDK 1.20.0 `offlinePumpAmm.ts` `buyInstructionsNoPool` / `sellInstructionsNoPool`:

1. *If `pool.is_cashback_coin`:*
   - buy: ATA(`user_volume_accumulator`, quote_mint) (W);
   - sell: that ATA (W), then `user_volume_accumulator` (W).
2. *If `pool.coin_creator != default`:* `pool-v2` PDA `["pool-v2", base_mint]` (read-only). In TX-BUY this is `6PHvNckgcxTkHQuuQpKvojG6GauPoaH7w9CvuJuLQsdw`, and **the account does not exist on-chain**: it is only an address marker [RPC `getAccountInfo` → null]. Its purpose is **UNVERIFIED**.
3. A `buyback_fee_recipient`, one of `GlobalConfig.buyback_fee_recipients` (read-only).
4. ATA(buyback_fee_recipient, quote_mint) (W).

The 2026-04-28 breaking upgrade made the two buyback accounts mandatory [PPD `docs/BREAKING_FEE_RECIPIENT.md`].

**For `sell`**, the IDL has 21 accounts: the same list **without** #19 `global_volume_accumulator` and #20 `user_volume_accumulator` (so `fee_config`/`fee_program` sit at 19/20). The same remaining accounts follow. TX-SELL has 24 accounts [PPD IDL; TX-SELL].

**Verified derivations.** Every PDA and ATA in TX-BUY was re-derived and matched:
- `user_volume_accumulator` `3JXY2VkGLhMAPCDVuvuc6Nh4QgbVsdbh4mvwuyHojRGX`;
- `coin_creator_vault_authority` `6dFXBGiYQ6tRxCWgXZ5ZDxu8bCfUqN2HnZPkjYDt7mP1`;
- `coin_creator_vault_ata` `6JoaXhc5nrT9wrs5apBU9MnksytZcEeRufAC3bviyN4p`;
- protocol fee ATA `94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb`;
- buyback ATA `HjQjngTDqoHE6aaGhUqfz9aQ7WZcBRjy5xB8PScLSr8i`.

**Fee recipients must be read from `GlobalConfig` on-chain.** The live list contains `JCRGumoE9Qi5BBgULTgdgTLjSgkCMSbF62ZZfGs84JeU` and not `CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbicfhtW4xC9iM`, which the docs list [RPC GlobalConfig vs PPD `FEE_RECIPIENTS.md`].

### 2.3 Fee structure

Live `GlobalConfig` [RPC, decoded with PPD IDL]:
- `lp_fee_basis_points` 20, `protocol_fee_basis_points` 5, `coin_creator_fee_basis_points` 5. These are legacy fallbacks, unused while `FeeConfig` exists [SDK `fees.ts`].
- `disable_flags` 0.
- `buyback_basis_points` **5000**.
- `boost_enabled` true.
- `creator_fee_configurable` true, `max_configurable_creator_fee_bps` 300.
- `mayhem_mode_enabled` true, `is_cashback_enabled` true.
- `admin` `FFWtrEQ4B4PKQoVuHYzZq8FabGkVatYzDpEVHsK5rrhF`.

**Which schedule a trade pays** (SDK `fees.ts` `feesForQuoteMint`, marked "rust reference: pump-fees FeeConfig::fees_for_quote_mint()"):
- Non-canonical pools pay `FeeConfig.flat_fees`. Live: LP 25 / protocol 5 / creator 0 = **30 bps**.
- Canonical pools, SOL-like quote, pay `fee_tiers` by market cap (table below).
- Canonical pools, USDC quote, pay `stable_fee_tiers`. Same bps pattern; thresholds are in USDC atoms: 59k, 300k, 500k, … 20M USDC.
- Canonical pools with any other quote pay `exotic_flat_fees`. Live: 20/5/5 = 30 bps. If that schedule is all zero, they fall back to `flat_fees`.
- If `GlobalConfig.creator_fee_configurable && pool.creator_fee_bps > 0`, the pool's creator bps replaces the schedule's creator bps ("rust reference: pump-amm compute_fees()").

Live `FeeConfig.fee_tiers` (SOL-quoted canonical pools), thresholds are market cap in lamports [RPC `5PHirr…`, decoded]:

```text
mcap < 420 SOL           : lp 2  / protocol 93 / creator 30  = 125 bps   <- all fresh graduates
420      ≤ mcap < 1,470  : 20 / 5 / 95  = 120 bps
1,470    ≤ mcap < 2,460  : 20 / 5 / 90  = 115
2,460 / 3,440 / 4,420    : creator 85 / 80 / 75
9,820 / 14,740 / 19,650 / 24,560 / 29,470 / 34,380 / 39,300 / 44,210 / 49,120 : creator 70 / 65 / 60 / 55 / 50 / 45 / 40 / 35 / 30
54,030 / 58,940 / 63,860 / 68,770 / 73,681 / 78,590 / 83,500 / 88,400 / 93,330 : creator 28 / 25 / 23 / 20 / 18 / 15 / 13 / 10 / 8
mcap ≥ 98,240 SOL        : 20 / 5 / 5 = 30 bps
```

**Tier selection** (SDK `calculateFeeTier`, "rust reference: pump-fees-math::calculate_fee_tier()"):
- If `mcap < tiers[0].threshold`, use tier 0.
- Otherwise use the highest tier with `threshold ≤ mcap`.
- `mcap = effective_quote_reserves × base_mint.supply / base_reserves`. Mayhem pools use supply = 1e15. This uses **pre-trade** reserves and the **live** mint supply [SDK `util.ts poolMarketCap`, `buy.ts`].
- A fresh graduate's mcap is 84.99 SOL × 1e9 / 206.9e6 ≈ **410.8 SOL < 420**, so it pays **1.25%**. TX-BUY paid tier 0 (2/93/30) at mcap ≈ 61.9 SOL.
- PumpSwap itself gets the bps by CPI into `pfee::get_fees_with_quote_mint`. Discriminator `9aed8a5ca202a2bb`; args `(is_pump_pool: bool, market_cap_lamports: u128, quote_mint: Pubkey)`; accounts `[fee_config, config_program_id]`; returns `Fees`. It uses 6,121 CU, and the return data in TX-BUY decodes to (2, 93, 30) [TX-BUY logs; PPD `idl/pump_fees.json`].

**Where fees go:**
- **LP fee** stays in the pool.
- **Protocol fee** is split: `buyback_basis_points/10_000` (= 50%) goes to the buyback recipient ATA and the rest to the protocol fee recipient ATA. This is a **split of the protocol fee, not an extra fee**. TX-BUY: protocol_fee 6,565,376 → 3,282,688 + 3,282,688; the event reports `buyback_fee` = 3,282,688 [TX-BUY balance diffs and `BuyEvent`].
- **Creator fee** goes to `coin_creator_vault_ata`. It is 0 if `coin_creator == default`. On cashback coins it goes to the trader's accumulator; on holder-reward coins, to a pump-controlled creator [PPD `HOLDER_REWARDS_README.md`, `PUMP_CASHBACK_README.md`].

### 2.4 Exact swap math and rounding (verified to the lamport)

Notation:
- `B` = `pool_base_token_account.amount`
- `Qr` = `pool_quote_token_account.amount`
- `V` = `pool.virtual_quote_reserves`
- `Qe` = `Qr + V`
- `bps_lp`, `bps_pr`, `bps_cr` come from §2.3 (`bps_cr = 0` if `coin_creator == default`)
- `fee(x, bps) = ceil(x·bps / 10_000)`

Sources: SDK `buy.ts`, `sell.ts`, `util.ts`. Each formula below was reproduced exactly from the decoded events.

**`buy` (exact out). This is the buyback cost for `b` base atoms.**

```text
require b < B
q      = ceil(Qe · b / (B − b))
cost   = q + fee(q, bps_lp) + fee(q, bps_pr) + fee(q, bps_cr)     // debited from user WSOL; require cost ≤ max_quote_amount_in
```

TX-BUY check: `B` = 482,553,257,944,361, `Qr` = 18,917,931,618, `V` = 17,584,505,333, `b` = 9,155,477,586,733.
- `q` = **705,954,395**, which matches the event's `quote_amount_in`. Using `Qr` alone would give 365,871,380, which proves VQR is used.
- Fees: 141,191 / 6,565,376 / 2,117,864.
- `cost` = **714,778,826**, which matches `user_quote_amount_in`.

**`buy_exact_quote_in`:**

```text
e0 = floor(spend · 10_000 / (10_000 + bps_lp + bps_pr + bps_cr))
if e0 + fee(e0,lp) + fee(e0,pr) + fee(e0,cr) > spend:  e = e0 − (that excess)  else e = e0
base_out = floor(B · (e − 1) / (Qe + e − 1))            // note the "− 1"
```

TX-BUYQ check: spend 706,095,586, so `e` = 697,378,355 and base_out = **8,709,390,094,921**, matching the event. Without the −1 it would be 8,709,390,107,180.

**`sell` (exact in):**

```text
q    = floor(Qe · b / (B + b))
out  = q − fee(q, bps_lp) − fee(q, bps_pr) − fee(q, bps_cr)      // require out ≥ min_quote_amount_out
require Qr ≥ q − fee(q, bps_lp)                                    // real-reserve cap (InsufficientRealQuoteReserves)
```

TX-SELL check: `q` = 667,199,838 and out = **658,859,839**, both matching the event.

All fee roundings are ceilings and the buy input is a ceiling division, so the pool never loses to rounding. **short.win's on-chain quote reproduces the program's charge exactly**, provided it uses the same pre-trade state within the same transaction.

**Spot price and marginal impact** (for the EMA and the §4 size caps; algebra on the formulas above):
- Spot price = `Qe / B` lamports per base atom.
- Buying `b` moves marginal price by `(B / (B − b))²`.
- So "buyback moves price ≤ 5%" means `b ≤ B·(1 − 1/√1.05) ≈ 2.41%·B`. For a fresh graduate (B = 206.9M tokens, Qe ≈ 85 SOL), that is ≈ 4.99M tokens, or about 2.1 SOL of notional per position.
- Similarly ≤ 20% means `b ≤ 8.71%·B` ≈ 8.1 SOL per market.

---

## 3. CPI constraints

1. **A PDA as `user`.** `user` is an Anchor `Signer`, `mut` [PPD IDL], so any PDA of short.win can be the user via `invoke_signed`.
   - Pump itself CPIs PumpSwap with PDA signers. `migrate_v2` → `create_pool` has the `pool-authority` PDA as pool creator [TX-MIG].
   - The boost vault authority PDA appears as `user` in boost `BuyEvent`s [TX-BOOST1].
   - **Sells with a third-party PDA `user` are on mainnet** (review correction 2026-09-27; the txs were already cited in `jupiter-cpi.md` §1.3 and re-checked with `getTransaction`, `maxSupportedTransactionVersion: 1`):
     - Program `FqGg2Y1FNxMiGd51Q6UETixQWkF5fB92MysbYogRJb3P` → Jupiter `route` (h2) → **PumpSwap `sell` (h3)** with `user` = `2HVnqFoy7AXGaJj4HF5wPWXRzR4G7G5L4cEQQsBTd8Vb`, tx `4dJY8x1e6SFWyBW521EZRgpeDvooTzSA8F3Xf8AYLiV1syTJhnC8VFCddsagxNqhrC5uB87gm1RhYL9BZQDkvT5y`. The user is off-curve, not a transaction signer, and **owned by `FqGg2Y1F…` with 752 bytes of data**.
     - Same shape with `user` = `8XWbPs7eR7Q6cTyhrVA2tKtyuj3qZa2ssEPJhF8fRvaV` (same owner, 752 B) in the v1-format tx `2Dh3weVYseEfmfim7dUzPfgFnx7VQrudvJu6hzX2fgtWppQK2BVb9bTM67j1qpjWGekPzDTSr6oBJQSLa8WaxkXg`.
     - Same shape again on 2026-09-27 with `user` = `2SkNL4n1dXX9H1VhY2JV41GaDtnM1rHFNV3Afjf6EgTF`, e.g. `P5H4QkXRa5pPyLqUQS2WGr9frnx9ewjU8Y8Pd2UTXrzVxQ1u6XMARL4qTPKeBVH2epPJLUVoJb1oVxQaYTYUG6h` (slot 450,828,978).

     A `sell` moves WSOL into `user_quote_token_account` by Token transfer and did not debit `user`'s lamports in these txs (all venue-side accounts existed), so even a data-carrying PDA worked. For the pump curve's `sell_v2` with a non-whitelisted PDA `user`, see `pumpfun-bonding-curve.md` §6.1.
   - **Still not observed on mainnet: a PumpSwap `buy` with a third-party PDA `user`**, in particular the System debits from `user` when the venue creates the UVA or fee ATAs. A 2026-09-27 scan of 245 recent txs of six integrator programs (`6Vo3245e…`, `FqGg2Y1F…`, `FLASHX8D…`, `GMgnVFR8…`, `4DvQwk6W…`, `FAdo9NCw…`) found PDA users only on sells. Test the buy path on a mainnet fork (**UNVERIFIED** in production).
   - **Rent payer caveat.** `buy` may create `user_volume_accumulator`, `protocol_fee_recipient_token_account` and `coin_creator_vault_ata` "unless already created", paid by `user` [PPD IDL docs on `buy_exact_quote_in`]. For cashback coins, the `user_volume_accumulator_wsol_ata` passed as `remaining_accounts[0]` "will be initialized if needed" [PPD IDL docs on `buy` and `buy_exact_quote_in`].
     - **Worst case** on a buy of an existing cashback coin: UVA 1,346,200 + 3 × 1,488,440 (protocol-fee WSOL ATA, coin-creator-vault WSOL ATA, UVA WSOL ATA) = **5,811,520 lamports** at current rent (lamports_per_byte_year 5,080, threshold 1.0: 137 B → 1,346,200; 165 B → 1,488,440).
     - `sell` also lists `system_program` and `associated_token_program` [PPD IDL], so it *may* create a missing fee ATA at `user`'s expense. The IDL docs do not say so (**UNVERIFIED**).
     - A PDA that carries data (owned by short.win) cannot be the `from` of a System Program transfer or create. Therefore:
       - **(a)** make the `user` a **data-less, system-owned vault-authority PDA** holding a small lamport buffer (it can then pay via `invoke_signed`); **and/or**
       - **(b)** pre-create the accounts in the same tx: `init_user_volume_accumulator` takes a separate `payer` (W,S) and a non-signer `user` [PPD IDL], and `CreateIdempotent` ATAs can be created for the fee recipient, the creator vault and (cashback coins) the UVA's WSOL ATA.
   - **CPI depth.** In the observed trades PumpSwap's own CPIs sit one level below it: TX-BUY (top-level) has inner max height 2, and the `sell` at h3 in `4dJY8x1e…` has inner calls at h4. So a top-level short.win instruction reaches height 3 on a normal swap.
     - `buy` and `sell` also take `associated_token_program`, and `buy` documents creating `protocol_fee_recipient_token_account`, `coin_creator_vault_ata` and the cashback UVA WSOL ATA when missing.
     - If that creation is a CPI into the ATA program (the usual Anchor `init_if_needed` pattern; pAMM is closed source, and no mainnet example was found, so **UNVERIFIED**), the chain becomes short.win (1) → pAMM (2) → ATA program (3) → Token/System (4). That leaves **one** level spare, not two. It is two only when every venue-side ATA and the UVA already exist, which clients can ensure by prepending `CreateIdempotent`.
   - Pools shorter than 300 bytes also need an `extend_account` prepended; it is permissionless and its payer is the `user` signer [PPD IDL `extend_account`; SDK `withFixPoolInstructions`].
2. **WSOL is required.**
   - PumpSwap moves quote with SPL `TransferChecked` from `user_quote_token_account` [TX-BUY logs]. The SDK wraps SOL into a WSOL ATA before buying and closes it after selling [SDK `offlinePumpAmm.ts` `withWsolAccount`].
   - The pump **bonding curve** uses **native lamports** [PPD `PUMP_CASHBACK_README.md` "Pump AMM uses WSOL whereas the Pump program uses native lamports"].
   - So a position vault that spans graduation must wrap: move lamports into its WSOL account and `SyncNative`. Alternatively, graduated positions keep their SOL as WSOL the whole time.
   - Base token accounts must be **Token-2022** accounts for new pump mints [TX-BUY account 11].
3. **Volume accumulators and other required accounts.**
   - `buy` requires `global_volume_accumulator` and `user_volume_accumulator`. Pass `track_volume = [false]`: PUMP-token incentives are irrelevant to short.win.
   - `sell` requires neither, unless the coin is a cashback coin.
   - Cashback coins need the accumulator's WSOL ATA in remaining accounts. If those accounts are omitted, the fee goes to the creator instead [PPD `PUMP_CASHBACK_README.md`].
   - Always required: `fee_config` and `fee_program`, the `pool-v2` marker, and the buyback recipient plus its ATA (§2.2).
   - Mayhem pools need a reserved fee recipient.
4. **Compute units** (measured on mainnet):
   - PumpSwap `buy`: 85,114 CU [TX-BUY].
   - `buy_exact_quote_in`: 83,513 [TX-BUYQ].
   - `sell`: 74,157 [TX-SELL].
   - Via Jupiter: PumpSwap 74,357 inside Jupiter's 85,336 [TX-JUP].
   - These include the `pfee` CPI (6,121). Budget ~100k for the swap, plus short.win's own logic and any wrap/ATA creation. Pump's FAQ recommends a static 100k limit for its bonding-curve swaps [PPD `docs/FAQ.md`].
5. **Transaction size.** 24–26 PumpSwap accounts plus short.win's market, position and vault accounts will need an **Address Lookup Table**. Keep the static PumpSwap accounts in a short.win-owned ALT: global config, programs, fee config, event authority, the 8 protocol and 8 buyback recipients with their WSOL ATAs.
6. **Jupiter path.**
   - Jupiter CPIs PumpSwap with the wallet as `user`; the instruction is `RouteV2` [TX-JUP logs].
   - Jupiter program `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4` is upgradeable (authority `CvQZZ23qYDWF2RUpxYJ8y9K4skmuvYEEjH7fK58jtipQ`, last deploy 2026-09-22T05:12:12Z) [RPC].
   - Current API is **Swap V2**, `https://api.jup.ag/swap/v2`. `/build` returns raw instructions for "custom builds, CPI, and composability", using Metis on-chain routing only. JupiterZ (RFQ) is *not* composable. The docs say all endpoints need an `x-api-key`, but keyless `GET /swap/v2/build` returned HTTP 200 on 2026-09-27 (0.5 RPS keyless tier; `jupiter-cpi.md` §2.1). Metis v1 (`/swap/v1`) is "no longer actively maintained" [developers.jup.ag/docs/swap; developers.jup.ag/docs/swap/build-swap-transaction].
   - Whether Jupiter offers **ExactOut** routes through PumpSwap is **UNVERIFIED**. The buyback needs exactly `Q` tokens.

---

## 4. Pool age

- **No creation timestamp is stored.**
  - `Pool` has none (§1.2).
  - The pump `BondingCurve` has none: its fields are reserves, supply, `complete`, `creator`, flags, `quote_mint` and fee fields [RPC `8WC5HaekFsf2wxzC6r6NbzMb3u85CNmtJ1ysZhdiSLGs` decoded with PPD `idl/pump.json`].
  - Solana accounts do not record a creation slot.
  - The only timestamps are in *events*: `CreatePoolEvent.timestamp`, `CompletePumpAmmMigrationEvent.timestamp` [TX-MIG]. Programs cannot read events.
- **Migration is fast.** The curve's completing trades and `migrate_v2` landed within about 1 s (bonding curve `8WC5Ha…`: last successful trades at t−1 s, migrate at t, failed trades after) [RPC `getSignaturesForAddress`]. Migrations can also fail and retry: two failed `InvalidAccountData` transactions appear in the `withdraw_authority` history at 21:42:22Z [RPC].
- **Other venues:**

  | Venue | Age field | Usable as age? | Source |
  |---|---|---|---|
  | Raydium CPMM | `open_time` | Yes, as a lower bound: clamped to `max(open_time, now+1)` at creation | raydium-cp-swap `initialize.rs` @59fb845a9e |
  | Raydium AMM v4 | `pool_open_time` | **No.** Not clamped; admin can reset it (`SetOpenTime`); **0** on Fartcoin's pump-migration pool `Bzc9NZfMqkXR6fz1DBph7BDf9BroyEf6pnzESP7v5iiw` | raydium-amm `processor.rs` @d26944bfb7; RPC |
  | Meteora DAMM v2 / DLMM | `activation_point` (slot or timestamp) | Lower bound, if creation-time activation holds (**UNVERIFIED**) | damm-v2 `state/pool.rs` @a85c926607; dlmm-sdk `idls/dlmm.json` |
  | Orca Whirlpool | none | No | (no field) |

- **Recommendation.** Record age on short.win's side:
  - **(a) Graduation observer.** Add a permissionless crank, `record_graduation(mint)`. It checks `bonding_curve.complete == true` and that the canonical pool exists, then writes `graduated_at = Clock::unix_timestamp` into `Market` once. Keepers call it within seconds for markets created before graduation, so age is exact there.
  - **(b) Markets created on already-graduated tokens.** Set `graduated_at = market_created_at`. This is conservative: the token sits in "< 24h" for the first day and never reaches "≥ 7 days" until 7 days after listing. It only makes tiers stricter, so it is lender-safe.
  - **(c) Optional, needs an owner decision.** An attestor role could set an earlier `graduated_at` from the verifiable `CreatePoolEvent`, bounded so it can only *raise* age once and is logged. The spec limits admin powers, so this needs a decision-log entry.

---

## 5. Measuring liquidity against USD thresholds

**Numbers today.**
- Pyth SOL/USD was **$121.544 (conf ±$0.021) at 2026-09-26T22:07:41Z** [RPC `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE`].
- $100k therefore ≈ **823 SOL** and $500k ≈ **4,114 SOL**.
- Defined as pool TVL = 2 × quote side, those are ≈ 411 SOL and ≈ 2,057 SOL of real quote reserves.
- A fresh graduate starts at 67.4 SOL real (+17.6 virtual), so TVL ≈ $16k real / $21k effective. **It is Tier C.**
- Established graduates can reach Tier A/B. The canonical e/acc pool `4JAnKF…` holds 2,455 SOL real, ≈ $597k TVL [RPC; price above].

**Which reserve to use.** For *tiering*, count **real quote reserves only**: exclude `virtual_quote_reserves` and the boost vault. For *executable cost*, use effective reserves (§2.4).

**Pyth pull-oracle model on Solana (2026):**
- **Programs:**
  - Receiver `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ` (owns price accounts).
  - Push-oracle `pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT` (derives the sponsored feed PDAs).
  - Source: `pyth-network/pyth-crosschain` @ `bd2a4b6f43`, `target_chains/solana/pyth_solana_receiver_sdk/src/lib.rs`.
  - A `pro-compatible` cargo feature switches to `rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp` / `pyt2F414BA6dPttK6RddPZUdHfapoBN24GL5wbrPCou` [same file].
- **Two ways to consume:**
  1. **Sponsored "price feed accounts"** at a fixed address `PDA([shard_id u16 LE, feed_id], push_oracle)`. Pyth keeps shard 0 fresh for common feeds [docs.pyth.network/price-feeds/core/use-real-time-data/pull-integration/solana].
  2. **Post your own `PriceUpdateV2`** from Hermes in the same tx, with Wormhole-verified or partially verified updates [same doc].
- **SOL/USD specifically:**
  - Feed id `ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d` [Hermes `/v2/price_feeds?query=SOL/USD`].
  - Shard 0 account `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE`: owner = receiver, 134 bytes, discriminator = `sha256("account:PriceUpdateV2")`, `verification_level = Full`.
  - **Update cadence (corrected 2026-09-27).** The earlier "every ~0–4 s" came from the account's signature list. That list includes every transaction that merely *reads* the account, and failed competing pushes; it is not the update rate.
    - Pyth's docs: sponsored Solana feeds use a "55 seconds heartbeat / 0.5% price deviation" by default, with a few exceptions not named per feed ([docs.pyth.network/price-feeds/core/push-feeds/solana](https://docs.pyth.network/price-feeds/core/push-feeds/solana), fetched 2026-09-27).
    - Measured by polling `getAccountInfo` every ~2 s from 00:11:24 to 00:15:24 UTC on 2026-09-27 (slots 450,831,897–450,832,790; 113 reads, RPC ≤ 1.6 s behind wall clock): only **5 distinct `publish_time`s**, stepping **54 / 52 / 53 / 53 s**. Staleness against the block time of the read slot reached **45 s** (median 27 s against wall clock).
    - A fact-check sample earlier the same day saw gaps of 25–41 s.
    - Of the last 1,000 signatures on the account (359 s), 660 succeeded; those are mostly consumer transactions.
    - Consequences:
      - "usually well under a minute old, but routinely 30–55 s old";
      - an `oracle_max_age_s` of 60 s leaves only a few seconds of margin over the heartbeat, so use ≥ 90–120 s for the tier cache, or have the keeper post a fresh Hermes update (way 2 below) when it is stale;
      - keepers must retry `crank_sol_usd` rather than expect every call to find fresh data.
  - Shard 1 `6bWEn5B8eJCRAek5acd3R7d4Sx3e7JWvt84srqqfYgt` was ~19 h stale [RPC]. **Use shard 0 only.**
- **`PriceUpdateV2` layout:** `write_authority: Pubkey`, `verification_level` (enum), `price_message { feed_id [32], price i64, conf u64, exponent i32, publish_time i64, prev_publish_time i64, ema_price i64, ema_conf u64 }`, `posted_slot u64` [decoded on RPC; SDK `price_update.rs`].
- **Rust:**
  - Crate `pyth-solana-receiver-sdk` **2.0.0** (2026-06-15), built against `anchor-lang = "1.0.2"` and `solana-program = "3"` [crates.io versions API; pyth-crosschain `target_chains/solana/Cargo.toml`].
  - Use `PriceUpdateV2::get_price_no_older_than(&clock, max_age, &feed_id)`. It requires Full verification; there is a `…_with_custom_verification_level` variant [`price_update.rs`].
  - Validate the account owner and the feed id [Pyth doc above].
  - **Check Anchor version compatibility** with the toolchain choice.
- **Switchboard: do not use.** Secondary sources report that Switchboard Technology Labs announced on 2026-09-19 that "all implementations are deprecated effective immediately" and support ended 2026-09-25 [solanacompass.com/projects/switchboard; crypto.news 2026-09-25]. The primary announcement was not located (**UNVERIFIED**).

**Options:**

| Option | How it works | Pros | Cons |
|---|---|---|---|
| A. SOL-denominated thresholds | `GlobalConfig.tier_liq_lamports[]`, changed by timelocked (48h) param update | No oracle; deterministic; cheapest; no oracle-failure modes | USD meaning drifts with SOL price (±50%/quarter is common); needs governance action to stay honest |
| B. Pyth on every evaluation | Pass the sponsored `PriceUpdateV2` into every instruction that computes tier | Exact USD semantics | +1 account everywhere; staleness/confidence handling on hot paths; a Pyth outage or sponsor lapse blocks tier evaluation; tier can flap as SOL moves |
| **C. Hybrid (recommended)** | Thresholds in USD in config. A cached `sol_usd` (price, conf, ts) in `GlobalConfig`, refreshed permissionlessly by `crank_price` from shard 0 (accept only if `publish_time` is fresh enough, `conf/price` ≤ 1–2%, and the change per update is bounded; *corrected 2026-09-27:* with a ~55 s heartbeat a 60 s bound fails intermittently, so use ≥ 90–120 s or post a Hermes update when stale). Tier evaluation converts USD→SOL with the cache. If the cache is older than e.g. 1 h: **no tier upgrades and no new opens**, with Tier C assumed for graduated tokens. **Closes and liquidations never depend on it.** | Keeps USD semantics; the oracle stays off hot paths; fails safe | Slightly more state; a manipulated or stale oracle only affects *new* opens |

**Also recommended, for the protocol/economics agents:** fix liquidation-relevant tier parameters **at position open**, so that an oracle or liquidity move cannot retroactively change a position's liquidation CR.

---

## 6. Older graduates and tokens never on pump.fun

### 6.1 Where volume and liquidity are (as of 2026-09-26)

**DefiLlama, Solana**, categories *Dexs + Launchpad*: 30-day volume $79.5B [`api.llama.fi/overview/dexs/solana`, fetched 2026-09-26 ~22:10Z].

| Venue | 30d volume share | 24h volume share | Solana TVL [`api.llama.fi/protocols`] |
|---|---|---|---|
| PumpSwap | **21.7%** ($17.2B) | 18.0% | $400M |
| Raydium (DefiLlama "Raydium AMM": v4 + CPMM + CLMM, **UNVERIFIED** split) | 10.9% | 8.7% | $1,364M |
| Orca (Whirlpools) | 9.3% | 9.8% | $298M |
| Meteora DLMM | 7.6% | 8.6% | $191M |
| pump.fun bonding curve | 3.5% | 5.0% | — |
| Meteora DAMM v2 / v1 / DBC | 0.4% / 0.1% / 0.5% | — | $25.5M / $52.9M / — |
| Raydium LaunchLab | 0.4% | 0.8% | — |
| Prop AMMs / CLOBs (BisonFi, Manifest, HumidiFi, Tessera V, Scorch, QuantumAMM, GoonFi, AlphaQ, SolFi…) | ≈ 40% combined | | Mostly SOL/stable majors (**UNVERIFIED** from primary data) |

- PumpSwap turns over about 43× its TVL per 30 days (17.2B / 0.40B).
- The figures above are for *all* tokens. **There is no primary-source split of memecoin-only volume.**

**Per-token liquidity split**, sampled via DexScreener `token-pairs/v1/solana/{mint}` (fetched 2026-09-26 ~22:15Z; share of listed pool liquidity; DexScreener returns at most 30 pools; an unlabeled `raydium` pool is treated as AMM v4, which was verified on-chain only for Fartcoin's pool, the rest is inferred from DexScreener labelling CPMM/CLMM explicitly):

| Token (origin) | Deepest venue | Share of liquidity in constant-product pools |
|---|---|---|
| Fartcoin (pump, graduated to Raydium v4 in Oct 2024) | Raydium AMM v4 `Bzc9NZ…` ($9.27M; owner `675kPX9…` and `pool_open_time = 0` verified on RPC) | 82% |
| PNUT, GOAT, jellyjelly (pump → Raydium v4) | Raydium AMM v4 | 96–99.6% |
| WIF, POPCAT, MOODENG (non-pump or early) | Raydium AMM v4 | 96–99% |
| USELESS (letsbonk/LaunchLab) | Raydium **CPMM** | 57% (DLMM 32%, Orca 10%) |
| e/acc (pump, 2026) | Meteora DLMM 67% / PumpSwap canonical 33% | 33% |
| BONK | Orca 49%, DLMM 33%, Raydium CLMM 11% | ~3% |
| TRUMP (Meteora launch) | Meteora DLMM 83% | ~0% |

**Timeline.** Before PumpSwap (docs first published 2025-04-04 [PPD commit `df5013e0f9`]; launch widely reported as March 2025, exact date **UNVERIFIED**), pump.fun migrated graduates to **Raydium AMM v4** with LP burned. That is why the older pump memes above are v4-heavy. Everything since migrates to PumpSwap canonical pools (§1).

### 6.2 On-chain feasibility of computing the executable buyback cost

| AMM | Program ID | Curve | Reserves read on-chain | Difficulty | Notes |
|---|---|---|---|---|---|
| PumpSwap | `pAMMBay6…EA` | x·y=k over (real + virtual quote) | 2 vaults + Pool (VQR) + mint supply + FeeConfig | **Easy** (§2.4) | Canonical pool derivable from the mint |
| Raydium AMM v4 | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` | x·y=k | `vault.amount − state_data.need_take_pnl_{coin,pc}`; fee `swap_fee_numerator/denominator` (25/10000 on `Bzc9NZ…`); offsets: need_take_pnl 192/200, pool_open_time 224, coin/pc vault 336/368 | **Easy** | `calc_total_without_take_pnl_no_orderbook` [raydium-amm `math.rs`, `processor.rs`, `state.rs` @d26944bfb7; RPC]. Native (non-Anchor) program; `swap_base_in`/`swap_base_out` take 17–18 accounts |
| Raydium CPMM | `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C` | x·y=k | `vault − (protocol_fees + fund_fees + creator_fees)` (`vault_amount_without_fee`); `swap_base_output` exists | **Easy** | Has `open_time`; `update_pool_status` admin can pause [raydium-cp-swap `states/pool.rs`, `lib.rs` @59fb845a9e] |
| Meteora DAMM v2 | `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG` | Concentrated liquidity on **one** price range `[sqrt_min, sqrt_max]` (`liquidity`, `sqrt_price`) | Pool account only | **Medium** | Closed-form single-range math, but base fee scheduler and dynamic (volatility) fees add complexity; `set_pool_status`, `update_pool_fees` admin [damm-v2 `state/pool.rs`, `lib.rs` @a85c926607] |
| Meteora DLMM | `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo` | Discrete bins | LbPair + N bin-array accounts; iterate bins | **Hard** | Variable fees; account list depends on price path [dlmm-sdk `idls/dlmm.json` v0.12.0] |
| Orca Whirlpool | `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc` | CLMM ticks | Whirlpool + tick arrays; tick-crossing loop | **Hard** | [orca-so/whirlpools `programs/whirlpool/src/lib.rs`] |
| Raydium CLMM | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK` | CLMM ticks | Pool + tick arrays + bitmap extension | **Hard** | [raydium-clmm `programs/amm/src/lib.rs`] |

"Hard" means feasible but CU-heavy (~100k+ per crossing loop, **UNVERIFIED** estimate), and the keeper must supply the right tick or bin arrays. Keeper-supplied accounts can be *under-supplied* to fake "no liquidity", so the program must validate them.

### 6.3 MVP coverage recommendation

> **Superseded in part (review, 2026-09-27).** The owner has decided (SPEC v0.3 §0, §2.1, §3, §14 #16):
> - Raydium AMM v4 and CPMM are **MVP** venues, traded directly;
> - graduated pump.fun tokens trade the canonical PumpSwap pool directly;
> - Jupiter is a **v1.1** optional better-price path, bounded by the on-chain pool quote;
> - CLMM/DLMM/Whirlpool-only tokens are listable and lendable but not shortable.
>
> The analysis below is kept as research context. The "v1.1" and "via Jupiter" parts no longer describe the plan.

- **MVP: "graduated" = pump tokens with an existing canonical PumpSwap pool, SOL-quoted.**
  - Covers every pump graduate since ~March 2025.
  - The pool is derivable from the mint, cannot be spoofed, and its LP is locked.
  - Reject at market creation, or keep in Tier D-like handling (owner call): USDC and exotic-quote pools, and mayhem-mode pools.
- **v1.1: reference pools of type Raydium AMM v4 or CPMM (constant product)** for pre-PumpSwap pump graduates and LaunchLab/letsbonk tokens.
  - Selection rule, since these are not derivable from the mint:
    - the market creator names the pool;
    - the program checks the program owner, that the mints are {token, WSOL}, and a minimum real liquidity;
    - anyone may switch the reference to a *strictly deeper* pool of a supported type (EMA reset rules apply).
  - Liquidity split across venues does not matter much for safety as long as the reference pool is the deepest constant-product pool. Jupiter may execute better than the reference quote, never worse beyond slippage.
- **Deferred: DAMM v2, DLMM, Whirlpool, CLMM** as reference pools. Tokens whose only meaningful liquidity is there (e.g. BONK, TRUMP) stay unsupported for "graduated" tiers until a CLMM quoting module exists. Jupiter can still *execute* through those venues.
- **Execution.** The spec (§2.1, Q16) says graduated trades go "via Jupiter". The findings suggest a guarded approach:
  - The on-chain reference-pool quote is the **ceiling** on the SOL a buyback may spend, and the **floor** on the SOL a short sale must receive.
  - Any Jupiter route must beat or match it within slippage.
  - Add a **direct PumpSwap `buy`/`sell` adapter** as the fallback, and as the default for canonical-pool tokens: exact-out buyback, 24–26 fixed accounts, ~85k CU, no API key, no dependence on Jupiter uptime for liquidations (S3). This is an owner decision because it extends Q16's "Jupiter" venue.

---

## 7. PumpSwap admin powers that could affect open positions

Sources: PPD `idl/pump_amm.json`, `idl/pump_fees.json`, `docs/PUMP_SWAP_README.md`; RPC GlobalConfig/FeeConfig.

| Power | Who | Effect on short.win |
|---|---|---|
| `disable(create_pool, deposit, withdraw, buy, sell)` via `disable_flags` | `GlobalConfig.admin` `FFWtrEQ4…rhF` | **Global** halt of buys and/or sells across all pools (errors `DisabledBuy`/`DisabledSell`). Closes, liquidations and opens all fail. This is the spec's S3 "venue halted" case. There is **no per-pool pause field** in `Pool` (§1.2) |
| `update_fee_config` (GlobalConfig fees, recipients, `coin_creator_fee_basis_points`, `admin_set_coin_creator_authority`) | admin | Legacy fallback fees; recipient lists change, so **fee recipients must be read on-chain per tx** |
| `pfee` `update_fee_config` / `upsert_fee_tiers` / `update_stable_fee_config` / `upsert_stable_fee_tiers` / `set_exotic_flat_fees` / `set_disable_flags` | `FeeConfig.admin` = the same `FFWtrEQ4…rhF` [RPC] | Changes swap fees at any time, with no on-chain timelock visible in the IDL. Buyback cost is fee-sensitive; today's range is 30–125 bps |
| `update_buyback_config` | admin | Changes the protocol-fee split only (not the total) |
| `toggle_boost`, `set_boost_authority`; `boost_buy_and_burn` | admin; boost authority `HTVZ…S5r` | **Pump-controlled buys** into pools (§1.4). This is predictable post-graduation upward pressure |
| `admin_cto_pool(coin_creator, is_holder_reward, creator_fee_bps)` | `payer` + pump `pool-authority` PDA signer (i.e. via the pump program) | Changes a pool's creator fee rate (≤ `max_configurable_creator_fee_bps` = 300 today) and recipient |
| `update_creator_fee_config(creator_fee_configurable, max_configurable_creator_fee_bps)` | admin | Raises the cap on per-pool creator fees |
| `toggle_mayhem_mode`, `toggle_cashback_enabled`, `set_reserved_fee_recipients` | admin | Changes fee-recipient requirements and pool behaviour |
| **Program upgrade** | `7gZufwwAo17y5kg8FMyJy2phgpvv9RSdzWtdXiWHjFr8` (all three programs) | Arbitrary behaviour change. Breaking account-list or fee changes shipped on 2025-09-01 (fee tiers) [PPD `FEE_PROGRAM_README.md`], 2026-04-28 (buyback recipients) [PPD `BREAKING_FEE_RECIPIENT.md`], 2026-07-15 (VQR, "switch now") [PPD README], and 2026-09-12 (holder rewards IDL refresh; pAMM redeploy) [PPD; RPC deploy slot] |

**No PumpSwap instruction in the IDL lets the admin withdraw pool reserves** directly. Upgrades could change that, and the code is closed-source, so this is **UNVERIFIED** beyond the IDL.

---

## 8. Design implications for short.win

1. **Venue model.** `Market.venue` goes to `PumpCurve` → `Migrating` → `PumpSwapCanonical(pool)`.
   - The pool is **derived on-chain** from the mint, never passed in unchecked (§1.3).
   - `Migrating` = `bonding_curve.complete && canonical pool account does not exist`.
   - Graduation typically completes in about 1 s, but can fail and retry.
2. **On-chain buyback quote.** Inputs:
   - `Pool`: VQR at offset 245 (i128), `coin_creator` at 211, `creator` at 11, `creator_fee_bps` at 261, `is_mayhem_mode` at 243, `quote_mint` at 75;
   - both vault amounts;
   - the base mint supply;
   - `FeeConfig`, plus `GlobalConfig.creator_fee_configurable`.

   Implement §2.4 with checked u128 math and ceilings. Either reimplement `calculate_fee_tier` from `FeeConfig`, or CPI `pfee::get_fees_with_quote_mint` (6k CU) to stay in lock-step with pump, then apply the per-pool creator override. Read short legacy `Pool` accounts as zero-padded.
3. **Use effective reserves for price and cost; real reserves for liquidity and sale caps.**
   - The EMA and CR use `Qe/B`.
   - Tier liquidity uses real quote reserves only.
   - Open-short sale proceeds `S` are capped by real SOL (`InsufficientRealQuoteReserves`).
4. **Graduation shock.** Every non-mayhem graduation is followed by a pump-controlled buy-and-burn of 20.69% of the migrated quote (about 17.6 SOL for SOL coins) within minutes, which alone is ≈ +46% on a fresh pool (§1.4). The economics agent should:
   - model Tier D positions that span graduation with this deterministic jump;
   - consider extending the post-migration liquidation grace, or gating on `boost_vault_remaining == 0` (the boost vault ATA is readable on-chain; PDA `["boost_vault", pool]`);
   - consider blocking new opens on a pool until its boost vault is empty.
   - *Review correction (2026-09-27):* the boost ATA is closed when the boost ends, and anyone can re-create it and donate 1 atom of WSOL (§1.4 item 5). An open gate of the form "ATA missing or `amount == 0`" can therefore be griefed into blocking opens permanently.
     - Gate instead on `amount > dust_threshold`, **and** cap the gate with a time window after graduation, e.g. `now < graduated_at + boost_window` (the observed boost lasted ~6 min).
     - Both parameters are configurable.
5. **Vault authority design.**
   - Use a **system-owned, zero-data PDA** per position (or per market) as PumpSwap `user`. It owns a WSOL token account and a Token-2022 base account, and keeps a lamport buffer for rent.
   - Pre-create its `user_volume_accumulator` with `init_user_volume_accumulator`, using the keeper or borrower as payer.
   - Prepend `CreateIdempotent` for `coin_creator_vault_ata`, the protocol-fee ATA and the buyback ATA when missing, and `extend_account` for pools < 300 bytes.
   - Keep graduated vault SOL as WSOL; wrap curve-era lamports at the first PumpSwap interaction.
   - *Review note (2026-09-27):* anyone can donate lamports to a system-owned PDA and create ATAs for it (`pumpfun-bonding-curve.md` §9.2). A per-market `user` PDA can therefore start a leg with unexpected lamports or a pre-existing WSOL ATA. Normalise that state first (sweep, close) and measure deltas from the post-sweep baseline.
6. **Account plumbing.**
   - Pick `protocol_fee_recipient` and `buyback_fee_recipient` from on-chain `GlobalConfig` (reserved recipients for mayhem pools).
   - Follow the remaining-account order exactly (§2.2).
   - Ship a short.win ALT with all static PumpSwap accounts.
   - Budget ~150–250k CU for open/close (**UNVERIFIED** until a fork test).
7. **Pool age.** Add `Market.graduated_at` with a permissionless `record_graduation` crank. For tokens listed post-graduation, default `graduated_at` to market creation (conservative). The attestor option (§4c) needs a decision-log entry if wanted.
8. **USD thresholds.** Use the hybrid Pyth design (§5C) with sponsored shard-0 SOL/USD, cached in `GlobalConfig` by `crank_price`, with staleness/confidence gates. Oracle failure never touches close/liquidate. No Switchboard.
9. **Coverage.**
   - MVP: SOL-quoted canonical PumpSwap pools.
   - v1.1: Raydium AMM v4 and CPMM reference pools, with the "strictly deeper" switch rule.
   - Defer CLMM/DLMM/Whirlpool/DAMM v2 quoting.
   - Mayhem-mode, USDC-paired and exotic-quote coins: exclude or special-case (owner decision).
   - *Superseded (SPEC v0.3):* Raydium AMM v4 and CPMM are MVP venues, not v1.1 (§6.3 note).
10. **Jupiter vs direct.** Keep Jupiter per spec, bounded by the on-chain reference quote. Add a direct PumpSwap adapter as the liveness fallback, and possibly the default for canonical pools. Jupiter V2 `/build` needs an API key and excludes RFQ. ExactOut availability through PumpSwap is unverified.
    - *Superseded (SPEC v0.3 §2.1, §14 #16):* the MVP trades canonical pools directly, and Jupiter is v1.1 only.
    - Correction: a keyless `GET /swap/v2/build` works at 0.5 RPS (`jupiter-cpi.md` §2.1), so an API key is not strictly required.
11. **Venue-change risk (R5).**
    - Version the PumpSwap adapter by `(program deploy slot, IDL hash)`.
    - Auto-pause *new opens* when the PumpSwap or `pfee` programdata changes, or when `GlobalConfig.disable_flags != 0`.
    - Closes and liquidations keep running.
    - A global `disable_flags` buy/sell halt is exactly the S3 path, so starting the S3 timer on `DisabledBuy` is correct.
12. **Fees in economics.** Round trips on fresh graduates cost ≈ 2 × 1.25% in venue fees plus short.win's 0.25% open fee. Established pools (mcap ≥ 98k SOL) cost 2 × 0.30%. Caps derived "from live pool reserves" should use `Qe` and `B` from §2.4 (≤ 5% impact ⇒ `b ≤ 2.41%·B`).

---

## 9. UNVERIFIED items and open questions

**UNVERIFIED:**
- When boost/VQR was first activated on-chain. The interface dates from SDK 1.19.0 (2026-07-15).
- What changes a pool's `virtual_quote_reserves` after its boost has ended (2 of 34 pools older than one day drifted upward, §1.4). The boost size itself is resolved: 20.69% of the migrated quote (§1.4).
- The purpose of the `pool-v2` marker account.
- Whether PumpSwap can create `user_volume_accumulator` or fee ATAs when `user` is a data-carrying PDA. The expectation is that it cannot, hence §8.5.
- Production behaviour of PumpSwap **`buy`** with a third-party PDA `user` (System debit from `user`, venue-created UVA and ATAs): not observed on mainnet, needs a fork test. *(`sell` with a program-owned PDA `user` is observed on mainnet, §3.1.)*
- Whether pAMM creates missing ATAs by CPI into the ATA program, which costs one extra stack level (§3.1 "CPI depth"), and whether `sell` creates missing fee ATAs at `user`'s expense.
- Jupiter ExactOut support for PumpSwap routes.
- The Switchboard shutdown: secondary sources only.
- The exact PumpSwap launch date (~March 2025).
- The per-program split of DefiLlama "Raydium AMM" volume.
- Whether prop-AMM volume is majors-only.
- CU estimates for short.win's full open/close and for CLMM quoting.
- Whether the upgrade authority `7gZuf…` is a multisig.

**Open questions for the owner and other agents:**
- ~~Should graduated-token execution be *direct PumpSwap by default, Jupiter optional*? This extends Q16.~~ Decided by the owner (SPEC v0.3 §14 #16): direct canonical pools (PumpSwap; Raydium AMM v4/CPMM) in the MVP, Jupiter in v1.1.
- Should mayhem-mode, USDC-paired and custom-quote pump coins be listable?
- Should positions opened on the curve be subject to a special regime during the ~6-minute boost window?
- Is an age-attestor role acceptable, or is the conservative "age from listing" rule enough?

---

## Corrections

*Added 2026-09-27 by the architecture pass (`docs/design/ARCHITECTURE.md` §2.5, §13.2).*

1. **§8.1: the `Migrating` test misclassifies Raydium-graduated pump coins.**
   - "`Migrating` = `bonding_curve.complete && canonical pool account does not exist`" also matches pump coins that graduated to Raydium before PumpSwap existed.
   - Example: Fartcoin `9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump`. Its curve `TBHe5tJnuT4CQbHorJ1uVdfUoaYGPKgfCpiv2jgesVN` has `complete = 1` and `virtual_token_reserves = virtual_quote_reserves = real_* = 0`. Its canonical PumpSwap pool `EVtHHoEb2V35V1fpzUZcaASgFn6N4yGWoaZ1twNDVmjW` does not exist (mainnet RPC, slot 450,811,173).
   - Under the old test these markets would be "migrating" forever.
   - Correct test: `Migrating ⇔ complete && virtual_token_reserves > 0 && no canonical pool`. A completed curve still holds `vT = 279.9e12` until `migrate` zeroes it (`pumpfun-bonding-curve.md` §2.1). `complete && vT == 0 && no canonical pool` means "migrated elsewhere": no supported venue in the MVP.
2. **§1.1 table: "whether `migrate_v2` is callable by anyone is UNVERIFIED".** The sibling note `pumpfun-bonding-curve.md` §7.4 resolves this. The pump docs state the instruction is permissionless, and 13 traced migrations were signed by 10 different keys, including the coin's own creator (tx `3NyrUcBP…`).

---

## Review corrections (2026-09-27)

These apply the first-pass review findings that target this note. Each was re-checked against primary sources: mainnet RPC, PPD @ `81091419`, and Pyth and Jito docs, all on 2026-09-27.

1. **C1-7: PDA `user` on mainnet (§0 item 4, §3.1, §9).**
   - PumpSwap `sell` with a **program-owned, 752-byte, off-curve PDA** as `user` at stack height 3 is on mainnet: `4dJY8x1e…`, `2Dh3weVY…` (v1 tx) and `P5H4QkXR…` (2026-09-27).
   - The "no third-party PDA user observed" statement is narrowed to the **buy** path: System debit from `user` and venue-created UVA/ATAs. A scan of 245 txs of six integrator programs found PDA users on sells only. The fork test stays.
2. **C1-12: Pyth SOL/USD cadence (§0 item 6, §5).**
   - Replaced "updated every ~1–4 s": the earlier figure came from signature counts, which include readers.
   - Pyth's docs give a 55 s heartbeat / 0.5% deviation for sponsored Solana feeds. Polling measured `publish_time` steps of 52–54 s and staleness up to ~45 s against block time.
   - Recommend `oracle_max_age_s` of 90–120 s (60 s leaves little margin) or a Hermes-posted update when stale, plus keeper retries.
3. **C1-13: boost size (§0 item 2, §1.4, §8.4, §9).** *Confirmed.*
   - `VQR = floor(quote_amount_in × 2069/10000)` exactly, for all 130 non-mayhem migrations checked (SOL, USDC and 6 custom quote mints); 0 of 14 mayhem migrations were boosted.
   - **Added:** live VQR can drift upward later (2 older pools); the cause is **UNVERIFIED**. Read VQR live.
4. **Research part of C1-2: boost-vault lifecycle (§1.4 item 5, §8.4).**
   - The boost-vault PDA and its WSOL ATA are gone after the boost (null at slot 450,831,815 for two pools). Anyone can re-create the ATA, since the ATA program checks only seeds, and donate dust. Only `boost_authority` can spend it.
   - Gate opens on `amount > dust` **and** a post-graduation time window, not on "missing or 0".
5. **Research part of C1-8: CPI depth (§3.1, §9).**
   - Observed PumpSwap CPIs are one level below PumpSwap.
   - If PumpSwap creates missing ATAs through the ATA program (**UNVERIFIED**), a top-level short.win leg reaches height 4, leaving ≥ 1 spare level, not 2.
   - All eight protocol-fee-recipient ATAs already exist for WSOL, USDC and five custom quote mints (`GlobalConfig` recipients, `getMultipleAccounts`). So do the coin-creator-vault ATAs of the five custom-quote canonical pools checked. Venue-side ATA creation is therefore rare in practice.
6. **Research part of C1-9: rent worst case (§3.1).**
   - The cashback UVA WSOL ATA is "initialized if needed" on `buy` [PPD IDL]. The worst case on a cashback-coin buy is 1,346,200 + 3 × 1,488,440 = 5,811,520 lamports.
   - `sell` also takes the System and ATA programs, so it may create missing fee ATAs (**UNVERIFIED**).
7. **Research part of C0-3 / C1-3: trader state (§8.5).** Third parties can fund a system-owned PDA and create ATAs for it. Normalise before measuring.
8. **Owner decisions in SPEC v0.3 (context, not a finding).**
   - Marked §0 item 7, §6.3, §8.9, §8.10 and the §9 Jupiter question as superseded or decided: Raydium AMM v4/CPMM in the MVP, traded directly; Jupiter in v1.1; CLMM-only tokens not shortable.
   - Also corrected §3.6 and §8.10: keyless `/swap/v2/build` works at 0.5 RPS (`jupiter-cpi.md` §2.1).

No review finding against this note was rejected.

---

## Update 2026-09-29 (M5 re-verification)

*Re-verified for milestone M5 against mainnet on 2026-09-29 (slots ≈ 451,665,000–451,673,000, 14:10–14:50 UTC) and against PPD HEAD `cb188ce08b5069196eef1f3e4a0c43b70099793b`. The sections above are not edited; where an item here supersedes a statement above, it says so. The pump-curve side, the Squads authority and the migration census are in `pumpfun-bonding-curve.md`, Update 2026-09-29 (U1, U8). The throwaway scripts and raw data live outside the repo, in `~/.cache/shortwin-work/m5/research/`.*

### G1. Deploys, IDLs, configuration

- **Programs.** No upgrade since the research.
  - PumpSwap's last deploy is still slot 446,462,733 (2026-09-12 15:23:56, `3kTQq7ETwHfaq6mfRAYy2NZmiD9Qm7EJ6moPcoU2Wt18tTX9zUFYZ55daC1okHBueQWXqNx9gZxwYGom3N3SQvTD`).
  - pump-fees is still at slot 446,465,969 (2026-09-12 15:41:00, `647tW2A7fgG6PMvg2VuQJjXMgVeQgKvrEKnELpyMV9vQeU4KKs1LwFbosjLRsdJ3e5uVCr7PcewAwtbjg5doFcv9`).
  - The upgrade authority `7gZuf…` is the vault-0 PDA of Squads v4 multisig `2yMoQqQrtbhq3nQ3wFoQQawWS65qcqUXcwHEYha4rshW`: **3 of 4, time_lock 0**. This resolves the §9 item "whether the upgrade authority `7gZuf…` is a multisig".
- **IDLs.**
  - PPD `idl/pump_amm.json` and `idl/pump_fees.json` are unchanged since `e0687ae9`.
  - The **on-chain** PumpSwap IDL (`5fLnXNNoZcZt9Qku6HARM3un3Ttm2cGsR7gN9Zp1R7h3`) and pump-fees IDL (`6hgWp61YgGzJ9QmvxyFtLnGfA8MYgx93Hby6fdq8gG31`) were last written 2026-05-14.
  - The on-chain PumpSwap IDL lacks the boost instructions, `admin_cto_pool`, `update_creator_fee_config` and 19 errors, and has pre-VQR `Pool`/event layouts. Keep using PPD.
- **Live configuration.** Unchanged from §2.3.
  - `GlobalConfig` (949 B, slot 451,666,038): admin `FFWtrEQ4…`, `disable_flags` 0, `buyback_basis_points` 5000, `boost_authority` `HTVZ…`, `boost_enabled` true, `creator_fee_configurable` true with a maximum of 300 bps. The 8 protocol recipients (with `JCRGumoE…`) and the 8 buyback recipients are as before.
  - `FeeConfig` `5PHirr…` (slot 451,665,972): flat fees 25/5/0; the same 25 `fee_tiers` from 0 → 2/93/30 up to 98,240 SOL → 20/5/5; the same 25 `stable_fee_tiers`; exotic flat fees 20/5/5.

### G2. Negative `virtual_quote_reserves` from "September 30" (PPD `cb188ce0`)

**What the notice says.** PPD `README.md` and the new `docs/NEGATIVE_VIRTUAL_QUOTE_RESERVES.md` (committed 2026-09-29 11:50:38 UTC):

- "Starting **September 30**, `Pool::virtual_quote_reserves` on PumpSwap pools can be **negative**."
- The field stays `i128` and must be decoded as signed from `Pool`, `BuyEvent` and `SellEvent`.
- "`effective_quote_reserves` itself is **never negative**: the program never lets `virtual_quote_reserves` drop further below zero than the vault balance can cover".
- Pump "guarantee[s] that `pool_quote_token_account.amount + virtual_quote_reserves` will never overflow and will never be negative … always fit in a `u64`".
- "The pricing formula is unchanged": buys and sells price on `effective = quote vault + VQR`, and base reserves stay the raw vault amount.

**Not announced.** The notice does not say *what* makes VQR negative, whether it needs a program upgrade, or whether migration and the boost change. All three are **UNVERIFIED**. The PumpSwap program was not upgraded before the notice (G1). If shipping it needs a deploy on or after 09-30, short.win's `VenueGuard` latch will see it.

**State today:**
- None of the 131 sampled PumpSwap trades (G3) had a negative VQR. 44 had a non-zero VQR, all positive.
- On 150 random boosted pools from the pump-note U8 census, live VQR still equals the `InitBoostEvent` value on 149. One pool moved *down* by 2 lamports 29.6 h after migration (`8ewuF2o8ACro7fqWepZ3tKhkbBjndZU4Ldk9scRPw83d`: 17,584,505,652 → 17,584,505,650; migration `3J3MY992mf1JuSKy5ocW7cwHzekzFcLuo7yoZr8iGqLbFaafNEZSnKXFSggwRZy3AMBf1457AhYzWj59Di3pRVNk`).
- §1.4 had only upward drift. VQR therefore moves in both directions today, still for an **UNVERIFIED** reason.

**What this supersedes above:**
- **§1.4 "Sells are capped by real reserves … a token has a pricing floor of `VQR / base_reserves`".** Both hold only while VQR ≥ 0.
  - With VQR < 0, the effective reserve is *below* the real vault.
  - So the real-reserve cap (`InsufficientRealQuoteReserves`: real ≥ `g − lp_fee`) cannot bind, and there is no floor.
  - Real SOL then *overstates* the SOL that prices trades.
- **§5 "For tiering, count real quote reserves only: exclude `virtual_quote_reserves`".** This was written for VQR ≥ 0.
  - With VQR < 0, the executable depth is `quote vault + VQR` < real.
  - So "real only" overstates depth, and `min(real, effective)` is the conservative reading.
- **§8.2–§8.3.** "VQR at offset 245 (i128)" stays; any reader that rejects or saturates a negative VQR must be changed (design notes).

### G3. PumpSwap swap math: reproduced again

**Sample.** Every 6th successful PumpSwap transaction over two rounds: **131 trade instructions** (`buy` 31, `buy_exact_quote_in` 25, `sell` 75), slots 451,670,031–451,670,205 (2026-09-29 14:34:11–14:34:57 UTC). The event's `pool_*_token_reserves` are the pre-trade reserves.

**Result: 131 of 131 match the §2.4 formulas exactly.**
- The sample spans canonical pools at nine fee tiers (2/93/30, and 20/5/x for x from 5 to 95), non-canonical flat-fee pools (25/5/0) and custom-quote pools (20/5/100).
- Examples on WSOL canonical pools with VQR > 0:
  - `buy`: `d7NF8Q6HbwQ2pZabvB98SWbEWCyYRm9CUwAzJeBeoidUvnapw981R4U7k5oPGhQWqeMFAvhgQ5ugd5SRU3hPjUP`, `2uuAo8tgrRvur3ekWRyaDuGZLvAJEAKTf2tP2aqnykf8MwNT8uurSWpqSA72oGvw8GRF8WFx2jFo8PEpd25W8Moi`, `469VGx4JvM6DMBaEkgs8k3wxsQyQ7msfiaLvoCCUmwkXJFfcvBx89oNdFHUDxWoXU1jSeNDnHj3Lwn6cytVeQCiJ`;
  - `sell`: `3XzoTxbW86M1FZdaRUAcuGb642WkvfojMic8tjrZLzmU8BdDKmP8SrTSfo9ujegtSU1LBS2kmRFkP7w4Z9QN8Wmp`, `3e58XqWh6GP1rc3zL2ohFXuAJbuj2bmJjfTERrzDK1HrafpLQaTB8kfBuhDFE64GiFdvzjgGuxHW2k1QJByyGcw7`, `a5ynJN73fcmiQ686ypJUHGymCLmTsUhuWBUeMQuhQK2MVsZT1961Z4ptaV6mzk9hgPoqABJ4piqudm33M7TmkpW`;
  - `buy_exact_quote_in`: `3jDjso2UoEdcZAK3MxPkoUP1X28Tpeq4PvhHn37YwDAU8uuCJHNw7AJEqSgMGnvYE2tH4mnkPHjYifU4Esx8etUi`, `bSVgXpHg6Jae331yUJPEYzuCWcvPYDoQ1xwUzHiwoKiNsTYPhqmPBcWgqcyWcT3hohudUNv5cUH83L9Qij54nTK`, `1n2XSKCQGtTpgjwyQSKHSgiRUsfytwy4qhFvw9jijYasBaKzUEaUZnNwVKggcr9JZ4XoSwatkqNGgLy1bdyayeS`.

**Reproduced with shortwin-math itself.**
- Method: the repo's `quote::sol_leg::buy_exact_out(…, eps1 = false)` / `sell_exact_in` with the real-reserve cap, and `fees::pump_amm` on the live 25 tiers with the event's `base_supply`.
- Quotes: 34 of 34 WSOL `buy`/`sell` identical.
- Tier resolution: 33 of 33 canonical trades identical. The 34th trade is on a non-canonical 25/5/0 pool.

**Event-field note for `buy_exact_quote_in`.** In its `BuyEvent`, `quote_amount_in` is the *spendable* argument, and `user_quote_amount_in` is the net `e` that enters the pool. `base_amount_out` = ⌊B·(e − 1)/(Qe + e − 1)⌋ as §2.4 says.

**Account counts are unchanged.** `buy` and `buy_exact_quote_in` have 25 accounts, or 26 with the `pool-v2` marker; `sell` has 23, or 24. None carried extra trailing accounts, so there is still no evidence either way on U24 for PumpSwap.

### G4. Fresh pools: size, boost, trading

- **The boost rule holds on every fresh graduate.**
  - Census (pump note U8): 2,102 pool-creating migrations from 2026-09-27 18:13 to 09-29 14:27 UTC.
  - All 1,610 non-mayhem pools (1,455 SOL, 94 USDC, 61 custom quote) got VQR = ⌊`quote_amount_in`·2069/10⁴⌋ exactly. None of the 492 mayhem pools was boosted. §1.4's rule is confirmed on about 12× the earlier sample.
- **SIM of completion and migration in one transaction.**
  - `buy_v2(rT)` then `migrate_v2` on the live curve `G5yt7iw9…` created a pool with effective quote = rQ + n(rT) − 15,000,001 exactly, and base = 206.9·10¹².
  - This equals shortwin-math `migrated_pool` (Y_p +1 lamport, by design).
  - `B̂_str`'s post-migration pool is therefore exactly what `migrate_v2` builds today.
- **New pools are 271 bytes, and trade at 271 bytes.**
  - `create_pool` allocates 271 B (rent 2,026,920). Examples: `5QvPCCGK5rEPPtt3MqU2RwGiFji9sQCveKo5Wb5nYxGZSm22sx6ucxoqHAFspuzUJhFUJJ15h3Sh92TvkXVF8yjs` and the SIM above. 271 bytes ends exactly after `is_holder_reward` (@270).
  - The first trade on that pool, `47G9kQKc33RbRviKTiuCArcShJH8dT2rhkBfPFeXjdtHRgMuSXgaSSAuuWr3phqhFz4GJ2L5S1wCGFzqi28utWH1` (`buy_exact_quote_in`, same slot as the migration), ran on 271 bytes without `extend_account`.
  - 14 sampled trades on six 271-byte non-canonical pools did the same.
  - A client prepended `extend_account` 7 slots later (`2BoLeZtrcjtDNMMjKGLe97dbrdzAavWGb6dqsoKPcqzj4MfP658465aC3xVArchvi4Ggm4U5XpHcmvZ9EdJgQhnB`), taking the pool to 301 B (rent 2,179,320). All 150 sampled boosted pools are now 301 B.
  - **Corrects §1.2** ("Pools shorter than 300 bytes need a permissionless `extend_account` before trading"). 271-byte pools trade; the 300-byte threshold is the SDK's `POOL_ACCOUNT_NEW_SIZE`, not a program requirement.
- **Boost lifecycle.**
  - For `3mqeWNwuiCY6uEaMuYujdTQQNwM2qGfvhktXUDJB6oL9`, migrated at slot 451,668,597 (≈ 14:17 UTC), the boost vault's WSOL ATA `FQJWVKog…` no longer existed at slot 451,673,077 (≈ 14:47 UTC).
  - The boost authority `HTVZ…` transacted about every 12 s at that time.
  - Consistent with §1.4 item 5; the exact end time was not measured.
- **`migrate_v2` can be CPI'd.**
  - 117 pool-creating calls ran from six third-party programs at height 2 and peaked at stack height 5. All 204 top-level calls checked peak at 4.
  - This refines §3.1: a wrapped migration works at exactly the maximum depth.

### G5. Still UNVERIFIED after this pass

- The mechanism behind negative VQR from September 30, its effect on `migrate_v2` / `init_boost`, and whether it ships with a PumpSwap upgrade.
- What moves VQR after the boost, now in both directions (§1.4; G2).
- U24 for PumpSwap: no trade in the sample carried extra trailing accounts.
- A PumpSwap `buy` with a third-party PDA `user` (T-PDA-USER, pAMM part), and ATA creation by the venue (T-ATA, T-DEPTH): no new evidence.

### G6. Found by the M5 build on the dumped ELFs (2026-09-29)

*Executing the dumped PumpSwap ELF (deploy slot 446,462,733, G1) in LiteSVM with the pool `migrate_v2` creates (pump note Update U10):*

- **A fresh canonical pool is 271 bytes; PumpSwap `extend_account` takes it to 301 bytes**, not 300 (G4's mainnet observation, reproduced on the ELF). `tests/litesvm/tests/pump_it/straddle.rs`, `straddle_it.rs`.
- **`buy(Q)` on the new pool right after the migration (and `extend_account`) cost exactly short.win's `B̂_str`**: 604,827,436 lamports on the `near` fixture, both after the keeper's read and after a curve buy landed between the read and the bundle — the pool `migrate_v2` builds is the one `quote::migrated_pool` predicts (G4) and does not depend on the curve's path before completion. The tests assert `spent ≤ B̂_str` within 4–8 lamports (`B̂_str`'s rounding is conservative by design); the runs are exact. `pump_it::straddle::b_str_prices_the_executed_bundle`, `straddle_it::a_straddled_position_prices_the_bundle_it_waits_for`.

---

## Update 2026-09-30 (M6 re-verification)

*Re-verified for milestone M6 against mainnet from 2026-09-29 22:14 UTC (slot ≈ 451,773,000) to 2026-09-30 00:07 UTC, against PPD HEAD `cb188ce0` and against the pump crates on crates.io and npm. The sections above are not edited; where an item here supersedes a statement above, it says so. "ELF" means the **dumped, deployed** PumpSwap program (fixture `pump_amm_program_data`, deploy slot 446,462,733, sha256 `c0d71b15…`) executed in LiteSVM 0.16 with the repo's fork loaders; "candidate" means the unreleased ELFs shipped inside `pump-rust-client` 0.2.0 (H2), run the same way in place of the deployed ones. The probes are research-only (no short.win state exists in them, so short.win's invariant hooks have nothing to check); the throwaway crate, scripts and raw outputs live in `~/.cache/shortwin-work/m6/research/` (`pamm-probe/`, `t*.txt`). The pump-curve side is in `pumpfun-bonding-curve.md`, Update 2026-09-30 (K1–K6).*

### H1. Deploys: the "September 30" change has not been deployed (as of 2026-09-30 00:07 UTC)

- **No upgrade.** The ProgramData headers still read pump 449,734,335, PumpSwap 446,462,733 and pump-fees 446,465,969, read at 22:15 UTC and then polled every 2 minutes from 22:21 UTC to 2026-09-30 00:07 UTC (last read at slot 451,798,083). The September upgrade list is the one in `pumpfun-bonding-curve.md` U1 / G1 above (pump 09-09 `5bD2JZEj…`, 09-12 `SgEwKjom…`, 09-15 `5mfXpjh9…`, 09-23 `4WRMFG9Y…`; PumpSwap 09-12 `3kTQq7ET…`; pump-fees 09-12 `647tW2A7…`); nothing has been added to it. The newest signatures on the three ProgramData accounts are the 09-23 upgrade (pump) and read-only MEV transactions of 09-28 (PumpSwap, pump-fees), as in U1.
- **Nothing is queued.** The Squads multisig `2yMoQqQr…` still has `transaction_index` 140 (the 09-23 upgrade) and no transaction since; an upgrade needs a new vault transaction first (3 of 4, no time lock, minutes end to end, U1).
- **VenueGuard baseline: unchanged.** The deploy slots equal `VENUE_GUARD_SLOTS_2026_09_27` (`programs/shortwin/src/constants.rs`) and the `pump_programs` fixtures (ELF sha256 pump `742f3c81…`, PumpSwap `c0d71b15…`, pump-fees `7df54523…`). No new program fixture was dumped. When the upgrade lands, the keeper's latch duty (ARCH §7.1 duty 10) trips; the integrator then dumps the new ProgramData as a **new** group next to `pump_programs` (the old one stays for T-LATCH) and moves the baseline constant deliberately (`dump-fixtures.py` refuses a changed deploy slot in `pump_programs`).
- **Sources.** PPD has no commit after `cb188ce0` (branches `main`, `feat/cashback`, `feat/holder-rewards`, `fix/holder-docs` unchanged; open PRs #37, #47, #49 unrelated). The on-chain Anchor IDLs were last written 2026-09-12 (pump) and 2026-05-14 (PumpSwap, pump-fees), as in G1. npm `@pump-fun/pump-swap-sdk` is still 1.20.0 and `@pump-fun/pump-sdk` 2.0.0. **New: `pump-rust-client` 0.2.0** was published to crates.io at **2026-09-29 16:36:53 UTC** (vcs sha `46eefb38…`), about 5 hours after the PPD notice — H2.
- **Live configuration: byte-identical** to the M5 dump. `GlobalConfig` (`disable_flags` 0), the PumpSwap `FeeConfig`, pump `Global` and pump `FeeConfig` read at slots 451,784,607 / 451,785,107 equal the `pump_state` fixture bytes.

### H2. What the change is: `pump-rust-client` 0.2.0 ships it (pre-release, UNVERIFIED as the deployment)

PumpSwap's documentation says only that VQR "can be negative" (G2). The official Rust client that PPD's `CPI_README.md` recommends for CPI published, on 09-29, IDLs, math and **ELF artifacts** of programs that do not match the deployed ones (artifact sha256: `pump_amm.so` `057f73a9…`, 1,138,168 B, against the deployed 1,041,896 B; `pump.so` `36dfc0a0…`; `pump_fees.so` `ca22c578…`; crate `4c940fb1…`). Its PumpSwap IDL, README and `src/math/amm.rs` describe the mechanism, and the artifacts, run in LiteSVM, behave as described:

- **Fee buckets.** `Pool` gains `protocol_fees: u64` @271 and `creator_fees: u64` @279 (287 bytes). New trade instructions **`buy_v2` (`b817ee61…`), `sell_v2` (`5df6823c…`), `buy_exact_quote_in_v2` (`c2ab1c46…`)** take 17 accounts (pool, user S, `GlobalConfig`, both mints, both user token accounts, both vaults, both token programs, System, the user's UVA, `FeeConfig`, the buyback recipient's quote ATA, event authority, program: no protocol or creator fee accounts, no pump-fees CPI). They pay the buyback slice in the trade and leave the rest of the protocol fee and the creator fee **in the quote vault**, counted in the buckets. Canonical pump pools only (6078 `OnlyPumpPools`); cashback coins (6079) and mayhem pools (6080) must stay on the v1 instructions.
- **Negative VQR is the bucket offset.** The client: "`virtual_quote_reserves` … is the boost sigma minus the un-swept fee buckets". A v2 trade moves its retained fees into the vault and subtracts them from VQR, so the price (`vault + VQR`) excludes them. **A pool that was never boosted (VQR 0) goes negative on its first v2 trade.** Candidate run on the `legacy211` fixture: `buy_v2` of 1 % of the base → VQR −898,484 = `protocol_fees` 898,484 (its coin creator is default); `sell_v2` → −1,349,985. On the `mature` fixture (VQR 18,912,050,248): VQR 18,809,238,116 with buckets 6,854,143 + 95,957,989.
- **Sweeps.** `sweep_protocol_fee` (`0830be07…`) and `sweep_creator_fee` (`20f6bf34…`): 12 accounts, any payer signs (the recipient's ATA is created idempotently at the payer's cost), recipient = a listed protocol fee recipient or the coin-creator vault authority. On the candidate both return VQR exactly to its boost value (mature: back to 18,912,050,248) and take the buckets out of the vault. Per the client README, `admin_cto_pool`, fee-sharing setup and `update_fee_shares` fail `CreatorFeesNotSwept` (6081) until the creator bucket is swept (not run).
- **Sells are capped by `vault − buckets`.** The client: "pay sells out of the vault less the un-swept fee buckets". Candidate: with a patched bucket of half the vault, a **v1** `sell` whose payout lies between `vault − buckets` and `vault` fails 6063 `InsufficientRealQuoteReserves` (`sell.rs:159`); the deployed ELF executes the same sale. `sol_leg::sell_exact_in` with `real_quote_cap = vault − buckets` returns `None` for it.
- **v1 is unchanged for short.win.** On the candidate, v1 `buy` / `sell` with short.win's account lists (§2.2, ARCH §3.12) succeed on every `pamm_state` pool, on the synthetic negative-VQR pool and on pools carrying buckets, and cost / pay exactly `quote::sol_leg` on `effective = vault + VQR` (`fees::pump_amm` on the live tiers; 12 of 12 runs, `tcand.txt`). The client's own `effective_quote_reserve` is the same `i128` sum with an overflow check.
- **Pools grow to 287 bytes.** The candidate's `create_pool` (and `migrate_v2` through it) allocates **287** bytes. A v1 trade grows a shorter pool to 287 and tops its rent up to `minimum_balance(287)` from `user`: **81,280** lamports on a 271-byte pool held at `minimum_balance(271)` (a pool today's `create_pool` made and nobody has extended yet; most other 271-byte canonical pools were grown from 261 bytes and still hold the old-rent 2,707,440, enough for 287 bytes); a 0-lamport user fails with System error 0x1 (`tcand2.txt`). 301-byte pools already hold the fields (bytes 271–287 are zero on all 663,965 pools of 301 B and 507,221 of 300 B today, H8).
- **`disable_flags` is the same on the candidate** (H4): bits 0–4 = create_pool, deposit, withdraw, buy, sell; `buy_v2` / `buy_exact_quote_in_v2` fail 6020 under bit 3 and `sell_v2` 6021 under bit 4; bits 5–7 do nothing. The client's `check_trading_enabled` hard-codes "bit 3 turns buys off, bit 4 sells".
- **New errors** 6078–6083: `OnlyPumpPools`, `CashbackCoinNotSupported`, `MayhemPoolNotSupported`, `CreatorFeesNotSwept`, `FeeTiersEmpty`, `FeeConfigTooShort`.
- **UNVERIFIED:** that these artifacts are what will be deployed, and when; whether the deployed version will differ. The PPD notice and the client agree on the pricing rule, and the client ships the programs its own tests run against.

### H3. Negative VQR on the deployed ELF, and on mainnet

- **Mainnet: none yet.** `getProgramAccounts` over every PumpSwap pool size that holds the field found **no negative VQR** at slots 451,773,435–451,774,105 (22:19–22:26 UTC): 301 B 663,877 pools (53,518 positive), 300 B 507,221 (all 0), 271 B 13,176 (12 positive), 270 B 931 (1), 261 B 10,026 (66). The background monitor repeated the `memcmp` scan (byte 260 = 0xff) every 10 minutes until 2026-09-30 00:07 UTC: 0 each time; the full census again at **2026-09-30 00:05 UTC** (slots 451,797,710–451,797,760): 664,047 / 507,221 / 13,440 / 9,955 / 924 pools of 301 / 300 / 271 / 261 / 270 B, none negative. **No real pool could be dumped; the fixture is synthetic** (H10).
- **The deployed ELF already prices a negative VQR** (`tnegvqr.txt`, the graduated fixture pool with its VQR patched): `buy`, `sell` and `buy_exact_quote_in` price on `vault + VQR` computed as `i128`, the fee tier is chosen on the effective market cap, and every buy and sell equals `quote::sol_leg` exactly at VQR = −vault/4, −vault/2 and −(vault − 10⁶) (so effective 10⁶ lamports), and the buy at −(vault − 1). A sum outside `[0, u64::MAX]` fails **6023 `Overflow`** (`state/pool.rs:353`) on all three instructions. So the negative VQR itself needs no new pricing code in the program; only the mechanism that produces it (H2) is new.
- **Edge cases on the ELF.** At effective 0 a `buy` or `sell` whose pre-fee quote amount is 0 fails 6053 (`BuybackFeeRecipientNotAuthorized`, `global_config.rs:326`) rather than a zero-amount error, and `buy_exact_quote_in` of 0.1 SOL drains the whole base vault; `sol_leg` returns `None` at `y = 0`, so short.win never quotes such a pool as executable.
- **What this supersedes above.** G2's "real SOL overstates depth" is now concrete: after the upgrade the vault holds un-swept fees that are neither price nor payable liquidity. Pump's own definitions (H2) are: pricing `effective = vault + VQR`; payable liquidity `real = vault − protocol_fees − creator_fees`. Since VQR = boost − buckets with boost ≥ 0, `real ≤ min(vault, effective)` always, and `min(vault, effective) − real = min(buckets, boost)`. §1.4's "sells are capped by real reserves" becomes "by `vault − buckets`"; §5's "real quote reserves only" for tiers becomes `vault − buckets` (design notes).
- **The drift is explained: LP deposits and withdrawals scale VQR** (H11). e/acc's pool `4JAnKF…` went from VQR 17,597,859,231 (09-26, §1.4) to 18,910,092,988 (22:40 UTC today) and 18,912,050,248: the step is the `deposit` `47Q8eagsmAamEqK7L5uaLNBehNYG6YMXqpyps89biXj2s7C5JJiuZvxz9dVQxBzLuESPmBmHmWh9EeWFK4nNYx6g` (slot 451,781,325, 22:52:06 UTC, top-level, 466,749,240 LP to `ENNwHxhn…`), whose `DepositEvent` gives base in 3,673,493,414 on 35,491,490,015,003 — ⌊18,910,092,988 × 3,673,493,414 / 35,491,490,015,003⌋ = **1,957,260**, the observed step to the lamport. A v1 buy or sell never changes VQR (every probe above) and `boost_buy_and_burn` keeps it constant (H9); G2's 2-lamport fall on `8ewuF2o8…` fits a withdrawal's rounding (not traced).

### H4. T-DISABLE resolved on the ELF (hard gate)

Run on the dumped ELF with `GlobalConfig.disable_flags` (u8 @56) set to 0, each single bit and 0xff, one fresh VM per value, every instruction built as mainnet sends it (`tdisable.txt`, `tmigrate.txt`):

| Bit | Mask | Instructions that fail | Error (source line) |
|---|---|---|---|
| 0 | `0x01` | `create_pool`, and therefore pump `migrate_v2` (it CPIs `create_pool`) | 6017 `DisabledCreatePool` (`create_pool.rs:193`) |
| 1 | `0x02` | `deposit` | 6018 `DisabledDeposit` (`liquidity/deposit.rs:52`) |
| 2 | `0x04` | `withdraw` | 6019 `DisabledWithdraw` (`liquidity/withdraw.rs:53`) |
| 3 | `0x08` | `buy`, `buy_exact_quote_in`, `boost_buy_and_burn` | 6020 `DisabledBuy` (`swap/buy.rs:296`, `swap/buy.rs:588`, `boost.rs:238`) |
| 4 | `0x10` | `sell` | 6021 `DisabledSell` (`swap/sell.rs:89`) |
| 5–7 | `0xe0` | none of the instructions tested | — |

- **Scope of each bit:** only the instructions listed. Under any value (0xff included) `extend_account`, `init_user_volume_accumulator` and `claim_cashback` succeed; under bit 3 `sell`, `migrate_v2` and its `init_boost` succeed (the boost then sits in its vault because `boost_buy_and_burn` is halted); under bit 4 buys succeed. The same holds on canonical and non-canonical pools.
- **How the admin sets it:** `disable(create_pool, deposit, withdraw, buy, sell)` (5 bools) **overwrites** the byte with those bools as bits 0–4 (`disable(buy)` then `disable(sell)` leaves `0x10`); a signer other than `GlobalConfig.admin` fails 6005 `InvalidAdmin`. The instruction cannot set bits 5–7, so a value with them set can only come from an upgrade or a new instruction.
- **Candidate (H2): identical**, including the v2 trade instructions.
- **Constants for ARCH §5.3 / A24 (C0-10):** `PAMM_BUY_DISABLED_BIT = 1 << 3` (`0x08`); the sell bit is `1 << 4` (`0x10`). A pool is executable for exits iff the buy bit is clear; opens need the sell bit clear too (A24 already refuses opens on any non-zero value). A set create_pool bit halts graduation: `migrate_v2` fails 6017, the market stays `PumpMigrating`, and ARCH §5.3's stuck-migration S3 (70 min) is the path.

### H5. Trades reproduced; account lists; trailing accounts

- **Sample A:** every 6th successful transaction on PumpSwap, 641 transactions, **300 trade instructions** (`sell` 164, `buy_exact_quote_in` 73, `buy` 63; 243 top-level, 57 under CPI), slots 451,774,716–451,774,747 (2026-09-29 22:22:27–22:22:36 UTC). Trades on canonical WSOL pools 155 (one on a cashback coin), canonical other-quote pools 8 (one mayhem), non-canonical pools 137 (flat 25/5/0). **Sample B:** up to 8 trades from each of 30 blocks spread evenly over slots 451,750,000–451,790,000 (20:31–23:31 UTC): **241 trade instructions** (`sell` 141, `buy_exact_quote_in` 63, `buy` 37; 159 top-level, 80 at stack height 2, 2 at height 3); trades on canonical WSOL pools 180 (one on a cashback coin, one by a normal user on a mayhem pool), canonical other-quote 19, non-canonical 42. **Sample C (after midnight):** up to 8 trades from each of 15 blocks spread over slots 451,796,515–451,797,912 (**2026-09-30 00:00:00–00:06:12 UTC**): 120 trade instructions (`sell` 80, `buy_exact_quote_in` 25, `buy` 15; 87 top-level, 32 at height 2, 1 at height 3), 2 of them by the whitelisted agent; no event carried a negative VQR.
- **All 299 + 241 + 118 trades by normal users reproduce exactly** with §2.4 (tier included; for the cashback coin the creator slice is paid as `cashback`, 30 bps, and the event's creator bps is 0). The other 3 trades (1 in A, 2 in C) are the whitelisted mayhem agent `BwWK17…` (= `GlobalConfig.whitelist_pda`) trading mayhem pools fee-free (A's buy at exactly its `max_quote_amount_in`), as it does on the curve (U4).
- **shortwin-math:** `fees::pump_amm` + `sol_leg::buy_exact_out(eps1 = false)` / `sell_exact_in` (cap = the vault) reproduce **99 of 99** (sample A: 42 buys / 57 sells) **127 of 127** (sample B: 29 / 98) and **49 of 49** (sample C: 11 / 38) canonical WSOL `buy` / `sell` trades to the lamport, tier resolution included (`pamm-probe repro`).
- **Tiers seen:** 2/93/30 (fresh graduates), 20/5/x for x from 5 to 95 (the 25 tiers) plus per-pool creator overrides (100, 150, 300 bps), 25/5/0 flat, 2/93/0 + 30 bps cashback.
- **Account lists unchanged.** Every normal trade (299 + 241 + 118) has exactly §2.2's list: 23 (`buy`, `buy_exact_quote_in`) or 21 (`sell`) fixed accounts, then the cashback ATA (buy) or the cashback ATA and the UVA (sell) on cashback coins, the `pool-v2` marker when the coin creator is set, the buyback recipient and its ATA; no extra trailing account. Instruction data: `buy` / `buy_exact_quote_in` 24 or 25 bytes (`track_volume` optional), `sell` 24.
- **Trailing accounts and data are accepted by the ELF** (U24's PumpSwap half, `textras.txt`): `jitodontfront…` or two random accounts appended, and one or two extra data bytes (0 or 1) appended, leave `buy`, `sell` and `buy_exact_quote_in` unchanged to the lamport. There is no PumpSwap equivalent of the curve's `partial_fill` byte. `buy` of exactly the base reserve fails 6025 `DivisionByZero`, of more 6016; a sale paying out more than the vault fails 6063.
- **Non-ATA WSOL accounts are normal on mainnet:** 145 of 300 (A), 86 of 241 (B) and 54 of 120 (C) trades used a `user_quote_token_account` that is not `ATA(user, WSOL)`.
- **CU on the ELF** (top-level, pump-fees CPI included): `buy` 86–90 k, `buy_exact_quote_in` 88–92 k, `sell` 78.6 k; + 18–26 k when the trade creates a fee ATA; `claim_cashback` 22.8 k; `extend_account` 11.5 k; `disable` 11.4 k; `boost_buy_and_burn` 53.8 k; `create_pool` 114–141 k; `migrate_v2` 329.5 k. Candidate v2 trades: ≈ 50 k.

### H6. T-ATA, T-PDA-USER and T-RENT (pAMM) on the ELF

`user` is a fresh system-owned, 0-data account whose signature is forged (LiteSVM signature check off for that transaction), standing in for short.win's `invoke_signed` trader PDA; its token accounts are **non-ATA** accounts created by the Token program (`trent.txt`, `tcashback.txt`):

| Setup | `buy` / `buy_exact_quote_in` | `sell` |
|---|---|---|
| UVA pre-created by another payer, fee ATAs exist, `user` holds **0 lamports** | ok, `user` debited 0; stack height 2, 8 trace entries | ok, same |
| pAMM UVA missing | needs exactly **1,346,200** from `user` (1,346,199 fails, System 0x1) | not needed (non-cashback coin) |
| `coin_creator_vault_ata`, the protocol-fee ATA or the buyback ATA missing | created by pAMM through the ATA program (stack height 3, 13 entries) at **1,488,440** from `user` each; a 0-lamport `user` fails 0x1 | the same: **`sell` creates missing fee ATAs at `user`'s expense** |
| cashback coin, UVA WSOL ATA missing | created at 1,488,440 (height 3); with the UVA also missing `user` paid 2,834,640 | not tested (the IDL documents the ATA's creation on buys only); omitting the accounts fails (H7) |

- **T-ATA: resolved — non-ATA accounts are accepted** for both `user_base_token_account` and `user_quote_token_account`, on every trade instruction (they were used in every probe above; mainnet does it too, H5). The `trader_wsol` design of ARCH §2.4 works as specified; the ATA fallback is not needed.
- **T-PDA-USER (pAMM buy): works on the ELF** with a 0-lamport, 0-data user once the UVA exists. It is still not seen on mainnet (G5).
- **The pAMM UVA** (137 B, 1,346,200 lamports) is created by `init_user_volume_accumulator` with any payer and an unsigned `user`; `close_user_volume_accumulator` without `user`'s signature fails 3010 `AccountNotSigner` (`tuva.txt`), so nobody but the trader can close short.win's UVA.
- **T-RENT (pAMM): with ARCH §3.12's existence requirement (T10, T11, T14, T17, T18 exist) the venue takes no rent from `user` on 271-, 300- and 301-byte pools.** The only remaining venue-side rent is a short pool's growth (H8): 0 on today's short canonical pools (created at the old rent and over-funded), at most 304,800 for a 211-byte pool held at the current minimum, and after the H2 upgrade 81,280 on a 271-byte pool. All are far below `exec_rent_tol`.
- **Stack:** with every account present pAMM's CPIs sit one level below it (pump-fees, Token, its event); a created ATA adds the ATA program one level down and its System / Token calls two levels down, so under short.win the peak is 4 (C1-8's case, now measured).

### H7. Cashback (T-CASHBACK, pAMM part)

- **17,085 canonical SOL cashback pools** (301 B; one more at 271 B) exist (`getProgramAccounts`, `is_cashback_coin` @244 = 1); cashback is deprecated for new coins but these trade.
- **The cashback remaining accounts are mandatory on the ELF** despite the IDL's "optionally pass": a cashback-coin `buy` without the UVA WSOL ATA fails **6061** `InvalidCashbackAccumulatorAta` (`buy.rs:50`), a `sell` without the ATA and the UVA **6060** (`sell.rs:369`); the creator fee does not fall back to the creator (PPD's `PUMP_CASHBACK_README` says it would). On a cashback `sell` the UVA itself is `remaining_accounts[1]`, so a cashback sell needs the trader's pAMM UVA too (ARCH §3.12 lists T14 for buys only).
- **Accrual:** the creator slice is paid as WSOL into `ATA(UVA(user), WSOL)`; the UVA (137 B) counts `cashback_earned` @74 and `total_cashback_claimed` @82 (IDL `UserVolumeAccumulator`). PPD: "unclaimed cashback … = the token balance of the WSOL ATA of the UserVolumeAccumulator for the Pump AMM program".
- **`claim_cashback` (`253a237e…`, 9 accounts: user W, UVA W, WSOL mint, Tokenkeg, `ATA(UVA, WSOL)` W, `user_wsol_token_account` W, System, event authority, program) needs no signature from `user`** (IDL: not a signer; ELF: ok with `user` unsigned and another fee payer). It moves the ATA's **whole WSOL balance** (143 in the probe) to `user_wsol_token_account`, which may be **any WSOL token account owned by `user`** (a non-ATA one works; one owned by someone else fails 2015 `ConstraintTokenOwner`), and sets `total_cashback_claimed = cashback_earned`. With an existing but empty UVA WSOL ATA it **succeeds** (15.7–20.2 k CU, no error), but **without the ATA it fails 3012 `AccountNotInitialized`** (`user_volume_accumulator_wsol_token_account`), and without the UVA 3012 on `user_volume_accumulator` (`tclaim_missing.txt`) — the normal state of a trader that never traded a cashback coin. `disable_flags` 0xff does not affect it. Stack height 2 (one Token transfer), 3 trace entries.
- **Mainnet shape:** Jupiter routes send `[buy_exact_quote_in, claim_cashback, close_user_volume_accumulator]` in one transaction for cashback coins (`5YzcSajC…`, 2026-09-29), leaving no UVA behind; the user signed there, but the ELF does not require it.
- **Consequence for M5-SEC-1's remainder (design notes):** "nothing claimable" on PumpSwap is readable without the venue executing — `ATA(UVA_pAMM(trader), WSOL)` missing or at amount 0 — and a claim is a permissionless top-level `claim_cashback(user = trader, user_wsol = ATA(trader, WSOL))`, which `sweep_trader` already closes and sweeps. An unconditional CPI of `claim_cashback` cannot serve non-cashback markets: it reverts while the ATA does not exist.

### H8. Pool sizes; growth; `extend_account`

- **Census** (`dataSize` filters, 22:20 UTC): 211 B 37,600 · 243 B 74,445 · 244 B 40,644 · 245 B 101,510 · 261 B 10,026 · 270 B 931 · 271 B 13,176 · 300 B 507,221 · 301 B 663,877 (≈ 1.45 M pools); plus 8,567,595 pAMM UVAs (137 B). Canonical SOL pools among the short sizes: 211 B 2, 244 B 1, 245 B 96, 261 B 172, 270 B 4, 271 B 30 (the 244/245/270 ones checked are mayhem pools).
- **Canonical SOL pools by boost state** (index 0, `creator` = the mint's pool authority; read 2026-09-29 23:40–23:45 UTC): 301 B — 43,983 boosted (VQR > 0) and 52,091 never boosted (VQR 0) normal pools, 7,989 + 9,096 cashback pools, 35,244 mayhem pools; 300 B — 34,762 never boosted; 271 B — 12 boosted, 18 mayhem; none negative. Under H2, the **86,853 never-boosted, non-cashback canonical pools turn negative on their first v2 trade**, and the boosted ones once their un-swept fees exceed the boost.
- **The ELF grows a pool shorter than 271 bytes to 271 on its next trade** (`buy`, `sell`, `buy_exact_quote_in`) and tops its rent up to `minimum_balance(271)` from `user`: 304,800 from a 211-byte pool held at `minimum_balance(211)`, 50,800 from 261 bytes; a 0-lamport `user` then fails 0x1 (`tgrowth.txt`). The two mainnet examples (`3wvwyzsX…` 211 B, `D3CDscKF…` 261 B) were created at the old rent (6,960 lamports per byte) and already hold more than `minimum_balance(301)`, so their growth costs 0 (`tshort.txt`). This is the curve's growth to 125 bytes (U10) on the pool side; after H2 the target is 287.
- **`extend_account` takes any pool to 301** and charges `(301 − len) × 5,080` from `user` whatever the pool already holds (457,200 from 211 B, 203,200 from 261 B, even when over-funded).
- **Fresh pools are extended within seconds.** All 195 pool-creating migrations 2026-09-29 20:30–22:47 UTC (slots 451,749,681–451,780,329) allocated 271 bytes, but the pool the fixture dump picked (`5768Z3Vc…`, created by `migrate_v2` at slot 451,784,368, 23:05:42 UTC) was already 301 B 239 slots (65 s) later (G4 saw 7 slots).
- **Bytes 271–287 are zero on every 300- and 301-byte pool** (1,171,186 checked), so a reader can read H2's bucket fields before the upgrade and see 0.

### H9. Boost

- **The rule still holds:** the same 195 migrations: 147 non-mayhem (142 SOL, 1 USDC, 4 custom quote) all got VQR = ⌊`quote_amount_in`·2069/10⁴⌋ exactly (`quote_amount_in` 81.4 M–9.6 T atoms), 48 mayhem pools none; `base_amount_in` 206.9·10¹² in all 195.
- **`boost_buy_and_burn` on the ELF** (the graduated fixture, the boost authority key patched): VQR stays constant; a request above the vault's remainder spends exactly the remainder, and the call that empties the vault **closes the boost vault's WSOL ATA**; any later call fails 3012 `AccountNotInitialized` (`boost_vault`). The whole remaining boost (16.53 SOL) moved that pool's price ×1.4149. Bit 3 halts it (H4).
- **Mainnet:** the fresh fixture pool still held 13.98 SOL in its boost vault 65 s after its migration (slot 451,784,607), of the 17.58 SOL `init_boost` put there.

### H10. Fixtures added (groups `pamm_state`, `pamm_synthetic`)

`scripts/dump-fixtures.py` gained two groups (`--group pamm_state [--pool role=MINT|fresh=latest]`); the existing groups are byte-for-byte unchanged, and `check` covers the new ones (roles, vault owners, single slot, and that every synthetic account is its source with only its patch applied).

- **`pamm_state`**, one `getMultipleAccounts` at **slot 451,784,607 (2026-09-29 23:06:47 UTC)**, 76 entries (18 recorded absent): `GlobalConfig`, the PumpSwap `FeeConfig`, the global volume accumulator, WSOL, the Rent sysvar, the 16 recipient WSOL ATAs, and per role the pool, mint, both vaults, LP mint, coin-creator vault authority and ATA, boost vault and ATA, pool authority and the migrated curve:

  | Role | Mint | Pool | State |
  |---|---|---|---|
  | `fresh` | `6Rs3yJGQ…pump` | `5768Z3Vc…` | 301 B, VQR 17,584,505,288, boost vault 13.98 SOL, quote vault 237.8 SOL |
  | `mature` | `CbcyNo7m…` (e/acc) | `4JAnKFdd…` | 301 B, VQR 18,912,050,248, boost vault closed, quote vault 1,787.5 SOL |
  | `cashback` | `34Bkw9UM…` | `xA3sC2FB…` | 301 B, `is_cashback_coin`, VQR 17,584,505,294 |
  | `legacy211` | `G8ZmGTba…pump` | `3wvwyzsX…` | 211 B, no coin creator, VQR field absent (0) |
  | `legacy261` | `Ch89LLQz…pump` | `D3CDscKF…` | 261 B, VQR 17,584,505,390, never extended |

- **`pamm_synthetic`: `pamm_mature_pool_negative_vqr`** — **SYNTHETIC, not a mainnet account.** The `mature` pool's bytes with `virtual_quote_reserves` (@245, i128) set to −⌊vault/2⌋ = **−893,771,728,130** (vault 1,787,543,456,260, so effective 893,771,728,130); every other byte and the address, owner and lamports are the source's. It is marked `"synthetic": true` with `derived_from` and `patch` in the manifest and re-derived offline by the script. Its fee buckets (@271, @279) stay 0, so it is the deployed program's view of a negative VQR (`real = vault > effective`); under H2's accounting a negative VQR comes with buckets of at least −VQR, which a test can add by writing @271 (the deployed ELF ignores those bytes, the candidate caps sells by them).
- **Validated on the ELF** (`tfixture.txt`): on every pool above and on the synthetic one, a v1 `buy` and `sell` of 0.1 % of the base cost / pay exactly `quote::sol_leg` on `vault + VQR` (12 of 12). The synthetic pool's halved effective reserve moves its market cap tier from creator 35 bps to 60 bps. The short pools grow to 271 bytes on that trade at no cost to the user (they are over-funded).
- **Not in the fixtures:** a live negative-VQR pool (none exists), a pAMM UVA with claimable cashback (mainnet claims and closes them in the same transaction; the probe accrues one in the VM), the candidate ELFs (not mainnet state; `pump-rust-client` 0.2.0 has them).

### H11. Third-party liquidity on canonical pools: deposits and withdrawals work

**Supersedes §1.4 ("canonical boost-pool liquidity cannot be pulled; it only changes through trading") and the statement built on it in ECON §1.2 ("LP drains are impossible on canonical PumpSwap pools").** The migrated LP is burned, but anyone can add liquidity to a canonical pool and take it out again:

- **On the ELF** (`tlp.txt`, `pamm_state` pools): a `deposit` of LP equal to the pool's whole `lp_supply` succeeds on the `fresh` pool (its boost vault still holding 13.98 SOL), the `mature` pool and the `cashback` pool (≈ 33 k CU), doubles both vaults **and VQR** (×2.000000), and a `withdraw` of the depositor's LP in the next transaction restores all three exactly. `BoostPoolLiquidityUnsupported` (6064) was not raised on any of them. On the 211- and 261-byte pools `deposit` fails (`BorshIoError`: it writes the 271-byte struct without growing the account).
- **On mainnet:** 5 of the 127 canonical, non-mayhem WSOL pools traded in samples A and B carry third-party LP (LP mint supply > 0), e/acc's the most: 316,580,483,980 of `lp_supply` 4,509,968,775,391 (**7.02 %**), then 5.04 %, 2.97 %, 0.10 % and one below 0.01 %. Every other pool's LP mint supply is 0.
- **Consequences:** a canonical pool's locked share `λ` = burned LP / `lp_supply` is 1 at migration and falls as third parties deposit; a deposit or withdrawal moves `√(X·Y)` (and VQR with it) without moving the price, and it can be wrapped around another instruction in one transaction, exactly as on Raydium (ARCH §2.6, M4F-1, FITS-2). `disable_flags` bits 1 and 2 (H4) are the only global switch, and they are clear today.

### H12. Still UNVERIFIED after this pass

- When the negative-VQR change deploys, and whether the deployed programs equal `pump-rust-client` 0.2.0's artifacts (H2).
- A PumpSwap `buy` by a third-party PDA `user` on mainnet (T-PDA-USER is verified on the ELF only).
- Whether v2 trades will change the fee tier's market-cap basis (the candidate prices on `vault + VQR` like v1; only v1 was compared with shortwin-math).
- What `BoostPoolLiquidityUnsupported` (6064) guards: no deposit or withdrawal in the probes raised it, a boost vault holding SOL included (H11); and whether `8ewuF2o8…`'s 2-lamport fall was a withdrawal (H3).

---

## Update 2026-10-03 (M10)

*M10 findings that touch this note. The sections above are not edited except where they point here.*

### I1. `BuyEvent` / `SellEvent` carry `virtual_quote_reserves` (§1.4, confirmed)

- The 2026-09 events end with the cashback / buyback fields and then `virtual_quote_reserves`
  as an i128 (`services/indexer/src/ingest/decode/venue.rs`, `pamm_virtual_quote_reserves`).
- On the canonical pool `4JAnKFddd5fFTk5PxDj3PpTWJuTEfSWQEjZJcgKQ9KwW`, 20 recorded mainnet
  transactions (slots 452,463,723 to 452,464,091; `scripts/gapcal/tests/fixtures/pamm.json.gz`):
  the event's VQR (17,600,247,874) is the same after every buy and sell, and the effective
  reserves `vault + VQR` price every sale exactly, the vault alone does not
  (`services/indexer/tests/venue_mainnet.rs`). No trade in that window moved VQR. What else moves
  it (an admin action, a migration) is still **UNVERIFIED**.
- The M8 indexer stored the quote vault alone as the pool's SOL reserve; since M10 (GC-F1) it
  stores `vault + VQR` (INDEXER.md §7.2).

### I2. A third party can create a non-canonical WSOL pool of any coin

`create_pool` with the PPD IDL's argument list (`index`, `base_amount_in`, `quote_amount_in`,
`coin_creator`, `is_mayhem_mode`, then the four option fields, 18 accounts) executes on the dumped
pAMM ELF for a stranger's index-1 pool (`tests/litesvm/src/pamm.rs` `pamm_create_pool_ix` /
`create_third_party_pool`; `tests/litesvm/tests/pamm_it/refusals.rs`, M10 adversarial suite, G-A2:
such a pool in the ReadSet is refused). Only index 0 by pump's pool authority is canonical; short.win's detection
never adopts another index (ARCH §2.5).

### I3. pump, PumpSwap and pump-fees were redeployed on 2026-10-02

`scripts/dump-fixtures.py check --live` at slot 452,907,481 (2026-10-03) reads new deploy slots for
all three programs: PumpSwap 452,654,882, pump 452,654,932 and pump-fees 452,655,002 (block times
2026-10-02 15:47:07Z / 15:47:21Z / 15:47:39Z), against 446,462,733 / 449,734,335 / 446,465,969 in
the fixtures and `VENUE_GUARD_SLOTS_2026_10_02`. What changed was not examined here (whether it is
the "September 30" negative-VQR change of G2 / H2 is **UNVERIFIED**); the repository's pump and
PumpSwap tests still run the 2026-09 ELFs until the programs are dumped as new groups and
re-validated, as M10 did for Raydium CPMM (`raydium-amm.md` §13).
*Resolved by I4 below.*

### I4. The 2026-10-02 redeploys, examined (M10 pump revalidation)

**Method.** All three programs re-dumped from mainnet (`solana program dump`; group
`pump_programs_2026_10`, trimmed ELFs pump `b0b777c6…0c75`, PumpSwap `01557035…3cc3`, pump-fees
`f166c296…62fd`; the same upgrade authority `7gZufw…`) and compared with the 2026-09 dumps, with
`pump-rust-client` 0.2.0's unreleased ELF artifacts (H2) and with the on-chain IDLs: the Anchor
discriminators of every instruction, account and event of the PPD, 0.2.0 and on-chain IDLs searched
in each ELF as `lddw` immediates and raw bytes; `.text` / `.rodata` sizes and bytes; every Anchor
error origin (file, line, code) from the disassembly. **They are the 0.2.0 release:** the same
discriminators, the same `.rodata` byte for byte and the same error sites as its artifacts, with
`.text` 312 / 352 / 1,296 B longer (pump / PumpSwap / pump-fees): source lines added in pump
`buy_v2.rs` and `lib.rs`, PumpSwap `swap/buy.rs`, `swap/sell.rs`, `swap/trade_v2.rs`, pump-fees
`fee_sharing/*` (one more `NotEnoughRemainingAccounts` check; short.win calls none of it). Not the
same build, and there is no public source: what the added code does is **UNVERIFIED**; coverage is
behavioural.

**Instructions, ELF to ELF (the 2026-09 build against the redeploy):** pump adds only `buy_v3`,
`sell_v3`, `buy_exact_quote_in_v3`, `sweep_protocol_fee`, `sweep_creator_fee`; PumpSwap only
`buy_v2`, `sell_v2`, `buy_exact_quote_in_v2`, `sweep_protocol_fee`, `sweep_creator_fee`; pump-fees
none; nothing is removed. `set_quote_control_mint_reserves`, `set_quote_control_reserves_admin` and
`QuoteControl.reserves_admin` are new only against the stale on-chain IDL (2026-09-12): the 2026-09
pump ELF already dispatches both. The redeploy also adds pump errors `CashbackCoinNotSupported`
("v3 trades do not support cashback coins"), `CreatorFeesNotSwept`, `FeeTiersEmpty` and
`FeeConfigTooShort`. The only new events are pump `SweepBondingCurveFeeEvent` and PumpSwap
`SweepPoolFeeEvent` (the indexer names them since M10 program batch 4). Every instruction short.win
CPIs keeps its discriminator, account list, PDA seeds and arguments (`buy_v2` gains an optional
trailing `partial_fill` byte short.win does not send).

**Behaviour, verified on the dumped ELFs** (`pamm_it::buckets`, `pump_it::buckets`, `tests/fixtures/
README.md`):
- v2 PumpSwap trades pay the buyback slice in the trade and keep the rest of the protocol fee and
  the creator fee in the quote vault, counted in `Pool.protocol_fees` @271 and `creator_fees` @279
  (287-B pools) and taken off `virtual_quote_reserves`: **`VQR = boost − buckets`**, so the price
  `vault + VQR` excludes them; a v2 trade costs or pays exactly the v1 quote. The v2 `BuyEvent`
  keeps the v1 layout (`ix_name` "buy_v2") and carries the pre-trade VQR, so the indexer's
  reconstruction stays exact.
- **A v1 `sell` is paid from `vault − buckets`** (6063 `InsufficientRealQuoteReserves` above it; the
  2026-09 build paid up to the vault). short.win's v1 `sell` / `buy` stay exact and leave the
  buckets as they are. short.win read `R = min(vault, vault + VQR)` and the tier subtracted
  `max(VQR, 0)`, overstating payable SOL by `min(buckets, boost)` (PUMP-2026-10-1): **fixed in M10
  program batch 4** (`R = min(vault − buckets, vault + VQR)`, the tier's virtual part `max(VQR +
  buckets, 0)`).
- The permissionless sweeps pay a bucket to its recipient (a listed protocol-fee recipient, the coin
  creator's vault authority) and add it back to the VQR: the price does not move.
- Every trade grows a pool shorter than 287 B to 287 (271 before) and tops its rent up from `user`:
  81,280 lamports more than before on any such pool, 386,080 from 211 B at minimum rent
  (PUMP-2026-10-2: disclosed; ARCH §8.5).

**Mainnet census.** At slots 452,964,412–452,964,638 no fixture pool or curve and none of the 1,873
PumpSwap pools of 287 B carried a bucket, and 40 + 40 recent transactions held no v2 / v3 trade. A
full census at slots 452,994,774–452,995,262 (`getProgramAccounts` by `dataSize` with a
`dataSlice`, public RPC): the bucket mechanism is **live on venues short.win cannot list** — 904
non-SOL-quote curves of 151 B, 627 of 141 B and 265 non-WSOL pools of 301 B hold non-zero buckets —
and **absent on every SOL / WSOL venue**: 0 of 5,996,499 SOL-quoted 151-B curves, 0 SOL-quoted
141-B curves, 0 of 165,688 WSOL 301-B pools, 0 of 48 WSOL 287-B pools. PUMP-2026-10-1 was therefore
latent for listable venues, not hypothetical.

**In the repository:** every curve and PumpSwap test, bench and fuzz profile runs the redeploys;
T-LATCH replays the real loader upgrades from the 2026-09 builds with positions opened on them
(`latch_pump_it` / `latch_pumpswap_it` `::the_2026_10_*`); `VENUE_GUARD_SLOTS_2026_10_03`;
`check --live` clean at 452,966,546 and 453,037,762.

## Update 2026-10-13 (the pump-final re-pin; short.win 1.0.1)

**I4 is superseded** as the description of the deployed builds: PumpSwap was redeployed on
2026-10-08 and pump-fees on 2026-10-12 (pump on 2026-10-08 and 2026-10-13: `pumpfun-bonding-curve.md`'s
update of the same date). The re-pin's gates G0 / G0b ran on the dumped ELFs
(`tests/fixtures/README.md`); `check --live` is clean against `VENUE_GUARD_SLOTS_2026_10_13` (slot
456,579,297 on 2026-10-13, slot 457,523,244 on 2026-10-16).

- **Deploys.** PumpSwap 454,596,406 (trimmed ELF `3ffd5e3b…2ee1`), pump-fees 456,132,171
  (`8ad2ca93…b8b8`; a 2026-10-08 pump-fees build at 454,596,501 preceded it and was never examined:
  it can no longer be fetched); group `pump_programs_2026_10_13`. Jupiter moved to 454,465,850.
- **Interface.** PumpSwap adds `multi_hop_swap` (`2b644913…`; exact-in only, through a pump curve hop;
  its `BuyEvent` / `SellEvent` name the hop's pool, the mints are the hop's accounts — the indexer
  reads them so since pump-final P0), pump-fees adds `withdraw` (`b712469c…`, undocumented); nothing
  is removed; errors appended. short.win's PumpSwap CPIs (`buy`, `sell`, `claim_cashback`,
  `init_user_volume_accumulator`) keep their discriminators, accounts and seeds.
- **pump-fees inside short.win's PumpSwap legs** (T-PFEE-CPI, `pamm_it::v3_era`): one `get_fees` /
  `get_fees_with_quote_mint` per sale and buy on `[PumpSwap's FeeConfig, PumpSwap]`, read-only;
  never `withdraw`, no short.win account. **`withdraw` (R-PFEE, read from the 10-12 ELF):** it moves
  the vault ATA of `referral_vault` (PDA of `["referral-vault", user]`) to `user`'s own ATA; its
  writable accounts are `user` (signer), that PDA, its ATA and an ATA `user` owns — never a
  `FeeConfig`, a pool or a curve. The design's stop rule does not trigger; pinning pump-fees on
  mainnet stays the owner's step (known issues PUMP-2026-10-13-5).
- **Fee buckets are now live on SOL / WSOL venues** (I4's census found none on 2026-10-03): the
  2026-10-13 survey's read-only sample found non-zero buckets on 8 of 10 SOL curves and 15 of 16 WSOL
  pools of 301 B. PUMP-2026-10-1's fix covers them (re-run on the new builds).
- **Graduation after a synthetic completion** (`pumpfun-bonding-curve.md`'s update): `migrate_v2`
  creates the pool `(X₁, Y₁)` = (the live `curve_base_ata`, `rQ + pcQ − fee`), donations included, and
  the boost is **`VQR = ⌊X₁·Y₁/10¹⁵⌋`** — the proportional `⌊migrated × 2069/10⁴⌋` (H9, §1.4) is its
  value when `X₁` is the standard 206.9e12 and nothing was donated; `vault + VQR = Y₁`, so the boost
  moves no price. 98u8RJ's pool `D4PACvyr…` was created at exactly (200,661,149,081,575,
  87,632,834,741), ρ = 1.063 above the curve's final price.
- **short.win 1.0.1** prices a stuck synthetic migration on that pool (ARCH §4.5) and lists a
  graduated cashback coin on PumpSwap as before (only live cashback curves are refused).

