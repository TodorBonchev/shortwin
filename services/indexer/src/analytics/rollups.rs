//! Hourly rollups (INDEXER.md §8.6). Owner: ANALYTICS.
//!
//! [`run_once`] fills `market_stats_hourly` for every hour from `rollup_state.complete_before`
//! to the last complete hour of the chain clock (`sw_chain_now()`), then `global_stats_hourly`
//! from it, and advances `rollup_state`. State columns come from `market_state_log` (the last row
//! at or before the bucket's end, carried forward) and the price in force then
//! (`market_snapshots` / `positions.ema_wad_open`); flow columns sum `fees`, `positions`,
//! `settlements`, `venue_trades`, `coverage_events`, `invocations` and the lender events over the
//! bucket. A rollback below `complete_before` (§3.4) resets it to the rolled-back hour (the
//! writer's side, [`reset_from`]); a transaction committed after the last pass whose block time
//! falls before `complete_before` (a gap-fill, a late confirmation) moves it back too, whether
//! or not it carries an event (`rollup_state.last_tx_seq` over `ingest_txs.seq`, and
//! `last_event_id` for events without a block time; M8-SEC-14). Buckets are recomputed
//! idempotently (`INSERT … ON CONFLICT DO
//! UPDATE`) by the SQL functions `sw_rollup_market_hours` / `sw_rollup_global_hours`
//! (`migrations/0006_analytics.sql`).
//!
//! One pass is one `REPEATABLE READ` transaction holding a transaction-level advisory lock, so
//! concurrent `rollups` processes serialise and every pass reads one snapshot.

use anyhow::Context;
use sqlx::PgPool;

/// Seconds per bucket.
pub const BUCKET_S: i64 = 3_600;

/// `pg_advisory_xact_lock` key of a rollup pass ("SWROLL").
pub const ROLLUP_LOCK: i64 = 0x5357_524f_4c4c;

/// What a pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RollupPass {
    /// First hour recomputed (`complete_before` at the start, after a late-data reset).
    pub from: i64,
    /// End of the last hour computed (exclusive; the new `complete_before`).
    pub to: i64,
    /// Market-hours written.
    pub market_hours: u64,
    /// Global hours written.
    pub global_hours: u64,
    /// The pass moved `complete_before` back for events committed since the last pass.
    pub late_reset: bool,
}

/// `⌊ts / 3600⌋ · 3600`.
pub fn bucket_of(ts: i64) -> i64 {
    ts.div_euclid(BUCKET_S) * BUCKET_S
}

/// One pass up to `until_ts` (exclusive, chain seconds; `None`: `sw_chain_now()`); returns the
/// number of market-hours written.
pub async fn run_once(pool: &PgPool, until_ts: Option<i64>) -> anyhow::Result<u64> {
    Ok(pass(pool, until_ts).await?.market_hours)
}

/// [`run_once`] with the details of the pass.
pub async fn pass(pool: &PgPool, until_ts: Option<i64>) -> anyhow::Result<RollupPass> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ROLLUP_LOCK)
        .execute(&mut *tx)
        .await?;
    let until = match until_ts {
        Some(t) => t,
        None => {
            sqlx::query_scalar::<_, i64>("SELECT sw_chain_now()")
                .fetch_one(&mut *tx)
                .await?
        }
    };
    let (complete_before, last_event_id, last_tx_seq): (i64, i64, i64) = sqlx::query_as(
        "SELECT complete_before, last_event_id, last_tx_seq FROM rollup_state \
         WHERE name = 'market_stats_hourly' FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await
    .context("rollup_state")?;
    // Late data: any transaction written since the last pass (with its rows: invocations,
    // trades, events), and any event (whose `ev_ts` stands in for a missing block time).
    let (max_id, late_ev, max_seq, late_tx): (Option<i64>, Option<i64>, Option<i64>, Option<i64>) =
        sqlx::query_as(
            "SELECT (SELECT max(id) FROM events), \
                    (SELECT min(COALESCE(block_time, ev_ts)) FROM events WHERE id > $1), \
                    (SELECT max(seq) FROM ingest_txs), \
                    (SELECT min(block_time) FROM ingest_txs WHERE seq > $2)",
        )
        .bind(last_event_id)
        .bind(last_tx_seq)
        .fetch_one(&mut *tx)
        .await?;
    let late = match (late_ev, late_tx) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let mut from = complete_before;
    let mut late_reset = false;
    if let Some(t) = late
        && t < complete_before
    {
        from = bucket_of(t);
        late_reset = true;
    }
    let to = bucket_of(until).max(from);
    let market_hours: i64 = sqlx::query_scalar("SELECT sw_rollup_market_hours($1, $2)")
        .bind(from)
        .bind(to)
        .fetch_one(&mut *tx)
        .await
        .context("sw_rollup_market_hours")?;
    let global_hours: i64 = sqlx::query_scalar("SELECT sw_rollup_global_hours($1, $2)")
        .bind(from)
        .bind(to)
        .fetch_one(&mut *tx)
        .await
        .context("sw_rollup_global_hours")?;
    sqlx::query(
        "UPDATE rollup_state SET complete_before = $1, last_event_id = $2, last_tx_seq = $3, \
           updated_at = now() WHERE name IN ('market_stats_hourly', 'global_stats_hourly')",
    )
    .bind(to)
    .bind(max_id.unwrap_or(last_event_id).max(last_event_id))
    .bind(max_seq.unwrap_or(last_tx_seq).max(last_tx_seq))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(RollupPass {
        from,
        to,
        market_hours: u64::try_from(market_hours).unwrap_or(0),
        global_hours: u64::try_from(global_hours).unwrap_or(0),
        late_reset,
    })
}

/// Moves `complete_before` back to the hour of `ts` (the writer's rollback path, §3.4 —
/// `ingest::finality::rollback_signature` calls it inside its transaction —; also what an
/// operator runs after re-ingesting a range). Never moves it forward. `exec`: the pool, or the
/// caller's transaction.
pub async fn reset_from<'e, E>(exec: E, ts: i64) -> anyhow::Result<()>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query(
        "UPDATE rollup_state SET complete_before = least(complete_before, $1), \
         updated_at = now() WHERE name IN ('market_stats_hourly', 'global_stats_hourly')",
    )
    .bind(bucket_of(ts))
    .execute(exec)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_floor_to_the_hour() {
        assert_eq!(bucket_of(0), 0);
        assert_eq!(bucket_of(3_599), 0);
        assert_eq!(bucket_of(3_600), 3_600);
        assert_eq!(bucket_of(1_790_003_599), 1_790_002_800);
        assert_eq!(bucket_of(-1), -3_600);
    }
}
