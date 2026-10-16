//! `[ingest]` (INDEXER.md §2, §12.2). Owner: INGEST (fields may be added; the existing keys are
//! the contract of `indexer.example.toml`).

use anyhow::bail;
use serde::Deserialize;

/// `[ingest]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestConfig {
    /// JSON-RPC endpoint (`SHORTWIN_RPC_URL` overrides).
    pub rpc_url: String,
    /// PubSub WebSocket endpoint (`SHORTWIN_WS_URL` overrides).
    pub ws_url: String,
    /// short.win's program id (base58); the crate's `declare_id!` by default. (The straddle
    /// helper's `complete_curve_id` is gone with its ingestion, design pump-final §9, owner
    /// decision D2: a configuration that still sets it fails to load as an unknown field.)
    pub program_id: String,
    /// `"logs_subscribe"` (JSON-RPC PubSub, the default) or `"yellowstone"` (§2.4).
    pub live_source: String,
    pub yellowstone: YellowstoneConfig,
    pub backfill: BackfillConfig,
    pub finality: FinalityConfig,
    pub refresher: RefresherConfig,
    pub venue_trades: VenueTradesConfig,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            rpc_url: "http://127.0.0.1:8899".to_string(),
            ws_url: "ws://127.0.0.1:8900".to_string(),
            program_id: shortwin::ID.to_string(),
            live_source: "logs_subscribe".to_string(),
            yellowstone: YellowstoneConfig::default(),
            backfill: BackfillConfig::default(),
            finality: FinalityConfig::default(),
            refresher: RefresherConfig::default(),
            venue_trades: VenueTradesConfig::default(),
        }
    }
}

impl IngestConfig {
    /// Rejects values the RPC methods cannot honour (checked at the start of `backfill` and
    /// `live`).
    pub fn check(&self) -> anyhow::Result<()> {
        self.backfill.check()?;
        if !(1..=256).contains(&self.finality.status_batch) {
            bail!(
                "ingest.finality.status_batch = {}: getSignatureStatuses takes 1 to 256 signatures",
                self.finality.status_batch
            );
        }
        Ok(())
    }
}

/// `[ingest.yellowstone]` (§2.4; not built in M8: deferred to v1.1, INDEXER.md §14 IX-4).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct YellowstoneConfig {
    pub endpoint: String,
    pub x_token: String,
}

/// `[ingest.backfill]` (§2.2).
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BackfillConfig {
    /// `getSignaturesForAddress` page size (the RPC maximum is 1,000).
    pub page_limit: usize,
    /// `getTransaction` calls in flight.
    pub fetch_concurrency: usize,
    /// Upper bound on RPC requests per second (a shared provider's limit).
    pub max_rps: u32,
    /// Stop going back at this signature (base58; e.g. the deployment's first transaction).
    pub until_signature: Option<String>,
}

impl Default for BackfillConfig {
    fn default() -> Self {
        Self {
            page_limit: 1_000,
            fetch_concurrency: 8,
            max_rps: 20,
            until_signature: None,
        }
    }
}

impl BackfillConfig {
    /// `page_limit` within `getSignaturesForAddress`'s 1 to 1,000 (M8-SEC-11: a larger value is
    /// silently capped by the node), positive concurrency and rate.
    pub fn check(&self) -> anyhow::Result<()> {
        if !(1..=1_000).contains(&self.page_limit) {
            bail!(
                "ingest.backfill.page_limit = {}: getSignaturesForAddress returns 1 to 1,000 \
                 signatures per page",
                self.page_limit
            );
        }
        if self.fetch_concurrency == 0 || self.max_rps == 0 {
            bail!("ingest.backfill.fetch_concurrency and max_rps must be positive");
        }
        Ok(())
    }
}

/// `[ingest.finality]` (§3.2).
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FinalityConfig {
    /// How often the finality tracker promotes confirmed transactions.
    pub poll_ms: u64,
    /// `getSignatureStatuses` batch size (the RPC maximum is 256).
    pub status_batch: usize,
}

impl Default for FinalityConfig {
    fn default() -> Self {
        Self {
            poll_ms: 2_000,
            status_batch: 256,
        }
    }
}

/// `[ingest.refresher]` (§2.6).
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RefresherConfig {
    pub enabled: bool,
    /// At most one `Market` + `MarketVault` read per market per this many milliseconds.
    pub market_min_interval_ms: u64,
    /// Venue admin watch cadence (§7.3; the keeper's monitor runs at 30 s too).
    pub venue_watch_interval_s: u64,
    /// `OracleCache` read cadence.
    pub oracle_interval_s: u64,
    /// Fetch token metadata (Metaplex / Token-2022) at `MarketCreated`.
    pub token_metadata: bool,
}

impl Default for RefresherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            market_min_interval_ms: 2_000,
            venue_watch_interval_s: 30,
            oracle_interval_s: 30,
            token_metadata: true,
        }
    }
}

/// `[ingest.venue_trades]` (§7.1).
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VenueTradesConfig {
    /// Also ingest third-party trades of every listed market's pool (one signature stream per
    /// pool); short.win's own legs are always decoded (they are inside its transactions).
    pub third_party: bool,
    /// At most this many pools tracked, by open interest then TVL.
    pub max_pools: usize,
}

impl Default for VenueTradesConfig {
    fn default() -> Self {
        Self {
            third_party: false,
            max_pools: 200,
        }
    }
}
