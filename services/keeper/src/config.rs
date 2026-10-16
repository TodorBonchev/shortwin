//! Keeper configuration (ARCHITECTURE §7.1 "Operations" 7–9, 14; duties 10 and 12; owner item
//! O17: the daily budget — one global budget since the owner's decision A of 2026-10-01 —, the
//! priority-price ceiling and the markets covered are the owner's to set).
//!
//! Every field has the design default (ECON §5.8 duties, ARCH §7.1), so an empty file is a
//! valid configuration. `keeper.example.toml` documents each one.

use std::str::FromStr;

use anyhow::{Context, bail};
use serde::Deserialize;
use solana_pubkey::Pubkey;

/// Which markets the keeper covers (O17).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MarketSelection {
    /// Every `Market` account of the program.
    #[default]
    All,
    /// Only these markets (base58 keys).
    Only(Vec<String>),
}

impl MarketSelection {
    /// The selected keys (`None` = all).
    pub fn keys(&self) -> anyhow::Result<Option<Vec<Pubkey>>> {
        match self {
            Self::All => Ok(None),
            Self::Only(v) => v
                .iter()
                .map(|s| Pubkey::from_str(s).with_context(|| format!("market key {s}")))
                .collect::<anyhow::Result<Vec<_>>>()
                .map(Some),
        }
    }
}

/// Which transaction format batches use (ARCH §7.1 duty 7).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TxFormatPolicy {
    /// v1 while the cluster accepts it; v0 after a rejection (re-probed after
    /// `v1_reprobe_s`).
    #[default]
    Auto,
    /// Always v1.
    V1,
    /// Always v0.
    V0,
}

/// The longest idle cadence (ARCH §7.1 duty 1: "otherwise at least every 30 s for markets with
/// open positions"), and the bound of the timing tolerances. Below `COVERAGE_GAP_S` (40 s since
/// OWNER-M7-12): the idle cadence plus its 0–2-s landing jitter emits no `CoverageGap`.
pub const IDLE_INTERVAL_MAX_S: i64 = 30;
const _: () = assert!(IDLE_INTERVAL_MAX_S + 2 < shortwin_math::consts::COVERAGE_GAP_S);

/// `oracle.aging_alert_s` must stay below this: the floor of `oracle_stale_s` (600 s; at
/// `oracle_stale_s` every graduated market tiers C), so the alert precedes Tier C at every value
/// governance can set (owner decision C of 2026-10-01, OWNER-M7-15).
pub const ORACLE_STALE_FLOOR_S: i64 = 600;

