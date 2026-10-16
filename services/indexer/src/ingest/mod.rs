//! Ingest (INDEXER.md §2, §3, §5, §6, §7). Owner: INGEST, except the FOUNDATION modules
//! [`raw`], [`source`] and [`session`] (frozen interfaces).
//!
//! The entry points below are the cross-track contract: `main.rs` and the replay harness
//! (`services/indexer-litesvm`) call them with exactly these signatures.

use std::ops::Range;

use solana_pubkey::Pubkey;
use sqlx::PgPool;

use crate::IndexerConfig;

pub mod config;
pub mod decode;
pub mod finality;
pub mod live;
pub mod project;
pub mod raw;
pub mod refresher;
pub mod replay;
pub mod rpc;
pub mod session;
pub mod source;
pub mod store;
pub mod writer;

pub use config::IngestConfig;

/// The program whose events are decoded (configured; the session header's for a replay). The
/// retired straddle helper `complete_curve` is no longer ingested (design pump-final §9, owner
/// decision D2): its transactions, if any, are written like any other and decode no event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramIds {
    pub shortwin: Pubkey,
}

impl Default for ProgramIds {
    fn default() -> Self {
        Self {
            shortwin: shortwin::ID,
        }
    }
}

/// What an ingest run wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IngestReport {
    /// Transactions received from the source.
    pub txs_seen: u64,
    /// Transactions written (new).
    pub txs_written: u64,
    /// Transactions already present (idempotent skip, §3.1).
    pub txs_duplicate: u64,
    /// Failed transactions recorded (no events decoded).
    pub txs_failed: u64,
    /// `events` rows written.
    pub events: u64,
    /// `venue_trades` rows written.
    pub venue_trades: u64,
    /// `ingest_quarantine` rows written.
    pub quarantined: u64,
    /// Markets rebuilt after out-of-order events (§3.3).
    pub markets_rebuilt: u64,
}

/// `shortwin-indexer backfill`: every signature stream from its cursor back to its history's
/// start (or `backfill.until_signature`), oldest first, at `finalized` (§2.2).
pub async fn run_backfill(cfg: &IndexerConfig, pool: &PgPool) -> anyhow::Result<IngestReport> {
    rpc::run_backfill(cfg, pool).await
}

/// `shortwin-indexer live`: subscribe, gap-fill from the cursors, then stream at `confirmed`
/// with the finality tracker and the refresher running alongside (§2.3, §3.2).
pub async fn run_live(cfg: &IndexerConfig, pool: &PgPool) -> anyhow::Result<()> {
    live::run(cfg, pool).await
}

/// `shortwin-indexer replay <session.jsonl>`: ingests a recorded session (§11.3).
pub async fn replay_file(
    _cfg: &IndexerConfig,
    pool: &PgPool,
    path: &std::path::Path,
) -> anyhow::Result<IngestReport> {
    let session = session::read(path)?;
    ingest_session(pool, &session, 0..session.txs.len()).await
}

/// Ingests `session.txs[range]` (each at `finalized`, with the session's program ids) through
/// the same conversion as the RPC backfill, the decoder, the writer and the projections; the
/// account refresher's role is played by the session's checkpoints (§11.3). Idempotent: calling
/// it again with an overlapping range writes nothing twice.
pub async fn ingest_session(
    pool: &PgPool,
    session: &session::Session,
    range: Range<usize>,
) -> anyhow::Result<IngestReport> {
    replay::ingest_session(pool, session, range).await
}

/// `shortwin-indexer rebuild [--market <pubkey>]`: recomputes the aggregate projections of one
/// market (or every market) from its events in chain order (§3.4, §6.1).
pub async fn rebuild(pool: &PgPool, market: Option<Pubkey>) -> anyhow::Result<u64> {
    project::rebuild(pool, market).await
}
