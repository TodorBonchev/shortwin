# Jupiter swaps via CPI from an Anchor program: research for short.win

*Research note, 2026-09-27. Baseline: `docs/SPEC.md` v0.3 (§2.1, §2.2, §4, §5, §9). This note does not change the spec. Where it recommends something that departs from the spec, it says so and lists it as an owner decision (§9).*

Sibling notes: `docs/research/pumpfun-bonding-curve.md` (bonding-curve venue, canonical PumpSwap pool derivation), `docs/research/toolchain-and-layout.md` (Anchor 1.2.0, `declare_program!`), `docs/research/mint-safety-filter.md`.

**Method.**
- Live mainnet reads by JSON-RPC (api.mainnet-beta.solana.com and solana-rpc.publicnode.com) around slots 450.80M–450.81M (2026-09-26/27 UTC).
- Live calls to the Jupiter API (`api.jup.ag`, keyless).
- Primary GitHub sources, cited by commit:
  - `jup-ag/docs@16e9e9d5` (2026-09-21)
  - `anza-xyz/agave@f7716596` (2026-09-26)
  - `solana-foundation/solana-improvement-documents` (main)
  - `pump-fun/pump-public-docs@81091419`
  - Jupiter example repos, cited at their commits.
- My scratch scripts (IDL-driven Borsh decoder, PDA derivation, tx analyser) are in the session scratchpad and are not part of the repo.

Anything I could not check against a primary source is marked **UNVERIFIED**.

---

## 0. TL;DR

1. **Program.** Jupiter Aggregator v6 is `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4`.
   - It is **upgradeable** and **closed-source**. It was last redeployed at slot 449,280,566 (**2026-09-22**).
   - Its on-chain Anchor IDL (spec 0.1.0) now includes **`route_v2`, `shared_accounts_route_v2`, `exact_out_route_v2` and `shared_accounts_exact_out_route_v2`**. The Swap V2 API emits these by default.
   - The Swap enum has **189 AMM variants**, including `PumpSwapBuy/SellV3` and `PumpWrappedBuy/Sell*` (the bonding curve).
2. **Official CPI examples are stale.** We can use their pattern, but not their code.
   - `jupiter-cpi` is **archived** (last commit 2024-01-10).
   - `jupiter-cpi-swap-example`: last code commit 2025-02-09, Anchor 0.30.1. Its bundled IDL has no v2 instructions, and its client targets `quote-api.jup.ag/v6`, which **did not connect** on 2026-09-27 (curl HTTP 000).
   - `sol-swap-cpi`: 2024-11-25, Anchor 0.27.
3. **Getting a route.** Use **`GET https://api.jup.ag/swap/v2/build`** (the "Router" path: Metis on-chain routing only, raw instructions, documented "for CPI").
   - Auth is the `x-api-key` header. Keyless works at 0.5 RPS; paid tiers go up to 150 RPS.
   - I checked live that **`taker` can be a PDA**. The API returns a `route_v2` instruction with the PDA as `user_transfer_authority` (signer). The PDA also appears again as a **writable signer passed down to the AMM** (PumpSwap).
   - Use `payer=<keeper>`, `destinationTokenAccount=<ours>` and `wrapAndUnwrapSol=false`.
   - `/build` is **ExactIn only** and has no `useSharedAccounts` switch.
4. **Hard limits (mainnet, 2026-09-27).**
   - **CPI depth:** max stack height **5**, i.e. 4 nested CPIs. SIMD-0268 (→9) is accepted but **not activated**.
   - **Account locks:** **64** per tx. The 128 feature is not activated; SIMD-0596 (96 for v1) is a draft.
   - **CPI account-infos:** **255** (SIMD-0339, activated 2026-02-27).
   - **Transaction size:** **1232 bytes** for legacy/v0. **4096 bytes for the new v1 format**, activated **2026-09-15**; v1 has **no ALTs** and 64 inline addresses.
   - **Compute:** 1.4M CU per tx.
   - **Observed depth:** most Jupiter routes add 2 stack levels below Jupiter (Jupiter → AMM → Token/self-event). A few, e.g. Jupiter Lend DEX legs, add 3. From a top-level short.win instruction that is height 4 (one level spare) or 5 (none spare).
   - **Observed size:** single-hop PumpSwap routes via Jupiter use 36–44 account metas and about 95–160k CU.
5. **Security.** When our PDA signs the Jupiter CPI, the signature **propagates to Jupiter and to every AMM program in the route**. Those programs can use it for anything the PDA controls: transfer, approve, `SetAuthority` or close any token account it owns, and move its lamports if it is system-owned. The route (accounts and data) is supplied off-chain by the borrower or keeper and can be crafted by hand; the Jupiter program does not check that a route is "best".
   - **Required defenses:** a dedicated, per-position, empty-by-default swap authority. Fund it with exactly `in_amount`. Accept only a whitelisted instruction discriminator. Check the fixed account slots and args. Do balance-diff checks after the CPI. Compute min-out / max-in **on-chain**. Check that the owner, delegate and close-authority are unchanged, then sweep and close in the same instruction.
6. **Pricing.** Our program **cannot compute the executable cost of a Jupiter route on-chain**. Jupiter has no quote instruction; routing is off-chain, and the program only enforces a min-out.
   - ⇒ The spec's "CR from executable cost" must use **on-chain math on a reference pool** (for pump.fun tokens, the canonical PumpSwap pool, derivable by PDA).
   - A keeper or front-end quote is only a routing hint. It is **never** an input to CR, caps or settlement.