/// Observation coverage (ARCH §7.1 duties 1, 2, 5; ECON §5.8).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CoverageConfig {
    /// Cadence for a market with open positions and nothing near a threshold (duty 1), and the
    /// only cadence of a market below the value-at-risk floor (duty 8): 30 s, at most
    /// [`IDLE_INTERVAL_MAX_S`].
    pub idle_interval_s: i64,
    /// Cadence for a market without a route (duty 5; keeps the S3 chain continuous, which
    /// restarts after `S3_OBS_GAP_S` = 60 s): 30 s.
    pub no_route_interval_s: i64,
    /// A position is "near" when its live CR is below this multiple of its `L`, in bps
    /// (1.3·L = 13,000).
    pub near_cr_bps: u32,
    /// A position is "near" when its deadline is within this many seconds: 60.
    pub deadline_window_s: i64,
    /// A market needs per-second coverage when `|P_spot/P_ema − 1|` exceeds this, bps: 200.
    pub ema_dev_bps: u32,
    /// A foreign observation deviating from the keeper's own view by more than this starts a
    /// burst (duty 2), bps: 200.
    pub burst_dev_bps: u32,
    /// Length of a deviation burst (every slot), seconds: 60.
    pub burst_s: i64,
    /// FITS-2 (b): on a market with open positions, a foreign bucket liquidity (`llo` / `lhi`)
    /// more than `burst_dev_bps` off the keeper's own `√(X·Y)` of that second starts this many
    /// seconds of **per-second** observations from that second, extended by every recurrence
    /// (an LP-wrapped crank buys at most this many keeper observations; while an attacker keeps
    /// wrapping, one per second: 1:1); 0 disables it: 10.
    pub liq_cover_s: i64,
    /// Consecutive seconds of missed required observations that make a market degraded
    /// (duty 9): 3.
    pub degraded_after_misses: u32,
    /// Seconds of restored coverage before the degraded state lifts (ARCH §7.1 duty 9:
    /// 10 minutes).
    pub recovery_s: i64,
    /// How often a position on a market whose S3 is due is probed with a simulated
    /// `sol_settle` when nothing says it is liquidatable or expired: 10 s.
    pub s3_probe_interval_s: i64,
    /// Seconds between `getProgramAccounts` discoveries of markets: 10.
    pub discovery_interval_s: i64,
    /// Cadence for a Raydium market with no open position inside its listing window (M4V-5):
    /// 60 s. Its last `EMA_WARMUP_BUCKETS + 2` seconds, and the seconds after it until the
    /// post-change warm-up is over (`Market.ema_warm == 0`, M4R), are observed every second, so
    /// the first open after the window finds an honest `P_ema` and a confirmed EMA.
    pub listing_interval_s: i64,
    /// M7-SEC-1 (a): a `CoverageGap{gap_s}` the program emits in the keeper's own transaction
    /// degrades the market only when `gap_s > max(idle_interval_s + gap_tolerance_s,
    /// COVERAGE_GAP_S)` ([`CoverageConfig::gap_degrades_above`]). The program measures the gap
    /// between two integrations' **landing** seconds, the keeper schedules its idle crank from the
    /// second it **read** (a `confirmed` clock), and the two differ by the send latency (0–2 s on
    /// mainnet). As built at M7, with `COVERAGE_GAP_S` = 30, a crank landing one second later
    /// relative to its read than the previous one was a 31-s gap — jitter, not an outage; since
    /// the owner raised `COVERAGE_GAP_S` to 40 (OWNER-M7-12, 2026-10-01) the program emits no gap
    /// at the 30-s idle cadence under that jitter, and with the defaults every gap it emits (41 s
    /// or more) degrades. A crank that does not land at all is a miss
    /// ([`crate::coverage::MarketTrack::close_second`]). 4; at most [`IDLE_INTERVAL_MAX_S`].
    pub gap_tolerance_s: i64,
    /// M7-SEC-1 (b): a tick that follows the previous one by more than `1 + tick_gap_tolerance_s`
    /// seconds on a market with open positions counts as "the keeper was down" (a degraded
    /// market's recovery restarts). A shorter skip — a tick that waited for its transactions'
    /// confirmation, which takes about a second on mainnet — misses only the seconds that
    /// **required** an observation (per-second coverage: they count toward
    /// `degraded_after_misses` like any missed required second); on the idle cadence the skipped
    /// seconds required nothing. 5; at most [`IDLE_INTERVAL_MAX_S`].
    pub tick_gap_tolerance_s: i64,
    /// M7-SEC-8: a list-only market, or a curve market far from completion
    /// (`hot_curve_completion_lamports`), without open positions and with nothing time-critical
    /// pending (no re-detection, pool-selection window or chain, graduation step, post-wipe
    /// sweep, coverage pause or burst) is read every `cold_interval_s` instead of every tick —
    /// staggered over the interval, so the per-tick RPC work does not grow with the markets
    /// anyone can list for a fee and rent; a market that has open positions is read every tick
    /// (the set of markets with positions is known each tick from the `ShortPosition`
    /// accounts), and so is every PumpSwap or Raydium market. (0, 60]: 30.
    pub cold_interval_s: i64,
    /// M7-SEC-8: a curve market whose completion (buying its remaining real tokens) costs at most
    /// this many lamports is read every tick, so its graduation (duty 6: `migrate_v2`, the move
    /// onto the pool) follows at once; a curve farther away — a fresh or junk coin, ≈ 85 SOL
    /// from completion — is cold without positions (a buy that completes it is seen within
    /// `cold_interval_s`): 20,000,000,000 (20 SOL).
    pub hot_curve_completion_lamports: u64,
}

impl Default for CoverageConfig {
    fn default() -> Self {
        Self {
            idle_interval_s: 30,
            no_route_interval_s: 30,
            near_cr_bps: 13_000,
            deadline_window_s: 60,
            ema_dev_bps: 200,
            burst_dev_bps: 200,
            burst_s: 60,
            liq_cover_s: 10,
            degraded_after_misses: 3,
            recovery_s: 600,
            s3_probe_interval_s: 10,
            discovery_interval_s: 10,
            listing_interval_s: 60,
            gap_tolerance_s: 4,
            tick_gap_tolerance_s: 5,
            cold_interval_s: 30,
            hot_curve_completion_lamports: 20_000_000_000,
        }
    }
}

impl CoverageConfig {
    /// The largest `CoverageGap{gap_s}` in the keeper's own transaction that is landing jitter
    /// of the idle cadence, not an outage ([`Self::gap_tolerance_s`], M7-SEC-1 (a)):
    /// `max(idle_interval_s + gap_tolerance_s, COVERAGE_GAP_S)`.
    pub fn gap_degrades_above(&self) -> i64 {
        self.idle_interval_s
            .saturating_add(self.gap_tolerance_s)
            .max(shortwin_math::consts::COVERAGE_GAP_S)
    }
}

