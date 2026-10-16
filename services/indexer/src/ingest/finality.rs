//! Commitment promotion and rollback (INDEXER.md §3.2, §3.4). Owner: INGEST.
//!
//! Every `finality.poll_ms` ([`run_once`]): the cluster's finalized slot F; the `confirmed`
//! transactions at or below F are checked with `getSignatureStatuses` **with
//! `searchTransactionHistory`** (batches of `finality.status_batch`; without it a node answers
//! only from its ~300-slot status cache, and every older transaction would look abandoned,
//! M8-SEC-1):
//!
//! - finalized **in the stored slot** → `commitment = 'finalized'`;
//! - finalized **in another slot** (re-included after a fork switch, M8-SEC-12) → rolled back and
//!   re-ingested from `getTransaction` at `finalized` (its true slot, block position and time);
//! - no status → *not* yet an abandoned fork: `getTransaction` at `finalized` is asked (another
//!   fork may hold it — re-ingested in place or moved as above), then the block at the stored
//!   slot: only a finalized block that does not carry the signature (or a slot the finalized
//!   chain skipped) proves the fork was abandoned → [`rollback_signature`]. When the node cannot
//!   tell (the block is pruned or not stored), the transaction stays `confirmed` and holds the
//!   finalized watermark back (the finality backlog alert) — history is never deleted on a guess.
//!
//! [`rollback_signature`]: delete its `ingest_txs` row (everything decoded from it cascades),
//! mark its markets `needs_rebuild` (and the global projection when it held global inputs), reset
//! every stream cursor that named it to the stream's newest finalized transaction (M8-SEC-5), log
//! `ingest_rollbacks`, reset the rollup clock to the transaction's hour,
//! `pg_notify(NOTIFY_ROLLBACK, <slot>)`; the writer then rebuilds.
//! `ingest_state.finalized_slot` advances only past slots whose transactions are all resolved.
//!
//! The rollback functions run under the writer lock (the caller's [`Writer`]).

use anyhow::Context;
use solana_signature::Signature;
use sqlx::PgPool;

use super::raw::RawTx;
use super::writer::Writer;
use crate::db::NOTIFY_ROLLBACK;

/// What a rollback removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RollbackReport {
    pub rolled_back: u64,
    pub markets_marked: u64,
    pub global_marked: bool,
}

/// Rolls back one non-finalized transaction (module docs); a finalized or unknown one is left
/// alone. The caller holds the writer lock and rebuilds afterwards
/// ([`super::project::rebuild_marked`]).
pub async fn rollback_signature(
    pool: &PgPool,
    signature: &str,
    reason: &str,
) -> anyhow::Result<RollbackReport> {
    let mut tx = pool.begin().await?;
    let row: Option<(i64, Option<i64>, String)> = sqlx::query_as(
        "SELECT slot, block_time, commitment FROM ingest_txs WHERE signature = $1 FOR UPDATE",
    )
    .bind(signature)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((slot, block_time, commitment)) = row else {
        return Ok(RollbackReport::default());
    };
    if commitment == "finalized" {
        tracing::warn!(signature, "refusing to roll back a finalized transaction");
        return Ok(RollbackReport::default());
    }
    let markets: Vec<String> = sqlx::query_scalar(
        "SELECT market::text FROM events WHERE signature = $1 AND market IS NOT NULL \
         UNION SELECT market::text FROM invocations WHERE signature = $1 AND market IS NOT NULL",
    )
    .bind(signature)
    .fetch_all(&mut *tx)
    .await?;
    let kinds: Vec<String> = super::project::GLOBAL_KINDS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let global: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events WHERE signature = $1 AND market IS NULL AND kind = ANY($2)) \
         OR EXISTS (SELECT 1 FROM invocations WHERE signature = $1 AND name = 'initialize_config')",
    )
    .bind(signature)
    .bind(kinds)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM ingest_txs WHERE signature = $1")
        .bind(signature)
        .execute(&mut *tx)
        .await
        .context("deleting the rolled-back transaction")?;
    // A cursor must never name a transaction the chain does not have: the next listing would
    // fail on it (`getSignaturesForAddress(until)` → -32020) forever (M8-SEC-5). Back to the
    // stream's newest finalized transaction (never rolled back), or empty; the gap-fill lists
    // the head again (duplicates are no-ops).
    reset_cursors(&mut tx, signature).await?;
    // Account reads at or after the abandoned slot may show the abandoned fork: the refresher
    // reads them again.
    sqlx::query("DELETE FROM account_snapshots WHERE slot >= $1")
        .bind(slot)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO ingest_rollbacks (signature, slot, reason) VALUES ($1, $2, $3)")
        .bind(signature)
        .bind(slot)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
    let marked = sqlx::query("UPDATE markets SET needs_rebuild = TRUE WHERE market = ANY($1)")
        .bind(&markets)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if global {
        sqlx::query("UPDATE protocol_state SET needs_rebuild = TRUE WHERE id")
            .execute(&mut *tx)
            .await?;
    }
    if let Some(bt) = block_time {
        // The rollup clock back to the transaction's hour (§3.4; ANALYTICS' own entry point).
        crate::analytics::rollups::reset_from(&mut *tx, bt).await?;
    }
    sqlx::query(
        "UPDATE ingest_state SET latest_block_time = (SELECT max(block_time) FROM ingest_txs), \
           updated_at = now() WHERE id",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(NOTIFY_ROLLBACK)
        .bind(slot.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(signature, slot, reason, "rolled back");
    Ok(RollbackReport {
        rolled_back: 1,
        markets_marked: marked,
        global_marked: global,
    })
}

/// The cursors that named `signature` (deleted in this transaction) as their newest or oldest:
/// newest → the stream's newest remaining finalized transaction, oldest → its oldest remaining
/// transaction (both NULL when there is none; `backfill_complete` stays as it was).
async fn reset_cursors(conn: &mut sqlx::PgConnection, signature: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE ingest_cursors c SET newest_signature = f.signature, newest_slot = f.slot, \
           updated_at = now() \
         FROM ingest_cursors c0 LEFT JOIN LATERAL ( \
             SELECT s.signature, s.slot FROM ingest_tx_streams s \
             JOIN ingest_txs t ON t.signature = s.signature \
             WHERE s.stream = c0.stream AND t.commitment = 'finalized' \
             ORDER BY s.slot DESC, s.tx_index DESC LIMIT 1) f ON TRUE \
         WHERE c.stream = c0.stream AND c0.newest_signature = $1",
    )
    .bind(signature)
    .execute(&mut *conn)
    .await
    .context("resetting the newest cursors")?;
    sqlx::query(
        "UPDATE ingest_cursors c SET oldest_signature = o.signature, oldest_slot = o.slot, \
           updated_at = now() \
         FROM ingest_cursors c0 LEFT JOIN LATERAL ( \
             SELECT s.signature, s.slot FROM ingest_tx_streams s \
             WHERE s.stream = c0.stream ORDER BY s.slot, s.tx_index LIMIT 1) o ON TRUE \
         WHERE c.stream = c0.stream AND c0.oldest_signature = $1",
    )
    .bind(signature)
    .execute(&mut *conn)
    .await
    .context("resetting the oldest cursors")?;
    Ok(())
}

