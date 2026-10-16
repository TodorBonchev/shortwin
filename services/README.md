# services

The off-chain services of short.win (ARCHITECTURE §7). A **host** Cargo workspace with its own
`Cargo.lock` (ARCH §1.2): the Agave client crates and tokio never enter the on-chain
workspace's lockfile.

| Crate | What |
|---|---|
| `common/` (`shortwin-services-common`) | The chain-client abstraction (`Chain`: clock, accounts, account headers, program accounts — also by exact size, a `dataSize` filter on JSON-RPC —, blockhash, simulate, send, recent priority fees, Jito bundles: send and pre-flight), its JSON-RPC backend (`RpcChain`; bundles through a Jito block engine's `sendBundle`, **UNVERIFIED** against a live engine), transaction building: v1 (SIMD-0385) with a v0 fallback (ComputeBudget instructions, optional lookup tables), `⌈simulated × 1.1⌉` compute units, sizes, decompilation |
| `keeper/` (`shortwin-keeper`) | Keeper v1 (ARCH §7.1 duties 1–14 on every venue, with duty 5's Raydium fast path and M7's O17 degraded mode through the `coverage_authority`) and its binary; `shortwin-build-alt`, the protocol address lookup table of A28 (`scripts/build-alt.sh`) and M7's per-market tables |
| `keeper-litesvm/` (`shortwin-keeper-litesvm`) | TEST-ONLY: the in-process LiteSVM backend (the program-test harness's invariant hooks after every instruction of every keeper transaction) and the keeper's scenario tests on the mock venue and on the Raydium and pump mainnet forks, and across short.win's own upgrade from 1.0.0 (`upgrade_window`) |
| `indexer/` (`shortwin-indexer`) | The indexer and API (ARCH §7.2, M8; contract [`docs/design/INDEXER.md`](../docs/design/INDEXER.md)): RPC backfill and live ingest of short.win and venue events into Postgres (`migrations/`), projections and analytics, axum REST + WebSocket (`openapi.yaml`), one binary `shortwin-indexer` (`migrate`, `backfill`, `live`, `serve`, `rollups`, `run`, `replay`, `rebuild`); its TEST-ONLY `embedded-pg` feature runs PostgreSQL 17 from the user cache (no Docker, no root); the reference deployment is `indexer/deploy/` |
| `ops/` (`shortwin-ops`) | `shortwin-gov`, the Squads v4 governance tool (M10 WP-7; [below](#governance-shortwin-gov-m10)): creates the Governance and Emergency multisigs, initializes the config with their vaults, stages / approves / executes / cancels vault transactions (the checked upgrade-authority transfer, `Authorities` changes, upgrades, every admin instruction; the transaction-buffer path for large messages), applies timelocked changes, verifies the whole layout |
| `indexer-litesvm/` (`shortwin-indexer-litesvm`) | TEST-ONLY: records LiteSVM sessions (the program-test harness, the keeper scenarios) in the indexer's session format and replays them into an embedded Postgres, asserting the indexed state against the chain (INDEXER.md §11.3, §11.4) |

## Keeper

What it does, per tick (one every `--tick-ms`, 400 ms by default, about a slot;
`keeper/src/keeper.rs`):

- **Reads** (`view.rs`; M7-SEC-8): the open positions first (one `getProgramAccounts`), then in
  full every market with positions, every PumpSwap and Raydium market, a curve within
  `hot_curve_completion_lamports` of completion and any market with something time-critical
  pending; the other list-only and curve markets every `cold_interval_s` (30 s), staggered by key
  (metrics `markets_read`, `markets_cold_skipped`). The tick's due sweep checks are one read; at
  most `pool_select.discoveries_per_tick` (2) pool discoveries a tick, each one's four
  `getProgramAccounts` in parallel; `RpcChain` sends a read's 100-key chunks in parallel (8 at a
  time). Junk list-only markets (≈ 0.035–0.038 SOL each to list at ARCH §3.2's rent; the M7
  review counted ≈ 0.047) no longer grow every tick's reads. **Decision A (2026-10-01):** a market
  with positions **below the value-at-risk floor** (`[budget] var_floor_lamports`) is read only when
  its 30-s idle crank is due, in the tick its set of positions changes, and while the keeper's own
  `set_coverage_pause` on it is pending (metric `markets_idle_skipped`), so junk with positions does
  not grow the per-tick reads either.

- **Venues** (`venue.rs`): Raydium AMM v4 and CPMM markets (M3), through the adapters' own key
  functions (`shortwin::venues::raydium_v4::{read_keys, trade_keys}`, the same in
  `raydium_cpmm`) with ARCH §3.12's writability in trades (pool and vaults writable; the LP mint
  and the CPMM `AmmConfig` read-only — a config is shared by every pool on it; `trader_wsol` and
  the CPMM observation writable); the pump curve (M5) and PumpSwap (M6: the adapter's ReadSet
  P0–P5 with the pool and its vaults writable, TradeExtras T0–T18 with a protocol-fee and a
  buyback recipient of the live `GlobalConfig` and the live coin creator's vault; the mint's
  supply is read with the ReadSet, since PumpSwap's fee tier is its market cap); and the
  TEST-ONLY mock (feature `test-venue`).
- **Duty 1, coverage** (`coverage.rs`, `decide.rs`): one observation per second on a **covered**
  market (open positions and value at risk at or above `var_floor_lamports`, decision A) with a
  position near a threshold (live CR below 1.3·L, deadline within 60 s, armed) or with
  `|P_spot/P_ema − 1| > 2 %`; every 30 s on markets with open positions — and nothing more below
  the floor: no per-second coverage, no bursts, no liquidity-anomaly coverage, exits on its idle
  reads —; every 30 s on markets
  without a route (the continuous S3 chain, duty 5); none on markets without positions — except
  a Raydium market inside its listing window (M4V-5): every `listing_interval_s` (60 s) through
  the window and every second in its last `EMA_WARMUP_BUCKETS + 2` (10) seconds, and after it
  until its post-change warm-up is over (`ema_warm == 0` as the next instruction's roll sees it,
  M4R), so the first open after the window passes the program's gate (`EmaWarmingUp`) on a
  `P_ema` the pool's own buckets seeded. After a `PoolSwitched` (any market with open positions
  and `ema_warm > 0` on an executable venue; M4R, duty 5) every second until the warm-up is over:
  the program refuses keeper exits until then, so the positions' plan is `Watch` and no
  `liquidate` / `expire` is sent; the first second the projection shows `ema_warm == 0` (adoption
  + 9 s with per-second observations) the exits go out as usual. The per-second cadence then
  lasts for as long as the post-change reference guard is set (`ema_guard == 1`,
  `MarketNeeds::guarded`; M4F-1): every second keeps its honest bucket low, so deposit-wrapped
  cranks in otherwise idle seconds cannot lift the ring's liquidity lows and clear the guard.
- **Duty 2, bursts**: an observation in the open or last completed bucket whose **price**
  deviates by more than 2 % from what the keeper itself read in that second and the one before was
  not the keeper's, and — when its excursion **reverted**, the keeper's latest read more than 2 %
  away from it too (M7-SEC-2; an atomic `[pump, crank, dump]` leaves exactly that) — starts one
  observation every slot for 60 s from that second (never while an S3 on an executable venue is
  due: once per second there, duty 5). A foreign price still there at the next read is a genuine
  move (metric `foreign_moves`; duty 1's per-second coverage follows it): as built at M7 a plain
  `crank_price` after anyone's honest move started a 116-crank burst.
  The per-slot liquidity bursts M4F-1 added stay reverted (M4G-2: an atomic `[deposit,
  crank_price, withdraw]` bought a 60-s per-slot burst, 25–150× its cost in the keeper's).
- **Duty 2, liquidity coverage** (FITS-2 (b), owner decision of 2026-09-29): on a market **with
  open positions**, an observation in the open or last completed bucket whose **liquidity**
  extremes (`llo` / `lhi`; the last completed bucket's from the ring entry that integrated it)
  deviate by more than 2 % (`burst_dev_bps`) from the `√(X·Y)` the keeper itself read in that
  second — an LP-wrapped `[withdraw, crank_price, deposit]` (the withdraw drag of `L_ema` /
  `L_slow`) or `[deposit, crank_price, withdraw]` (the freeze) — starts one observation **per
  second** (never per slot) for `liq_cover_s` (10 s) from that second, extended by every
  recurrence (`MarketNeeds::liq_covered`). While the attacker keeps wrapping, every second carries
  the keeper's honest bucket extreme — no attacker-only second — at one keeper crank per attacker
  transaction (1:1); an isolated wrap buys at most 10 keeper observations. It holds `L_ema`, it
  cannot lift a low reached genuinely before the wrapping started (an LP withdrawn for real for a
  few seconds: nothing foreign then) — FITX-1, ECON §12.12 residual 10, owner item O18. A market without open
  positions never gets liquidity-triggered coverage (nothing there reads the liquidity EMA);
  `liq_cover_s = 0` disables it. A venue change drops the depths viewed before it. The rule is
  venue-independent: since the owner's decision of 2026-09-30 (M6R-1; canonical PumpSwap pools
  take third-party LP) it runs on PumpSwap markets as on Raydium, the depth being the keeper's own
  `√(X·(Y + VQR))` (scenarios `pamm_lp.rs`).
- **Duty 3, exits**: `liquidate` at the first second the program accepts it (the fixed-δ bound;
  retried every second on `ExitBoundNotMet`), `expire` when the expiry bound is met. Trading
  exits try the extras sets `current`, `none`, `previous`, `stable` and keep the first that
  simulates (duty 10's rule; `stable` is M2-SEC-2's set), moving on after a venue failure or a
  refusal of the set itself (`BadExtras`, `BadRemainingAccounts`, `DeniedAccount` — since M10
  batch 3 also a set naming the borrower or the adopted pool's own accounts). On PumpSwap, when
  the pool's live coin creator is not the market's cached one (anyone can change it through
  pAMM; the next `crank_price` re-caches it), the exit's transaction starts with
  `create_idempotent` of the new creator vault's WSOL ATA (T11), which the program's existence
  requirement needs (M6-SEC-4). The exit projection's risk fee (the live CR of duty 1, the spot
  and EMA legs) is `min(φ_live, cap)` with the program's own cap: the position's hard cap, and
  on PumpSwap at most the tier of `P_ema`'s market cap, which a same-transaction trade cannot
  raise (AIR-10, `VenueState::exit_risk_cap_bps` on the exits' venue read,
  `read_state_boxed_for_exit`; `2c1fce9`). A live market cap in a higher-fee tier than
  `P_ema`'s no longer makes the keeper send liquidations the program refuses.
- **Duty 4, `sol_settle`**: at most once per second for armed positions and for liquidatable
  positions without a normal buyback (it arms, keeps the clock, disarms on box evidence, or
  settles), and for positions whose S3 is due (on a halted venue: probed by simulation, sent
  only when it settles).
- **Duty 5, no route**: a halted venue (a Raydium admin halt, a CPMM status bit, a pool that stops
  parsing) or a fee above the market's S3-chain cap (the lower of `venue_fee_hard_cap_bps` and
  `Market.min_open_hard_cap_bps`, the lowest cap a position locked since the market last
  emptied: AIR-19) starts the S3 chain at the keeper's next tick and is observed every 30 s
  after, so the chain never restarts. A position's S3 timer follows its own route, the fee
  against the lower of the global cap and its own locked cap (`route::position_no_route`), as
  `sol_settle` does (`6a46cde`): a fee between the mark and a position's own cap runs the chain
  without making that position S3-due.
- **Duty 7, batching**: an exit is the market's observation for the second (no separate crank);
  the remaining cranks share transactions, up to 6 per v1 transaction and 4 per v0 transaction
  where the cluster refuses v1 (re-probed hourly), each batch **cut where its transaction would
  exceed the format's size or account limit** (so any mix of the mock's 3-key, AMM v4's 4-key and
  CPMM's 5-key ReadSets packs as densely as fits); each batch is simulated and a market whose
  crank fails is dropped from it and cranked alone.
- **Duties 8, 14, budget and priority** (`budget.rs`, `fees.rs`, config `[budget]`; O17, owner
  decisions of 2026-09-30 and 2026-10-01): compute-unit limit `⌈simulated × 1.1⌉`; priority price =
  p75 of the recent per-account fees on the transaction's markets, ×1.5 per consecutive missed
  second, capped at the O17 ceiling of **100,000 µlamports/CU** (observations and exits alike).
  **The global budget** (owner decision A of 2026-10-01; ARCH §7.1 duty 8 has the exact
  algorithm): one daily budget `G` (`global_daily_lamports`, PROPOSED 1 SOL a UTC day; the M7
  `per_market_daily_lamports` is retired and refused) funds markets in order of their lenders'
  value at risk `V = ⌊borrowed_q·min(ema_price, P_ACC_MAX)/10¹⁸⌋`. Covered markets (positions and
  `V ≥ var_floor_lamports`, 1 SOL) are ranked by `V` and hold entitlements of twice a quiet day
  (`reserve_margin_bps` 10,000), funded in rank order at 00:00 UTC — a market covered later gets
  its prorated entitlement from the free pool, then from the lowest-ranked markets —; the rest of
  `G` is the free pool. A charge (a crank's share of its batch's fee) draws on its market's
  entitlement, then the free pool, then — exit-critical and baseline charges only, never price
  bursts or liquidity-anomaly coverage — lower-ranked entitlements, lowest first. The cranks exits
  depend on (a position near a threshold, armed or warming up, a liquidatable or expired one
  waiting for a fresh reference or its bound, the S3 chain, a market without a route) are never
  throttled — at most one crank a second per market (a price burst's further cranks are
  discretionary; M7D-4) —: beyond `G` they are an overdraft (`budget_overdraft_lamports`,
  `ExitOverdraft` once a day), and so are the warm-ups and listing windows of markets without
  positions. A market is funded at most once a UTC day; what its join takes from lower-ranked
  markets pays no burst and returns to them when it stops being covered (M7D-3). Before a market's
  first crank its per-crank charge is estimated at the keeper's measured average, or `⌈5,000 /
  batch_v1⌉` at a start, and re-estimated at its first crank, the excess released and the markets
  still short topped up in rank order (M7D-5). Markets below the floor get the idle cadence only,
  from the free pool within `tail_share_bps` (2,000: 0.2 SOL a day), read in the tick their idle
  crank is due and — while a position is near an exit — in the seconds after an observation while
  its reference is fresh, so the exit goes the second after the crank (M7D-1); the crank such an
  exit waits on is exit-critical (beyond the tail it overdraws; M7D-2). Any other charge the budget cannot cover is refused (`BudgetExhausted` once a day) and, on
  a market with open positions, is degraded mode **at once** (`Degraded{Budget{spent, budget,
  rank}}`, duty 9). Exits, pool selection, graduation moves (the crank a graduation step forces
  included), `crank_sol_usd` and `set_coverage_pause` are neither charged nor throttled. Metrics per
  market: `var_lamports`, `budget_tier`, `budget_rank`, `budget_unspent_lamports`, the day's spend
  per class, `budget_refused`; globally `budget_{global,free,tail,overdraft,spent,excess}_lamports`.
- **T-CASCADE, exit compute per slot** (`[budget] market_cu_per_slot`, `payer_cu_per_slot`; C0-17,
  ARCH §6.6): every exit write-locks the `Market` and its `MarketVault`, and every keeper
  transaction its fee payer — 24,000,000 CU per account and block, charged with each transaction's
  **requested** limit. The keeper requests at most 12,000,000 CU of exits per market per slot and
  20,000,000 across its exits in a slot; the rest of a cascade (most under-collateralised first)
  goes out in the next slot (`ExitResult::Deferred`, metric `exits_deferred`).
- **Duty 6, graduation** (`graduation.rs`, config `[graduation]`; M5, M6; ARCH §2.5, §5.1, §7.1;
  pump-final §7): the keeper covers `PumpCurve` and `PumpMigrating` markets with the curve
  adapter's own ReadSet and, for a curve exit, pump v3's TradeExtras T0–T9 (a buyback recipient of
  the live `Global` at T7, the market's cached curve keys); its risk view is the program's own
  quoter (`VenueState::quoter`: `B̂_v3` above a live curve's `rT`, at the synthetic fee on the
  mint's live supply, which the keeper reads with the ReadSet; the post-complete fields and a
  ρ-scaled FV on a migrating curve).
  - **Exits past `rT`** (pump-final §5.2, owner D1 (a), D2, D3): a liquidatable or expired position
    whose `Q` exceeds its live curve's `rT` is liquidated or expired like any other — pump's
    `buy_v3` buys `rT` on the curve and `Q − rT` from the would-be pool in the exit itself
    (synthetic migration), priced `B̂_v3`, and the market moves to `PumpMigrating` in that
    instruction; `buyback_allowed` and σ are judged on `B̂_v3`, so above `σ_max` the position goes
    to `sol_settle` (S2) at once. The keeper then sends `migrate_v2` and the transition in the same
    tick (metric `synthetic_exits`). The 3-transaction Jito straddle bundle, the `complete-curve`
    helper (gone from the workspace, Governance and the indexer since pump-final P6), `Plan::Straddle` and `Alert::StraddleWaiting` are retired; `[graduation] straddle`,
    `send_without_exit`, `complete_slippage_bps`, `max_complete_lamports`, `tip_lamports`,
    `tip_accounts`, `block_engine` and the binary's `--block-engine` are refused ("removed: pump
    v3 exits need no bundle (design pump-final)").
  - **The TradeExtras format across short.win's upgrade** (pump-final §7, §15 "Release
    coordination"; for one release): the keeper builds both a curve exit's v3 list (10 keys) and
    shortwin 1.0.0's (20, `venue::pump_trade_extras_v2`), and uses the one the deployed short.win
    parses, known per short.win's ProgramData deploy slot (its header, read once a tick when a curve
    exit is due). At start-up and at each new deploy slot the first curve exit is simulated with V3,
    then with V2 when V3 fails on account parsing (`BadRemainingAccounts`, `VenueAccountMismatch`);
    the format that gets past account parsing is kept until the deploy slot changes (an exit with
    it failing on account parsing tries the other at once). Neither, on a deploy slot no exit has
    found the format of: `Alert::CurveExtrasFormatUnknown`, once per position, deploy slot and pair
    of refusal codes, with the program's error lines (metrics `curve_format_probes`,
    `curve_format_failures`; `TickReport::curve_formats`). The same two codes come from a market's
    own ReadSet (`check_read`) or VenueExtras (`parse_extras`): the error lines' source lines tell
    which, and if other curve markets' exits parse, the cause is that market's. Neither on a deploy
    slot whose format an exit already found is that exit's own refusal: the known format's is
    paged (`ExitRefused`, its detail the program's error line), and the format stays. Every send is
    simulated first, on a bank at or past the deploy slot read — a node of an RPC pool behind that
    read answers for the build before, so the exit is not sent and is tried again the next tick
    (an error of the tick) —, so the Governance upgrade costs no failed send (keeper-litesvm
    `upgrade_window`). On 1.0.0 a curve exit past `rT` is refused `CurveStraddle` (its exits
    never buy past `rT`, and the bundle is retired): the keeper sends the position's `sol_settle`
    instead — S2 when its legs hold, else a no-op crank, at most once a second — pages nothing, and
    exits it past `rT` on the first tick after the upgrade (§15 step 1). The next release deletes
    V2.
  - **`migrate_v2` as soon as a curve completes** (ADOPTED Q16; on by default again in M6): a
    covered market whose curve is complete with `vT > 0` and whose canonical PumpSwap pool does
    not exist gets pump's `migrate_v2` — top-level, alone with its compute budget (A15), its 27
    IDL accounts and the two boost accounts — at most once a second, and right after the keeper's
    own exit completed the curve. A failure raises `Alert::MigrateFailed` once and is retried every
    second.
  - **The `→ PumpSwap` transition** (M6): as soon as a curve market's canonical pool is valid
    (after the keeper's `migrate_v2` or anyone's; with `migrate` off too) the keeper sends one
    transaction: the accounts the pool's trades require that do not exist (ARCH §3.12's existence
    requirement: the trader's pAMM UVA — `init_user_volume_accumulator(payer = keeper, user =
    trader)` —, the coin-creator vault's WSOL ATA, the recipients' WSOL ATAs, on a cashback coin
    the trader's cashback ATA), `crank_price` on the curve's ReadSet (the program moves the market
    onto the pool: the ring rewrite, the post-change warm-up) and `crank_price` on the pool (its
    first observation). No `extend_account`: a fresh 271-byte pool trades as it is. A PumpSwap
    market whose accounts are missing (a listing, someone else's transition) gets them created;
    `Alert::TransitionFailed` on a failure.
  - **List-only markets** (M5, M6): a list-only (`None`) market is covered with its detection set
    `[bonding_curve, canonical_pool]` as ReadSet (the program's own for `crank_price` and
    `sol_settle`). One with positions open — a curve that graduated elsewhere — is observed on the
    no-route cadence and its positions are settled in S3 when due (duty 5; M2-SEC-5), and the pool
    monitor collects its Raydium pool in a window (duty 12). One whose detection set shows a live
    or migrating curve or a valid canonical pool again (the program's `classify_pump`) is cranked,
    which moves it onto that venue; the keeper then creates the trader accounts the legs require
    (`crank_price` has no payer and creates none): the base ATA and pump UVA on the curve, the pool
    accounts above on PumpSwap.
  - **PumpSwap warm-up without positions** (M6, a keeper choice): a PumpSwap market with no open
    position is observed every second while its warm-up runs (after a graduation, and from the
    price start of a PumpSwap listing, which starts at its first observation), so the first open
    finds a `P_ema` the pool's own honest buckets seeded — M4V-5's listing-window duty on
    Raydium, for PumpSwap.
- **Duties 6 and 13, the trader side** (`sweep.rs`, config `[sweep]`; M6; ARCH §3.9, §7.1;
  R4A-7): right after a full wipe (a live wipe record, the post-wipe bits not both set) the keeper
  sends, in the same second, the venue cashback claims the trader needs and `sweep_trader` in one
  transaction — pump `claim_cashback(user = trader)` when the trader's pump UVA holds lamports
  above its rent; on PumpSwap `create_idempotent ATA(trader, WSOL)` and pAMM
  `claim_cashback(user = trader, user_wsol = ATA(trader, WSOL))` when `ATA(pAMM UVA(trader), WSOL)`
  holds any, preceded by PumpSwap `init_user_volume_accumulator(payer = keeper, user = trader)`
  when the trader's pAMM UVA does not exist (M6-SEC-2: anyone can fund that ATA of a UVA never
  created, and the claim fails 3012 without it) — so the deposit, and hence the open, that waits
  for both bits can land (no signature but the keeper's; M5-SEC-1's UVA-reading `sweep_trader`, no
  `claim_venue_rewards`; on `PumpMigrating` too, M6-SEC-5). Every
  `check_interval_s` (60 s) it reads the trader side and sends `sweep_trader` when
  `ATA(trader, WSOL)` exists, the trader PDAs hold lamports or a pump venue's trader base ATA holds
  tokens; on a cashback coin whose claimable cashback reaches `cashback_min_lamports` the claim
  goes with the sweep, at most every `cashback_interval_s` (a week). `Alert::SweepFailed` on a
  failure (severe after a wipe: deposits wait).
- **Duty 9, degraded mode** (`coverage.rs`, `alerts.rs`, config `[coverage_pause]`; O17, ARCH
  §3.1 `set_coverage_pause`): three consecutive seconds with a required observation and none landed
  (outage, contested blocks), a `CoverageGap` event from the program (a gap above 40 s since
  OWNER-M7-12), or a charge of the market the global budget refused while it has open positions
  raise `Alert::Degraded`. The **designated pauser** (one
  instance, `enabled = true`, holding the `coverage_authority` key — a separate key from the fee
  payer, `--coverage-keypair`) then sends `set_coverage_pause(market, true)` in the same tick —
  simulated first, at the exit ceiling, outside the budget, retried every second until the flag
  matches — which refuses opens only (`CoveragePaused`); closes, `add_margin`, liquidations,
  expiries, settlements, cranks, deposits, withdrawals and claims continue. After ten minutes of
  restored coverage (`recovery_s`; seconds that require nothing count as met; seconds the keeper
  was down do not) `Alert::CoverageRestored` and `set_coverage_pause(market, false)`. A flag found
  set at the pauser's first sight (a restart) starts the market `Recovering`
  (`CoveragePauseFound`): never lifted at once, so two instances never flip-flop it; one that turns
  set while the pauser runs without its having sent the set raises **`CoveragePauseForeign`** (a
  second or compromised coverage key; OWNER-M7-3 (b)) and is adopted the same way. The pauser sets
  a cleared flag again only on a market it degraded itself and still holds degraded; a flag it only
  adopted that someone else — the Emergency multisig — clears stays clear. Without the key, with the
  role unset on chain
  (`coverage_authority` default) or with a rotated key, the pauser raises
  `Alert::CoveragePauseUnavailable` once and the `Degraded` alert's page to the Emergency multisig
  (`set_pause`, reason `KeeperCoverage`) is the pause path; a refusal raises
  `Alert::CoveragePauseFailed`. Markets without positions are never coverage-paused.
- **Duty 11, the SOL/USD cache** (config `[oracle]`; ARCH §4.7, §7.1): every `interval_s` (30 s)
  the keeper reads the configured Pyth `PriceUpdateV2`; an update newer than the `OracleCache` that
  the program's own rules accept (`pyth::check`: verification, feed, age, confidence, step) is sent
  as `crank_sol_usd` at once (a global cost line, never a market's budget), a failed one retried
  every second. `Alert::OracleRefused` (once per reason) for a newer update the rules refuse,
  `Alert::OracleAging` (once per stall, hourly while it lasts; M7D-6) when the cache is older than `aging_alert_s` (300 s, validated in
  (0, 600); at `oracle_stale_s` — 600 s since the owner's decision of 2026-10-01, OWNER-M7-15, was
  3,600 — every graduated market tiers C), `Alert::OracleUnreadable` for a missing or undecodable
  feed.
- **Duty 10, the venue drift monitor** (`monitor.rs`, config `[venue_watch]`):
  - **Upgrades.** Every tick the keeper reads the 45-byte headers (`getMultipleAccounts` with
    `dataSlice`: the accounts are up to 10 MiB) of the six ProgramData accounts the `VenueGuard`
    latch indexes (pump, pAMM, pfee, AMM v4, CPMM, Jupiter) and compares their deploy slots with
    the latch, before the tick's markets. Without a recorded fingerprint (pump, PumpSwap, and
    every venue before its hashed revalidation) the slot decides: bit clear and slot ≠
    `guard_slot` → flag; bit set and slot ≠ `flagged_slot` → a second upgrade during review,
    refresh. With one (AIR-01, KI-69; `d2bb168`, `a1edf78`, `bf037ef`), a moved slot with the
    bit clear is judged by `flag_venue_upgraded`'s own simulation, the program's rule on the
    whole account, before the tick's markets. All such venues of a tick are simulated together:
    one priority-fee read (`getRecentPrioritizationFees`) and one blockhash, read at the same
    time, then the simulations in parallel, so two round trips in a row before the exits for
    any number of moved venues. Each simulation also hashes the venue's trimmed ELF on the RPC
    node (the latch instruction after an extension: pump-fees 438,358 CU, CPMM 369,393, AMM v4
    210,386; Jupiter not measured).
    - An upgrade passes. It is paged and flagged (simulated again, then sent) in the slot it is
      seen, before any exit or crank.
    - `VenueNotUpgraded` from a simulation whose context slot is at or past the header's deploy
      slot is the loader's `ExtendProgram`. It is neither flagged nor paged, only logged and
      counted (`TickReport::extended`, `shortwin_keeper_venue_extensions_total`), and that
      deploy slot is not simulated again.
    - `VenueNotUpgraded` from an earlier context slot comes from an RPC node behind the header
      read: it still holds the previous deploy, where the program gives the same answer for an
      upgrade. The keeper does not take it as an extension. It reads the account whole after its
      exits and cranks and applies the program's rule: an upgrade is paged with
      `VenueFlagFailed` ("simulated on slot …, before deploy slot …"), an extension is counted,
      and the flag is simulated again the next tick.
    - If the simulation fails for another reason, the same whole read decides: an upgrade is
      paged with `VenueFlagFailed` and flagged again the next second, an extension is counted,
      and if that read fails too, the slot is paged (fail-safe).
    - If short.win itself refuses the simulation as not deployed in that slot (AIR-18), the same
      whole read decides: an upgrade is paged in that slot, without `VenueFlagFailed`, and
      flagged in the first slot where short.win runs again; an extension is counted. A stranger
      who extends short.win in every slot delays the flag, not the page; opens ordered before
      each extension in its slot still run.

    The keeper reads a deploy slot's whole account once. During a review, a move past the
    deploy the keeper recorded is read whole after the exits: an extension of the recorded
    deploy is neither refreshed nor paged, another upgrade is. The deploy the keeper flags, or
    finds under review after a restart, is read whole once after the exits and recorded. The
    keeper sends `flag_venue_upgraded(venue)` at the exit priority
    ceiling (simulated first; a latch someone else already set is not repeated; an unflagged
    upgrade is retried every second) and raises `Alert::VenueUpgraded` once per upgrade and
    `Alert::VenueUpgradeFlagged` when its flag lands — page the Emergency multisig to run the
    fork suite against the new binary and `revalidate_venue(venue, slot, elf_hash)` (the hashed
    revalidation of
    [Venue fingerprints after `init-config`](#venue-fingerprints-after-init-config-air-01)).
    Either Raydium program's upgrade blocks opens on every
    Raydium market (one mask). Token-2022's ProgramData is watched too, alert only: it is read
    whole once at start, each later change is judged against the last whole read, and only an
    upgrade raises `Alert::ProgramUpgraded` (an extension is counted, not alerted).
  - **Pool admin actions.** Raydium's pool admin (a 2-of-3 Squads v3 vault with no timelock) can
    halt a pool (AMM v4 `SetParams` status 2–5 or 7 with a future open time; CPMM
    `update_pool_status` bit 2) and raise fees (AMM v4 `SetParams` fees per pool; CPMM
    `update_amm_config` for every pool on a config). A polling keeper watches the accounts those
    instructions write — each market's pool and CPMM config, already in the ReadSet it reads —
    and raises `Alert::VenueAdmin` for every change: route lost / restored (with the status),
    status, open time, and the one-way fee measured against `venue_fee_cap_bps` (opens refused)
    and against the market's S3-chain cap (`s3_no_route`): `venue_fee_hard_cap_bps`, or
    `Market.min_open_hard_cap_bps` when lower (AIR-19, `FeeCaps::of_market`), naming the
    instruction that writes the field. A pool that stops parsing is reported as such ("not
    executable", A63).
- **Duty 12, the Raydium pool monitor, and duty 5's Raydium fast path** (`pool_monitor.rs`,
  config `[pool_select]`; ARCH §2.5 `None`, §2.6):
  - **Discovery.** Every Raydium SOL pool of each covered Raydium or list-only market's mint,
    through `getProgramAccounts` byte filters: AMM v4 `coin_vault_mint` @400 / `pc_vault_mint`
    @432 and CPMM `token_0_mint` @168 / `token_1_mint` @200, the mint on one side and WSOL on
    the other (four calls per mint, each filtered to the one pool size the program verifies —
    `dataSize` 752 B for AMM v4, 637 B for CPMM), at the first sight of a market and every
    `discovery_interval_s` (300 s) after — every `fast_discovery_interval_s` (10 s) while the
    incumbent fails `pool_fail` or a collection window runs (never while a ranking is still in
    progress). The calls can go to another endpoint (`discovery_rpc`, `--discovery-rpc`).
  - **Ranking** (M4-SEC-7: anyone can create pools for a mint, ≈ 0.17 SOL each). Each discovered
    pool is scored once per discovery by its locked SOL `λ·R` — its pool account (a status or
    nonce the verifier would refuse scores nothing) and one batched read of its WSOL vault and
    LP mint, at most `rank_keys_per_tick` (200) accounts a tick and market, a large discovery
    over several ticks with the previous ranking in force — and only the best `max_candidates`
    (8) are evaluated. Unlocked liquidity scores nothing, so junk pools rank last.
  - **Evaluation.** Each ranked candidate's ReadSet, read fresh, goes through the program's own
    verifier (`engine::pool_select::verify_candidate`: the adapters' `verify` and `eligible()`,
    the locked-depth floor at the market's `P_ema`; plus `window_floor_ok`, the window floor of
    a collection window's candidate, M4V-1) on host `AccountInfo`s, which gives the
    program's locked depth `D`; a list-only market's D1 check is the program's `classify_pump`
    on its detection set. Pool selection runs after the tick's exits and crank batch, so its RPC
    work never delays duties 1–4 (the observation that keeps a failing market's `pool_fail`
    chain goes out in a batch of its own right after). Every `eval_interval_s` (30 s),
    and every tick while the fast path is armed, a window runs, the incumbent just started or
    stopped failing, or a waiting candidate meets a market whose last position just closed.
  - **Decision.** The program's own `engine::pool_select::decide` on the market projected to
    the clock (the prologue: `pool_d` refreshed, the `pool_fail` chain stepped), for the deepest
    eligible candidate other than the incumbent and the window's own: `propose_pool` exactly
    when it accepts — a list-only market's first pool, a listing-window replacement (5 %), a
    zero-position adoption (25 %; in the tick the keeper sees the last position closed), the
    fast path's collection window (opened at the first armed second, outbidding a shallower
    window of someone else's); `execute_pool_switch` at the first second after the window (a
    cancelled window is proposed again if the fast path is still armed; a list-only market with
    open positions — M5's curve that graduated elsewhere — collects its candidates in a window
    the same way, M4-SEC-4). A window is opened or improved only with a candidate above the
    window floor (else nothing, `PoolTooThin`). Nothing while positions are open and the
    incumbent is healthy (`PositionsOpen`); a candidate whose own program the `VenueGuard` latch
    holds is left out of the ranking (`VenueUpgraded`; M4-SEC-8: an upgrade of the other Raydium
    program does not hold the fast path up), and a window whose candidate's program is latched
    is executed anyway — the program cancels it (M4V-4) and the best pool on the other program
    opens the next window in the same tick. A failing market **without** positions, which duty 5's no-route cadence
    does not observe, is observed by the monitor (at once, then every 30 s) while a candidate is
    deep enough for the fast path, so its `pool_fail` chain arms. Sends are simulated first, at
    the exit priority ceiling, outside the O17 observation budget.
  - **The marks** (ECON #100). The plan runs the program's own `carried_fit` on the market as
    its view holds it: `max_q_hwm` (falls only when `open_positions` returns to 0) and
    `min_open_fee_ppm` (the lowest fee an open paid since then; the fee-scaled caps take
    `min(min_open_fee_ppm, φ_cand)`, FIT-2). The keeper has no duty to lower the mark: a large
    position that has closed while others stay open keeps a thinner candidate refused
    (`CarriedPositionsTooLarge`) and the positions exit on the failed pool through the S3 chain
    at fair value (FIT-3, a disclosed residual; `pool_select`'s scenario below).
  - **Reports.** The decoded `PoolProposed` / `PoolSwitched` / `PoolSwitchCancelled` of the
    keeper's transactions; `Alert::PoolSwitched`, `PoolWindow`, `PoolSwitchCancelled` for the
    keeper's actions and for anyone else's seen on chain; `Alert::PoolPinned` when a candidate
    with ≥ 25 % more locked depth has waited `pinned_alert_s` (1 h) behind open positions
    (R4B-10: page the Emergency multisig to pause opens there so the market drains);
    `Alert::PoolSelectRefused` for an unexpected refusal. After an adoption the duty-10 admin
    watch starts afresh on the new pool.
- Per-market metrics (coverage seconds, observations, exits, settlements, CU, lamports, misses,
  bursts, gaps, venue admin changes, pool proposals / executions / switches, migrations,
  synthetic exits, transitions, sweeps, cashback claims, exits deferred, coverage pauses and
  unpauses, budget degradations, the `paused_coverage` flag, degraded; on a migrating curve the
  gauges `synthetic_completion_delta_bps` (ρ − 1) and `curve_base_ata_donation`) and
  `venue_flags`, `synthetic_exits`, `curve_format_{probes,failures}`,
  `venue_extensions` (a venue's or Token-2022's ProgramData extended, not upgraded: AIR-01),
  `not_deployed` (refusals of a program deployed in the refused slot: AIR-18),
  `pool_discoveries`, `coverage_pause_failures`, `oracle_{cranks,reads,refusals,lamports}` in the
  Prometheus text format (`--metrics`; each global counter as `shortwin_keeper_<name>_total`).

**The program's own code decides.** The keeper links the `shortwin` crate (account types,
instruction builders, the venue reader — `venues::read_state_boxed_for_exit`, the exits' reader,
which also resolves PumpSwap's fee at `P_ema` for AIR-10's cap — and the adapters' account
lists, the market prologue `engine::prologue::run`, the exit references `engine::bounds`, the
pool selection's `engine::pool_select::{verify_candidate, decide}` and
`venues::detect::classify_pump`), `shortwin-venue-state` (the AMM v4 / CPMM / ProgramData
readers) and `shortwin-math` (legs, CR, buyback test, S3 timer): each decision is the program's
on a copy of the market projected to the cluster's clock. Every send is still simulated first,
and the simulation is the final word (a program refusal is reported, never retried blindly; the
loader's one-slot "Program is not deployed" is told apart by the program's ProgramData header,
AIR-18, [below](#extendprogram-griefing-air-18-accepted-by-the-owner-on-2026-10-12)).

Not yet (later milestones, ARCH §7.1): Jito bundles with tips for liquidations (the priority-fee
RPC path is built; T-SIZE shows a tip transfer fits the worst Raydium exit; `services/common`
keeps the block-engine transport, which no duty uses since the straddle bundle's retirement,
pump-final §7); Yellowstone gRPC subscriptions (the keeper polls `getMultipleAccounts` /
`getProgramAccounts`, and watches admin actions through the accounts they write rather than the
admin's transactions; it sees other parties' pool selection through the market's state, not their
transactions); duty 13's lender pot (`sync_lender`, `sweep_protocol_fees`); building per-market
lookup tables automatically at a listing (`shortwin-build-alt market` builds one on request).

### Run

```sh
cargo build --manifest-path services/Cargo.toml -p shortwin-keeper --release
services/target/release/shortwin-keeper --rpc https://… --keypair keeper.json \
  --config services/keeper/keeper.example.toml --metrics 127.0.0.1:9464 \
  [--discovery-rpc https://…]   # getProgramAccounts for the pool discovery (default: --rpc)
  [--coverage-keypair coverage.json]   # the designated pauser's coverage_authority (O17)
```

The keypair is a hot fee payer with no protocol authority. The `coverage_authority` keypair (O17)
is a second, separate key that can only pause and unpause opens per market; load it on exactly
one running instance, with `[coverage_pause] enabled = true` (the binary refuses a pauser without
the key, and the fee payer as the key). Governance installs its public key with a timelocked
`Authorities` change; until then the instruction is inert and the pauser falls back to paging. `keeper.example.toml` documents every
setting (all optional; the file is the defaults, a unit test checks it). At start the keeper warns
if its `lookup_tables` lack a protocol ALT key.

### Degraded mode: runbook and drill (O17)

**Install the role (once).** Generate the key (`solana-keygen new -o coverage.json`; keep it apart
from the fee payer and from every multisig key). Governance proposes an `Authorities` change with
`coverage_authority` = its public key (the other three authorities unchanged) and applies it after
the timelock (48 h by default, 24 h floor). Until then `set_coverage_pause` fails
`CoverageAuthorityDisabled` whoever signs — except the Emergency vault's clear (OWNER-M7-3 (b)),
which is accepted with the role unset —, and the pauser alerts `CoveragePauseUnavailable
{AuthorityUnset}` instead of pausing.

**Run exactly one pauser.** One instance: `[coverage_pause] enabled = true` and
`--coverage-keypair coverage.json`. Every other instance (another region, a standby): `enabled =
false` and no key — they alert only. A second enabled instance is tolerated (it adopts a pause it
finds, never lifts it early; `coverage_pause::two_instances_never_flip_flop_the_flag`) but pays
for its own failed attempts, and each instance pages the other's sets as `CoveragePauseForeign`.

**What pages.** `Degraded` (a market with open positions lost coverage: an outage over
`COVERAGE_GAP_S` = 40 s, 3 missed required seconds in a row — a tick that came late skips seconds,
which count when the market needed per-second coverage —, or a charge the global budget refused at
its rank) is followed in the same tick by
`CoveragePauseSent{paused: true}` (metric `shortwin_keeper_market_paused_coverage` 1). Page the
Emergency multisig to `set_pause(market)` with reason `KeeperCoverage` only on
`CoveragePauseUnavailable` (no key, role unset, key rotated away) or a repeated
`CoveragePauseFailed`. Opens resume by themselves `recovery_s` (600 s) after coverage returns
(`CoverageRestored`, then `CoveragePauseSent{paused: false}`); a budget-degraded market stays
paused until the UTC day rolls over and funds it again at its rank (or `global_daily_lamports` is
raised: the difference goes to the free pool), then 600 s. Exits are never paused: nothing needs
doing for them.

**Timing (M7-SEC-1, OWNER-M7-12).** The program's `CoverageGap` fires above `COVERAGE_GAP_S` = 40 s
(30 s as built at M7, raised by the owner on 2026-10-01) between two integrations' landing seconds;
the keeper schedules its idle crank from its `confirmed` read, so landing jitter makes 31–32-s gaps
on healthy markets — below 40 s, so the program emits nothing for them and indexers need no
tolerance; every gap it emits degrades (`gap_degrades_above` = max(30 + 4, 40) = 40;
`shortwin_keeper_market_coverage_gaps_tolerated` stays 0 at the defaults). A tick that waits for its confirmations (≈ 1 s)
skips a second: that costs nothing on the idle cadence and is a miss on a market that needed
per-second coverage; only a stall over `tick_gap_tolerance_s` (5 s) counts as the keeper down.

**A foreign clear, set or open (M7-SEC-3; OWNER-M7-3 (b)).** `CoveragePauseCleared` (the flag found
clear while the keeper holds the market degraded and did not clear it), `CoveragePauseForeign` (the
flag turned set without the pauser having sent the set) and `OpenWhileDegraded` (a position opened
while the flag was last seen set) mean the `coverage_authority` key signed outside the pauser (or,
for a clear, the Emergency multisig did): page the Emergency multisig — it can `set_pause(market)`
(a pause this key cannot clear), clear a coverage pause itself and have the key rotated. On a
market the pauser degraded itself it sets the flag again in the same tick; opens wait
`COVERAGE_CLEAR_DELAY_S` (5 s) after any clear, by either signer, so no clear can bracket an open
in one transaction.

**Key lost or compromised (OWNER-M7-3 (b), owner decision of 2026-10-01).** The coverage key and the
**Emergency multisig** (`pause_authority`) can clear a coverage pause; only the key can set one.
A hijacked key: page the Emergency, which clears each market the key paused (one
`set_coverage_pause(market, false)` per market, ≈ 8,000 CU, signed by the Emergency vault; the
role need not be installed) and every later set the same way — the pauser leaves a pause it did
not set cleared — while Governance rotates the key with the timelocked `Authorities` change
(≥ 86,400 s). A lost or rotated key: the Emergency clears the flags the old key left (or the new
pauser recovers each market: a flag it finds set is lifted after 600 s of its own coverage). **To
override a pause of the honest pauser itself, stop it first** (`[coverage_pause] enabled = false`,
or restart it without `--coverage-keypair`), then clear: a running pauser sets its own pause again
while it holds the market degraded. A compromised key can only pause opens.

**Drill** (devnet or a local validator with the program and a market with an open position;
`keeper-litesvm`'s `coverage_pause::drill_*` run the same steps in-process on every CI run):

1. Start the pauser; wait until the market shows `covered_seconds` rising and no alert.
2. Stop the keeper for 45 s, start it again. Expect `Degraded{CoverageGap{gap_s ≥ 46}}` and
   `CoveragePauseSent{paused: true}` in the first tick, and the market's flags bit 6 set.
3. Try an open: it fails `CoveragePaused` (6051). A `close_short`, an `add_margin`, a lender
   `withdraw` and `claim_sol` land; the keeper's liquidations and settlements continue.
4. After 600 s: `CoverageRestored`, `CoveragePauseSent{paused: false}`, bit 6 clear; an open lands
   5 s later (`COVERAGE_CLEAR_DELAY_S`; within them it fails `CoveragePaused`).
5. Repeat with a small `[budget] global_daily_lamports` (e.g. 10,000) to see `BudgetExhausted` →
   `Degraded{Budget}` → the pause in the same tick; restore the budget and restart to lift it.
6. The Emergency's clear (OWNER-M7-3 (b)): with the pauser running, set a market's flag with the
   coverage key from outside it — expect `CoveragePauseForeign` —, then sign
   `set_coverage_pause(market, false)` with the Emergency vault: `CoveragePauseCleared`, the flag
   stays clear, opens land 5 s later. On a market the pauser itself paused the flag is set again;
   stop the pauser first to override it.

## The protocol ALT (`shortwin-build-alt`, `scripts/build-alt.sh`)

ARCH §6.4 A28: one address lookup table with the protocol's static accounts, used by every v0
transaction (the keeper's v0 fallback, the frontend). The list (`keeper/src/alt.rs`), in order:

1. the programs a short.win instruction names as accounts (pump, pAMM, pfee, AMM v4, CPMM, SPL
   Token, Token-2022, ATA, System) and short.win's event authority; the WSOL mint; pump `global`,
   `event_authority`, `fee_config`, global volume accumulator, and the same four of PumpSwap; the
   AMM v4 and CPMM vault authorities; the 21 CPMM `AmmConfig`s; short.win's `config`,
   `venue_guard`, `oracle_cache`;
2. the four `VenueExtras` PDAs (an addition to ARCH's enumeration: static protocol accounts that
   every trading instruction with an extras set names);
3. with `--rpc`: the live pump and PumpSwap fee and buyback recipients (`Global` @41, @162, @741;
   `GlobalConfig` @57, @643) and their WSOL ATAs — 17 distinct recipients today.

83 addresses on mainnet (2026-09-28). ComputeBudget and short.win's own id stay static keys (a v0
message cannot load a program id). No sysvar appears: no instruction on these paths takes one.

```sh
scripts/build-alt.sh print                                   # the static part, no RPC
scripts/build-alt.sh print  --rpc https://api.mainnet-beta.solana.com
scripts/build-alt.sh create --rpc <url> --keypair payer.json [--authority auth.json] [--freeze]
scripts/build-alt.sh extend --rpc <url> --keypair payer.json --table <alt>   # add what is missing
scripts/build-alt.sh check  --rpc <url> --table <alt>        # exit 1 if an address is missing
scripts/build-alt.sh market --rpc <url> --market <market> [--table <alt>] [--keypair payer.json]
```

**Per-market tables (M7, OWNER-M7-1's fallback):** `market` lists a market's own table
(`alt::market_entries`: the market, its mint, vaults and trader, the venue's ReadSet and trade
extras, the keys its `current` extras sets resolve to — less the protocol table's addresses) and
its rent, and with `--keypair` creates it, extendable (never frozen). 3,698,240 lamports for 17
addresses, 162,560 per address (mainnet's 5,080 lamports per byte, ARCH V5). Measured (`tests/bench/m7_keeper/tsize_pumpswap.md`): the PumpSwap
open with the largest (4-entry) extras set is over 1,232 B through A28 as ARCH §6.4 enumerates it
and fits through the table `create` builds (with the `VenueExtras` PDAs) and, with far more
room, through that table plus the market's; a table per market also lifts the v0 crank batch
(`tests/bench/m7_keeper/results.md`).

`create` derives the table from the authority and the latest finalized slot, extends it in chunks
of 24 and optionally freezes it. A frozen table can never be deactivated (a deactivated table
breaks every v0 transaction naming it) but also never extended: a new pump recipient then rides
inline (31 bytes) until a new table replaces it. Entries are usable one slot after they are
added. Put the address in the keeper's `lookup_tables`.

## Governance (`shortwin-gov`, M10)

ARCH §1.1 "Admin", §8.5; owner decisions 2026-10-02 (Q4). The layout file
(`ops/governance.example.toml`) names **public keys only** — the members, the two create keys, the
keeper's `coverage_authority` — and the launch values `initialize_config` takes (program timelock,
venues, Pyth feed, the venue programs' deploy slots). Everything else is derived and never typed:
Governance (autonomous 3-of-5, Squads time lock 172,800 s) and Emergency (autonomous 2-of-4, time
lock 0) are the Squads PDAs of their create keys; `governance` and the upgrade authority of
short.win (Governance's whole upgrade set: the straddle helper `complete_curve` was retired
before it was deployed, pump-final D2, and a layout naming it is refused) = Governance vault 0, `fee_recipient` = Governance vault 1,
`pause_authority` = Emergency vault 0; each multisig's rent collector is its own vault 0. The
four authorities must be pairwise distinct (SQ-G8). The example's keys are placeholders and the
tool refuses to send with it (`example = true`).

```sh
G="cargo run --release --manifest-path services/Cargo.toml -p shortwin-ops --bin shortwin-gov --"
$G verify                    --config gov.toml --rpc <url>     # every PDA / authority; exit 1 on a FAIL
$G create-multisigs          --config gov.toml --rpc <url> --payer p.json --gov-create-key g.json --emg-create-key e.json
$G init-config               --config gov.toml --rpc <url> --authority deployer.json
$G stage-upgrade-authority   --config gov.toml --rpc <url> --checked --key member.json
$G stage-authorities         --config gov.toml --rpc <url> --key member.json     # → the file's authorities
$G stage-upgrade             --config gov.toml --rpc <url> --key member.json --buffer <buffer> [--spill <addr>]
$G stage set-pause global on --config gov.toml --rpc <url> --key emg-member.json  # any admin ix (--help)
$G approve --ms governance --index 3 --config gov.toml --rpc <url> --key member.json  [--execute]
$G execute --ms governance --index 3 --config gov.toml --rpc <url> --key member.json  [--co-signer deployer.json]
$G cancel  --ms governance --index 3 --config gov.toml --rpc <url> --key member.json
$G apply   --config gov.toml --rpc <url> --key payer.json        # apply_config after short.win's timelock
$G status  --config gov.toml --rpc <url> [--ms governance --index 3]
$G close-buffer --ms emergency --config gov.toml --rpc <url> --key member.json [--buffer-index 0]
```

- **Keys** are keypair files named on the command line, read only when a command signs, never
  printed (a file that does not parse is reported by path only; a group- or world-readable file
  draws a warning). Hardware-wallet paths (`usb://…`) are refused: the remote-wallet stack is not
  linked (M11 decides); Ledger members approve and execute in the Squads app, after checking the
  transaction with `status --ms … --index …`.
- **Every transaction** is legacy, paid by the acting key (no separate fee payer), at most 1,232
  bytes, and **simulated first**: one that would fail is not sent, and the error is named by
  program and Anchor name (a Squads `TimeLockNotReleased` and a short.win `TimelockNotElapsed` are
  told apart even through `vault_transaction_execute`). `--dry-run` simulates and sends nothing
  (a command of several transactions stops after its first). A sent transaction is awaited for
  90 s (`--confirm-timeout <s>`), past its blockhash's lifetime, so one reported as not
  confirmed has expired; the error says to check `status` before rerunning. There is no priority
  fee yet, and no compute-budget instruction: a transaction gets the runtime's default limit,
  200,000 CU per instruction of a non-builtin program such as Squads (a bare `execute` 200,000,
  `approve --execute` 400,000; see the revalidation after an extension, below).
- **Staging** is one transaction (`vault_transaction_create` + `proposal_create` + the stager's
  approval). A message above about 700 bytes (the transaction would exceed 1,232 B; T-SQ-3's
  worst-case `set_venue_extras` is 1,243 B staged directly) goes through the stager's
  transaction buffer instead (create, extends, `vault_transaction_create_from_buffer`);
  `--force-buffer` always does. A buffer an interrupted run left holding the start of the same
  message is **resumed** (only the missing bytes are sent); one holding anything else is refused,
  and `close-buffer` closes it and returns its rent. A staging whose message the multisig's
  newest transaction already holds, still open, is refused (a rerun after a send that landed
  late; `--allow-duplicate` overrides). `status`, `approve` and `execute` print what the
  transaction runs, decoded: short.win instruction names with their arguments and accounts
  (scope and market, venue bit and name, extras entries; every value of a `ConfigChange`,
  `feed_id` in hex, and for `Params` / `EmaParams` / `Kink` what moves against the live config),
  loader and System instructions, and any other program's data with every account and its
  flags; `execute`
  rebuilds the accounts from the **stored** transaction and demands `--co-signer` for any other
  signer of the message (the deployer in the checked transfer). `approve --execute` joins the
  execution to the threshold approval when the time lock is 0: an Emergency action is two
  transactions.
- **Refused before anything is sent:** the unchecked upgrade-authority transfer
  (`--checked` is required; T-SQ-5's typo hazard), `stage-upgrade` while the program's upgrade
  authority is not yet Governance vault 0, an upgrade buffer the vault does not own (the
  loader would refuse either only after the 48 h time lock), an ELF that does not fit the ProgramData
  (the `solana program extend` to run first), `init-config` by anything but the upgrade
  authority or with a guard slot that differs from the live ProgramData, `stage-authorities`
  when the config already matches, a non-member key, `propose-*` by the Emergency and the
  Emergency-only powers by Governance.
- **`verify`** FAILs: Squads not executable or not immutable; a multisig missing, not autonomous,
  with another threshold, time lock, member set, partial permissions or rent collector; the fee
  vault unfunded or not a System account; `GlobalConfig` missing, or any of its four authorities
  or `timelock_s` not the layout's, or the authorities not distinct; an upgrade authority that is
  not Governance vault 0. It WARNs on a pending config change, on Active or Approved proposals
  at every index of both multisigs (an Approved vault transaction never expires: `cancel` it) and
  on `venue_enabled` differing from the launch set.
- **Not supported:** moving `governance` itself to a new multisig (the tool stages through the
  layout's two multisigs only; a layout naming a new Governance no longer names the current one),
  Squads config transactions (members, threshold, time lock), `Params` / `EmaParams` proposals,
  address lookup tables (M11).

Tests: unit tests per module, and `ops/tests/gov_e2e.rs` (T-OPS-1/2): the whole sequence through
the command line on the mainnet Squads ELF, every transaction replayed through the program
harness's `Env::send` ([`tests/litesvm/README.md`](../tests/litesvm/README.md)).

### Venue fingerprints after `init-config` (AIR-01)

`initialize_config` records each venue program's deploy slot in the `VenueGuard` latch, but no
ELF fingerprint, and without one the slot alone decides. The loader's `ExtendProgram`, which
anyone can send, moves that slot without touching the ELF, so one extension lets anyone latch
the venue and stop its opens until the Emergency revalidates it (known-issues KI-69). **Right
after `init-config`, the Emergency runs one `revalidate_venue` with the expected ELF hash for
each venue whose ProgramData can still grow.** pump (index 0) and PumpSwap (1) sit at the
loader's 10 MiB cap, no extension can move their slot, and they keep the slot-only rule.

| Venue | `VenueGuard` index |
|---|---|
| pump-fees | 2 |
| Raydium AMM v4 | 3 |
| Raydium CPMM | 4 |
| Jupiter | 5 |

```sh
$G stage revalidate-venue 2 <deploy-slot> --config gov.toml --rpc <url> --key emg-member.json
$G status  --ms emergency --index <n> --config gov.toml --rpc <url>
$G approve --ms emergency --index <n> --config gov.toml --rpc <url> --key emg-member2.json --execute
# then indices 3, 4 and 5 the same way
```

- `stage revalidate-venue <index> <slot>` reads the venue's ProgramData and refuses unless its
  deploy slot is `<slot>`. It prints that slot and the sha256 of the account's trimmed ELF (the
  bytes after the 45-byte header without the zero tail: what `solana-verify get-program-hash`
  prints), and stages `revalidate_venue(index, slot, hash)`. Before approving, compare all 64
  hex digits with the trimmed ELF the fork suites ran on. The full hashes are pinned in
  [`scripts/dump-fixtures.py`](../scripts/dump-fixtures.py) (AMM v4 in `PROGRAMS`, CPMM in
  `CPMM_PROGRAMS_2026_10`, pump-fees in `PUMP_PROGRAMS_2026_10_13` since the 2026-10-13 re-pin:
  its 2026-10-12 build) and recorded as `elf_trimmed_sha256` in
  [`tests/fixtures/manifest.json`](../tests/fixtures/manifest.json)
  (`raydium_amm_v4_program_data`, `raydium_cpmm_2026_10_program_data`,
  `pump_fees_2026_10_12_program_data`; the latch baseline is `VENUE_GUARD_SLOTS_2026_10_13`).
  [`tests/fixtures/README.md`](../tests/fixtures/README.md) shortens them to 12 digits and gives
  none for AMM v4. **pump-fees' 2026-10-12 build is not revalidated on mainnet before R-PFEE**
  (pump-final §3.2, PUMP-2026-10-13-5: `withdraw`'s accounts and authority read from a mainnet
  transaction or the ELF and recorded in the audit addendum; stop if it can write an account
  short.win's legs pass or a `FeeConfig` short.win reads). The execution records the account's
  length and the hash's first 12 bytes. From then on a pure extension of that venue is
  `VenueNotUpgraded`; a moved deploy slot latches it only if the account kept the recorded
  length (an `Upgrade`, even of the same ELF) or its trimmed ELF no longer matches or is above
  2 MiB, which is never hashed (KI-69).
- **Jupiter (index 5)** has no fixture, no pinned hash and no fork suite, so there is no
  reviewed build to compare the printed hash with: it fingerprints the ELF of the deploy staged
  (`dump-fixtures.py check --live` compares that deploy slot with the newest
  `VENUE_GUARD_SLOTS_YYYY_MM_DD`). Its ELF size and the hash path's cost on it are not
  measured: size the compute-unit limit of an execution after an extension by simulation
  (≤ 1.4 M). A trimmed ELF above 2 MiB is never hashed: its revalidation then needs the exact
  slot, and an extension still latches it (KI-69). In v1.0 Jupiter's bit is in no market's
  latch mask, so its latch blocks nothing yet.
- **After an extension, request ≥ 0.6 M CU, or size the limit by simulation.** Executed at the
  staged slot, the revalidation hashes nothing (a few thousand CU). If the venue was extended
  after the staging, its deploy slot has moved: the program then hashes the trimmed ELF and
  accepts the later slot only if it is still the named one, which costs up to ≈ 0.44 M CU on
  its own (measured: pump-fees 438,358 and CPMM 369,393 after one 10 KiB extension, AMM v4
  210,386 with a 9.7 MB zero tail), before Squads' own cost. The scan for the ELF's end in the
  zero tail adds ≈ 1 CU per 250 B, so pump-fees grown near the 10 MiB cap by a stranger's
  extensions (KI-73; [below](#extendprogram-griefing-air-18-accepted-by-the-owner-on-2026-10-12))
  needs ≈ 0.48 M (estimated with the tests' cost model, not measured). Execute such a
  revalidation with a compute-unit limit of at least 600,000 (`SetComputeUnitLimit` before
  `vault_transaction_execute`). `shortwin-gov` sets no limit, so it gets the runtime's default:
  200,000 CU for a bare `execute`, 400,000 for `approve --execute` (two Squads instructions,
  the approval's cost included). pump-fees exceeds both and AMM v4 exceeds a bare `execute`;
  whether CPMM, or AMM v4 under `approve --execute`, fits depends on Squads' cost, which is not
  measured. A transaction whose simulation runs out of compute units is not sent.
  - **Reliable:** execute from a client that sets the limit.
  - **Fallback:** stage a new revalidation at the current deploy slot (the same ELF, so the same
    hash; at the exact slot nothing is hashed). Anyone can defeat it again by extending the
    venue before that execution, at ≈ 0.052 SOL a round (KI-73), which puts it back on the
    hash path.
- Every later revalidation after a venue upgrade works the same way: stage it at the deploy slot
  the fork suite ran on, and check the printed hash against the reviewed ELF. Anyone's
  `flag_venue_upgraded` on an extended venue with a fingerprint hashes the trimmed ELF too and
  needs the same compute budget.

### `ExtendProgram` griefing (AIR-18; accepted by the owner on 2026-10-12)

Anyone can send the loader's `ExtendProgram` for a program whose ProgramData is below the 10 MiB
cap: at least 10 KiB, once per program per slot. It redeploys the program, which is unusable for
the rest of that slot: every invoke or CPI of it fails with `UnsupportedProgramId` ("Program is
not deployed"). The owner accepted this bounded griefing (known-issues KI-73: ≈ 0.052 SOL of the
attacker's rent per blocked slot until the program reaches the cap, ≈ 45 SOL for short.win at
its v1.0.0 ELF size), so short.win's ProgramData is **not** pre-extended to 10 MiB at
deployment (the retired `complete_curve` helper is not deployed at all, pump-final D2). What
operators see:

- **A venue extended** (pump-fees, which pump and PumpSwap call; AMM v4; CPMM; Token-2022; pump
  and PumpSwap themselves are at the cap): every exit that buys on it fails for the rest of that
  slot. The keeper's `liquidate` or `expire` fails in simulation or on chain with
  `UnsupportedProgramId`. The keeper reads the named program's ProgramData header as of the
  refused slot; when it shows a deploy in that slot (two slots of slack) it counts the refusal
  (`shortwin_keeper_not_deployed_total`) and sends again at its next tick, with no alert or
  back-off. `crank_price` and `sol_settle` make no venue CPI and keep landing.
- **short.win extended:** every short.win instruction fails for the rest of the slot (the
  keeper's cranks and exits, users' transactions), and the keeper treats its refusals the same
  way.
- **No action is needed** unless `Alert::ProgramNotDeployed` pages. The keeper ticks every
  400 ms by default (`--tick-ms 400`, about one slot), so its next tick is always in a later
  slot; each further blocked slot costs the attacker another extension, and the griefing ends
  for good when the program reaches the cap (≈ 867 slots for short.win, ≈ 887–947 for the
  venues at their current sizes: ≈ 5.8–6.3 min of slots at 400 ms).
- **`Alert::ProgramNotDeployed`:** such refusals of one program, at most 5 s apart, have lasted
  30 s (paged once per run). Someone is extending that program every slot or nearly: check its
  ProgramData's growth toward the 10 MiB cap and whether exits land in between.
- **A program that stays undeployable** (an old deploy slot, or no ProgramData: closed, or
  failing verification after a feature gate) gets the same answer from the loader. Its refusals,
  and those whose header cannot be read, are handled as any refusal: `Alert::ExitRefused`,
  `VenueFlagFailed` and so on.
- The extension also moves the deploy slot. On a venue with a recorded fingerprint it does not
  latch (above).

What the keeper does (AIR-18, `8945087`):

- The refused slot is the outcome's own: the simulation's context slot, or the failed
  transaction's slot. The tick's slot is used only when the backend gives none.
- The header is read as of that slot (`minContextSlot`), so a node of an RPC pool still behind
  refuses the read instead of answering stale. The read is tried twice, at most once per
  program and refused slot in a tick.
- A refused crank batch defers the rest of the queue to the next tick. A flag refused this way
  is sent again the next tick; for an AIR-01 slot the account is read whole after the exits, so
  an upgrade is paged at once, without `VenueFlagFailed` (`a1edf78`).

The upgrade step stays: **extend ProgramData before staging an upgrade to a larger ELF.**
`stage-upgrade` refuses an ELF that does not fit and names the `solana program extend <program>
<bytes>` to run first. That extension, like anyone's, blocks the program for the rest of its slot.

### Disabling a venue (AIR-24)

`stage disable-venue <venue>` (Emergency) also cancels a pending `EnableVenues` change whose bits
include that venue's, as `cancel_config` would (`ConfigCancelled`): the whole change, so
Governance proposes its other bits again, and the venue comes back only through a new proposal
and its full timelock.

## Tests

```sh
cargo test --manifest-path services/Cargo.toml --workspace
cargo test --manifest-path services/Cargo.toml -p shortwin-ops --test gov_e2e -- --nocapture   # T-OPS: shortwin-gov end to end
```

The scenario tests (`keeper-litesvm/tests/scenarios`) load the same builds as the program tests:
short.win's test-venue build (`scripts/build-test-venue.sh`, or `SHORTWIN_TEST_VENUE_SO`) and the
mock venue (`anchor build --tools-version v1.57 --arch v3` → `target/deploy/mock_cpamm.so`, or
`MOCK_CPAMM_SO`) for the mock scenarios, and the release build (`target/deploy/shortwin.so`, or
`SHORTWIN_SO`) with the committed mainnet fixtures (`tests/fixtures`) for the Raydium and pump
ones; `upgrade_window` starts from shortwin 1.0.0 (`tests/fixtures/shortwin/shortwin_1_0_0.so`,
checked against AUDIT §0's hashes) and upgrades it to the release build by the loader. Every
keeper transaction runs the harness's invariant hooks after every instruction.

M7 scenarios: `coverage_pause` (the T-COVERAGE drill — an outage and an exhausted budget pause
opens through `set_coverage_pause` in the same tick, exits and lender flows land under the pause,
the pause lifts 600 s after coverage returns; a restart, the role unset or rotated, two instances,
the role through the real timelock; since 2026-10-01 the Emergency's clear of a hijacked key's
pause, left clear, and of the pauser's own, set again until the pauser is stopped), `budget` (the
global budget, decision A: a burst storm on one market spends its entitlement and the free pool and
nothing else, junk below the floor gets the idle cadence only and stops at the tail, a junk market
above the floor preempts the lowest-ranked first, exhaustion degrades the lowest-ranked first),
`m7d` (the verification of decision A, fixed: below the floor an expiry and a liquidation land at
the first second the program accepts, after an outside cranker stops and on the keeper's idle
cranks alone, a cascade whose value at risk crosses the floor is liquidated — M7D-1 —; the crank a
below-floor exit waits on is exit-critical, and junk filling the tail cannot stop an honest expiry
— M7D-2 —; a burst on a market near a threshold charges one exit-critical crank a second — M7D-4
—; a keeper start funds its covered markets at the batched estimate — M7D-5),
`degraded`, `oracle` (duty 11: aging alerted at 301 s, Tier C from the first crank after 600 s; a
second stall within the hour alerted again before Tier C — M7D-6), `cascade` (T-CASCADE:
`M7_CASCADE_WRITE=1` writes `tests/bench/m7_cascade/results.{md,json}`), `m7_cost` (the ECON §5.8
cost-model inputs per venue, batch capacities, cadence, load: `M7_COST_WRITE=1` writes
`tests/bench/m7_keeper/results.{md,json}`), `pamm_alt` (OWNER-M7-1: the PumpSwap 4-extras
transactions through tables the keeper's `alt` builds on chain; `M7_COST_WRITE=1` writes
`tests/bench/m7_keeper/tsize_pumpswap.{md,json}`). The measurement tests run in release in CI's
`m7-keeper-bench` job (`--release … -- --include-ignored`); see `tests/bench/m7_keeper/README.md`.

Mock scenarios: cadence (30 s idle, per second near a threshold), batching (v1 6 + 1, v0 4 + 3
after a v1 refusal, the protocol ALT, a failing crank dropped and sent alone), bursts (an atomic
pump-crank-dump → 60 s per slot; honest observations start none), degraded mode (outage →
`CoverageGap` → restored after 10 minutes; an exhausted budget), priority (p75, the ceiling, the
fee charged), market selection and dry run, and the exits: a trend and a jump liquidated at the
first second an outside oracle's simulation accepts, a spike above the bound waited out
(`ExitBoundNotMet`) and liquidated the next second, an expiry at the deadline, S3 on a disabled
mock pool (continuous chain; an expired position settles, a healthy one stays frozen), S1 after
arming with one `sol_settle` per second, and extras (a published template, a hostile `current`
bypassed with `previous`, a `current` the program denies — a borrower's wallet, the mock pool —
bypassed with `none`). AIR-19 (`s3::air19_*`): with one position at the 500-bps cap and one
opened after Governance raised it to 1,000 bps, an 8 % fee starts the market's chain (its mark;
the global cap alone would not) and settles the first in S3 once expired, while the second is
never S3-due and expires.

Pump-curve and graduation scenarios (`graduation.rs`, duty 6, M5 and M6; the dumped pump,
pump-fees and PumpSwap programs and the near-complete curve): an outside buy completes a covered
market's curve and the keeper migrates it at its next tick (its `migrate_v2` is the harness's own
derivation) and moves the market onto its pool in the same second (the pool accounts created,
`→ PumpSwap`, the pool's first bucket), then observes its warm-up every second (no positions), once;
with `migrate` off a third party's migration is still moved onto the pool; a graduation with a
position open: the keeper sends no exit during the warm-up and liquidates the position on the
pool in the first second an outside key's simulation passes (+16 s after a ×3 pump right after the
transition); ★ pump-final §7: on a D@80 % curve bought down below two positions' `Q`, the
keeper's `liquidate` of the first completes the curve by pump's synthetic migration (the v3 list,
pump's pool part `q − rT`), the keeper migrates and transitions the market in the same tick, and
the second position is liquidated on the pool at `T + 8`; the same with two expired positions on
`near166` — the curve bought down by an outsider, or by the borrower itself (`rT − q + 1`,
M5-SEC-4) — the other expired on the pool at `T + 8` exactly, an outside key's `expire` refused
`EmaWarmingUp` at every second before; a position past `rT` whose `σ_max` is below σ(`B̂_v3`)
plans `SolSettle` and settles S2, no buyback sent; a third party's synthetic completion between
the keeper's simulation and its expiry: the expiry fails on chain (`VenueMigrating`, nothing
written), the next tick graduates the market and the position expires on the pool at the gate;
duty 10 alerts a live curve carrying post-complete fields or breaking `vQ − rQ = ivqr` and a
migrating curve with unknown post-complete fields, once each, and reports a migrating curve's
`ρ − 1`; below `rT` (`Q ≤ rT`) the keeper expires a curve position on the curve with the v3
list; a curve migrated elsewhere with an expired position open goes `→
None` at the keeper's first crank, which keeps the S3 chain on the no-route cadence and settles
the position in S3 exactly when the chain has run `s3_wait_s` (on the detection set); a list-only
market whose curve becomes readable is cranked onto it, its trader accounts are created once, and
an open lands.

PumpSwap scenarios (`pamm.rs`, M6; the `pamm_state` pools): a listing nobody observes has its
price started and its warm-up observed by the keeper (eight seconds), the keeper's PumpSwap lists
are the harness's for the same recipients, and a ×3 pump is liquidated on the pool in the first
second the program accepts it; `disable_flags` bit 0 is reported (severe; opens refuse
`VenueFlagged`), the buy bit as a lost route as well, clearing both as restored; a cashback coin
listed without the trader's cashback ATA gets it created once; a graduated coin listed list-only is
cranked onto its pool (`None → PumpSwap`), its trader base ATA and pAMM UVA are created once, and
an open lands; AIR-10 (`pamm::air10_*`): with the live market cap in the 50-bps tier and `P_ema`'s
in the 30-bps one, the program refuses a liquidation (`NotLiquidatable`) and the keeper's
projection sends none, while under a uniform 50-bps schedule both liquidate it.

PumpSwap third-party LP (`pamm_lp.rs`, M6R-1, the owner's decision of 2026-09-30; the `legacy261`
pool with as much third-party LP again, λ 50 %, pAMM's own `deposit` / `withdraw`): duty 2's
liquidity coverage against a withdraw drag (`[withdraw all, crank_price, deposit]` after each
keeper tick, from the first second its idle cadence skips) on a market with a position — the
first wrap is the only attacker-only second, `L_ema` holds at 0.905 × `√k` (the drag's one step),
the keeper observing every later second at one crank per wrap (119 for 119); with
`liq_cover_s = 0` the same drag pulls `L_ema` to the locked depth (0.500) — and none on a market
without positions (no trigger, the idle cadence kept).

Trader-side scenarios (`wipe.rs`, M6): a full wipe on a PumpSwap cashback coin — the keeper's S1,
then (with the duty on) `[create_idempotent ATA(trader, WSOL), pAMM claim_cashback, sweep_trader]`
in the same second: the cashback and the ATA's rent reach the wiped epoch's record, both bits, the
deposit refused before lands, and an open lands; a non-cashback PumpSwap market's post-wipe
transaction is `sweep_trader` alone; a legacy cashback curve's is `[pump claim_cashback,
sweep_trader]`; on the mock a donated `ATA(trader, WSOL)` and lamports on the trader PDA are swept
within the check interval, once; the weekly PumpSwap cashback claim runs once per interval;
M6-SEC-2: on a curve market graduated by third parties only (no pAMM UVA), fully wiped, whose
`ATA(UVA, WSOL)` a stranger funded, the pre-fix transaction fails 3012 and, with PumpSwap's
`GlobalConfig` unreadable (no account preparation), the keeper's post-wipe transaction creates the
UVA itself and lands.

Raydium scenarios (`raydium*.rs`, the dumped AMM v4 and CPMM programs and pools):

- `raydium`: six markets (one AMM v4, five CPMM: SPL and Token-2022 bases, WSOL as token 0 and 1,
  creator fees) in one v1 crank batch; after a v1 refusal, v0 batches packed by size; a whale's
  real buy on AMM v4 and CPMM liquidated at the first second an outside oracle accepts; healthy
  positions on AMM v4 and a Token-2022 CPMM pool expired at their deadlines.
- `raydium_admin` (the real admin instructions, signed as `GThUX1…` with signature checks off for
  that transaction): an AMM v4 `SetParams` halt reported at once, the S3 chain started that tick
  and kept continuous, the expired position settled in S3 at `max(deadline, chain + 600 s)`, the
  healthy one frozen, the resume reported; fee changes on both programs against the caps (opens
  refused between them; above the hard cap the S3 chain starts while the venue still executes);
  CPMM status bits (swap = lost route, deposit = status only); upgrades of either program flagged
  in the slot they are seen, once, refreshed during review, nothing after `revalidate_venue`, not
  repeated when someone else flagged first; Token-2022 alerted only; the watch switched off.
  AIR-01 (`air01_*`, AMM v4 revalidated with its fingerprint): a stranger's extensions are
  neither flagged nor paged (the flag's simulation answers `VenueNotUpgraded`, once per slot,
  no whole read) and an upgrade after them is; the upgrade's flag is the tick's first
  transaction, before its crank, and the whole read comes after both; a keeper restarted during
  a review records the deploy under review and pages no extension of it, only a second upgrade;
  a slot the flag's simulation cannot judge is judged on the account read whole (an extension
  counted; a failed read paged as an upgrade, fail-safe); an upgrade whose flag a node behind
  the header read simulates (`VenueNotUpgraded` on an earlier context slot) is not booked as an
  extension but paged once, with the flag's failure, after the exits and flagged the next slot;
  AMM v4 and CPMM extended in one slot are simulated together (one priority-fee read, one
  blockhash read, two simulations); Token-2022 is paged for an upgrade, not for an extension.
  Seven scenarios.
- `raydium_alt`: the protocol ALT created, extended with the recipients (from the committed
  mainnet dumps), frozen, through the keeper's own chain backend and `alt` module; it holds every
  address the program tests' T-SIZE table assumes; the keeper's v0 crank batch and a liquidation
  run through it.
- `pool_select` (M4; planted sibling pools, `shortwin_litesvm_tests::pool_select`, traded by the
  dumped ELFs): a thin eligible listing (Fartcoin's reversed pool) replaced by the main pool in
  the keeper's first tick, inside the listing window; a list-only market moved onto its deepest
  eligible pool (Fartcoin's real curve: D1 and `graduated_elsewhere`; a thinner CPMM sibling and
  the too-thin reversed pool passed over), a pump coin's list-only market never moved onto a
  Raydium pool (D1; since M5 the keeper cranks it onto its live curve instead); an
  incumbent halted with two positions open — the window opened on the deepest sibling at the
  first armed second (`pool_fail` for 120 s), executed at the first second after it, positions
  byte for byte unchanged, `P_ema` carried at the adoption, the ring's liquidity the new pool's
  anchor, the new pool observed every second through the post-change warm-up and its depth,
  a clear guard and a ≤ 1 % box after it, the positions closed on the new pool; underwater
  positions liquidated at adoption + 9 s with no exit sent during the warm-up (M4R); a front-running
  outsider's shallower window outbid and the deepest pool adopted; a deeper pool created during
  the window found by the fast discovery, proposed and adopted; a halted pool without
  positions taken through the fast path by the keeper's own chain observations; nothing
  proposed while a position is open and the incumbent healthy (twice the depth waiting), one
  `PoolPinned` alert, the adoption in the tick the position closed; proposals held by a flagged
  Raydium upgrade until `revalidate_venue`; M4V-5's listing-window observations (60 observations
  60 s apart and 10 in the last 10 s, the first open after the window lands; a window nobody
  observed is warmed up in the 8 s after it, the open refused `EmaWarmingUp` until then); FIT-3
  (#100, a disclosed residual): a position opened at the incumbent's size cap and closed at once
  leaves `max_q_hwm` above a 40 % sibling's cap while two fitting positions stay open — at the
  halt (900 s before their deadline) the keeper's plan is blocked `CarriedPositionsTooLarge`, it
  proposes nothing, and it settles both in S3 at fair value at their due second
  `max(deadline, no_route_since + s3_wait_s)` (lenders `FV` in full).
- `freeze` (M4F-1, M4G-1, M4G-3 after "switch only if all fit"; the M4X-2 shape: a 2,500-SOL λ 50 %
  AMM v4 incumbent halts with a position open, a 320-SOL λ 99.78 % CPMM sibling is the candidate):
  a position at the incumbent's size cap (≈ 3.1 % of the sibling's `X`, above its cap) is **not**
  adopted — the keeper's plan reports `CarriedPositionsTooLarge` and it proposes nothing (neither
  the CPMM sibling nor a 400-SOL AMM v4 one), a direct proposal is refused before and after an
  LP's held 7× deposit in the sibling, and the keeper settles the position in S3 at fair value at
  its deadline (lenders `FV` in full); a position that fits the sibling (95 % of its cap) is
  adopted and, under an attacker's `[deposit 7·LP, crank_price, withdraw]` after the keeper in
  every second, then a genuine 4× ramp: from the adoption the keeper observes every second for as
  long as the guard is set (the lows stay the pool's depth); from +40 s, after the honest guard
  clear, nothing starts a per-slot burst but the foreign liquidity keeps the keeper per second from
  the first wrap (FITS-2 (b)); with keeper misses at +60 / +80 / +100 s (M4G-1) and with a real
  deposit held 600 s and pulled at the pump (M4G-3) too — every run is liquidated at the
  control's second relative to the ramp (the pushes only delay; here by 0 s). #100: at a **1 %**
  sibling's fit limit taken at its own fee (`q` at 76 % of its impact cap, 1.87 % of `X`) the same
  held deposit, freeze and misses with a 4×/30 s ramp ended in S1 (FIT-2); the carried fit's
  fee-scaled caps take `min(min_open_fee_ppm, φ_cand, CARRIED_FEE_CAP_PPM)`, so that position is
  **not adopted** (the plan blocked, a direct proposal refused, no window) and settles in S3 at
  fair value at its due second, and a position at the new fit limit (95 % of the 25-bps class,
  0.75 % of `X₀`) is adopted and liquidated under every push (`fit2_*`; knobs `FIT_FEE`,
  `FIT_PM`, `FIT_RAMP`, `FIT_W`, `FIT_K`, `FIT_ATTACK`, `FIT_SLACK`); FITS-1: a **1 % → 1 %**
  switch (both pools on one 1 % config) at 60 % of the old fit limit (1.48 % of `X`, S1 under the
  pushes before the 25-bps ceiling) is not adopted and settles in S3 at fair value, and one at
  95 % of the 25-bps carried cap is liquidated under every push (`fits1_*`; `FITS_PM`); a position
  carried through a dump-wrapped proposal (FIT-1) is liquidated under them too (`fit1_*`);
  FITV-2 / FITS-2, the withdraw contest (an unlocked LP of 100 % of the counter on a 160-SOL
  λ 99.78 % 1 % pool, λ ≈ 49.9 %): the program-level sweep — around the honest threshold a
  `[withdraw all, liquidate, deposit]` is accepted only where the honest `liquidate` is (refused
  `NotLiquidatable` in the retreat's band; on a build with the spot leg at `L_live` it was
  accepted there), and the I18 spot-leg hook fires on a mutated event; the verifier's
  small-position contest (500 ‰ of the cap, 2.025×) and 25-minute drag (the cap, 1,500 s, 2.05×),
  each in the reproducer's timing and the adversarial one (the attacker after the keeper, from a
  second its idle cadence skips): no forced liquidation, the keeper per second through the drag at
  one crank per wrapped crank (1,859 for 1,859 over the 1,500-s drag); the residual, pinned: in
  the adversarial timing the first wrap's second is the attacker's (`L_ema` 0.917 × `√(X·Y)`,
  then clamped), which leaves a band of ≈ 0.1 % of price under the honest threshold (forced at a
  2.038× top, the honest keeper liquidating at 2.040×); a genuine withdrawal of the LP's half of
  the depth 10 s before, at and 40 s into a pump, and 90 s into a flat 2.03× hold, is liquidated at
  the old rule's second (0 s delay at 500 ‰: the EMA leg's own lag binds; at the cap the exit
  bound, not the spot leg, waits: `L_b`'s `L_slow` credit) (`fits2_*`, `fitv2_*`; knobs `FITV_*`,
  `FITS_*`); FITX-1 (ECON #102), the pinned residual of those drags: the coverage holds `L_ema`
  but cannot lift it, so an LP that first withdraws for real for 20 s (nothing foreign: the keeper
  reads the same depth), re-deposits and then withdraw-wraps a crank every second forces the
  500 ‰ position at 2.025× (+71 s), the 25-bps position at its open cap at 2.010× (+76 s) and the
  cap after a 25-minute freeze at 2.05× (+67 s), where the honest keeper leaves them open — each
  exactly the liquidation the honest keeper makes when the same LP withdraws for good at +1 s (the
  same second, price and buyback cost) (`fitx1_*`; knobs `FITX_HELD`, `FITX_GONE`); and M4G-2 /
  FITS-2 (b), the keeper's cost: one `[deposit 5 %, crank_price,
  withdraw]` buys 8 per-second keeper observations (≤ 10), one every 59 s for an hour 7.9 each
  (603 cranks against the quiet hour's 120; the example 0.03 SOL budget unspent), none on a market
  without positions, and no per-slot burst, while the price-wrapped control still starts bursts
  (`m4g2_*`).

Redeploy scenarios (`redeploy.rs`, AIR-18, 10 scenarios; LiteSVM reloads a program at once after
an extension, so each installs the refusal the cluster gives for the rest of that slot and lifts
it at the next): short.win, then the mock venue, not deployed in the second a liquidation first
becomes possible — refused, nothing alerted, the liquidation lands in the next slot of the same
second; the same judged through an RPC pool whose header reads lag behind short.win's last
deploy (`lag_headers_of`), still transient because the keeper reads as of the refused slot; a
Raydium flag, an S3 probe and a sweep refused while short.win is not deployed, each sent again
the next slot; an upgrade of a fingerprinted venue seen while short.win is not deployed for four
slots, judged on the account read whole and paged in the first of them, once, and flagged when
short.win runs again; an extension of a fingerprinted venue seen while short.win is not
deployed, booked at once; a venue deployed in every slot for 30 s, its
liquidation sent again every second and `ProgramNotDeployed` paged once; a venue that stays
undeployable, refused and alerted (`ExitRefused`) every tick, never counted as transient.

The in-process chain's failure knobs (`LiteSvmChain::fail_simulations`, `fail_reads_of`,
`lag_headers_of`, and `lag_simulations`: simulations answered from an earlier copy of the VM,
with that copy's slot as their context slot) and its call counter (`other_calls`: blockhash,
priority-fee and simulation calls) serve the AIR-01 and AIR-18 scenarios. The scenario binary
holds 142 scenarios; two, the release measurements of `cascade` and `m7_cost`, are ignored by
default.

The JSON-RPC backend has its own check against a live local cluster (`common/tests/rpc_local.rs`,
ignored by default): clock, accounts, program accounts, recent fees, and a transfer simulated and
sent as v1 and as v0 (both accepted by an Agave 4.1.2 `solana-test-validator`); since AIR-18 also
the outcomes' slots and `account_headers_at` (`minContextSlot`: answered at the current slot,
refused far ahead):

```sh
solana-test-validator --reset --quiet &
SHORTWIN_RPC_URL=http://127.0.0.1:8899 \
  cargo test --manifest-path services/Cargo.toml -p shortwin-services-common --test rpc_local -- --ignored
```

Measured on the mock: a v1 batch of 6 cranks is 1,360 B, 35 accounts, ≈ 146 k CU; a v0 batch of
4 is 1,024 B (965 B with a 3-key protocol ALT). On Raydium: a v1 batch of 6 cranks (1 AMM v4 +
5 CPMM) is 1,627 B, 43 accounts, ≈ 166 k CU; without v1 and without a table, v0 batches of 3
(977–1,010 B); through the built ALT, a v0 batch of 3 is 887 B and an AMM v4 liquidation 705 B.
The worst cases of every Raydium transaction are T-SIZE's (`tests/litesvm/tests/tx_size_raydium.rs`).

## Indexer (M8)

The contract is [`docs/design/INDEXER.md`](../docs/design/INDEXER.md) (the three tracks' file
ownership is its §13; what was built and how it was integrated, §13.5); the schema is
`indexer/migrations/`, the HTTP contract `indexer/openapi.yaml`, the configuration
`indexer/indexer.example.toml` (every key with its default; `DATABASE_URL`, `SHORTWIN_RPC_URL`,
`SHORTWIN_WS_URL` override).

```sh
cargo build --manifest-path services/Cargo.toml -p shortwin-indexer --release
I=services/target/release/shortwin-indexer
$I --config indexer.toml migrate          # the schema (the ingest role's DDL rights)
$I --config indexer.toml backfill         # every signature stream back to its start, at finalized
$I --config indexer.toml live             # logsSubscribe at confirmed + finality tracker + refresher
$I --config indexer.toml serve            # REST (openapi.yaml) + /v1/ws, read-only role
$I --config indexer.toml rollups          # the hourly rollups and the invocations retention prune
$I --config indexer.toml run              # live + rollups + serve in one process (stops on the first failure)
$I --config indexer.toml replay <session.jsonl>    # ingest a recorded session (§11.3)
$I --config indexer.toml rebuild [--market <key>]  # recompute projections from events (§3.4)
```

**One writer per database:** `backfill`, `live`, `replay` and `rebuild` take an advisory lock; a
second writer is refused (run `backfill` before `live`, or let `live` gap-fill from the cursors).
`live_source = "yellowstone"` is refused (deferred to v1.1, INDEXER.md §14 IX-4).

### Tests

The tests run a real PostgreSQL 17.11 as the current user (`postgresql_embedded`, the zonky builds
from Maven Central, which bundle their libraries): ~15 MB downloaded once into
`~/.cache/shortwin/postgresql-zonky` (`SHORTWIN_PG_INSTALL_DIR` overrides), data under
`services/target/tmp` (`SHORTWIN_PG_TMP` overrides), removed after each test. No Docker, no root, no
system Postgres. The production binary never links it (the TEST-ONLY `embedded-pg` feature; build
the binary with `-p shortwin-indexer`, as above — a `--workspace` build unifies the feature in
through `indexer-litesvm`; CI checks the `-p` dependency set).

```sh
M=services/Cargo.toml
cargo test --manifest-path $M -p shortwin-indexer --test pg_fixture      # the fixture gate (~2 s cached)
cargo test --manifest-path $M -p shortwin-indexer                        # everything below but the replay harness
cargo test --manifest-path $M -p shortwin-indexer-litesvm                # the replay harness (M8 exit; ~80 s)
cargo test --manifest-path $M -p shortwin-indexer --test ingest_rpc_validator -- --ignored   # needs solana-test-validator
```

- **Ingest (INGEST):** decoder unit tests (`src/ingest/decode/tests.rs`: every catalog event in both
  layouts, attribution at stack heights 2–4, forged log lines, quarantine, v0 / v1 conversion, the
  venue decoders); `ingest_writer` (quarantine, upgrades, failed and truncated transactions,
  metadata and admin rows); `ingest_sessions` on the committed sessions (`tests/data/ingest/`, 13
  fixtures, among them the borrower's synthetic exit past `rT` (pump-final §9: two trade rows, the
  `CompleteEvent`, the closed position) and a retired helper transaction that decodes nothing, and REPLAY's `tests/data/sessions/`): no quarantine, every session matches the chain,
  catalog coverage, idempotent re-ingest, rebuild, out-of-order delivery, the finality tracker's
  rollback, the single writer, NOTIFY. `ingest_rpc_validator` (opt-in) runs the RPC backfill, the
  live source and finality promotion against a local `solana-test-validator` (~40 s).
  `venue_mainnet` (M10 GC-F1) runs WP-9's recorded mainnet transactions
  (`scripts/gapcal/tests/fixtures/*.json.gz`) through `to_raw_tx`, the decoder and (CPMM) the
  writer: every venue's stored post-trade reserves are its pricing reserves — 20 consecutive CPMM
  swaps chain exactly in `venue_trades` (the `AmmConfig` protocol and fund shares and the creator
  fee out of the input, as the venue admin watch observed the rates; zero rates, M8's formula,
  break every link by exactly that share), PumpSwap's quote is the vault plus
  `virtual_quote_reserves` (on which every sale prices exactly), AMM v4's are the vaults net of
  `need_take_pnl`, pump's virtual reserves chain.
- **API (API):** `api_contract` (every operation and documented status against `openapi.yaml` on
  the SQL seed `tests/data/api_seed.sql`, the seeded values, paging), `api_limits` (GCRA, keys,
  `X-Forwarded-For`, timeouts, the query semaphore, CORS), `api_ws` (every channel, `since`,
  rollback, lagged and flood closes, connection caps); `api_replayed` (the contract on every
  committed session ingested through the pipeline, and the chain's end state served through the
  API).
- **Security review regressions (M8-SEC-1 … -15):** `security_ingest` (a mock JSON-RPC node: no
  unserved or erroring transaction is skipped, short pages and unknown listing bounds, the live
  missing-signature retries; the finality
  tracker deletes history only on proof; a rollback resets the cursors; a disabled venue stays
  disabled; trades on pools that are not the market's; longer event layouts; late eventless rows in
  the rollups), `security_api` (probe key lookups, the percent-encoded search, the deployment's
  trusted proxy and the WebSocket process cap, the replay's indexes / charge / semaphore, the
  trigger-kept counters, and the 30-day load test: 2,592,000 keeper observations plus attacker rows,
  every former whole-table page under 750 ms; ~3 min), two more `ingest_rpc_validator` tests
  (INDEXER.md §11.5a).
- **Analytics (ANALYTICS):** `analytics_views` — the SQL helpers against their Rust twins, the
  views on a seeded history, the rollups (carry-forward, flows, late data, `reset_from`), retention.
  Views are in `migrations/0006_analytics.sql`; the hourly rollups are `shortwin-indexer rollups`
  (`analytics::run_jobs`: a pass every `rollup_interval_s`, then the invocations retention prune,
  which keeps `initialize_config` and the admin instructions forever).
- **Replay harness (REPLAY, `indexer-litesvm/`, TEST-ONLY):** records nine LiteSVM sessions from the
  current builds (it needs `target/deploy/{shortwin,mock_cpamm}.so` and
  `target/test-venue/deploy/shortwin.so`, as the keeper scenarios do), checks the contract's
  projection rules against the chain at every checkpoint, then runs INDEXER.md §11.4 through the
  real `ingest::ingest_session`: projections vs the chain, the SOL ledgers, the analytics and
  rollups, idempotence, rebuild, out-of-order delivery, a rollback through the finality tracker
  (the rollups recomputed), catalog coverage and the API's read model. `SHORTWIN_RECORD_SESSIONS=<dir>`
  writes the sessions; `<dir>/committed/` holds the thinned copies for
  `indexer/tests/data/sessions/`.

### Deployment

`indexer/deploy/`: `docker-compose.yml` (`postgres:17.11`, a one-shot `migrate`, exactly one
`ingest` running `live`, `rollups`, two `api` replicas on host ports 127.0.0.1:8080–8081 for a load
balancer on the host — `indexer.toml` trusts its address, the compose network's gateway
`172.30.0.1`, for `X-Forwarded-For`; a balancer elsewhere: bind the ports to its network and list
its address in `[api] trusted_proxies`), the `Dockerfile`
(build context: the repository root, filtered by `Dockerfile.dockerignore` — no `target/`, no keys),
`initdb/01-roles.sh` (`shortwin_ingest` owns the schema; `shortwin_api` is read-only with a 2 s
statement timeout) and `indexer.toml`. Secrets are files under `deploy/secrets/` (git-ignored):

```sh
cd services/indexer/deploy
mkdir -p secrets && for r in admin ingest api; do openssl rand -hex 24 > secrets/pg_$r; done
export SHORTWIN_RPC_URL=https://… SHORTWIN_WS_URL=wss://…
docker compose up -d postgres migrate
docker compose run --rm ingest backfill    # history first (one writer)
docker compose up -d                       # live ingest, rollups, the API
```

Not run on the development machine (no Docker); CI validates the compose file. The tested database
path is the embedded fixture. Operations (alerts, rebuilds, re-ingesting a range): INDEXER.md §12.3.

## Lockfile and toolchain

The lockfile was seeded from the root `Cargo.lock`: the scenario tests link the LiteSVM harness
(`tests/litesvm`), so this workspace holds the same Agave **4.2.2** runtime line and solana-*
component versions (C1-14), and the RPC client is the matching `solana-rpc-client` 4.2.2 (ARCH
§1.1 names the 4.3 clients for services; they move together with LiteSVM / Mollusk). The
keeper's `solana-address-lookup-table-interface` 3.1 is the version LiteSVM already brought in.
Host toolchain from the root `rust-toolchain.toml`. `deny.toml` here is the root policy plus the
TLS stack's licences (ISC, CDLA-Permissive-2.0) and the PostgreSQL licence of the indexer's
TEST-ONLY fixture crates; `cargo audit -f services/Cargo.lock` uses the root's ignore list. The
indexer crates declare Rust 1.94 (sqlx 0.9, postgresql_embedded 0.21); M8 added their dependencies
without changing any existing lockfile entry. M10's `ops/` (WP-7) adds only its own package entry:
every dependency it names (`squads-v4-ix` through the LiteSVM harness, `sha2`, `toml`, the
solana-* components) was already in this lockfile.