/// Budget and priority policy (O17; ARCH §7.1 duties 8 and 14). The coverage budget is **one
/// global daily budget** funding markets in order of their lenders' value at risk (owner decision
/// A of 2026-10-01 on OWNER-M7-13 / -14; [`crate::budget`] has the exact algorithm); the M7
/// per-market budget `per_market_daily_lamports` is retired — a configuration that still sets it
/// is refused ([`KeeperConfig::from_toml`]).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct BudgetConfig {
    /// `G`: lamports the keeper may spend on coverage observations (`crank_price`) per UTC day,
    /// all markets together (0: no budget, every charge admitted). Covered markets' entitlements
    /// are funded from it in rank order, the rest is the free pool; price bursts and
    /// liquidity-anomaly coverage draw on the market's own entitlement and then the free pool;
    /// a charge the budget can no longer cover at the market's rank is refused, and on a market
    /// with open positions that is degraded mode at once (opens paused, exits continue). The
    /// cranks exits depend on are charged but never throttled (beyond `G` they are an alerted
    /// overdraft); exits (`liquidate`, `expire`, `sol_settle`), pool selection, graduation moves,
    /// `crank_sol_usd` and `set_coverage_pause` are neither charged nor throttled. PROPOSED
    /// 1,000,000,000 (1 SOL a day; OWNER-M7-17).
    pub global_daily_lamports: u64,
    /// A market with open positions is **covered** (ranked and funded) when its value at risk
    /// `V = ⌊borrowed_q · min(ema_price, P_ACC_MAX) / 10¹⁸⌋` is at least this; below it the
    /// 30-s idle cadence only, read at that cadence (and, near an exit, in the seconds after an
    /// observation: M7D-1), its cranks from the free pool within `tail_share_bps` of `G` — the
    /// crank an exit waits on exit-critical (M7D-2) — (0: every market with positions covered). Bounds [0,
    /// `market_max_notional`] — the compiled maximum here, the deployment's value at run time (a
    /// larger floor is clamped to it). PROPOSED 1,000,000,000 (1 SOL; OWNER-M7-17).
    pub var_floor_lamports: u64,
    /// A covered market's entitlement is its quiet day (one crank per `idle_interval_s` at its
    /// last per-crank charge) × (10⁴ + this) / 10⁴: twice it by default. Bounds [0, 100,000].
    /// PROPOSED 10,000 (OWNER-M7-17).
    pub reserve_margin_bps: u32,
    /// The most the below-floor markets' idle cadence may draw from the free pool in a day, bps of
    /// `G` (0.2 SOL at the defaults). Bounds [0, 10,000]. PROPOSED 2,000 (OWNER-M7-17).
    pub tail_share_bps: u32,
    /// The O17 priority-price ceiling for observations, micro-lamports per CU: 100,000 (O17,
    /// owner decision of 2026-09-30).
    pub priority_ceiling: u64,
    /// The priority-price ceiling for exits, `set_coverage_pause`, pool selection and the
    /// upgrade latch, micro-lamports per CU: 100,000.
    pub exit_priority_ceiling: u64,
    /// The lowest priority price, micro-lamports per CU.
    pub priority_floor: u64,
    /// Percentile of the recent per-account fees the price starts from, bps (7,500 = p75).
    pub priority_percentile_bps: u32,
    /// Escalation per consecutive missed observation on a market, bps (5,000 = ×1.5).
    pub escalate_bps: u32,
    /// T-CASCADE (C0-17, ARCH §6.6): the most compute units the keeper requests (its
    /// `⌈simulated × 1.1⌉` limits) per market per slot on exits — every exit write-locks the
    /// `Market` and its `MarketVault`, whose per-block budget is 24,000,000 CU
    /// (`MAX_WRITABLE_ACCOUNT_UNITS`). The rest of a cascade goes out in the next slot, most
    /// under-collateralised first: 12,000,000 (half the account's block budget, so a cascade
    /// never starves other writers of the market in a slot). 0: no cap.
    pub market_cu_per_slot: u64,
    /// T-CASCADE: the same across all of the keeper's exits in a slot — its fee payer signs, and
    /// write-locks, every one of its transactions, so the payer account's 24,000,000 CU block
    /// budget bounds the keeper's whole slot (≈ 109 PumpSwap liquidations): 20,000,000. 0: no cap.
    pub payer_cu_per_slot: u64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            global_daily_lamports: 1_000_000_000,
            var_floor_lamports: 1_000_000_000,
            reserve_margin_bps: 10_000,
            tail_share_bps: 2_000,
            priority_ceiling: 100_000,
            exit_priority_ceiling: 100_000,
            priority_floor: 0,
            priority_percentile_bps: 7_500,
            escalate_bps: 5_000,
            market_cu_per_slot: 12_000_000,
            payer_cu_per_slot: 20_000_000,
        }
    }
}

/// O17's automatic degraded mode (ARCH §3.1 `set_coverage_pause`, §7.1 duty 9): the keeper
/// instance designated as the pauser holds the `coverage_authority` key — a separate key from its
/// fee payer, which can only pause and unpause opens per market (no funds, never exits) — and
/// sends `set_coverage_pause(market, true)` when a market with open positions is degraded,
/// `false` after `recovery_s` of restored coverage. Exactly one instance (of the regions that run
/// concurrently) is the pauser; the others only alert, so they never flip-flop the flag.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CoveragePauseConfig {
    /// This instance is the designated pauser: false (the `Degraded` alert pages the Emergency
    /// multisig to `set_pause` instead).
    pub enabled: bool,
    /// Path of the `coverage_authority` keypair file (the binary's `--coverage-keypair`
    /// overrides it); required when `enabled`. Empty: none.
    pub authority_keypair: String,
}