/// `SourceItem::Rollback { slot }`: rolls back every non-finalized transaction at or above
/// `slot` (newest first) and lowers the confirmed watermark below it.
pub async fn rollback_from_slot(
    pool: &PgPool,
    slot: u64,
    reason: &str,
) -> anyhow::Result<RollbackReport> {
    let slot = i64::try_from(slot).unwrap_or(i64::MAX);
    let sigs: Vec<String> = sqlx::query_scalar(
        "SELECT signature::text FROM ingest_txs WHERE slot >= $1 AND commitment = 'confirmed' \
         ORDER BY slot DESC, tx_index DESC",
    )
    .bind(slot)
    .fetch_all(pool)
    .await?;
    let mut out = RollbackReport::default();
    for s in sigs {
        let r = rollback_signature(pool, &s, reason).await?;
        out.rolled_back += r.rolled_back;
        out.markets_marked += r.markets_marked;
        out.global_marked |= r.global_marked;
    }
    sqlx::query("UPDATE ingest_state SET confirmed_slot = LEAST(confirmed_slot, $1 - 1) WHERE id")
        .bind(slot)
        .execute(pool)
        .await?;
    Ok(out)
}

/// A signature's status at the cluster (`getSignatureStatuses`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxStatus {
    /// The slot the cluster says the transaction landed in.
    pub slot: u64,
    pub finalized: bool,
}

/// What the finalized block at a slot says about a signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockCheck {
    /// The finalized block carries it.
    Has,
    /// The finalized block does not carry it, or the finalized chain skipped the slot: the fork
    /// that held it was abandoned.
    Lacks,
    /// The node cannot tell (the block is pruned, not stored, or not available yet).
    Unknown,
}

/// What the finality tracker asks the cluster (the RPC in production, a stub in tests).
#[allow(async_fn_in_trait)]
pub trait StatusSource {
    /// The cluster's latest finalized slot.
    async fn finalized_slot(&self) -> anyhow::Result<u64>;
    /// Statuses of `sigs` (same order; `None` = unknown to the cluster), **searching the
    /// transaction history** (not only the status cache).
    async fn statuses(&self, sigs: &[Signature]) -> anyhow::Result<Vec<Option<TxStatus>>>;
    /// The transaction at `finalized`, if the cluster has it (a rolled-back transaction that
    /// another fork included).
    async fn fetch_finalized(&self, sig: &Signature) -> anyhow::Result<Option<RawTx>>;
    /// Whether the finalized block at `slot` carries `sig` (the proof of an abandoned fork).
    async fn block_check(&self, slot: u64, sig: &Signature) -> anyhow::Result<BlockCheck>;
}

/// One pass of the finality tracker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FinalityReport {
    pub promoted: u64,
    pub rolled_back: u64,
    pub reingested: u64,
    /// Transactions left `confirmed` because the cluster could not tell their fate.
    pub unresolved: u64,
    pub finalized_slot: u64,
}

