//! short.win indexer and API (ARCHITECTURE §7.2, milestone M8). The contract is
//! [`docs/design/INDEXER.md`](../../../docs/design/INDEXER.md); the schema is `migrations/`, the
//! HTTP contract `openapi.yaml`.
//!
//! ```text
//!   sources (RPC backfill · logsSubscribe live · Yellowstone · recorded session)
//!      │  RawTx (ingest::raw), in chain order per source
//!      ▼
//!   decode (ingest::decode): short.win events and invocations,
//!          venue trades and lifecycle events — attributed through the invocation stack
//!      ▼
//!   writer (ingest::writer): one Postgres transaction per Solana transaction, idempotent on
//!          (signature, ix_index, ev_source, ev_seq); projections (ingest::project); NOTIFY
//!      ▼
//!   Postgres ── analytics (views + hourly rollups, analytics::*) ── API (axum REST + WS, api::*)
//! ```
//!
//! Module ownership for the three M8 tracks is INDEXER.md §13: FOUNDATION modules ([`config`],
//! [`db`], [`numeric`], [`model`], [`ingest::raw`], [`ingest::source`], [`ingest::session`],
//! [`testing`]) are frozen interfaces; `ingest` belongs to INGEST, `analytics` to ANALYTICS, `api`
//! to API. All three are built and integrated (INDEXER.md §13.5).
//!
//! Host-only (the services workspace, its own lockfile; ARCH §1.2).

pub mod analytics;
pub mod api;
pub mod config;
pub mod db;
pub mod ingest;
pub mod model;
pub mod numeric;
#[cfg(feature = "embedded-pg")]
pub mod testing;

pub use config::IndexerConfig;