impl CoveragePauseConfig {
    /// The keypair path, if one is set.
    pub fn authority_keypair(&self) -> Option<&str> {
        let s = self.authority_keypair.trim();
        (!s.is_empty()).then_some(s)
    }
}

/// Duty 11 (ARCH §7.1, §4.7; C1-12): `crank_sol_usd` — the USD tiers read the cached Pyth SOL/USD
/// price, and a cache older than `oracle_stale_s` (600 s since the owner's decision of
/// 2026-10-01, OWNER-M7-15; was 3,600) puts every graduated market in Tier C.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct OracleConfig {
    /// Crank the configured `PriceUpdateV2` into the `OracleCache` whenever it holds a newer
    /// update: true.
    pub enabled: bool,
    /// Seconds between reads of the `PriceUpdateV2` (the sponsored feed updates on a 55-s
    /// heartbeat or a 0.5 % move; a newer update found is cranked at once, a failed crank retried
    /// every second): 30.
    pub interval_s: i64,
    /// `Alert::OracleAging` when the cache's publish time is older than this (half of
    /// `oracle_stale_s`; at `oracle_stale_s` every graduated market falls to Tier C), once per
    /// stall and hourly while it lasts (M7D-6): 300 (was 1,800 with the 3,600-s `oracle_stale_s`);
    /// validated in (0, 600) —
    /// below [`ORACLE_STALE_FLOOR_S`], the floor of `oracle_stale_s`.
    pub aging_alert_s: i64,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_s: 30,
            aging_alert_s: 300,
        }
    }
}

/// The venue drift monitor (ARCH §7.1 duty 10; [`crate::monitor`]).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct VenueWatchConfig {
    /// Watch the six venue ProgramData accounts (and Token-2022's, alert only) and call
    /// `flag_venue_upgraded` in the slot after an upgrade: true.
    pub flag_upgrades: bool,
    /// Report admin actions on every covered market's pool and CPMM config (Raydium
    /// `SetParams`, `update_pool_status`, `update_amm_config`): true.
    pub pool_admin: bool,
}

impl Default for VenueWatchConfig {
    fn default() -> Self {
        Self {
            flag_upgrades: true,
            pool_admin: true,
        }
    }
}

/// Raydium pool selection (ARCH §2.6; §7.1 duty 5's fast path and duty 12, the pool monitor;
/// [`crate::pool_monitor`]).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PoolSelectConfig {
    /// Run the pool monitor and the fast path: true.
    pub enabled: bool,
    /// Seconds between `getProgramAccounts` discoveries of a covered mint's Raydium SOL pools
    /// (four filtered calls per mint: AMM v4 and CPMM, WSOL on either side): 300. A market is
    /// discovered at once the first time the keeper sees it.
    pub discovery_interval_s: i64,
    /// The same while the market's incumbent fails `pool_fail` or a fast-path collection window
    /// runs (a pool created or refilled for the fast path is seen within this): 10.
    pub fast_discovery_interval_s: i64,
    /// Seconds between re-reads of the candidates' accounts when nothing is pending (at once
    /// when the incumbent starts or stops failing `pool_fail`; every tick while the fast path is
    /// armed, a window runs, or a waiting candidate meets a market whose last position just
    /// closed): 30.
    pub eval_interval_s: i64,
    /// R4B-10: a market whose adoption of a dominant candidate (≥ `pool_switch_margin_bps` more
    /// locked depth) is blocked this long by open positions is reported as pinned
    /// (`Alert::PoolPinned`: page the Emergency multisig to pause opens there so it drains):
    /// 3,600 s.
    pub pinned_alert_s: i64,
    /// A separate JSON-RPC endpoint for the discovery's `getProgramAccounts` calls (many
    /// providers restrict or meter them); empty: the keeper's own `--rpc`.
    pub discovery_rpc: String,
    /// M4-SEC-7: the best candidates (by locked SOL) evaluated each tick; the rest of a mint's
    /// pools — anyone can create them — are never fetched per tick: 8.
    pub max_candidates: usize,
    /// M4-SEC-7: the accounts (two per pool: its WSOL vault and LP mint) a discovery's ranking
    /// reads per tick and market; a larger discovery is ranked over several ticks: 200.
    pub rank_keys_per_tick: usize,
    /// M7-SEC-8: the most markets whose pool discovery (four `getProgramAccounts` calls each) runs
    /// in one tick; the due ones go by urgency — the fast cadence (a failing incumbent, a
    /// window), then markets with open positions, then the longest waiting. Listing is
    /// permissionless, so without it a batch of junk list-only markets made the ticks that
    /// discovered them (at first sight, then every `discovery_interval_s`) issue four calls per
    /// market at once. Pool selection runs after the tick's exits and cranks (M4-SEC-7), so these
    /// calls delay the next tick, not this one's duties: 2.
    pub discoveries_per_tick: usize,
}

