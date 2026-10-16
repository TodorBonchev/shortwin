//! Postgres access (INDEXER.md §4). FOUNDATION.
//!
//! - [`MIGRATOR`]: the schema, `migrations/*.sql` embedded at compile time (sqlx's `migrate!`
//!   needs no database at build time). Migration numbers are owned by track (§13.3).
//! - Queries use sqlx's runtime API (`sqlx::query` / `query_as` with `FromRow`), never the
//!   compile-time-checked macros: there is no database at build time (no Docker here, §11.1).
//!   sqlx 0.9 takes `&'static str` SQL; dynamic SQL must be wrapped in `sqlx::AssertSqlSafe` and
//!   may only splice identifiers from fixed allowlists (§10.2), never request input.
//! - u64 / u128 / i128 cross the boundary as decimal text ([`crate::numeric`]).

use std::time::Duration;

use anyhow::Context;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::config::DatabaseConfig;

/// The schema (`migrations/`).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// `NOTIFY` channel the writer signals after each committed batch; the payload is the highest
/// `events.id` committed (§9.3).
pub const NOTIFY_EVENTS: &str = "shortwin_events";
/// `NOTIFY` channel for rollbacks; the payload is the lowest slot rolled back (§3.4, §9.3).
pub const NOTIFY_ROLLBACK: &str = "shortwin_rollback";

/// Opens a pool on `cfg.url`.
pub async fn connect(cfg: &DatabaseConfig) -> anyhow::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&cfg.url)
        .await
        .context("connecting to Postgres")
}

/// Applies every pending migration.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .context("applying the indexer migrations")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_numbered_in_track_ranges() {
        // §13.3: 0001–0007 are the contract's; later files take their track's range.
        let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        assert!(!versions.is_empty());
        assert!(versions.windows(2).all(|w| w[0] < w[1]));
        for v in versions {
            assert!(
                (1..=7).contains(&v) || (100..400).contains(&v),
                "migration {v} outside the contract's ranges (INDEXER.md §13.3)"
            );
        }
    }
}
