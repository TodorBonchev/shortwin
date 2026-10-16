# shortwin-math-wasm

wasm-bindgen facade over [`shortwin-math`](../shortwin-math) (and the mint filter of
[`shortwin-venue-state`](../shortwin-venue-state)) for the frontend (`packages/math-wasm`,
ARCHITECTURE §1.2, §7.3). Every export converts its arguments, makes the `shortwin-math` calls
the program's handler makes, in its order, and converts the result, so the UI computes exactly
what the program will; the frontend never re-implements protocol math (FRONTEND.md FE-15: the
open guards, pool selection, the mint verdict and a withdrawal's SOL part were TypeScript ports
until M10).

## JS conventions

- `u64`/`i64`/`u128`/`i128` amounts, prices (WAD) and indices are `bigint`; bps, ppm,
  seconds and small counters are `number`.
- An undefined result is `undefined` (e.g. a quote with `q ≥ X`).
- A `MathError` is thrown as an `Error` whose message is its `as_str()` text, e.g.
  `"above pro-rata allowance"` or `"invalid input"`.
- Argument names in the generated `.d.ts` are the Rust (snake_case) names.

## Exports

| Area | Exports | Math |
|---|---|---|
| Venue and quotes | `Venue.{pumpSwap, rayV4, cpmm, pumpCurve}` (`pumpCurve(fees, offsetToken, offsetQuote, migrationFee, poolTokens, postCompleteQuoteIn, pool)`); `withSynthetic(syntheticFees)` (a live curve's buys past `rT` by pump's synthetic migration, `B̂_v3`; throws `invalid input` on a pool venue); `buy`, `sell` on `(x, y)` = (token, SOL); `buyCurve`, `sellCurve` on a `CurveState`; `buyRepriced(q, L, P)` (at the live fee tier, R3B-7 b); `capped(capBps)` (risk view: one combined fee at the cap on pump venues); `onewayPpm` (φ), `onewayBps` (display); `exceedsCap(φPpm, capBps)` (the fee gate); `BuyQuote`, `SellQuote` | `quote::{Pricing, CurvePricing}` (curve: `B̂_v3` or straddle, repriced, liquidation price) |
| Fees | `Fees` (`onewayPpm`, `onewayBps`, `capped`), `CurveSchedule` (`addTier`, `resolve`, `resolveAtSupply(vT, vQ, supply)`: the synthetic exit's fee on the mint's live supply), `PammSchedule` (`addTier`, `resolve`) | `fees::{pump_curve, pump_curve_at_supply, pump_amm, ray_v4, cpmm, exceeds_cap}` |
| States | `reprice` (`isqrt⁺(P)`, `X` floor, `Y` ceil; `undefined` when `X = 0`), `CurveState.reprice`, `CurveState.migratedPool(fee, poolTokens, postCompleteQuoteIn)`, `migratingFvPrice(p, x0, y0, x1, y1)` (ρ-scaled FV price; `undefined` on `X₁ = 0` or `Y₀ = 0`), `migrationBoost(x1, y1)`, `liquidity` (throws `overflow` if `X·Y ≥ 2¹²⁸`), `priceWad`, `Reserves` | `quote::{reprice, curve_reprice, migrated_pool, migrating_fv_price, migration_boost, liquidity}`, `risk::price_wad` |
| CR and liquidation | `vEff`, `crBps`, `crBelow`, `Venue.liquidationPrice`, `sigma`, `sigmaXy` | `risk` |
| Opening | `marginD4`, `openFee`; the open guards `spotNearEma(pSpot, pEma, devBps)` and `pOpenMax()` (`P_ema > P_OPEN_MAX` refuses) | `risk::{margin_d4, open_fee}`, `caps::spot_near_ema`, `consts::P_OPEN_MAX` |
| Pool selection | `lambdaBps(lpCounter, lpSupply)` (λ), `lockedDepth(lpCounter, lpSupply, x, y)` (`D`), `lockedSol(D, P)`, `lockedFloorOk(D, pRef, λ_min, poolMinRealSol)` (the listing floor) | `pool_select::{lambda_bps, locked_depth, locked_floor_ok}`, `tier::locked_sol` |
| Exit bounds | `maxInLiq` (no reserve), `maxInExp` (rent and fee senior), `piRef` | `bounds::{max_in_liq, max_in_exp, pi_ref}` |
| Settlement | `OracleState` (`push` ring entries, `settleBox(now, executable)`, `ringMedians`, `sustainedGrowth`), `SettleBox` (`pLo`, `pHi`, `lLo`, `lHi`, `isPoint`, `contains`), `ringLen`, `pprime`, `amountOk`, `fairValue`, `fvRateBps`, `settleKeeperReward`, `expiryKeeperReward` | `settle::{box_of, pprime, amount_ok}`, `observe::{ring_medians, sustained_growth}`, `risk` |
| Rent | `utilizationBps`, `rateBps` (display: each slope term floored), `minRent`, `RentBasis` (`accrue` preview, `P_ACC_MAX` clamp; `lenderCredit`: that accrual's lender credit on `Market.rent_aggr` with the carried `Market.lender_credit_rem` (AIR-03), what the next prologue adds to `receivable`), `PositionRent` (with its `rent_protocol_bps`; `accrued`, `due`) | `rent` |
| Waterfall previews | `previewClose`, `previewLiquidation` (keeper floor at `K_s`), `previewExpiry`, `previewSettlement` (vesting lump: on every kind, S1 too since AIR-21, the take above `⌊Q·P_ema/WAD⌋ + R_due`; `executable = false` prices the tokens at 0 as `sol_settle` does on a venue that cannot execute, AIR-09) → `Payout` (`keeperTopup`, `lump`) | `waterfall` |
| Caps | `maxQPosition`, `maxTotalQ`, `feeSizeCap`, `feeOiCap`, `oiCap` (φ in ppm), `utilOk`, `curveGammaOk`, `curveGammaMaxQ`, `walletOk`, `notionalOk`, `conservativeX0(venue, xNow, lSlow, pEma, lockedDepth, poolLockMinBps)` (venue codes as `TierInputs`; PumpSwap sizes on `D` itself, `λ_min` = `pumpswapLockMinBps()` = 10,000 whatever `poolLockMinBps` is — OWNER-M6D-1, ECON #106 — Raydium on `D/λ_min`) | `caps`, `consts::PUMPSWAP_LOCK_MIN_BPS` |
| Tiers | `TierInputs` (`tier`, `liquidityLamports`; `lambdaMinBps` is the config's `pool_lock_min_bps`, read on Raydium only — PumpSwap's bound is its burned LP's real SOL, OWNER-M6D-1), `tierParams`, `TierParams` | `tier` |
| Lender previews | `PoolState` (Market lender-pool group and `MarketVault`: pot `unmatched`, `protocolFees`, vest `vestBal`/`vestLast`/`vestEnd`, `maxRelease`, `wipeSwept`, `calDay`, `setLockBucket`), `LenderState` (`lockUntil`), `ShareEpochs`; `sharesForTokens`, `tokensForShares`, `previewTouch`, `previewAllowance`, `previewLock`, `previewLockedTotal`, `previewLockUntil`, `previewVestRelease`, `previewTokenValue`, `previewWithdraw` (`lockGraceS`; tokens), `previewWithdrawal` → `Withdrawal` (`tokens`, `sol` — `claim` right after, the last lender's vest flush included —, `lastSharesBorrowed`, the `lender` after: `lock`, `lockUntil`), `previewDeposit` (`rewardsRequired`; throws `post-wipe sweep pending`), `previewClaim` (pot draw); every preview takes `now` | `pool` (on copies) |
| Mint filter | `mintCheck(key, owner, data)` → `MintVerdict` (`ok`; `codes`: every failing rule as its program error, the first being `create_market`'s; `extensions`: the rejected type per reason, −1 otherwise; `types`: the TLV types in order; the base fields), `isAllowedExtension`, `allowedExtensions` | `shortwin_venue_state::mint::{check, read_base}` (each further reason is `check` again with the previous rule lifted on a copy) |
| Parameters | `ParamSet` (`get`, `set` by ECON §15 name and tier 0–3, `validate` (what the program refuses: the sim's rules, then the bounds of the parameters the sim does not model, `validate_econ`), `validateSim` (= `shortwin_sim.validate_params`), `violations`) | `params::{ParamSet, check, validate, validate_econ}` |

The keeper-side state machines (`observe::{record, integrate}`, `persist`, `settle::evaluate`
and the reference prices of `bounds`) are not exported: the frontend reads their results
(EMA, ring, arming state) from the accounts. `OracleState` rebuilds the box of the stored
state; `sol_settle` evaluates it after its own roll.

## Build and check

```sh
cargo test -p shortwin-math-wasm                     # native unit tests of the wrappers (Ok paths)
cargo build -p shortwin-math-wasm --target wasm32-unknown-unknown --release
# JS glue: the wasm-bindgen CLI must match the pinned crate (=0.2.129)
cargo install wasm-bindgen-cli --version =0.2.129 --locked
wasm-bindgen --target nodejs --out-dir target/wasm-pkg target/wasm32-unknown-unknown/release/shortwin_math_wasm.wasm
node crates/shortwin-math-wasm/js/smoke.cjs target/wasm-pkg   # BigInt ABI, undefined, thrown errors, worked examples
```

CI's `web` job builds the `.wasm`, binds it for Node and runs the smoke test, after
`pnpm --filter @shortwin/math-wasm build`. A
`bool` argument the caller leaves out reaches the facade as `false` (wasm-bindgen does not check
it); the generated `.d.ts` marks every argument required, so the type check catches it in
TypeScript callers.