impl Default for PoolSelectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            discovery_interval_s: 300,
            fast_discovery_interval_s: 10,
            eval_interval_s: 30,
            pinned_alert_s: 3_600,
            discovery_rpc: String::new(),
            max_candidates: 8,
            rank_keys_per_tick: 200,
            discoveries_per_tick: 2,
        }
    }
}

impl PoolSelectConfig {
    /// The discovery endpoint, if one is set.
    pub fn discovery_rpc(&self) -> Option<&str> {
        let s = self.discovery_rpc.trim();
        (!s.is_empty()).then_some(s)
    }
}

/// Graduation duties (ARCH §2.5, §5.1, §7.1 duty 6; [`crate::graduation`]).
///
/// pump-final §7 (owner D2): the straddle bundle's keys — `straddle`, `send_without_exit`,
/// `complete_slippage_bps`, `max_complete_lamports`, `tip_lamports`, `tip_accounts`,
/// `block_engine` — are removed, and a configuration that still names one is refused
/// ([`KeeperConfig::from_toml`], [`REMOVED_GRADUATION_KEYS`]): a curve position past `rT` exits by
/// pump v3's synthetic migration in its own `liquidate` / `expire`, with no bundle.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct GraduationConfig {
    /// Call pump `migrate_v2` top-level as soon as a covered market's curve completes (ADOPTED
    /// Q16) — including right after the keeper's own exit completed it — then the `→ PumpSwap`
    /// transition in the same second: true (M6; M5 suspended it, M5-SEC-4, because a migrated
    /// curve's market could not reach its pool).
    pub migrate: bool,
}

impl Default for GraduationConfig {
    fn default() -> Self {
        Self { migrate: true }
    }
}

/// `[graduation]` keys of the retired straddle bundle (pump-final §7, owner D2): a configuration
/// naming one fails to load with [`REMOVED_GRADUATION_MESSAGE`].
pub const REMOVED_GRADUATION_KEYS: [&str; 7] = [
    "straddle",
    "send_without_exit",
    "complete_slippage_bps",
    "max_complete_lamports",
    "tip_lamports",
    "tip_accounts",
    "block_engine",
];

/// Why a retired `[graduation]` key (or the binary's `--block-engine`) is refused.
pub const REMOVED_GRADUATION_MESSAGE: &str =
    "removed: pump v3 exits need no bundle (design pump-final)";

/// The trader side's value and the venue cashback (ARCH §7.1 duty 6 "right after a full wipe",
/// duty 13; [`crate::sweep`]). PROPOSED ops knobs (ECON §15 names none).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SweepConfig {
    /// Right after a full wipe, the claims the trader's venue cashback needs and `sweep_trader`
    /// (a deposit waits for both post-wipe bits, R4A-7): true.
    pub post_wipe: bool,
    /// `sweep_trader` when the trader side holds anything (`ATA(trader, WSOL)`, lamports on the
    /// `trader` / `trader_wsol` PDAs, tokens on a pump venue's trader base ATA): true.
    pub sweep: bool,
    /// Seconds between the reads of a market's trader side (duty 13): 60.
    pub check_interval_s: i64,
    /// Seconds between two cashback claims on one market (ARCH duty 13: weekly): 604,800.
    pub cashback_interval_s: i64,
    /// The claimable cashback a claim waits for, lamports (a PumpSwap claim costs the keeper the
    /// rent of `ATA(trader, WSOL)`, 2,039,280 at the pre-2026 rent, which the sweep credits to the
    /// lenders): 10,000,000. The post-wipe bundle claims whatever is claimable.
    pub cashback_min_lamports: u64,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            post_wipe: true,
            sweep: true,
            check_interval_s: 60,
            cashback_interval_s: 604_800,
            cashback_min_lamports: 10_000_000,
        }
    }
}

/// The keeper's configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct KeeperConfig {
    /// Markets covered (O17).
    pub markets: MarketSelection,
    /// Batch format policy.
    pub tx_format: TxFormatPolicy,
    /// Cranks per v1 batch at most (ARCH §7.1 duty 7: 6); a batch is also cut where its
    /// transaction would exceed the format's size or account limit.
    pub batch_v1: usize,
    /// Cranks per v0 batch at most (4 with the protocol ALT; T-SIZE), cut the same way.
    pub batch_v0: usize,
    /// Seconds before a v1 rejection is re-probed.
    pub v1_reprobe_s: i64,
    /// Address lookup tables for v0 transactions (the protocol ALT of A28), base58.
    pub lookup_tables: Vec<String>,
    /// Simulate only; never send (the keeper still reports what it would do).
    pub dry_run: bool,
    pub coverage: CoverageConfig,
    pub budget: BudgetConfig,
    pub coverage_pause: CoveragePauseConfig,
    pub oracle: OracleConfig,
    pub venue_watch: VenueWatchConfig,
    pub pool_select: PoolSelectConfig,
    pub graduation: GraduationConfig,
    pub sweep: SweepConfig,
}