/// Transactions without a status whose fate one pass looks into (`getTransaction` and
/// `getBlock` each); the others wait for the next pass (`unresolved`).
pub const DEEP_CHECKS_PER_PASS: usize = 64;

/// One pass of the tracker (module docs), through `writer` (which holds the lock).
pub async fn run_once(
    writer: &mut Writer,
    src: &impl StatusSource,
    batch: usize,
) -> anyhow::Result<FinalityReport> {
    let pool = writer.pool().clone();
    let f = src.finalized_slot().await?;
    let f_i64 = i64::try_from(f).unwrap_or(i64::MAX);
    let pending: Vec<(String, i64)> = sqlx::query_as(
        "SELECT signature::text, slot FROM ingest_txs WHERE commitment = 'confirmed' AND slot <= $1 \
         ORDER BY slot, tx_index",
    )
    .bind(f_i64)
    .fetch_all(&pool)
    .await?;
    let mut report = FinalityReport::default();
    // Proven abandoned (newest last), and moved to another slot (with the finalized copy).
    let mut gone: Vec<Signature> = Vec::new();
    let mut moved: Vec<(Signature, RawTx)> = Vec::new();
    let mut deep = 0usize;
    for chunk in pending.chunks(batch.clamp(1, 256)) {
        let sigs: Vec<(Signature, u64)> = chunk
            .iter()
            .map(|(s, slot)| {
                s.parse()
                    .map(|sig| (sig, u64::try_from(*slot).unwrap_or(0)))
                    .map_err(|e| anyhow::anyhow!("signature {s}: {e:?}"))
            })
            .collect::<anyhow::Result<_>>()?;
        let keys: Vec<Signature> = sigs.iter().map(|(s, _)| *s).collect();
        let statuses = src.statuses(&keys).await?;
        let mut finalized: Vec<String> = Vec::new();
        for ((sig, slot), st) in sigs.iter().zip(statuses) {
            match st {
                Some(s) if s.finalized && s.slot == *slot => finalized.push(sig.to_string()),
                Some(s) if s.finalized => match src.fetch_finalized(sig).await? {
                    Some(raw) if raw.slot == *slot => finalized.push(sig.to_string()),
                    Some(raw) => moved.push((*sig, raw)),
                    None => {
                        tracing::warn!(signature = %sig, slot, status_slot = s.slot,
                            "finalized in another slot but not served at finalized; kept");
                        report.unresolved += 1;
                    }
                },
                Some(_) => {}
                None if deep >= DEEP_CHECKS_PER_PASS => report.unresolved += 1,
                None => {
                    deep += 1;
                    match src.fetch_finalized(sig).await? {
                        Some(raw) if raw.slot == *slot => finalized.push(sig.to_string()),
                        Some(raw) => moved.push((*sig, raw)),
                        None => match src.block_check(*slot, sig).await? {
                            BlockCheck::Has => finalized.push(sig.to_string()),
                            BlockCheck::Lacks => gone.push(*sig),
                            BlockCheck::Unknown => {
                                tracing::warn!(signature = %sig, slot,
                                    "no status and the block is not available: kept at confirmed");
                                report.unresolved += 1;
                            }
                        },
                    }
                }
            }
        }
        if !finalized.is_empty() {
            report.promoted += sqlx::query(
                "UPDATE ingest_txs SET commitment = 'finalized' WHERE signature = ANY($1) \
                   AND commitment = 'confirmed'",
            )
            .bind(&finalized)
            .execute(&pool)
            .await?
            .rows_affected();
        }
    }
    // Newest first, so a market's later transactions leave before its earlier ones.
    let mut out: Vec<(Signature, String)> = gone
        .iter()
        .map(|s| {
            (
                *s,
                "abandoned fork: the finalized block at its slot does not carry it".to_string(),
            )
        })
        .chain(
            moved
                .iter()
                .map(|(s, raw)| (*s, format!("re-included at finalized in slot {}", raw.slot))),
        )
        .collect();
    out.reverse();
    for (sig, reason) in &out {
        let r = rollback_signature(&pool, &sig.to_string(), reason).await?;
        report.rolled_back += r.rolled_back;
    }
    if !out.is_empty() {
        writer.end_batch_force().await?;
    }
    for (sig, raw) in &moved {
        writer.write(raw).await?;
        report.reingested += 1;
        sqlx::query(
            "UPDATE ingest_rollbacks SET reingested = TRUE WHERE signature = $1 AND NOT reingested",
        )
        .bind(sig.to_string())
        .execute(&pool)
        .await?;
    }
    writer.end_batch().await?;
    // The finalized watermark: F, or below the oldest transaction still unresolved.
    sqlx::query(
        "UPDATE ingest_state SET finalized_slot = GREATEST(finalized_slot, LEAST($1, \
           COALESCE((SELECT min(slot) - 1 FROM ingest_txs WHERE commitment = 'confirmed'), $1))), \
           updated_at = now() WHERE id",
    )
    .bind(f_i64)
    .execute(&pool)
    .await?;
    report.finalized_slot = f;
    Ok(report)
}
