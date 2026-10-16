//! Analytics (INDEXER.md §8). Owner: ANALYTICS.
//!
//! Current values are SQL views (`migrations/0006_analytics.sql`); time series are the hourly
//! rollup tables this module's job fills ([`rollups`]); [`formulas`] is the Rust reference of
//! every formula. [`run_jobs`] is the `rollups` process: a rollup pass every
//! `rollup_interval_s`, then the `invocations` retention prune ([`prune_invocations`], §4.4).

use std::time::Duration;

use serde::Deserialize;
use sqlx::PgPool;

use crate::IndexerConfig;

pub mod formulas;
pub mod rollups;

/// `[analytics]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnalyticsConfig {
    /// Run the rollup job in this process.
    pub enabled: bool,
    /// Seconds between rollup passes.
    pub rollup_interval_s: u64,
    /// Keep `invocations` rows (non-admin) this many days (§8.6).
    pub invocation_retention_days: u32,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rollup_interval_s: 60,
            invocation_retention_days: 30,
        }
    }
}

/// The instructions whose `invocations` rows are kept forever (§4.4 prunes "non-admin
/// instructions"): governance, the Emergency's and the coverage key's powers, the latch, the
/// listing and the protocol's fee sweep — the transparency record of SPEC §11.
pub const RETAINED_INSTRUCTIONS: [&str; 14] = [
    "initialize_config",
    "propose_config",
    "apply_config",
    "cancel_config",
    "set_pause",
    "set_coverage_pause",
    "disable_venue",
    "set_route_broken",
    "set_venue_extras",
    "flag_venue_upgraded",
    "revalidate_venue",
    "flag_unsafe_mint",
    "sweep_protocol_fees",
    "create_market",
];

/// Rows deleted per statement by [`prune_invocations`] (short transactions, no long locks).
pub const PRUNE_BATCH: i64 = 10_000;

/// Deletes the `invocations` of non-retained instructions older than `retention_days` of chain
/// time **and** already rolled up (`block_time < rollup_state.complete_before`); returns the
/// rows deleted.
pub async fn prune_invocations(pool: &PgPool, retention_days: u32) -> anyhow::Result<u64> {
    let keep: Vec<String> = RETAINED_INSTRUCTIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let batch = u64::try_from(PRUNE_BATCH).unwrap_or(u64::MAX);
    let mut total = 0u64;
    loop {
        let n = sqlx::query(
            "DELETE FROM invocations WHERE ctid IN ( \
               SELECT ctid FROM invocations \
               WHERE block_time < least( \
                       (SELECT complete_before FROM rollup_state \
                        WHERE name = 'market_stats_hourly'), \
                       sw_chain_now() - $1::BIGINT * 86400) \
                 AND NOT (name = ANY($2)) \
               LIMIT $3)",
        )
        .bind(i64::from(retention_days))
        .bind(&keep)
        .bind(PRUNE_BATCH)
        .execute(pool)
        .await?
        .rows_affected();
        total += n;
        if n < batch {
            return Ok(total);
        }
    }
}

/// `shortwin-indexer rollups` (and the `run` command's background job): every
/// `rollup_interval_s`, [`rollups::run_once`] up to the chain clock, then the retention prune.
/// Returns at once when `[analytics] enabled = false`. A failed pass is logged and retried at the
/// next interval (a transient database error never stops the job).
pub async fn run_jobs(cfg: &IndexerConfig, pool: &PgPool) -> anyhow::Result<()> {
    let a = &cfg.analytics;
    if !a.enabled {
        tracing::info!("analytics job disabled ([analytics] enabled = false)");
        return Ok(());
    }
    let interval = Duration::from_secs(a.rollup_interval_s.max(1));
    loop {
        match rollups::pass(pool, None).await {
            Ok(p) => tracing::info!(
                from = p.from,
                to = p.to,
                market_hours = p.market_hours,
                global_hours = p.global_hours,
                late_reset = p.late_reset,
                "rollup pass"
            ),
            Err(e) => tracing::warn!(error = ?e, "rollup pass failed"),
        }
        match prune_invocations(pool, a.invocation_retention_days).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(rows = n, "invocations pruned"),
            Err(e) => tracing::warn!(error = ?e, "invocations prune failed"),
        }
        tokio::time::sleep(interval).await;
    }
}