impl Default for KeeperConfig {
    fn default() -> Self {
        Self {
            markets: MarketSelection::All,
            tx_format: TxFormatPolicy::Auto,
            batch_v1: 6,
            batch_v0: 4,
            v1_reprobe_s: 3_600,
            lookup_tables: Vec::new(),
            dry_run: false,
            coverage: CoverageConfig::default(),
            budget: BudgetConfig::default(),
            coverage_pause: CoveragePauseConfig::default(),
            oracle: OracleConfig::default(),
            venue_watch: VenueWatchConfig::default(),
            pool_select: PoolSelectConfig::default(),
            graduation: GraduationConfig::default(),
            sweep: SweepConfig::default(),
        }
    }
}

impl KeeperConfig {
    /// Parses and checks a TOML configuration. `[budget] per_market_daily_lamports` (M7's
    /// per-market budget) is refused with its own message: the global budget replaced it (owner
    /// decision A of 2026-10-01); so is a key of the retired straddle bundle
    /// ([`REMOVED_GRADUATION_KEYS`], pump-final §7).
    pub fn from_toml(s: &str) -> anyhow::Result<Self> {
        let raw: toml::Table = toml::from_str(s).context("keeper configuration")?;
        if raw
            .get("budget")
            .and_then(toml::Value::as_table)
            .is_some_and(|b| b.contains_key("per_market_daily_lamports"))
        {
            bail!(
                "[budget] per_market_daily_lamports is retired (owner decision A of 2026-10-01): \
                 the keeper budget is global — set global_daily_lamports (and var_floor_lamports, \
                 reserve_margin_bps, tail_share_bps); ARCHITECTURE §7.1 duty 8"
            );
        }
        if let Some(g) = raw.get("graduation").and_then(toml::Value::as_table)
            && let Some(key) = REMOVED_GRADUATION_KEYS.iter().find(|k| g.contains_key(**k))
        {
            bail!("[graduation] {key}: {REMOVED_GRADUATION_MESSAGE}");
        }
        let cfg: Self = toml::from_str(s).context("keeper configuration")?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Relational checks.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.batch_v1 == 0 || self.batch_v0 == 0 {
            bail!("batch sizes must be at least 1");
        }
        let c = &self.coverage;
        if c.no_route_interval_s <= 0
            || c.no_route_interval_s >= shortwin_math::consts::S3_OBS_GAP_S
        {
            bail!(
                "no_route_interval_s must be in (0, {}) so the S3 chain stays continuous",
                shortwin_math::consts::S3_OBS_GAP_S
            );
        }
        if c.idle_interval_s <= 0 || c.idle_interval_s > IDLE_INTERVAL_MAX_S {
            bail!("idle_interval_s must be in (0, {IDLE_INTERVAL_MAX_S}] (ARCH §7.1 duty 1)");
        }
        if c.burst_s < 0 || c.liq_cover_s < 0 || c.recovery_s < 0 || c.s3_probe_interval_s <= 0 {
            bail!("coverage intervals must be non-negative");
        }
        if c.listing_interval_s <= 0 {
            bail!("listing_interval_s must be positive");
        }
        if c.gap_tolerance_s < 0 || c.gap_tolerance_s > IDLE_INTERVAL_MAX_S {
            bail!("gap_tolerance_s must be in [0, {IDLE_INTERVAL_MAX_S}]");
        }
        if c.tick_gap_tolerance_s < 0 || c.tick_gap_tolerance_s > IDLE_INTERVAL_MAX_S {
            bail!("tick_gap_tolerance_s must be in [0, {IDLE_INTERVAL_MAX_S}]");
        }
        if c.cold_interval_s <= 0 || c.cold_interval_s > 60 {
            bail!("cold_interval_s must be in (0, 60]");
        }
        let b = &self.budget;
        if b.priority_floor > b.priority_ceiling || b.priority_floor > b.exit_priority_ceiling {
            bail!("priority_floor exceeds a ceiling");
        }
        if b.priority_percentile_bps > 10_000 {
            bail!("priority_percentile_bps above 10,000");
        }
        // Decision A (2026-10-01): the global budget's bounds (ECON §15 #109).
        let max_notional = shortwin_math::params::Rule::MarketMaxNotional.bounds().1;
        if b.var_floor_lamports > max_notional {
            bail!(
                "budget.var_floor_lamports must be in [0, market_max_notional] (at most \
                 {max_notional}, the compiled maximum)"
            );
        }
        if b.reserve_margin_bps > 100_000 {
            bail!("budget.reserve_margin_bps must be in [0, 100,000]");
        }
        if b.tail_share_bps > 10_000 {
            bail!("budget.tail_share_bps must be in [0, 10,000]");
        }
        let p = &self.pool_select;
        if p.discovery_interval_s <= 0
            || p.fast_discovery_interval_s <= 0
            || p.eval_interval_s <= 0
            || p.pinned_alert_s < 0
        {
            bail!("pool_select intervals must be positive");
        }
        if p.fast_discovery_interval_s > p.discovery_interval_s {
            bail!("pool_select.fast_discovery_interval_s exceeds discovery_interval_s");
        }
        if p.max_candidates == 0 || p.rank_keys_per_tick < 2 {
            bail!("pool_select.max_candidates must be positive and rank_keys_per_tick at least 2");
        }
        if p.discoveries_per_tick == 0 {
            bail!("pool_select.discoveries_per_tick must be positive");
        }
        let w = &self.sweep;
        if w.check_interval_s <= 0 || w.cashback_interval_s <= 0 {
            bail!("sweep intervals must be positive");
        }
        let o = &self.oracle;
        if o.interval_s <= 0 || o.interval_s > 30 {
            bail!("oracle.interval_s must be in (0, 30] (ARCH §7.1 duty 11)");
        }
        if o.aging_alert_s <= 0 || o.aging_alert_s >= ORACLE_STALE_FLOOR_S {
            bail!(
                "oracle.aging_alert_s must be in (0, {ORACLE_STALE_FLOOR_S}): below the floor of \
                 oracle_stale_s, so the alert precedes Tier C (ARCH §7.1 duty 11)"
            );
        }
        self.markets.keys()?;
        for t in &self.lookup_tables {
            Pubkey::from_str(t).with_context(|| format!("lookup table {t}"))?;
        }
        Ok(())
    }

