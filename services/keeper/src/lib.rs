//! short.win keeper v1 (ARCHITECTURE §7.1; ECON §5.8): duties 1–6 with batching, degraded mode
//! with the O17 coverage pause, duty 10's venue drift monitor, duty 11's SOL/USD cache and duty
//! 12's Raydium pool monitor (with duty 5's fast path, M4), on every venue and the TEST-ONLY mock:
//!
//! 1. **Observation coverage** ([`coverage`]): one observation per second on every market with
//!    a position near a threshold (live CR below 1.3·L, deadline within 60 s, armed) or with
//!    `|P_spot/P_ema − 1| > 2 %`, or, after a pool switch, through the post-change warm-up and
//!    for as long as the reference guard is set (`ema_guard == 1`, M4F-1); otherwise every 30 s
//!    for markets with open positions; every 30 s without a route (the continuous S3 chain;
//!    duty 5).
//! 2. **Deviation bursts**: after a foreign observation deviating from the keeper's own view
//!    by more than 2 % in price, one observation every slot for 60 s (M4F-1's per-slot liquidity
//!    bursts stay reverted: M4G-2). **Liquidity coverage** (FITS-2 (b)): on a market with open
//!    positions, a foreign bucket liquidity more than 2 % off the keeper's own `√(X·Y)` (an
//!    LP-wrapped crank) starts one observation **per second** for `liq_cover_s` (10 s), extended
//!    while it recurs — no attacker-only second while an attacker keeps wrapping, at one keeper
//!    transaction per attacker transaction; never on a market without positions.
//! 3. **Liquidate** at the first executable second (the fixed δ bound, retried each second on
//!    `ExitBoundNotMet`); **expire** when the expiry bound is met. Always simulated first.
//! 4. **`sol_settle`** every second for armed positions and positions without a normal
//!    buyback (it arms, keeps the clock, disarms on box evidence, or settles), and for
//!    positions whose S3 is due.
//! 5. **No route**: a halted venue (a Raydium admin halt, a fee above the hard cap) starts the
//!    S3 chain at the keeper's next tick and is observed every 30 s after. **Raydium fast
//!    path** ([`pool_monitor`]): once the incumbent has failed `pool_fail` for
//!    `pool_fast_route_s`, propose the deepest eligible candidate (opening the collection
//!    window, or improving someone else's), and `execute_pool_switch` at the first second after
//!    the window; a failing market without positions is observed to keep its chain.
//! 6. **Graduation** ([`graduation`]; M5, M6; pump-final §7): pump `migrate_v2` top-level as soon
//!    as a covered market's curve completes (ADOPTED Q16) — also right after the keeper's own
//!    exit completed it —, then — as soon as the canonical pool is valid — the `→ PumpSwap`
//!    transition in one transaction (the pool accounts the trades require, `crank_price` onto the
//!    pool, the pool's first observation); the market's other positions exit on the pool at the
//!    post-change warm-up's gate (`T + 8`). A liquidatable position whose `Q` exceeds its live
//!    curve's `rT` is liquidated or expired like any other: pump v3's synthetic migration buys past
//!    `rT` inside the exit, priced `B̂_v3` (the straddle bundle and the `complete-curve` helper
//!    are retired, owner D2); for one release the curve exits carry the TradeExtras list the
//!    deployed short.win parses (v3, or 1.0.0's: [`venue::CurveExtrasFormat`]). **Right after a
//!    full wipe** the venue cashback claims and `sweep_trader` ([`sweep`]), so the market takes
//!    deposits and opens again.
//! 8. **The global budget** ([`budget`]; owner decision A of 2026-10-01): one daily budget `G`
//!    funding markets in order of their lenders' value at risk `V = ΣB_Q·P_ema`; covered markets
//!    (positions and `V ≥ var_floor_lamports`) hold entitlements in rank order, the rest is the
//!    free pool; bursts and liquidity-anomaly coverage draw on the market's own entitlement, then
//!    the free pool, never another market's (nor what a join took from lower-ranked markets: a
//!    market is funded once a UTC day, M7D-3); markets below the floor get the 30-s idle cadence
//!    only, from the free pool within `tail_share_bps`, read at that cadence and — while a
//!    position is near an exit — in the seconds after an observation while its reference is fresh
//!    (M7D-1); the cranks exits depend on (at most one a second per market, M7D-4; below the floor
//!    the crank an exit waits on, M7D-2) are never throttled (an alerted overdraft beyond `G`).
//! 9. **Degraded mode** ([`coverage`], [`alerts`]; O17, M7): a market with open positions whose
//!    coverage lapses — `degraded_after_misses` missed seconds in a row, the program's
//!    `CoverageGap`, or a charge the global budget refused at its rank — is degraded; the
//!    designated pauser (`[coverage_pause]`, holding the `coverage_authority` key) sends
//!    `set_coverage_pause(market, true)` in the same tick (opens only; every exit continues) and
//!    `false` after `recovery_s` of restored coverage; without the key or the role it alerts, and
//!    the page is the pause path. Since 2026-10-01 the Emergency multisig can clear a coverage
//!    pause too (OWNER-M7-3 (b)): the pauser re-sets only a flag on a market it degraded itself,
//!    and pages `CoveragePauseForeign` on a set it did not send.
//! 10. **Venue drift monitor** ([`monitor`]): the six venue ProgramData deploy slots against the
//!     `VenueGuard` latch — an upgrade is flagged with `flag_venue_upgraded` in the slot it is
//!     seen, once, and refreshed when a second upgrade lands during review; a fingerprinted
//!     venue's ProgramData that only grew (the loader's `ExtendProgram`) is told from an upgrade
//!     by the program's own rule — the flag's simulation before the exits, from a bank that
//!     holds the deploy, else the whole account after them — and neither flagged nor paged
//!     (AIR-01); Token-2022's upgrade is alerted, its extension is not — and every
//!     covered market's pool and CPMM config (Raydium `SetParams`, `update_pool_status`,
//!     `update_amm_config`, seen as the account changes they make): route lost / restored,
//!     status, open time and fee against the opens and hard caps.
//! 11. **The SOL/USD cache** (M7): `crank_sol_usd` of every newer Pyth update the program's rules
//!     accept, read every ≤ 30 s (the USD tiers; a cache older than `oracle_stale_s` — 600 s
//!     since 2026-10-01 — tiers every graduated market C); refusals and an aging cache (300 s)
//!     alerted.
//! 12. **Raydium pool monitor** ([`pool_monitor`]): every Raydium SOL pool of each covered mint
//!     (`getProgramAccounts` on both programs, optionally through a separate endpoint), each
//!     verified and ranked by locked depth `D` with the program's own verifier; `propose_pool`
//!     exactly when the program's own `pool_select::decide` accepts — a list-only market's
//!     first pool (after D1's `classify_pump`), a listing-window replacement (5 %), a
//!     zero-position adoption (25 %, in the tick the last position closes); a market pinned by
//!     open positions to a much thinner pool is alerted (R4B-10); pool events are reported.
//! 13. **The trader side** ([`sweep`]; M6): `sweep_trader` when the trader side holds anything
//!     (a donated `ATA(trader, WSOL)`, lamports on the trader PDAs, tokens on a pump venue's trader
//!     base ATA), and the weekly venue cashback claim on cashback coins.
//!
//! Operations: several markets' cranks per transaction ([`keeper`]: v1 batches of up to 6, v0
//! batches of up to 4 where v1 is refused, each cut where its transaction would exceed the
//! format's limits), `⌈simulated × 1.1⌉` compute units, a priority price from recent fees capped
//! at the O17 ceiling of 100,000 µlamports/CU ([`fees`]), the global daily budget ([`budget`]; the
//! cranks exits depend on charged, never throttled), T-CASCADE's cap on the compute units of exits
//! requested per market and per slot (`budget.market_cu_per_slot`, `payer_cu_per_slot`),
//! per-market metrics and U28's `B̂_live/B̂_ref` histogram ([`metrics`]). The protocol address
//! lookup table of A28 — and, on request, a market's own table — is built and checked by
//! `shortwin-build-alt` ([`alt`]); `shortwin-v1-probe` checks whether a cluster takes v1
//! transactions (U25).
//!
//! **Same code as the program** (ARCH §7.1 "Stack"): the keeper links the `shortwin` crate for
//! its account types, its instruction builders, its venue reader and account lists (the Raydium
//! adapters' own key functions) and its market prologue, `shortwin-venue-state` for the venue
//! and ProgramData readers, and `shortwin-math` for every leg, reference and bound, so a keeper
//! decision is the program's decision on the state the next instruction sees ([`decide`]).
//!
//! The chain is pluggable ([`shortwin_services_common::Chain`]): JSON-RPC for a real cluster
//! (the binary), an in-process LiteSVM backend for tests (`services/keeper-litesvm`).
//! Venue kinds ([`venue`]): Raydium AMM v4 and CPMM (M3); the pump curve and a migrating curve
//! (M5; pump v3 since pump-final §7: the curve adapter's ReadSet and TradeExtras, the risk view
//! through the program's own quoter — `B̂_v3` above a live curve's `rT` at the synthetic fee on
//! the mint's supply, which the keeper reads, the post-complete fields on a migrating curve —,
//! duty 10 on pump's post-complete fields); PumpSwap (M6: the adapter's ReadSet and TradeExtras,
//! the mint's supply read for the fee tier, `disable_flags` watched by duty 10); the TEST-ONLY
//! mock (`test-venue` feature).

pub mod alerts;
pub mod alt;
pub mod budget;
pub mod config;
pub mod coverage;
pub mod decide;
pub mod events;
pub mod fees;
pub mod graduation;
pub mod ixs;
pub mod keeper;
pub mod metrics;
pub mod monitor;
pub mod pda;
pub mod pool_monitor;
pub mod sweep;
pub mod venue;
pub mod view;

pub use alerts::{Alert, DegradedReason, PauseUnavailable};
pub use config::KeeperConfig;
pub use keeper::{
    CurveFormatRecord, ExitKind, ExitRecord, ExitResult, Keeper, PoolActionRecord, PoolResult,
    TickReport, V1Support,
};