7. **Recommendation (MVP).**
   - **Graduated pump.fun tokens: CPI directly into PumpSwap** (`sell` = exact-in; `buy` = **exact base out**, a perfect fit for "buy back exactly Q"), priced from the same pool. Jupiter becomes an optional price-improvement path that must beat the on-chain PumpSwap quote.
   - **Other tokens:** Jupiter CPI, with a designated on-chain reference pool that sets the min-out / max-in bounds.
   - Direct PumpSwap departs from spec §2.1 ("Graduated: via Jupiter"), so it needs **owner confirmation** (§9).
   - *Decided (SPEC v0.3 §0, §2.1, §14 #16, 2026-09-27):* canonical pools are traded directly in the MVP: PumpSwap for pump.fun graduates, **Raydium AMM v4 / CPMM** otherwise. **Jupiter moves to v1.1** as an optional better-price path, bounded by the on-chain pool quote. Tokens only on concentrated-liquidity venues are listable and lendable but not shortable in the MVP. This note's Jupiter material now describes v1.1.

---

## 1. Program IDs, IDL, official examples

### 1.1 Program and IDL (mainnet facts)

| Item | Value | Source |
|---|---|---|
| Program | `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4` (BPF upgradeable loader) | `getAccountInfo` → `programData 4Ec7ZxZS6Sbdg5UGSLHbAnM7GQHp2eFd4KYWRexAipQT` |
| Last deploy | programdata `slot` = **449,280,566**, blockTime 1790053932 = **2026-09-22 05:12 UTC** | decoded programdata header + `getBlockTime` |
| Upgrade authority | `CvQZZ23qYDWF2RUpxYJ8y9K4skmuvYEEjH7fK58jtipQ` (off-curve, system-owned, so a PDA; **UNVERIFIED** which multisig) | programdata header; `getAccountInfo` |
| Anchor IDL account | `C88XWfp26heEmDkmfSzeXP7Fd7GQJ2j9dDTUsyiZbUTa` (`createWithSeed(PDA([],JUP6),"anchor:idl",JUP6)`), IDL authority `9u9iZBWqGsp5hXBxkVZtBTuLSGNAG9gEQLgpuVw39ASg`; zlib JSON, `metadata.spec = "0.1.0"`, name `jupiter` | `getAccountInfo`, decompressed |
| Event authority | `D8cy77BBepLMngZx6ZukaTff5hCt1HrWyKk3Hnd9oitf` (fixed `address` in the IDL) | IDL; the `/build` response |
| Source code | **Not public.** `jup-ag/jupiter-aggregator-program` returns 404. The Oct-2025 Offside Labs audit refers to that private repo at commit `41d454b8…` | `gh api repos/jup-ag/jupiter-aggregator-program` → 404; `jup-ag/docs@16e9e9d5:static/files/audits/swap-v6-offside-october-2025.pdf` |

**Instructions in the on-chain IDL** (discriminators are the first 8 bytes of instruction data):

| Instruction | Discriminator | Args (in order) | Fixed accounts (in order) |
|---|---|---|---|
| `route_v2` | `[187,100,250,204,49,196,175,20]` | `in_amount u64, quoted_out_amount u64, slippage_bps u16, platform_fee_bps u16, positive_slippage_bps u16, route_plan Vec<RoutePlanStepV2>` | 0 `user_transfer_authority` (s), 1 `user_source_token_account` (w), 2 `user_destination_token_account` (w), 3 `source_mint`, 4 `destination_mint`, 5 `source_token_program`, 6 `destination_token_program`, 7 `destination_token_account` (w, optional; the program id means None), 8 `event_authority`, 9 `program`, then the route's remaining accounts |
| `shared_accounts_route_v2` | `[209,152,83,147,124,254,216,233]` | `id u8, in_amount u64, quoted_out_amount u64, slippage_bps u16, platform_fee_bps u16, positive_slippage_bps u16, route_plan` | 0 `program_authority`, 1 `user_transfer_authority` (s), 2 `source_token_account` (w), 3 `program_source_token_account` (w), 4 `program_destination_token_account` (w), 5 `destination_token_account` (w), 6 `source_mint`, 7 `destination_mint`, 8 `source_token_program`, 9 `destination_token_program`, 10 `event_authority`, 11 `program` |
| `exact_out_route_v2` / `shared_accounts_exact_out_route_v2` | `[157,138,184,82,21,244,243,36]` / `[53,96,229,202,216,187,250,24]` | `out_amount, quoted_in_amount, slippage_bps, platform_fee_bps u16, positive_slippage_bps u16, route_plan` | as above |
| v1 (legacy) `route`, `shared_accounts_route`, `exact_out_route`, `*_with_token_ledger` | e.g. `route` `[229,23,203,151,122,227,173,42]` | `route_plan` **first**, then `in_amount, quoted_out_amount, slippage_bps u16, platform_fee_bps u8` | `route` has `platform_fee_account` at slot 6 |
| Admin/helpers | `claim`, `claim_token`, `close_token`, `create_token_account`, `close_wsol_token_account`, `create_token_ledger`, `set_token_ledger` | | |

- `RoutePlanStepV2 = { swap: Swap, bps: u16, input_index: u8, output_index: u8 }`.
- `Swap` is an enum with **189 variants**, one per venue or instruction flavour. Among them: `PumpSwapBuyV3`, `PumpSwapSellV3`, `PumpSwapBuyV3WithCashbackClaim`, `PumpWrappedBuyV6`, `PumpWrappedSellV5`, `RaydiumCP`, `RaydiumClmmV2`, `MeteoraDammV2WithRemainingAccounts`, `WhirlpoolSwapV2`, `HumidiFi*`, `DynamicV2`, and so on.
- The IDL defines 28 errors, including `6001 SlippageToleranceExceeded`, `6021 InvalidOutputTokenAccount`, `6023 InvalidAuthority` and `6027 IncorrectSwapProgramId`.
- There is **no quote/view instruction** (all from the decoded on-chain IDL).

Useful property for us: in all `*_v2` instructions, **`route_plan` is the last argument**, so the numeric args are at fixed offsets and our program can read them without decoding the 189-variant enum. In v1 `route`, `route_plan` comes first. ⇒ Whitelist only the v2 instructions.

The Swap V2 API emits v2 instructions by default: "Swap V2 defaults to `instructionVersion=V2` or the V2 routing instructions in the Jupiter Aggregator Program" (`jup-ag/docs@16e9e9d5:swap/migration/metis-to-build.mdx`). A live `/build` call returned `route_v2` (§2.3).

**Which IDL to vendor.**
- Fetch the on-chain IDL from `C88XWfp2…` at build time. It is snake_case and spec 0.1.0, so it works with `declare_program!`; compiling it under Anchor 1.2.0 is **UNVERIFIED**, see toolchain note §1.4.
- The IDL bundled with `jupiter-cpi-swap-example` has **only v1 instructions** (decoded: `route`, `shared_accounts_route`, `exact_out_route`, …, no `*_v2`).
- Treat Jupiter's IDL as a moving target. The program was upgraded 5 days before this note. Pin the IDL hash in CI and re-diff it on every upgrade.

### 1.2 Official CPI repos: are they maintained?

| Repo | Last commit | State | Notes |
|---|---|---|---|
| [`jup-ag/jupiter-cpi`](https://github.com/jup-ag/jupiter-cpi) | `12bc5f67b9`, 2024-01-10 | **Archived** | Anchor CPI crate generated from the v1 IDL. Jupiter's docs still show `jupiter-cpi = { git = …, rev = "5eb8977" }` with `SharedAccountsRoute` (`swap/v1/build-swap-transaction.mdx`), inside the "UNMAINTAINED" Metis v1 docs. |
| [`jup-ag/jupiter-cpi-swap-example`](https://github.com/jup-ag/jupiter-cpi-swap-example) | `b9bdc57075`, 2025-02-09 (repo "pushed" 2026-04-07, with no new commits on main) | Unmaintained reference | Anchor 0.30.1, `declare_program!(jupiter_aggregator)` with a v1-only IDL; invokes with the vault PDA signing. Client: `jupiter-swap-api-client` branch `2.1`, default base URL `https://quote-api.jup.ag/v6`. That host did **not connect** (curl HTTP 000) on 2026-09-27. |
| [`jup-ag/sol-swap-cpi`](https://github.com/jup-ag/sol-swap-cpi) | `a9c3132893`, 2024-11-25 | Unmaintained | Anchor 0.27. Contains `// TODO: Check the first 8 bytes. Only Jupiter Route CPI allowed.` and forwards `acc.is_signer` from the top level (see §4). |
| [`jup-ag/jupiter-swap-api-client`](https://github.com/jup-ag/jupiter-swap-api-client) (Rust) | `fb95cfad9f`, 2025-12-29 | Minimal maintenance | Only `/quote`, `/swap`, `/swap-instructions` (v1 flow); **no `/build`** (`jupiter-swap-api-client/src/lib.rs`). |
| `jup-ag/sol-swap-flash-fill`, `dca-cpi`, `jupiter-core-example` | 2023 | **Archived** | Flash Fill is the pre-2025 alternative to CPI. |

Jupiter's docs recommend CPI "as of January 2025" because of the *Loosen CPI restriction* feature (`swap/v1/build-swap-transaction.mdx`; `get-started/development-basics.mdx`). That feature (`loosen_cpi_size_restriction`, `GDH5TVdbTPUpRnXaRyQqiKUa7uZAbZ28Q2N9bhbKoMLm`) was activated at slot 312,768,000 (2025-01-09) per its mainnet feature account.

**Conclusion.** No first-party, current, v2-aware CPI example exists. Use the *pattern* (raw `invoke_signed` over `remaining_accounts` with opaque instruction data from the API) and write our own hardened wrapper (§7.2).

### 1.3 CPI into Jupiter from PDAs happens in production today

From decoding the most recent mainnet transactions:

| Tx | Format / size | Caller program | Jupiter ix | Route | `user_transfer_authority` |
|---|---|---|---|---|---|
| `4dJY8x1e6SFWyBW521EZRgpeDvooTzSA8F3Xf8AYLiV1syTJhnC8VFCddsagxNqhrC5uB87gm1RhYL9BZQDkvT5y` | v0, **1017 B**, 42 keys, 5 ALTs | `FqGg2Y1FNxMiGd51Q6UETixQWkF5fB92MysbYogRJb3P` | `route` (height 2) | `PumpSwapSellV3` | `2HVnqFoy7AXGaJj4HF5wPWXRzR4G7G5L4cEQQsBTd8Vb`: off-curve PDA, not a tx signer; **owned by `FqGg2Y1F…`, 752 B data**. Jupiter passes it to PumpSwap `sell` (h3) as `user` |
| `2Dh3weVYseEfmfim7dUzPfgFnx7VQrudvJu6hzX2fgtWppQK2BVb9bTM67j1qpjWGekPzDTSr6oBJQSLa8WaxkXg` | **v1, 1982 B**, 42 keys, 0 ALTs | same | `route` (height 2) | `PumpSwapSellV3` | `8XWbPs7eR7Q6cTyhrVA2tKtyuj3qZa2ssEPJhF8fRvaV`: PDA, owned by `FqGg2Y1F…` (752 B); PumpSwap `sell` `user` at h3 |
| `zsGhkQcrD6a1kmFaQtQ5ypE9fyZcYJ6puxWmnZceVJ7DKpkmJBaGcVMhiYThrh3zLu2fbzSesQKUBqKiq4vpfHB` | v0, 1169 B | `6Vo3245eszAb5wuqEMw8mGdbfRUdKbHhDHP5LcaGuTAB` | `route_v2` (height 2) | `PumpWrappedSellV5` → `RaydiumClmm` | `EMopL9kdFtQ3AYmsJqYVJYr4bNsgZm44LDvhGEEcSe8K`: PDA (0 lamports before and after), `slippage_bps = 5000`; pump `sell_v2` `user` at h3 |

So PDA-signed Jupiter CPI with pump routes works on current mainnet, including inside a >1232-byte v1 transaction.

These are also the mainnet evidence that PumpSwap `sell` and pump `sell_v2` accept a third-party PDA as `user` (re-checked 2026-09-27 for the review; see `graduated-venues.md` §3.1 and `pumpfun-bonding-curve.md` §6.1). All three are **sells**. No buy with a non-whitelisted PDA `user` was found.

The third row passes a 50% slippage to Jupiter. That is a common integrator anti-pattern that only a program-side min-out check makes safe (§4).

---

## 2. Obtaining a route

### 2.1 Current Jupiter API surface (Sept 2026)

The docs (`jup-ag/docs@16e9e9d5:swap/index.mdx`, `openapi-spec/swap/v2/swap.yaml`) describe one "Swap API V2" at `https://api.jup.ag/swap/v2` with two paths:

| Path | Endpoints | Routers | Usable for CPI? |
|---|---|---|---|
| Meta-Aggregator | `GET /order` + `POST /execute` | Metis, JupiterZ (RFQ), Dflow, OKX | **No**. "Transactions routed through JupiterZ cannot be modified … use cases that require CPI … must use the Router path." |
| **Router** | **`GET /build`** (+ optional `POST https://api.jup.ag/tx/v1/submit`) | **Metis only** | **Yes**. "Get raw swap instructions … Add custom instructions, CPI." No Jupiter swap fee. |
| Router, split (hidden page) | `GET /swap/v2/quote` + `POST /swap/v2/swap-instructions` | Metis | Yes. Requires an API key; not served on `lite-api` (`swap/quote-and-swap.mdx`, `hidden: true`). |
| Metis v1 (legacy) | `GET /swap/v1/quote` + `POST /swap/v1/swap-instructions` | Metis | Works today (live 200 on 2026-09-27), but the docs say "**no longer actively maintained** and has been superseded by Swap V2" (`swap/v1/index.mdx`). |

**Auth and rate limits** (`portal/rate-limits.mdx`, `portal/plans.mdx`, `portal/migration.mdx`):
- API key in the `x-api-key` header, from `developers.jup.ag/portal`.
- **Keyless** access on `api.jup.ag` is allowed at **0.5 RPS** (30/min). `lite-api.jup.ag` is being "phased out".
- Plans: Free 1 RPS; Developer $25/mo, 10 RPS; Launch $100/mo, 50 RPS; Pro $500/mo, 150 RPS.
- 60-second sliding window, **per organisation** (not per key). `/build` costs 1 credit. Returns 429 plus `x-ratelimit-*` headers.
- Live check: a keyless `GET /swap/v2/build` returned HTTP 200 with `x-ratelimit-remaining: 4`, `x-ratelimit-current: 1`.

The docs are inconsistent here: `swap/index.mdx` says "All endpoints require an API key", while `portal/plans.mdx` documents keyless access. Keyless worked in practice.

`/build` parameters that matter for a PDA-owned source (`openapi-spec/swap/v2/swap.yaml`, `swap/build/index.mdx`):

| Param | Use for short.win |
|---|---|
| `taker` (required) | **Our per-position swap-authority PDA.** Live-tested: accepted, see §2.3. |
| `payer` | Keeper or borrower wallet. It moves the rent payer for `setupInstructions` (ATA creation) off the PDA. Without it, setup instructions list the PDA as a *top-level* signer, which cannot sign. |
| `wrapAndUnwrapSol` | `false`. We manage wSOL ourselves. When `true`, Jupiter adds wrap/close instructions owned by the taker. |
| `destinationTokenAccount` | Our swap-authority output token account. It appears in `route_v2` slot 7. |
| `nativeDestinationAccount` | Avoid. Output would arrive as native SOL; semantics with program-owned recipients are **UNVERIFIED**. |
| `slippageBps` | Only a hint to Jupiter's own check. Our program enforces its own bound (§4, §5). |
| `maxAccounts` (1–64, default 64) | Cap route size so the whole tx stays at 64 locks or fewer (§3.4). The docs warn that values "below 50" can degrade routing. |
| `dexes` / `excludeDexes` | E.g. `dexes=Pump.fun Amm` to force the canonical pool (live-tested). Labels are case-sensitive. |
| `platformFeeBps` | Must be 0. Our program rejects non-zero fee args (§4). |
| `forJitoBundle` | `true` if we submit via bundles (spec §9 MEV). |
| *(absent)* `swapMode` | `/build` is **ExactIn only** ("`swapMode` Not supported. `/build` is ExactIn only", `swap/migration/metis-to-build.mdx`). |
| *(absent)* `useSharedAccounts` | No switch in v2. Jupiter picks `route_v2` or `shared_accounts_route_v2` itself (v1 had `useSharedAccounts`, `openapi-spec/swap/v1/swap.yaml`). |

Response (`swap/build/index.mdx`): `computeBudgetInstructions` (CU price only), `setupInstructions`, `swapInstruction`, `cleanupInstruction`, `otherInstructions`, `tipInstruction`, `addressesByLookupTableAddress` (ALT → addresses, no extra RPC needed), `blockhashWithMetadata`, `outAmount`, `otherAmountThreshold`, `priceImpactPct` and `routePlan`.

### 2.2 How the keeper or front-end passes the route into our program

This is the pattern of `jupiter-cpi-swap-example@b9bdc57:cpi-swap-client/src/main.rs`, updated for `/build`:

1. **Derive the swap authority** `A = PDA(short_win, ["swap_auth", position])` and its temp token accounts (e.g. `ATA(A, mint, token_2022)` and `ATA(A, WSOL, token)`). The position PDA must be derivable before the tx, e.g. `["position", market, borrower, nonce]`.
2. **Call** `GET /swap/v2/build?inputMint&outputMint&amount&taker=A&payer=<fee payer>&wrapAndUnwrapSol=false&destinationTokenAccount=<A's out account>&maxAccounts=<≈40>&slippageBps=<hint>`.
3. **Drop** Jupiter's `setupInstructions` and `cleanupInstruction`. Our instruction creates, funds, sweeps and closes the temp accounts itself.
4. **Drop** `computeBudgetInstructions` and `tipInstruction`, or keep the price instruction after clamping it (`swap/build/index.mdx`, "Validate the compute unit price").
5. **Build our instruction:**
   - `data = our_discriminator ‖ borsh(args…, jup_data: Vec<u8>)`, where `jup_data = base64decode(swapInstruction.data)`.
   - `accounts = [our fixed accounts…] ++ swapInstruction.accounts` (all `isSigner = false` at the top level; the PDA cannot sign a transaction, and no wallet signer may be forwarded, see §4).
   - The example client does exactly this: `account.is_signer = false` for every remaining account.
6. **Compile the transaction**:
   - **v0**, with Jupiter's ALTs (`addressesByLookupTableAddress`) plus a **short.win protocol ALT** (global config, programs, WSOL mint, per-market accounts). A v0 message can load ALTs for accounts used only inside a CPI. The example client compiles with `address_lookup_table_accounts`, and tx `4dJY8x1e…` above is a v0 Jupiter-CPI tx with 5 ALTs.
   - Jupiter's docs still say "CPI cannot use Address Lookup Tables" (`swap/build/common-instructions.mdx`). That statement is outdated.
   - **Or v1** (4096 B, no ALTs, all addresses inline; §3.1). Only where the signer's wallet supports v1.
7. **Simulate** with CU limit 1.4M, set the limit to about 1.2× the used units (per `swap/build/index.mdx`), sign, and send. Keepers should send privately or as bundles (spec §9, R4).

### 2.3 Live check: `/build` with a PDA taker

The call, keyless on 2026-09-27: `GET /swap/v2/build?inputMint=8vdJA3MYhUhirZZzjZ5i8jXhNkHgrCnirCrqDG23pump&outputMint=So111…112&amount=1000000000&taker=sETPbWCNcRQfDCwtqLm5iFwcVYUJsBeNiocg1dsfqtw&slippageBps=100&wrapAndUnwrapSol=false`. The taker is `PDA(8KQG1…, ["vault"])`, the vault PDA of Jupiter's example program.

The response:
- **HTTP 200.** One step, `Pump.fun Amm` (100%), `ammKey EGfAPuqSzg4zV1eFmNFpRmCX6DbmyPjVxQ3wQ3MqU79M`, `priceImpactPct 0.009`.
  - `EGfAPuq…` is exactly the canonical PumpSwap pool `PDA(pAMM…, ["pool", 0u16, PDA(pump, ["pool-authority", mint]), mint, WSOL])`; I re-derived it.
- `swapInstruction`: program `JUP6…`, **`route_v2`**, **37 account metas**, **39 bytes** of data.
  - Decoded args: `in_amount = 1_000_000_000, quoted_out_amount = 9_144_317, slippage_bps = 100, platform_fee_bps = 0, positive_slippage_bps = 0`.
  - `route_plan = [{PumpSwapSellV3, bps 10000, 0→1}]`.
- Slot 0 is the PDA (signer). Slots 1 and 2 are exactly `ATA(PDA, mint, Token-2022)` and `ATA(PDA, WSOL, Token)`. Slot 7 is the program id (no separate destination).
  - The PDA appears **again at remaining slot 12 (writable) and slot 32 (signer + writable)**: Jupiter passes our signer on to PumpSwap as its `user`.
- `setupInstructions`: one `createAssociatedTokenAccountIdempotent` whose **payer is the PDA** (signer). It cannot be used top-level as is.
- One ALT (`ApGGiwyKUmcUuTZPiTegPphkooRMs5J4Tj9k3Smc3F6Q`, 65 addresses).

A second call with `payer=<wallet>&destinationTokenAccount=<WSOL ATA>&dexes=Pump.fun Amm&maxAccounts=40` moved the setup payer to the wallet and put `destinationTokenAccount` at slot 7.

---

## 3. Hard limits and what fits

### 3.1 Limits table (mainnet, checked 2026-09-27)

| Limit | Value today | Pending changes | Sources |
|---|---|---|---|
| Transaction size | **1232 B** (legacy, v0). **4096 B for v1 transactions** | v1 activated at slot **447,120,000 (2026-09-15, epoch 1035)**. v1 has **no ALTs**, max 64 inline addresses, rejects duplicate addresses, and puts the compute budget in the message header (ComputeBudget instructions are no-ops in v1) | Feature `enable_tx_v1` `txv1aq4pp281K9um3tnPgkfX8UqtFT6wcVW3hNezGLL` mainnet account; [solana.com/upgrades/larger-transaction-sizes](https://solana.com/upgrades/larger-transaction-sizes); SIMD-0385, SIMD-0296; `agave@f7716596:svm/src/account_loader.rs` (`v1::MAX_TRANSACTION_SIZE`); `solana-foundation/solana-dev-skill@bb24c39:skills/solana-dev/references/transactions-v1.md` |
| CPI depth | **max instruction stack depth 5** (top level = 1, so 4 nested CPIs) | SIMD-0268 raises it to 9 (8 nested). Status Accepted; feature `6TkHkRmP7JZy1fdM6fg5uXn76wChQBWGokHBJzrLB3mj` has **no mainnet account (not activated)** at slot 450,806,150. SIMD-0268 says it "should only be activated after SIMD-0219" | `agave@f7716596:program-runtime/src/execution_budget.rs` (`MAX_INSTRUCTION_STACK_DEPTH = 5`, `…_SIMD_0268 = 9`); SIMD-0268; [solana.com/docs/core/cpi](https://solana.com/docs/core/cpi) ("Max instruction stack depth: 5 (9 with SIMD-0268)") |
| Account locks per tx | **64** | `increase_tx_account_lock_limit` (→128, `9LZdXeKGeBV6hRLdxS1rHbHoEUsKqesCC2ZAPTPKJAbK`): **not activated** (the address holds only system lamports). SIMD-0596 (v1 → 96): **Draft**, and it requires removing the 128 feature | `agave@f7716596:runtime/src/bank.rs` `get_transaction_account_lock_limit()` returns 64 unless the feature is on; SIMD-0596 |
| CPI account infos | **255** | SIMD-0339 activated at slot 403,056,000 (2026-02-27). Its motivation explicitly cites wrappers of Jupiter with >64 accounts. It also charges 1 CU per 250 B of account-info (80 B each) and of account metas (34 B each) | Feature `H6iVbVaDZgDphcPbcZwc5LoznMPWQfnJ1AM7L1xzqvt5`; `agave@f7716596:program-runtime/src/cpi.rs` (`MAX_CPI_ACCOUNT_INFOS = 255`); SIMD-0339 |
| Accounts per instruction | 255 | SIMD-0406 activated 2026-07-10 (`6aHuNsUmwSzCEMjrBzBCYaxHAyAcQBjVES92JigHBDuC`) | SIMD-0406; feature account |
| Instruction trace | 64 instructions per tx (top level + CPIs) | SIMD-0582 (draft) makes the check fail earlier | `agave@f7716596:transaction-context/src/lib.rs` L26 (`MAX_INSTRUCTION_TRACE_LENGTH`), enforced in `src/transaction.rs` L493–494. Observed usage (`getTransaction`, 1 + inner): a caller program → Jupiter `route` → PumpSwap `sell` took 11 entries (caller 1 + Jupiter 1 + PumpSwap 1 + 8 below them; `4dJY8x1e…`); `6Vo3245e…` → `route_v2` (curve sell + Raydium CLMM, incl. an ATA create) took 24 (`zsGhkQ…`); pump `migrate_v2` alone takes 51 (`pumpfun-bonding-curve.md` §6.9) |
| Compute | **1.4M CU per tx**, default 200k per instruction | Block limit 100M CU (SIMD-0286, active 2026-07-29) | `execution_budget.rs` (`MAX_COMPUTE_UNIT_LIMIT`); feature `P1BCUMpAC7V2GRBRiJCNUgpMyWZhoqt3LKo712ePqsz` |
| Re-entrancy | A→B→A is forbidden (`ReentrancyNotAllowed`); only direct self-recursion is allowed | none | `agave@f7716596:program-runtime/src/invoke_context.rs` ~L280–298 |
| CPI failure | A failing callee aborts the whole tx; the caller cannot catch it (`process_instruction(...)?` in `cpi_common`) | none | `agave@f7716596:program-runtime/src/cpi.rs` ~L853–856 |

About v1 transactions:
- Wallet support is per wallet (`supportedTransactionVersions.has(1)`).
- `@solana/kit` ≥ 8 supports v1. `@solana/web3.js` 1.x is read-only for v1 (solana.com upgrades page).
- Jupiter's docs still assume 1232 B and v0 (`transaction/submit.mdx`, `swap/advanced/reduce-transaction-size.mdx`). **UNVERIFIED** whether Jupiter's `/tx/v1/submit` accepts v1.
- Our keeper can build v1 itself from the `/build` instructions by inlining the ALT addresses, as long as there are 64 unique addresses or fewer and the tx is 4096 B or less.
- Adoption: 4 of the 149 Jupiter swaps I decoded were already v1.

### 3.2 Empirical shape of Jupiter routes

**Sample:** I decoded **149 Jupiter swap instructions** from the latest successful transactions (slots 450,802,197–450,802,221), using the on-chain IDL. `n_acc` counts account metas passed to Jupiter, including duplicates.

| Metric | Result |
|---|---|
| Instruction mix | `route_v2` 77, `route` 41, `shared_accounts_route_v2` 18, `shared_accounts_route` 12, `route_with_token_ledger` 1 |
| Called via CPI (height 2) | 7 of 149 |
| Max stack height, Jupiter top-level | **3** in 141 cases; **4** in 1 (`3CpKe9GgAkPqXaz97D4Y72vdEYVLE4PRPoF3JQyKDWSHXf7VGai59vyMY2Fsuj7D5hnVroumxfYNpvdLoLg1Aqk9`, `JupiterLendRedeem → AlphaQ`) |
| Max stack height, Jupiter at height 2 | **4** (all 7 cases) |
| Also seen outside the sample | `3MS23yFmtjW8ukTDt655HSHJDtDYvarrkza5JgNTddxvvGvysmHofawqHmGsM6B5Cy53PsFHjRcs7Ww3jxk4HdWJ` (`JupiterLendDexSwap → AlphaQ → Deriverse`) and `2gEUQLkC3hngYN4TxcFoG3UZowCKgiT8JgC6ji8AgqzQy5dkwQkyFnDjeXCR59NsgJLwJfPVpEXkcYTKVJzCDjMo` reach height 4 with Jupiter top-level: Jupiter → Lend DEX → `jupeiUmn81…` → Token |
| Account metas per Jupiter ix | min 20 / median 41 / p90 72 / max 91 |
| Unique accounts per Jupiter ix | 14 / 29 / 46 / 55 |
| Total tx keys | 17 / 32 / 50 / 56 |
| Tx CU consumed | 32.8k / 119k / 246k / 994k |
| Route steps | 1: 76, 2: 37, 3: 28, 4: 6, 5: 2 |
| Pump routes | 61 (41 single-step) |
| Single-step pump routes | 36–44 account metas; **~96k–158k CU** for the whole tx when Jupiter is top-level |

A sample of 24 slots is small; treat these as indicative.

**Depth budget for short.win.**
- **Chain:** short.win at height 1 → Jupiter at 2 → AMM (e.g. PumpSwap) at 3 → Token, System, or the AMM's event self-CPI at 4. Real CPI examples above top out at height 4.
- **Deep legs:** routes with a nested leg (Jupiter Lend DEX) reach 5, exactly `max = 5`.
- **Consequence:** the short.win swap instruction **must be called top-level**. If a keeper wraps it in another program or a multisig, simple routes reach 5, and nested-leg routes hit `CallDepth` (6).
- **Keeper setting:** exclude nested venues when a route will be wrapped (`excludeDexes`; labels via `/program-id-to-label`).
- **Pump routes:** `PumpSwap*` and `PumpWrapped*` (bonding curve) routes showed max height 3 when top-level, so 4 when wrapped.

### 3.3 Transaction size

- **v0 + ALTs.** Real Jupiter-CPI transactions are **1017 B** (`4dJY8x1e…`, 42 keys, 5 ALTs) and 1169 B (`zsGhkQ…`). Each ALT-resolved account costs 1 byte of index plus 34 B per ALT reference. Our own instruction adds its data (about 8 + our args + the Jupiter data, 39–73 B for typical `route_v2`) and 1 B per account index.
- **Fitting under 1232 B.** Use a protocol ALT for all static short.win accounts, plus a per-market ALT if needed. Keep signers at 1 (keeper or borrower).
- **v1.** A 64-address v1 transaction uses 2048 B for addresses and fits in 4096 B (solana-dev-skill note). The byte limit stops mattering, but **the 64-address limit is the same**.

### 3.4 Account locks: which routes fit

- **Our fixed accounts.** Estimate for `liquidate`: keeper, config, market, position, pool token vault, vault authority, position SOL vault, swap authority, 2 temp token accounts, token mint, WSOL mint, Token, Token-2022, System, Jupiter, plus reference-pool accounts for on-chain pricing (for PumpSwap: pool, base and quote vaults, `global_config`, `fee_config` …). That is about 15–22 accounts.
- **Overlap with the Jupiter route** is large: swap authority, temp accounts, mints, token programs, Jupiter, and the pool itself when Jupiter routes through the reference pool.
- **Net new keys** are therefore about 8–14 on top of the route's unique accounts (median 29, p90 46, max 55 in the sample).
- ⇒ **Start with `maxAccounts≈40`.** If `simulateTransaction` fails with too many account locks, the keeper decrements and retries (Jupiter's "requote with lower max accounts" pattern, `swap/v1/requote-with-lower-max-accounts.mdx`).
- **Single-hop pump routes** (36–44 metas, 26–32 unique; the live PDA `/build` route had 26 unique) fit easily. 3–5-hop split routes (70–91 metas) often will not.

**Compute.**
- Jupiter single-hop pump routes: about 100–160k CU. Multi-hop: 200–400k. Worst observed: about 1M.
- Our overhead: account checks, two reloads, reference-pool math, sweep and close, `emit_cpi!`. It should be on the order of tens of thousands of CU (**UNVERIFIED**; measure in LiteSVM or Surfpool).
- Classic Token is now p-token, which lowers Token CU (toolchain note §1.4).
- 1.4M CU is ample for anything that fits the 64-lock limit.

---

## 4. Security model

### 4.1 What a PDA signature buys the callee

- **Runtime rule.** A callee account may be a signer if "it must be either signed in the caller or by the program" (`agave@f7716596:program-runtime/src/invoke_context.rs` `verify_instruction_accounts`, ~L393–402). So once Jupiter receives our PDA as a signer, **Jupiter can forward that signature to any program it invokes**. The Solana docs say the same: "Account privileges (signer, writable) extend from caller to callee" ([solana.com/docs/core/cpi](https://solana.com/docs/core/cpi)).
- **Jupiter does forward it.** In the live `/build` output (§2.3), the PDA is handed to PumpSwap as a writable signer. A route can include any of the 189 venue variants.
- **Jupiter uses it too.** Per the audit excerpt of `programs/jupiter/src/lib.rs` L341–378 (Offside, Oct 2025), Jupiter itself calls `token_transfer(…, user_destination_token_account, …, destination_token_account, user_transfer_authority, …)` to move the output. It also transfers `positive_slippage_fee` from the user's destination account to a `positive_slippage_fee_account` taken from remaining accounts.
- **The code behind the signature can change.** Jupiter is upgradeable (last upgrade 2026-09-22) and closed-source. The venue programs it calls are independently upgradeable. **Pinning `JUP6…` pins an address, not behaviour.**
- **Program-owned data accounts are safe.** Only the owner program may debit lamports or change data, so a short.win program-owned PDA is safe even if passed writable to Jupiter.
- **The exposure is:**
  - (a) SPL/Token-2022 token accounts **whose authority is the signing PDA**: Transfer, Approve (a lingering delegate), **SetAuthority (taking ownership)**, CloseAccount (stealing rent or wSOL lamports);
  - (b) lamports of the PDA if it is a **system-owned** account. The official example uses `vault: SystemAccount` as the signer, which is exactly this risk.

### 4.2 Threats from a crafted route

The route is supplied by the borrower (open, close) or by any permissionless keeper (liquidate). None of its contents can be trusted.

| # | Attack | Mitigation |
|---|---|---|
| T1 | `user_source_token_account` set to **another account owned by the signing authority** (another position's escrow, the pool vault if it shares the authority), with a large `in_amount` | The signing authority owns nothing except this swap's temp accounts. Check slot 1 (or 2 for shared) == our temp source. Fund it with exactly `in_amount`. After the swap it must be 0 |
| T2 | `destination_token_account` (route_v2 slot 7, shared slot 5) = **attacker's account**. Jupiter transfers the output there using our signature | Require slot 7 == program id (None) or == our temp destination; shared slot 5 == our temp destination |
| T3 | `in_amount` arg larger or smaller than intended | Parse the arg; require `== our amount`; verify by balance diff |
| T4 | `platform_fee_bps > 0` or `positive_slippage_bps > 0` with the attacker's fee account in the remaining accounts, skimming output | Require both == 0 |
| T5 | Wrong Jupiter instruction (e.g. `*_with_token_ledger`, `exact_out_*`, `close_token`, `claim_token`) or a non-Jupiter program id | Pin the program id; whitelist the discriminators `route_v2` and `shared_accounts_route_v2` (optionally add `exact_out_route_v2` later) |
| T6 | A route through a keeper-controlled thin pool (any AMM that Jupiter supports). Jupiter enforces only `quoted_out_amount × (1 − slippage_bps)`, and the keeper chooses both | **Program-computed** min-out / max-in (§5); ignore `quoted_out_amount` and `slippage_bps` |
| T7 | An AMM (malicious, compromised, or upgraded, including Jupiter) uses the forwarded signature: Approve a delegate, SetAuthority, CloseAccount, or sweep | Post-CPI: reload the temp accounts; require the token-program owner, `owner == swap_authority`, `delegate == None` and `close_authority == None`, then sweep and close them in the same instruction. The authority never holds lamports and is never system-funded |
| T8 | Top-level signer forwarding. `sol-swap-cpi` copies `acc.is_signer` from the top level, so a borrower's or keeper's wallet signature would be handed to Jupiter and every AMM | Set `is_signer = true` **only** for our swap authority in the CPI metas. Reject remaining accounts that are sensitive short.win accounts (pool vault, vault authority, market, position SOL vault, fee payer) |
| T9 | Re-entry into short.win mid-swap | Blocked by the runtime (`ReentrancyNotAllowed`) |
| T10 | Sandwich or MEV around open, close or liquidation | On-chain bounds (§5), EMA dual-check (spec §5.1), bundle/private submission (spec §9) |
| T11 | Jupiter upgrade changes semantics (new fee, account order) | Fixed-slot checks and balance diffs fail closed. Add a `GlobalConfig` venue switch to **disable the Jupiter adapter** without blocking closes, which keep the direct-venue path (§6). Optionally record the Jupiter programdata `slot` and alert the keeper on change |

### 4.3 Required defenses (checklist for the program)

1. **Pin the program:** `jupiter_program.key == JUP6…` and it is executable. Put the id in `GlobalConfig` venue ids (spec §10), behind the timelock.
2. **Whitelist the instruction** by 8-byte discriminator (`route_v2`, `shared_accounts_route_v2`). Parse the fixed header at fixed offsets (§7.2).
3. **Isolate signer authority per position.** `swap_authority = PDA(["swap_auth", position])` is a data-less PDA that never holds lamports. It is the only key we ever `invoke_signed` for. It owns only two temp token accounts (in and out) that exist for the duration of the instruction. The lender pool vault and the position SOL vault have **different** authorities that are never passed to Jupiter.
4. **Exact input.** Move exactly `amount` into the temp source right before the CPI. Require `jup.in_amount == amount`. After the CPI require `pre − post == amount` and `post == 0`.
5. **Minimum out and maximum in are computed on-chain** from the reference pool (§5). Never use the route's `quoted_out_amount` or `slippage_bps`. Require `platform_fee_bps == 0` and `positive_slippage_bps == 0`.
6. **Destination is ours.** Check the fixed slots (source, destination, mints, destination-token-account). Diff the balance of the temp destination (≥ min-out, or ≥ Q for buybacks). Then sweep to the position SOL vault or pool vault.
7. **No lingering authority.** Post-CPI `reload()` (Anchor 1.x re-checks the owner on reload). Require `owner == swap_authority`, `delegate.is_none()`, `close_authority.is_none()`. Sweep and close both temp accounts in the same instruction. Never use Approve-based flows.
8. **Only one signer forwarded:** CPI metas set `is_signer` only for `swap_authority`.
9. **Deny sensitive accounts** in `remaining_accounts` (O(n) scan against a short list).
10. **Fail closed.** Any mismatch reverts. A Jupiter failure reverts the whole tx (the runtime cannot catch it), so a buyback that "tried Jupiter and failed" cannot be recorded on-chain (see §5.3).

---

## 5. Can the program compute the executable buyback cost of a Jupiter route on-chain?

### 5.1 No

- **Routing is off-chain.** Metis builds the route and the API returns it as opaque `route_plan` data.
- **The program only checks the result.** `route_v2` takes `quoted_out_amount` and `slippage_bps` and fails with `SlippageToleranceExceeded` if the output is below the threshold (IDL args and errors).
- **Nothing to query.** There is no quote or view instruction in the IDL.
- **Not a quote either:** the `DynamicV2` / `CandidateSwap*` types show that Jupiter can choose among candidate pools **at execution time**, but that is execution, not a quote we can call.
- **No try/catch.** A CPI "dry run" is impossible because a failing CPI aborts the tx (§3.1).

So **the executable cost of a Jupiter route is only knowable by executing it**, and even then only for the amount actually swapped.

### 5.2 Options for spec §4 "CR from executable cost"

| Option | On-chain verifiable? | Manipulable? | Use it for |
|---|---|---|---|
| **A. Reference-pool math on-chain.** Canonical PumpSwap pool for pump tokens (`x·y=k` with LP, protocol and creator fees from `global_config` / `fee_config`; `effective_quote_reserves = vault + Pool.virtual_quote_reserves`, per `pump-public-docs@81091419:docs/PUMP_SWAP_README.md`). For other tokens, a designated pool of a whitelisted AMM type whose math we implement | **Yes**, exact for that pool at that instant | Within the tx, by trading the pool (sandwich). Countered by the spec's EMA dual check and caps | **CR, tier liquidity, size caps, liquidation trigger, S1/S2 proofs, and the execution bounds for any venue** |
| B. Keeper- or front-end-supplied quote (Jupiter `/build` `outAmount`) | **No.** An arbitrary number | Trivially | Routing hint only; UI display |
| C. External oracle (Pyth/Switchboard) | Yes | Oracle risk | Mostly unavailable for long-tail memecoins (**UNVERIFIED** in general; not researched here) |
| D. Execution result (balance diff) | Yes, but only after executing | N/A | Settles the *actual* close or liquidation amounts |

**Why A is conservative for lenders.** If the reference pool is one of the venues Jupiter can use, the best aggregated route is at least as good as a direct swap on that pool. So `cost_ref(Q) ≥ best_executable_cost(Q)` approximately. That makes `CR_ref ≤ CR_true`: the protocol never over-estimates collateral. It errs towards earlier liquidation, which is the spec's stated priority (lenders bear bad debt, §5.4).

### 5.3 Recommendation

- **CR (spec §4) and the liquidation trigger (§5.1)** use `cost_ref(Q)` from the reference pool, read in the same instruction, and the reference-pool EMA (spec §9 "EMA price updated on every interaction").
- **Execution bounds for any route** come from the reference pool, not from the keeper:
  - **Open (sell Q):** `min_out = quote_ref_out(Q) × (1 − ε)`, and the borrower's max slippage relative to the EMA (spec §2.1).
  - **Close / liquidate (buy Q):** `max_in = min(cost_ref(Q) × (1 + ε), cost_ema(Q) × (1 + max_slippage_tier))`, and `out ≥ Q`.
  - `ε` covers only rounding and venue-fee differences (suggest 10–50 bps, configurable). The reference quote is taken on-chain at execution time, so there is no quote-staleness to absorb.
  - Effect: **a Jupiter route is accepted only if it does at least as well as the reference pool right now.** An adversarial keeper cannot route liquidations through its own thin pool at a bad price.
- **ExactIn buybacks.** `/build` is ExactIn only, so a buyback via Jupiter swaps `in = max_in` (or a keeper-chosen amount ≤ `max_in`) and requires `out ≥ Q`. Any surplus `out − Q` needs a rule. Proposal: credit it to the pool as lender income, or return it to the borrower as tokens. This is a **new parameter or decision**, see §9.
  - `exact_out_route_v2` exists on-chain, but the maintained API cannot produce ExactOut routes (the v1 Metis API had `swapMode=ExactOut` and is unmaintained).
  - PumpSwap's own `buy(base_amount_out, max_quote_amount_in)` is natively exact-out (§6).
- **SOL settlement S1/S2 (spec §5.3).** "Attempt normal buyback first" cannot be proven on-chain, because a failed attempt reverts. S1/S2 must therefore be decided by `cost_ref(Q)` vs vault SOL and vs max liquidation slippage, both on-chain.
  - Risk: for tokens with a lot of liquidity **outside** the reference pool, `cost_ref` overstates cost, and S1/S2 could fire when Jupiter could still have bought back.
  - Mitigations, proposed and configurable:
    1. At market creation or re-tiering, require the reference pool to be the dominant venue. For pump tokens the canonical pool normally is; **UNVERIFIED** as a general rule. In my Jupiter sample, 41 of 61 pump routes were single-step pump venues.
    2. Add a short settlement delay: S1/S2 become executable only after the condition has been observed for N slots, cranked by keepers. During the delay any keeper can still do a normal Jupiter buyback, which is allowed whenever it beats `cost_ref`.
- **S3 ("no executable route ≥ 10 min")** should also be defined on the reference pool: pool missing or disabled, reserves below a floor, or the pump migration state (see the bonding-curve note §7). It should not depend on "Jupiter returned no route", which is off-chain and unprovable.

---

## 6. Recommendation: which venue path for the MVP

### 6.1 Graduated pump.fun tokens: direct CPI to PumpSwap (primary), Jupiter optional

- **The same pool that prices also executes.** CR "from executable cost" is exact rather than approximate: `cost_ref(Q)` **is** the cost the program will pay.
  - The canonical pool is derived on-chain with no designation step (`["pool", 0u16, pool_authority, mint, WSOL]`; verified in §2.3 and in the bonding-curve note §7.4).
  - Jupiter itself routed our live test 100% through that pool.
- **Exact-Q buyback is native.** PumpSwap `buy(base_amount_out, max_quote_amount_in, track_volume)` and `sell(base_amount_in, min_quote_amount_out)` (`pump-public-docs@81091419:idl/pump_amm.json`). That matches spec §2.1 (sell exactly Q) and §2.2 (buy back exactly Q) with no ExactIn surplus problem.
- **Much smaller trust surface.**
  - Our signer goes to one program (PumpSwap; admin-upgradeable, upgrade authority `7gZufwwAo17y5kg8FMyJy2phgpvv9RSdzWtdXiWHjFr8` per the toolchain note) instead of Jupiter plus up to about 189 venue variants.
  - The swap authority still must be isolated: PumpSwap's `user` is a writable signer.
  - No Jupiter API key, rate limit or API-availability dependency: liquidations must not depend on `api.jup.ag` uptime.
- **Fewer accounts, less CU, one less CPI level.**
  - PumpSwap `sell` has 21 accounts and `buy` 23 (IDL), versus 36–44 metas through Jupiter.
  - Depth: short.win → PumpSwap → Token at height 3, leaving 2 spare levels. *Review correction (2026-09-27):* that holds only when every venue-side account already exists. `buy` can create `protocol_fee_recipient_token_account`, `coin_creator_vault_ata` and (cashback coins) the UVA's WSOL ATA. If it does so via the ATA program (**UNVERIFIED**), the chain is short.win → PumpSwap → ATA → Token/System = height 4, i.e. **≥ 1 level spare, 2 only if clients prepend `CreateIdempotent`** (`graduated-venues.md` §3.1).
  - Direct PumpSwap CU *is* measured on un-wrapped top-level txs in `graduated-venues.md` §3.1 item 4: `buy` 85,114, `buy_exact_quote_in` 83,513, `sell` 74,157. The earlier "none found" remark here was wrong.
- **Trade-off.** Direct PumpSwap misses price improvement when a graduated token has deeper liquidity elsewhere.
  - Mitigation (phase 2 or v1.1): also allow a **Jupiter route as a price-improvement path**, accepted only if it beats the on-chain PumpSwap quote (§5.3).
  - This keeps every safety property: the bounds come from PumpSwap and are computed on-chain.
- **Spec conflict.** Spec §2.1 step 3 and decision #16 say graduated tokens go "via Jupiter". Spec §3 says graduated tokens route "through the post-graduation AMM, currently PumpSwap". Spec §9 says "reserve reads from the canonical AMM pool for on-chain pricing".
  - Direct PumpSwap execution is a departure from the literal §2.1 wording, though consistent with the spirit of §3 and §9.
  - ⇒ **Owner decision required** (§9 Q1). If the owner keeps "via Jupiter", implement §4 and §5 exactly, still price from the canonical pool, and restrict routes with `dexes=Pump.fun Amm` when needed. That makes Jupiter a thin wrapper around the same pool, adding exposure without better prices.
  - *Resolved (SPEC v0.3 §14 #16):* direct PumpSwap in the MVP, Jupiter in v1.1 as a price-improvement path bounded by the pool quote.

### 6.2 Other (non-pump) SPL tokens (spec §1: "any Solana token")

> **Superseded for the MVP (SPEC v0.3 §0, §3, §14 #16, 2026-09-27).**
> - Tokens whose SOL liquidity is on a Raydium AMM v4 or CPMM pool are traded **directly** on that pool, and priced from it.
> - Tokens only on concentrated-liquidity venues (Orca Whirlpools, Meteora DLMM, Raydium CLMM) are listable and lendable but **not shortable** in the MVP.
> - Jupiter CPI below applies from v1.1, as an optional path bounded by the on-chain pool quote.

- **Execution: Jupiter CPI** (`/build` → `route_v2` / `shared_accounts_route_v2`) with all of §4.3.
- **Pricing: a designated on-chain reference pool per market**, stored in `Market` (spec §10 "venue state … + pool").
- **Pool type must be on an allow-list** of AMM types whose swap math short.win implements and fuzzes. For MVP, one or two constant-product types: PumpSwap and possibly Raydium CPMM or Meteora DAMM v2. **UNVERIFIED:** their current account layouts and fee models; they need their own research note. CLMM/DLMM pools are harder to quote on-chain and should be deferred.
- **Designation is permissionless but constrained.**
  - The mints must match (token/WSOL).
  - Tier liquidity (spec §4) is measured on that pool, so designating a thin pool only lowers the tier and caps.
  - Allow anyone to **re-point** a market to a deeper eligible pool (with an EMA reset or blend). Never allow re-pointing to a shallower one.
- **No eligible reference pool ⇒ the market cannot open shorts** (it is listed but paused for opens). This keeps the mint-safety and permissionless-listing model (spec §3) without trusting keeper quotes.

### 6.3 Bonding-curve (un-graduated) tokens

Direct CPI to the Pump.fun bonding curve, as spec §2.1 already says; details are in `pumpfun-bonding-curve.md`. Jupiter also routes the curve (`PumpWrapped*`), but it adds nothing here.

### 6.4 Alternatives considered

- **Flash Fill** (our pre-instruction moves tokens to a borrower- or keeper-controlled account, a top-level Jupiter swap runs, and our post-instruction verifies through instruction introspection).
  - Upside: it avoids delegating our PDA signature.
  - Downsides: it gives custody of Q to an external signer for the length of the tx, and it needs careful introspection.
  - Jupiter now recommends CPI (`swap/v1/build-swap-transaction.mdx`), and the example repo is archived. Not recommended.
- **Jupiter Meta-Aggregator (`/order`, RFQ).** Not composable, so it cannot be used from a program (`swap/index.mdx`).

---

## 7. Rust: the CPI pattern

### 7.1 From primary sources

The three public examples below are summarised, not quoted: their repositories carry no licence.
Each link pins the commit that was read.

**[`jup-ag/jupiter-cpi-swap-example`](https://github.com/jup-ag/jupiter-cpi-swap-example/blob/b9bdc57075/cpi-swap-program/programs/cpi-swap-program/src/lib.rs)
@ `b9bdc57075`, `cpi-swap-program/programs/cpi-swap-program/src/lib.rs`** (Anchor 0.30.1). The
program checks that the Jupiter program account is `JUP6Lkb…`, forwards `remaining_accounts` as
the instruction's accounts, marking only its global `vault` PDA (seeds `[b"vault"]`) as a signer,
and calls `invoke_signed` with the caller's opaque instruction data and the vault's seeds. Its
accounts are the input and output mints and token programs, the `vault` (a `SystemAccount`), the
vault's two token accounts and the Jupiter program.

Gaps against §4.3:
- No discriminator or arg checks.
- No account-slot checks (the named vault token accounts are validated, but nothing ties them to the route's slots).
- No balance diff.
- One global vault PDA that is a **`SystemAccount`**, so its lamports are exposed to the forwarded signature.

**[`jup-ag/sol-swap-cpi`](https://github.com/jup-ag/sol-swap-cpi/blob/a9c3132893/programs/swap-to-sol/src/lib.rs)
@ `a9c3132893`, `programs/swap-to-sol/src/lib.rs`** (Anchor 0.27). `swap_on_jupiter` copies each
remaining account's signer and writable flags into the forwarded instruction and calls
`invoke_signed` with no seeds; a TODO notes that the first 8 bytes are not checked.

This example forwards top-level signer flags (T8), and its own TODO admits the discriminator is not checked (T5).

**Jupiter docs** ([`jup-ag/docs`](https://github.com/jup-ag/docs/blob/16e9e9d5/swap/v1/build-swap-transaction.mdx)
@ `16e9e9d5`, `swap/v1/build-swap-transaction.mdx`). The documented pattern uses the archived
`jupiter-cpi` crate's typed v1 `shared_accounts_route`: it fills a `SharedAccountsRoute` accounts
struct (the token programs, the program authority, the user's transfer authority, the source and
destination token accounts and mints, and the platform fee account), builds the context with
`CpiContext::new_with_signer`, and passes the route plan with the input and quoted output
amounts, the slippage and the platform fee.

Typed CPI like this needs the route plan decoded into the 189-variant `Swap` enum. It also breaks on every Jupiter enum addition, and it uses the Anchor 0.3x `CpiContext::new(AccountInfo, …)` form (Anchor 1.x takes a program id; toolchain note §1.3). **Not recommended.** Pass the opaque bytes and check the fixed header.

### 7.2 Proposed hardened adapter for short.win

**Illustrative. Not compiled.** Targets anchor-lang 1.2.0; the exact 1.x APIs are UNVERIFIED until the toolchain is installed. The discriminators and account slots come from the on-chain IDL `C88XWfp2…`.

```rust
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{instruction::{AccountMeta, Instruction}, program::invoke_signed};
use anchor_spl::token_interface::TokenAccount;

pub const JUPITER_V6: Pubkey = pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");
pub const ROUTE_V2: [u8; 8] = [187, 100, 250, 204, 49, 196, 175, 20];
pub const SHARED_ACCOUNTS_ROUTE_V2: [u8; 8] = [209, 152, 83, 147, 124, 254, 216, 233];

pub enum JupKind { RouteV2, SharedV2 }
pub struct JupHeader {
    pub in_amount: u64,
    pub quoted_out_amount: u64,   // ignored for safety decisions
    pub slippage_bps: u16,        // ignored for safety decisions
    pub platform_fee_bps: u16,
    pub positive_slippage_bps: u16,
}

/// In *_v2 instructions `route_plan` is the LAST arg, so the numeric args are at fixed offsets.
pub fn parse_jup_header(data: &[u8]) -> Result<(JupKind, JupHeader)> {
    require!(data.len() >= 8, SwErr::BadRoute);
    let disc: [u8; 8] = data[..8].try_into().unwrap();
    let (kind, off) = match disc {
        ROUTE_V2 => (JupKind::RouteV2, 8),
        SHARED_ACCOUNTS_ROUTE_V2 => (JupKind::SharedV2, 9), // skip `id: u8`
        _ => return err!(SwErr::BadRoute),
    };
    require!(data.len() >= off + 22 + 4, SwErr::BadRoute); // 22 B of scalars + vec len
    let u64_at = |o: usize| u64::from_le_bytes(data[o..o + 8].try_into().unwrap());
    let u16_at = |o: usize| u16::from_le_bytes(data[o..o + 2].try_into().unwrap());
    Ok((kind, JupHeader {
        in_amount: u64_at(off),
        quoted_out_amount: u64_at(off + 8),
        slippage_bps: u16_at(off + 16),
        platform_fee_bps: u16_at(off + 18),
        positive_slippage_bps: u16_at(off + 20),
    }))
}

/// Fixed account slots from the on-chain IDL (see §1.1).
pub fn check_route_accounts(
    kind: &JupKind, ras: &[AccountInfo], auth: &Pubkey,
    src: &Pubkey, dst: &Pubkey, in_mint: &Pubkey, out_mint: &Pubkey,
) -> Result<()> {
    let k = |i: usize| -> Result<Pubkey> { Ok(*ras.get(i).ok_or(error!(SwErr::BadRoute))?.key) };
    match kind {
        JupKind::RouteV2 => {
            require_keys_eq!(k(0)?, *auth);                 // user_transfer_authority
            require_keys_eq!(k(1)?, *src);                  // user_source_token_account
            require_keys_eq!(k(2)?, *dst);                  // user_destination_token_account
            require_keys_eq!(k(3)?, *in_mint);              // source_mint
            require_keys_eq!(k(4)?, *out_mint);             // destination_mint
            let d = k(7)?;                                  // destination_token_account (optional)
            require!(d == JUPITER_V6 || d == *dst, SwErr::BadRoute);
            require_keys_eq!(k(9)?, JUPITER_V6);            // program
        }
        JupKind::SharedV2 => {
            require_keys_eq!(k(1)?, *auth);
            require_keys_eq!(k(2)?, *src);                  // source_token_account
            require_keys_eq!(k(5)?, *dst);                  // destination_token_account
            require_keys_eq!(k(6)?, *in_mint);
            require_keys_eq!(k(7)?, *out_mint);
            require_keys_eq!(k(11)?, JUPITER_V6);
        }
    }
    Ok(())
}

/// The only place short.win ever signs for anything handed to Jupiter.
pub fn jupiter_cpi<'info>(
    jupiter_program: &AccountInfo<'info>,
    ras: &[AccountInfo<'info>],
    data: Vec<u8>,
    swap_auth: &Pubkey,
    swap_auth_seeds: &[&[u8]],
    denylist: &[Pubkey],                                    // pool vault, vault authorities, market, …
) -> Result<()> {
    require_keys_eq!(*jupiter_program.key, JUPITER_V6);
    require!(jupiter_program.executable, SwErr::BadRoute);
    let metas: Vec<AccountMeta> = ras.iter().map(|a| {
        AccountMeta { pubkey: *a.key, is_signer: a.key == swap_auth, is_writable: a.is_writable }
    }).collect();
    require!(ras.iter().all(|a| !denylist.contains(a.key)), SwErr::BadRoute);
    let mut infos = ras.to_vec();
    infos.push(jupiter_program.clone());
    invoke_signed(&Instruction { program_id: JUPITER_V6, accounts: metas, data }, &infos, &[swap_auth_seeds])?;
    Ok(())
}

/// Called in e.g. `liquidate` after `swap_src` was funded with exactly `max_in` WSOL
/// (the pre-CPI transfer is signed by the *vault* authority, never by swap_auth).
pub fn verify_after_swap(
    swap_src: &mut InterfaceAccount<TokenAccount>, src_pre: u64,
    swap_dst: &mut InterfaceAccount<TokenAccount>, dst_pre: u64,
    hdr: &JupHeader, swap_auth: &Pubkey, min_out_onchain: u64,
) -> Result<u64> {
    swap_src.reload()?;                                     // fails if closed / owner changed
    swap_dst.reload()?;
    require!(hdr.platform_fee_bps == 0 && hdr.positive_slippage_bps == 0, SwErr::BadRoute);
    let spent = src_pre.checked_sub(swap_src.amount).ok_or(SwErr::Math)?;
    require!(spent == hdr.in_amount && swap_src.amount == 0, SwErr::InputMismatch);
    let got = swap_dst.amount.checked_sub(dst_pre).ok_or(SwErr::Math)?;
    require!(got >= min_out_onchain, SwErr::SlippageExceeded);  // min_out from §5, never from the route
    for ta in [&**swap_src, &**swap_dst] {
        require_keys_eq!(ta.owner, *swap_auth);
        require!(ta.delegate.is_none() && ta.close_authority.is_none(), SwErr::AuthorityTampered);
    }
    Ok(got) // caller sweeps `got` to the position/pool vault and closes both temp accounts
}
```

Notes for the implementer:
- `remaining_accounts` already contains our named swap-authority and temp accounts, because Jupiter needs them. Anchor 1.x rejects duplicate *mutable named* accounts by default (toolchain note §1.3). **UNVERIFIED:** whether that check also looks at `remaining_accounts`. I expect it does not; test it. Keep the named temp accounts `mut` and rely on `reload()` after the CPI.
- Pump tokens are **Token-2022** (live route slot 5 = `TokenzQd…`), so the temp source is a Token-2022 account; move funds with `transfer_checked`. The mint-safety filter already excludes hooks and fees (spec §3).
- **Buys via Jupiter with PumpSwap** may need the `user` (our swap authority) to pay rent, e.g. for a `user_volume_accumulator` (PumpSwap `buy` has it writable). **UNVERIFIED**. If needed, route `track_volume` off or pre-create the account. This is another reason to prefer direct PumpSwap CPI, where we control every account.

---

## 8. Concrete design implications for short.win

1. **Venue adapters.**
   - `pump_curve` (direct), `pump_swap` (direct, canonical pool), and `jupiter` (opaque route, hardened).
   - `jupiter` is a *price-improvement* adapter whose bounds always come from an on-chain reference quote.
   - `GlobalConfig` gets per-adapter enable flags and program ids (spec §10), timelocked, plus an emergency *disable* for Jupiter that leaves closes and liquidations running through the direct venue (spec §11 "never pause closes or liquidations").
2. **Accounts (additions to spec §10).**
   - `ShortPosition` derives a data-less `swap_authority = PDA(["swap_auth", position])`.
   - Temp in/out token accounts owned by it are created and closed inside the instruction; rent is paid and refunded by the tx fee payer.
   - The pool vault authority and position SOL-vault authority are separate PDAs and are **never** passed to external programs.
   - `Market` stores the `reference_pool` (type + address; canonical and derived for pump tokens) and the EMA of the reference price.
3. **Instruction interface.**
   - `open_short`, `close_short` and `liquidate` take `venue: u8` and `route_data: Vec<u8>` (empty for direct venues), with `remaining_accounts` = the route accounts.
   - Our args never include a trusted quote. Min-out and max-in are computed in-program.
4. **Pricing (spec §4, §5).**
   - CR, tier liquidity, size caps, the liquidation trigger and S1/S2 all come from **on-chain reference-pool math** (plus the EMA).
   - The keeper and front-end `/build` `outAmount` is display and routing only.
   - S3 is defined on reference-pool state, not on API route availability.
5. **ExactIn vs exact-Q buyback.**
   - The direct PumpSwap `buy` gives exactly Q.
   - For Jupiter buybacks (ExactIn), define the surplus-token policy and the `ε` tolerance: new configurable parameters for the decision log (spec §15 says "any new parameter must be configurable and listed").
6. **Keeper and front-end.**
   - A backend proxy holds the Jupiter API key (at least Developer, 10 RPS, for a keeper that re-quotes; keyless 0.5 RPS is fine for dev only).
   - Request parameters: `taker = swap_authority`, `payer = fee payer`, `wrapAndUnwrapSol=false`, `destinationTokenAccount = temp out`, `maxAccounts ≈ 40` with decrement-on-failure, `forJitoBundle=true` when bundling.
   - Strip Jupiter setup and cleanup instructions.
   - Maintain a protocol ALT and per-market ALTs.
   - Build v0 by default. Build v1 (4096 B) for keeper transactions once our client stack supports it (`@solana/kit` ≥ 8). Build v1 for borrower transactions only when the wallet reports v1 support.
7. **Transaction topology.**
   - The short.win swap instruction must be **top-level** (CPI depth: short.win 1 → Jupiter 2 → AMM 3 → Token 4; nested legs such as Jupiter Lend DEX reach 5 = max). Do not wrap it in keeper programs. Keepers should simulate every route; a `CallDepth` failure means re-quoting with `excludeDexes`.
   - A route of more than about 45 unique accounts will not fit the 64-lock limit next to our accounts, so liquidation for such tokens relies on the direct reference-pool venue.
8. **Testing.**
   - Fork-test against real `JUP6…` and PumpSwap accounts (Surfpool or LiteSVM, see the toolchain note).
   - Include adversarial routes for each of T1–T8: forged destination slot, forged source slot, non-zero fee bps, `route_with_token_ledger`, extra signer forwarded, an AMM that calls Approve / SetAuthority / CloseAccount (use a mock AMM program behind a hand-built `route_v2`), and an over-large `in_amount`.
   - Re-run on every Jupiter programdata-slot change. The IDL hash is pinned in CI.
9. **Monitoring.** The keeper watches the programdata `slot` of Jupiter (`4Ec7ZxZS…`) and PumpSwap. On change, it alerts and optionally auto-disables the Jupiter adapter until the tests re-pass (spec R5 "Adapters versioned").
10. **Fees.** `/build` charges no Jupiter swap fee (`swap/build/index.mdx`). Do not set `platformFeeBps`; the protocol's 0.25% open fee (spec §6.2) is taken in-program.

---

## 9. Open questions (owner or other agents)

- ~~**Q1 (owner).** Graduated pump.fun tokens: approve **direct PumpSwap CPI** as the primary execution venue, with Jupiter only as an optional price-improvement path bounded by the on-chain PumpSwap quote? This departs from spec §2.1 and decision #16 wording ("via Jupiter").~~ **Decided** (SPEC v0.3 §14 #16): yes, and Jupiter moves to v1.1.
- ~~**Q2 (owner).** Non-pump tokens: which reference-pool AMM types are eligible at MVP (proposal: PumpSwap plus one CPMM type)? Should markets without an eligible pool be listed but closed to opens?~~ **Decided** (SPEC v0.3): Raydium AMM v4 and CPMM in the MVP, traded directly. CLMM/DLMM/Whirlpool-only tokens are listed and lendable but closed to opens.
- **Q3 (economic agent).** For Jupiter ExactIn buybacks: where does the surplus `out − Q` go (pool or borrower)? Set the `ε` tolerance. Choose between the S1/S2 settlement delay and the dominant-pool requirement (§5.3).
- **Q4 (protocol agent).** Borrower transaction format: v0 with ALTs everywhere, or v1 where the wallet supports it? Keeper: v1 by default?
- **Q5 (ops).** Jupiter API plan and key custody; behaviour when `api.jup.ag` is down (direct venue fallback).
- **Q6 (security).** Policy on Jupiter program upgrades: alert only, or auto-disable the adapter?

## 10. UNVERIFIED items

- Whether the Jupiter program checks every `Swap` variant's AMM program id. The error `6027 IncorrectSwapProgramId` exists, but the source is closed. Our design does not rely on it.
- Which multisig controls the Jupiter upgrade authority `CvQZZ23q…` (a system-owned PDA).
- Whether `/build` can ever return v1 `route` or `shared_accounts_route` rather than `*_v2`. The docs say V2 by default, and the live test got `route_v2`. Our whitelist rejects v1 either way, and the keeper would re-request.
- `/tx/v1/submit` and Jupiter tooling support for v1 (4096 B) transactions.
- The rent payer for PumpSwap `user_volume_accumulator` when the swap authority is the `user` on a Jupiter-routed buy.
- short.win's own CU overhead, and direct PumpSwap CU (not measured).
- Compile-correctness of the §7.2 snippet under anchor-lang 1.2.0 (the toolchain was not installed; do not run cargo or anchor here).
- Account layouts and fee models of non-pump reference AMMs (Raydium CPMM, Meteora DAMM v2), which need a separate note.
- The statistics in §3.2 come from a 24-slot sample. They are indicative, not a distribution.

## 11. Sources

**Mainnet data** (JSON-RPC, 2026-09-26/27):
- **Jupiter program and IDL:** program `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4`; programdata `4Ec7ZxZS6Sbdg5UGSLHbAnM7GQHp2eFd4KYWRexAipQT`; IDL `C88XWfp26heEmDkmfSzeXP7Fd7GQJ2j9dDTUsyiZbUTa`.
- **Feature accounts:**
  - `txv1aq4pp281K9um3tnPgkfX8UqtFT6wcVW3hNezGLL` (active, slot 447,120,000)
  - `6TkHkRmP7JZy1fdM6fg5uXn76wChQBWGokHBJzrLB3mj` (absent)
  - `9LZdXeKGeBV6hRLdxS1rHbHoEUsKqesCC2ZAPTPKJAbK` (not activated)
  - `H6iVbVaDZgDphcPbcZwc5LoznMPWQfnJ1AM7L1xzqvt5` (slot 403,056,000)
  - `GDH5TVdbTPUpRnXaRyQqiKUa7uZAbZ28Q2N9bhbKoMLm` (slot 312,768,000)
  - `P1BCUMpAC7V2GRBRiJCNUgpMyWZhoqt3LKo712ePqsz` (slot 435,888,000)
  - `6aHuNsUmwSzCEMjrBzBCYaxHAyAcQBjVES92JigHBDuC` (slot 432,000,000)
  - `C37iaPi6VE4CZDueU1vL8y6pGp5i8amAbEsF31xzz723` (absent)
- **Transactions:**
  - `4dJY8x1e6SFWyBW521EZRgpeDvooTzSA8F3Xf8AYLiV1syTJhnC8VFCddsagxNqhrC5uB87gm1RhYL9BZQDkvT5y`
  - `2Dh3weVYseEfmfim7dUzPfgFnx7VQrudvJu6hzX2fgtWppQK2BVb9bTM67j1qpjWGekPzDTSr6oBJQSLa8WaxkXg`
  - `zsGhkQcrD6a1kmFaQtQ5ypE9fyZcYJ6puxWmnZceVJ7DKpkmJBaGcVMhiYThrh3zLu2fbzSesQKUBqKiq4vpfHB`
  - `2ia1QXQAfzuWs5E8hDGFhMWEfgPT54xHohvKw4Zmm8RhxxRVkJ8ZTYqre8qp6ZD8ERSVgrDWRaji7BJ2rY6ioYoe` (single-step PumpSwapBuyV3, 39 metas, 122,457 CU)
  - plus the 149-instruction sample (§3.2).
- **Canonical pool:** `EGfAPuqSzg4zV1eFmNFpRmCX6DbmyPjVxQ3wQ3MqU79M` for mint `8vdJA3MYhUhirZZzjZ5i8jXhNkHgrCnirCrqDG23pump`.

**Jupiter API** (live, keyless, 2026-09-27):
- `GET https://api.jup.ag/swap/v2/build` (200)
- `GET https://api.jup.ag/swap/v1/quote` (200)
- `GET https://lite-api.jup.ag/swap/v1/quote` (200)
- `https://quote-api.jup.ag/v6/quote` (no connection)

**Jupiter docs**, [`jup-ag/docs@16e9e9d5`](https://github.com/jup-ag/docs/tree/16e9e9d51819331c636079945047a595a44c7ee0):
- `swap/index.mdx`
- `swap/build/index.mdx`
- `swap/build/common-instructions.mdx`
- `swap/migration/metis-to-build.mdx`
- `swap/quote-and-swap.mdx`
- `swap/advanced/reduce-transaction-size.mdx`
- `swap/v1/index.mdx`
- `swap/v1/build-swap-transaction.mdx`
- `openapi-spec/swap/v2/swap.yaml`
- `openapi-spec/swap/v1/swap.yaml`
- `portal/rate-limits.mdx`
- `portal/plans.mdx`
- `portal/migration.mdx`
- `get-started/development-basics.mdx`
- `static/files/audits/swap-v6-offside-october-2025.pdf`

**Jupiter repos:**
- [`jupiter-cpi-swap-example@b9bdc57075`](https://github.com/jup-ag/jupiter-cpi-swap-example/tree/b9bdc5707555c62b2b1154ea36d0f46b905c8a41)
- [`sol-swap-cpi@a9c3132893`](https://github.com/jup-ag/sol-swap-cpi)
- [`jupiter-cpi`](https://github.com/jup-ag/jupiter-cpi) (archived, `12bc5f67b9`)
- [`jupiter-swap-api-client@fb95cfad9f`](https://github.com/jup-ag/jupiter-swap-api-client)

**Solana runtime**, [`anza-xyz/agave@f77165963c`](https://github.com/anza-xyz/agave/tree/f77165963c709934e8ea8564187c00f081b597d1):
- `program-runtime/src/execution_budget.rs`
- `program-runtime/src/cpi.rs`
- `program-runtime/src/invoke_context.rs`
- `runtime/src/bank.rs`
- `transaction-context/src/lib.rs`
- `feature-set/src/lib.rs`
- `svm/src/account_loader.rs`

**SIMDs**, [`solana-foundation/solana-improvement-documents`](https://github.com/solana-foundation/solana-improvement-documents/tree/main/proposals): 0268, 0296, 0339, 0385, 0406, 0582, 0596.

**Other Solana docs:**
- [solana.com/docs/core/cpi](https://solana.com/docs/core/cpi)
- [solana.com/upgrades/larger-transaction-sizes](https://solana.com/upgrades/larger-transaction-sizes)
- [`solana-foundation/solana-dev-skill@bb24c39:…/transactions-v1.md`](https://github.com/solana-foundation/solana-dev-skill/blob/main/skills/solana-dev/references/transactions-v1.md)

**Pump:**
- [`pump-fun/pump-public-docs@81091419`](https://github.com/pump-fun/pump-public-docs/tree/81091419e4457566469d4e2a27f64ed84d42419c): `idl/pump_amm.json`, `docs/PUMP_SWAP_README.md`, `docs/CPI_README.md`.

**Anchor:**
- `solana-foundation/anchor` (redirects to `otter-sec/anchor`): release v1.2.0 (2026-09-04), CHANGELOG.

---

## Review corrections (2026-09-27)

These apply the parts of the first-pass review findings that touch this note, plus the owner decisions now recorded in SPEC v0.3. Each fact was re-checked on mainnet or in the cited source on 2026-09-27.

1. **C1-7: the §1.3 transactions are the mainnet evidence for PDA sellers.**
   - Added to the §1.3 table: `2HVnqFoy…` and `8XWbPs7e…` are owned by `FqGg2Y1F…` with 752 B of data, and Jupiter passes them to PumpSwap `sell` as `user` at h3. `EMopL9…` is pump `sell_v2`'s `user` at h3, with 0 lamports before and after.
   - All three are sells. No non-whitelisted PDA **buy** was found (see `graduated-venues.md` §3.1, `pumpfun-bonding-curve.md` §6.1).
2. **C1-1: instruction trace (§3.1 row).** Added the enforcement point (`src/transaction.rs` L493–494) and observed usage:
   - caller → Jupiter `route` → PumpSwap `sell`: 11 entries;
   - `6Vo3245e…` → `route_v2` with two legs and an ATA create: 24;
   - pump `migrate_v2` alone: 51.
3. **Research part of C1-8: depth (§6.1).**
   - Changed "leaving 2 spare levels" to "≥ 1 spare; 2 only when every venue-side ATA and the UVA already exist". PumpSwap `buy` may create fee ATAs and the cashback UVA ATA. Whether it does so through the ATA program is **UNVERIFIED**.
   - Also corrected "direct PumpSwap CU … none un-wrapped found". Top-level PumpSwap CU is measured in `graduated-venues.md` §3.1 (`buy` 85,114; `sell` 74,157).
4. **Owner decisions (SPEC v0.3 §0, §2.1, §14 #16), context rather than a finding.**
   - §0 item 7, §6.1, §6.2 and §9 Q1–Q2 are marked decided or superseded: direct canonical-pool execution in the MVP (PumpSwap; Raydium AMM v4/CPMM); Jupiter in v1.1, bounded by the on-chain pool quote; CLMM-only tokens not shortable.
   - The Jupiter analysis in this note now describes the v1.1 path.

No review finding against this note was rejected.