    /// The configured lookup tables.
    pub fn lookup_table_keys(&self) -> anyhow::Result<Vec<Pubkey>> {
        self.lookup_tables
            .iter()
            .map(|s| Pubkey::from_str(s).with_context(|| format!("lookup table {s}")))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_the_defaults() {
        assert_eq!(
            KeeperConfig::from_toml("").unwrap(),
            KeeperConfig::default()
        );
    }

    #[test]
    fn the_example_file_is_the_defaults() {
        let cfg = KeeperConfig::from_toml(include_str!("../keeper.example.toml")).unwrap();
        assert_eq!(cfg, KeeperConfig::default());
    }

    #[test]
    fn parses_every_section() {
        let k = Pubkey::new_unique();
        let cfg = KeeperConfig::from_toml(&format!(
            r#"
            markets = {{ only = ["{k}"] }}
            tx_format = "v0"
            batch_v0 = 3
            [coverage]
            idle_interval_s = 20
            [budget]
            global_daily_lamports = 2000000000
            var_floor_lamports = 0
            reserve_margin_bps = 50000
            tail_share_bps = 1000
            priority_ceiling = 5000
            market_cu_per_slot = 6000000
            [coverage_pause]
            enabled = true
            authority_keypair = "/etc/shortwin/coverage.json"
            [venue_watch]
            pool_admin = false
            [pool_select]
            eval_interval_s = 5
            discovery_rpc = "https://gpa.example"
            [graduation]
            migrate = false
            [sweep]
            cashback_min_lamports = 1
            "#
        ))
        .unwrap();
        assert_eq!(cfg.markets.keys().unwrap(), Some(vec![k]));
        assert_eq!(cfg.tx_format, TxFormatPolicy::V0);
        assert_eq!(cfg.batch_v0, 3);
        assert_eq!(cfg.coverage.idle_interval_s, 20);
        assert_eq!(cfg.coverage.burst_s, 60);
        assert_eq!(cfg.coverage.liq_cover_s, 10);
        assert_eq!(
            (
                cfg.budget.global_daily_lamports,
                cfg.budget.var_floor_lamports,
                cfg.budget.reserve_margin_bps,
                cfg.budget.tail_share_bps
            ),
            (2_000_000_000, 0, 50_000, 1_000)
        );
        assert_eq!(cfg.budget.priority_ceiling, 5_000);
        assert_eq!(cfg.budget.market_cu_per_slot, 6_000_000);
        assert_eq!(cfg.budget.payer_cu_per_slot, 20_000_000);
        assert!(cfg.coverage_pause.enabled);
        assert_eq!(
            cfg.coverage_pause.authority_keypair(),
            Some("/etc/shortwin/coverage.json")
        );
        // O17 (owner decision of 2026-09-30): both ceilings 100,000 µlamports/CU; the pauser is
        // opt-in per instance.
        let d = KeeperConfig::default();
        assert_eq!(
            (d.budget.priority_ceiling, d.budget.exit_priority_ceiling),
            (100_000, 100_000)
        );
        assert!(!d.coverage_pause.enabled && d.coverage_pause.authority_keypair().is_none());
        // Decision A (2026-10-01): the global budget's PROPOSED defaults (OWNER-M7-17).
        assert_eq!(
            (
                d.budget.global_daily_lamports,
                d.budget.var_floor_lamports,
                d.budget.reserve_margin_bps,
                d.budget.tail_share_bps
            ),
            (1_000_000_000, 1_000_000_000, 10_000, 2_000)
        );
        // Decision C (2026-10-01): the aging alert at half the 600-s `oracle_stale_s`.
        assert_eq!((d.oracle.interval_s, d.oracle.aging_alert_s), (30, 300));
        assert!(!cfg.venue_watch.pool_admin && cfg.venue_watch.flag_upgrades);
        assert_eq!(cfg.pool_select.eval_interval_s, 5);
        assert_eq!(cfg.pool_select.discovery_rpc(), Some("https://gpa.example"));
        assert!(cfg.pool_select.enabled);
        assert_eq!(KeeperConfig::default().pool_select.discovery_rpc(), None);
        assert!(!cfg.graduation.migrate);
        // M6: the migrate_v2 duty (with the → PumpSwap transition) is on by default again
        // (M5-SEC-4 suspended it).
        let d = KeeperConfig::default();
        assert!(d.graduation.migrate);
        assert_eq!(cfg.sweep.cashback_min_lamports, 1);
        assert!(cfg.sweep.post_wipe && cfg.sweep.sweep);
        assert_eq!(d.sweep.cashback_interval_s, 7 * 86_400);
    }

    /// pump-final §7 (owner D2): every key of the retired straddle bundle is refused with the
    /// removal's own message, whatever its value; `migrate` stays.
    #[test]
    fn the_straddle_bundles_keys_are_refused_as_removed() {
        for (key, value) in [
            ("straddle", "true"),
            ("straddle", "false"),
            ("send_without_exit", "true"),
            ("complete_slippage_bps", "500"),
            ("max_complete_lamports", "20000000000"),
            ("tip_lamports", "100000"),
            ("tip_accounts", "[]"),
            ("block_engine", "\"https://mainnet.block-engine.jito.wtf\""),
        ] {
            let e =
                KeeperConfig::from_toml(&format!("[graduation]\nmigrate = true\n{key} = {value}"))
                    .unwrap_err()
                    .to_string();
            assert!(e.contains(REMOVED_GRADUATION_MESSAGE), "{key}: {e}");
            assert!(e.contains(key), "{key}: {e}");
        }
        assert_eq!(
            REMOVED_GRADUATION_MESSAGE,
            "removed: pump v3 exits need no bundle (design pump-final)"
        );
        assert!(KeeperConfig::from_toml("[graduation]\nmigrate = false").is_ok());
        // Named outside `[graduation]` it is only an unknown key.
        assert!(KeeperConfig::from_toml("straddle = true").is_err());
    }

    #[test]
    fn refuses_what_breaks_the_s3_chain_or_the_coverage_rule() {
        for bad in [
            "[coverage]\nno_route_interval_s = 60",
            "[coverage]\nidle_interval_s = 31",
            "[coverage]\nliq_cover_s = -1",
            "[coverage]\ngap_tolerance_s = -1",
            "[coverage]\ngap_tolerance_s = 31",
            "[coverage]\ntick_gap_tolerance_s = -1",
            "[coverage]\ncold_interval_s = 0",
            "[coverage]\ncold_interval_s = 61",
            "batch_v1 = 0",
            "[budget]\npriority_floor = 200000",
            "unknown = 1",
            "markets = { only = [\"nope\"] }",
            "[pool_select]\neval_interval_s = 0",
            "[pool_select]\nfast_discovery_interval_s = 600",
            "[pool_select]\ndiscoveries_per_tick = 0",
            "[pool_select]\nunknown = 1",
            "[graduation]\nunknown = 1",
            "[sweep]\ncheck_interval_s = 0",
            "[sweep]\nunknown = 1",
            "[coverage_pause]\nunknown = 1",
            "[oracle]\ninterval_s = 31",
            "[oracle]\ninterval_s = 0",
            "[oracle]\nunknown = 1",
            // Decision C: the aging alert below the 600-s floor of `oracle_stale_s`.
            "[oracle]\naging_alert_s = 0",
            "[oracle]\naging_alert_s = 600",
            "[oracle]\naging_alert_s = 1800",
            // Decision A: the global budget's bounds, and the retired per-market budget.
            "[budget]\nvar_floor_lamports = 100000000000001",
            "[budget]\nreserve_margin_bps = 100001",
            "[budget]\ntail_share_bps = 10001",
            "[budget]\nper_market_daily_lamports = 30000000",
            "[budget]\nglobal_daily_lamports = -1",
        ] {
            assert!(KeeperConfig::from_toml(bad).is_err(), "{bad}");
        }
        let retired = KeeperConfig::from_toml("[budget]\nper_market_daily_lamports = 1")
            .unwrap_err()
            .to_string();
        assert!(retired.contains("retired"), "{retired}");
        // The bounds themselves are accepted.
        for good in [
            "[oracle]\naging_alert_s = 599",
            "[budget]\nvar_floor_lamports = 100000000000000\nreserve_margin_bps = 100000\n\
             tail_share_bps = 10000\nglobal_daily_lamports = 0",
            "[coverage]\nidle_interval_s = 30\ngap_tolerance_s = 30\ntick_gap_tolerance_s = 30",
        ] {
            assert!(KeeperConfig::from_toml(good).is_ok(), "{good}");
        }
    }
}
