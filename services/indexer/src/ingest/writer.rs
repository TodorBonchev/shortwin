//! The writer (INDEXER.md §3.1, §3.3, §6.1). Owner: INGEST.
//!
//! One Postgres transaction per Solana transaction: `INSERT … ON CONFLICT (signature) DO NOTHING`
//! on `ingest_txs` decides; if the row already existed nothing else is written (§3.1). Otherwise:
//! `invocations`, `events`, `ingest_quarantine`, `venue_trades` / `venue_events` /
//! `venue_program_upgrades`, the projections ([`super::project::apply`]) — an event older than
//! its market's watermark only marks the market `needs_rebuild`, and the writer rebuilds marked
//! markets at the end of the batch ([`Writer::end_batch`], §3.3) — `ingest_state` and the
//! stream cursors, then `pg_notify(NOTIFY_EVENTS, <max events.id>)` (delivered at commit, §9.3).
//!
//! **Single writer.** [`WriterLock`] holds `pg_try_advisory_lock(0x5357_494e_4458)` ("SWINDX") on
//! a dedicated session for the writer's lifetime; a second writer fails to start. One writer
//! means `events.id` commits in increasing order (the WebSocket's resume key) and projections
//! apply in one order.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, bail};
use serde_json::Value;
use solana_pubkey::Pubkey;
use sqlx::{Connection, PgConnection, PgPool, Row};

use super::decode::{self, Decoded, venue};
use super::project::{self, EventIn, InitConfigIn, Item, WRITER_LOCK};
use super::raw::{Commitment, RawTx};
use super::store::{ChainKey, i16_idx, i32_of, opt_key, slot_i64};
use super::{IngestReport, ProgramIds};
use crate::db::NOTIFY_EVENTS;

/// The single-writer session lock (§3.1).
pub struct WriterLock {
    conn: PgConnection,
}

impl WriterLock {
    /// Takes the lock on a dedicated connection, or fails if another writer holds it.
    pub async fn acquire(pool: &PgPool) -> anyhow::Result<Self> {
        let mut conn = pool.acquire().await?.detach();
        let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(WRITER_LOCK)
            .fetch_one(&mut conn)
            .await?;
        if !got {
            let _ = conn.close().await;
            bail!(
                "another indexer writer holds the lock (INDEXER.md §3.1): run one writer per database"
            );
        }
        Ok(Self { conn })
    }

    /// Releases the lock (dropping the lock closes its session, which releases it too).
    pub async fn release(mut self) -> anyhow::Result<()> {
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(WRITER_LOCK)
            .execute(&mut self.conn)
            .await?;
        self.conn.close().await?;
        Ok(())
    }
}

/// A stream cursor advanced with a transaction (§3.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorAdvance {
    /// `program:<id>` | `programdata:<key>` | `pool:<key>`.
    pub stream: String,
    pub address: Pubkey,
}

/// What [`Writer::write`] did with one transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Written; `events` rows inserted (a notification followed when > 0).
    Written {
        events: u64,
    },
    Duplicate,
}

/// The single writer (module docs).
pub struct Writer {
    pool: PgPool,
    ids: ProgramIds,
    source: &'static str,
    lock: Option<WriterLock>,
    pub report: IngestReport,
    /// Markets marked for rebuild during the current batch.
    pending_rebuild: HashSet<Pubkey>,
    pending_global: bool,
}

impl Writer {
    /// Takes the writer lock and opens a writer for `source` (`model::SOURCES`).
    pub async fn open(
        pool: &PgPool,
        ids: ProgramIds,
        source: &'static str,
    ) -> anyhow::Result<Self> {
        let lock = WriterLock::acquire(pool).await?;
        let mut w = Self {
            pool: pool.clone(),
            ids,
            source,
            lock: Some(lock),
            report: IngestReport::default(),
            pending_rebuild: HashSet::new(),
            pending_global: false,
        };
        // A crash between a batch and its rebuild leaves marks behind: finish them first.
        w.report.markets_rebuilt += project::rebuild_marked(&w.pool).await?;
        Ok(w)
    }

    /// The pool this writer writes to.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The programs decoded.
    pub fn ids(&self) -> ProgramIds {
        self.ids
    }

    /// Rebuilds the markets (and the global projection) marked during the batch (§3.3).
    pub async fn end_batch(&mut self) -> anyhow::Result<u64> {
        if self.pending_rebuild.is_empty() && !self.pending_global {
            return Ok(0);
        }
        let n = project::rebuild_marked(&self.pool).await?;
        self.pending_rebuild.clear();
        self.pending_global = false;
        self.report.markets_rebuilt += n;
        Ok(n)
    }

    /// Rebuilds whatever is marked in the database (after a rollback), whether or not this
    /// batch marked it.
    pub async fn end_batch_force(&mut self) -> anyhow::Result<u64> {
        let n = project::rebuild_marked(&self.pool).await?;
        self.pending_rebuild.clear();
        self.pending_global = false;
        self.report.markets_rebuilt += n;
        Ok(n)
    }

    /// Ends the batch and releases the lock.
    pub async fn close(mut self) -> anyhow::Result<IngestReport> {
        self.end_batch().await?;
        if let Some(lock) = self.lock.take() {
            lock.release().await?;
        }
        Ok(self.report)
    }

    /// Writes one transaction (idempotent).
    pub async fn write(&mut self, raw: &RawTx) -> anyhow::Result<WriteOutcome> {
        self.write_with(raw, &[]).await
    }

    /// Writes one transaction and advances `cursors` in the same Postgres transaction.
    pub async fn write_with(
        &mut self,
        raw: &RawTx,
        cursors: &[CursorAdvance],
    ) -> anyhow::Result<WriteOutcome> {
        self.report.txs_seen += 1;
        let sig = raw.signature.to_string();
        // A different transaction at the same block position (a fork switch at confirmed the
        // finality tracker has not resolved yet): the newer observation replaces it.
        let other: Option<(String, String)> = sqlx::query_as(
            "SELECT signature::text, commitment FROM ingest_txs \
             WHERE slot = $1 AND tx_index = $2 AND signature <> $3",
        )
        .bind(slot_i64(raw.slot))
        .bind(i32::try_from(raw.tx_index).unwrap_or(i32::MAX))
        .bind(&sig)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((old, commitment)) = other {
            if commitment == "finalized" {
                bail!(
                    "{sig} at ({}, {}) collides with finalized {old}",
                    raw.slot,
                    raw.tx_index
                );
            }
            super::finality::rollback_signature(
                &self.pool,
                &old,
                "replaced at the same block position",
            )
            .await?;
            self.pending_global = true;
        }
        let pools = self.pool_ctx(raw).await?;
        let decoded = decode::decode(raw, &self.ids, &pools, &|_| None);
        let mut tx = self.pool.begin().await?;
        let inserted = insert_tx(&mut tx, raw, &decoded, self.source).await?;
        if !inserted {
            tx.commit().await?;
            self.report.txs_duplicate += 1;
            return Ok(WriteOutcome::Duplicate);
        }
        if !raw.ok() {
            self.report.txs_failed += 1;
        }
        insert_invocations(&mut tx, raw, &decoded).await?;
        let market_of_mint = mint_markets(&mut tx, &decoded).await?;
        let market_of_pool = pool_markets(&mut tx, &decoded).await?;
        let (event_rows, max_id) = insert_events(&mut tx, raw, &decoded).await?;
        insert_quarantine(&mut tx, raw, &decoded).await?;
        let trades =
            insert_venue_rows(&mut tx, raw, &decoded, &market_of_mint, &market_of_pool).await?;
        // Projections.
        let mut items: Vec<Item> = event_rows.into_iter().map(Item::Event).collect();
        if raw.ok() {
            for inv in decoded
                .invocations
                .iter()
                .filter(|i| i.name == "initialize_config")
            {
                items.push(Item::Init(InitConfigIn {
                    signature: sig.clone(),
                    key: ChainKey {
                        slot: slot_i64(raw.slot),
                        tx_index: i32::try_from(raw.tx_index).unwrap_or(i32::MAX),
                        ix_index: i16_idx(inv.ix_index),
                        ev_source: -1,
                        ev_seq: inv.inner_index.map_or(-1, i32_of),
                    },
                    block_time: raw.block_time,
                    args: inv.args.clone().unwrap_or(Value::Null),
                }));
            }
        }
        items.sort_by_key(Item::key);
        let outcome = project::apply(&mut tx, &items, true).await?;
        self.pending_rebuild
            .extend(outcome.rebuild_markets.iter().copied());
        self.pending_global |= outcome.rebuild_global;
        // State, cursors, notification.
        update_state(&mut tx, raw).await?;
        for c in cursors {
            advance_cursor(&mut tx, c, raw).await?;
        }
        if let Some(id) = max_id {
            sqlx::query("SELECT pg_notify($1, $2)")
                .bind(NOTIFY_EVENTS)
                .bind(id.to_string())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        let events = u64::try_from(decoded.events.len()).unwrap_or(0);
        self.report.txs_written += 1;
        self.report.events += u64::try_from(decoded.events.len()).unwrap_or(0);
        self.report.venue_trades += trades;
        self.report.quarantined += u64::try_from(decoded.quarantine.len()).unwrap_or(0);
        Ok(WriteOutcome::Written { events })
    }

    /// The `venue_pools` rows of the pools `raw` trades on (AMM v4 orientation, pAMM mints), and
    /// the observed rates of the CPMM configs its swaps name (account 2): per config and field,
    /// the `venue_admin_states` value observed last at or before the transaction's slot, else the
    /// earliest observed (a backfill decodes transactions older than the watch); a config never
    /// observed keeps [`venue::CpmmFeeRates::MAINNET`].
    async fn pool_ctx(&self, raw: &RawTx) -> anyhow::Result<venue::PoolCtx> {
        let mut keys: Vec<String> = Vec::new();
        let mut configs: Vec<String> = Vec::new();
        for top in &raw.instructions {
            for ix in std::iter::once(&top.ix).chain(top.inner.iter()) {
                if ix.program_id == venue::RAYDIUM_CPMM
                    && let Some(c) = ix.accounts.get(2)
                {
                    configs.push(c.to_string());
                }
                let k = if ix.program_id == venue::RAYDIUM_V4 {
                    ix.accounts.get(1)
                } else if ix.program_id == venue::PUMP_AMM {
                    ix.accounts.first()
                } else {
                    None
                };
                if let Some(k) = k {
                    keys.push(k.to_string());
                }
            }
        }
        let mut ctx = venue::PoolCtx::default();
        if !keys.is_empty() {
            let rows = sqlx::query(
                "SELECT pool::text, mint::text, market::text, sol_is_first FROM venue_pools WHERE pool = ANY($1)",
            )
            .bind(keys)
            .fetch_all(&self.pool)
            .await?;
            for r in rows {
                let pool: String = r.try_get("pool")?;
                let mint: String = r.try_get("mint")?;
                let market: Option<String> = r.try_get("market")?;
                ctx.pools.insert(
                    crate::numeric::parse_key(&pool)?,
                    venue::PoolInfo {
                        mint: crate::numeric::parse_key(&mint)?,
                        market: market.map(|m| crate::numeric::parse_key(&m)).transpose()?,
                        sol_is_first: r.try_get("sol_is_first")?,
                    },
                );
            }
        }
        if !configs.is_empty() {
            let rows = sqlx::query(
                "SELECT DISTINCT ON (account, field) account::text, field, new_value \
                 FROM venue_admin_states WHERE account = ANY($1) AND field IN ($2, $3) \
                 ORDER BY account, field, observed_slot > $4, \
                   CASE WHEN observed_slot <= $4 THEN -observed_slot ELSE observed_slot END",
            )
            .bind(configs)
            .bind(venue::CPMM_PROTOCOL_FEE_RATE)
            .bind(venue::CPMM_FUND_FEE_RATE)
            .bind(slot_i64(raw.slot))
            .fetch_all(&self.pool)
            .await?;
            for r in rows {
                let account: String = r.try_get("account")?;
                let field: String = r.try_get("field")?;
                let value: String = r.try_get("new_value")?;
                let Ok(value) = value.parse::<u64>() else {
                    continue;
                };
                let rates = ctx
                    .cpmm_configs
                    .entry(crate::numeric::parse_key(&account)?)
                    .or_insert(venue::CpmmFeeRates::MAINNET);
                if field == venue::CPMM_PROTOCOL_FEE_RATE {
                    rates.protocol = value;
                } else {
                    rates.fund = value;
                }
            }
        }
        Ok(ctx)
    }
}

async fn insert_tx(
    tx: &mut PgConnection,
    raw: &RawTx,
    d: &Decoded,
    source: &str,
) -> anyhow::Result<bool> {
    let row = sqlx::query(
        "INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, \
           fee_payer, fee, compute_units, err, err_ix_index, err_code, logs_truncated, shortwin_ixs) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::numeric, $9, $10, $11, $12, $13, $14) \
         ON CONFLICT (signature) DO NOTHING RETURNING 1 AS one",
    )
    .bind(raw.signature.to_string())
    .bind(slot_i64(raw.slot))
    .bind(i32::try_from(raw.tx_index).unwrap_or(i32::MAX))
    .bind(raw.block_time)
    .bind(raw.commitment.as_str())
    .bind(source)
    .bind(raw.fee_payer.to_string())
    .bind(raw.fee.to_string())
    .bind(raw.compute_units.and_then(|c| i64::try_from(c).ok()))
    .bind(raw.err.clone())
    .bind(d.err_ix_index)
    .bind(d.err_code)
    .bind(d.logs_truncated)
    .bind(d.shortwin_ixs)
    .fetch_optional(&mut *tx)
    .await
    .context("ingest_txs")?;
    Ok(row.is_some())
}

async fn insert_invocations(tx: &mut PgConnection, raw: &RawTx, d: &Decoded) -> anyhow::Result<()> {
    for inv in &d.invocations {
        sqlx::query(
            "INSERT INTO invocations (signature, ix_index, inner_index, stack_height, slot, \
               tx_index, block_time, program, name, market, args, failed) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) ON CONFLICT DO NOTHING",
        )
        .bind(raw.signature.to_string())
        .bind(i16_idx(inv.ix_index))
        .bind(inv.inner_index.map_or(-1, i16_idx))
        .bind(i16::try_from(inv.stack_height).unwrap_or(i16::MAX))
        .bind(slot_i64(raw.slot))
        .bind(i32::try_from(raw.tx_index).unwrap_or(i32::MAX))
        .bind(raw.block_time)
        .bind(inv.program.to_string())
        .bind(&inv.name)
        .bind(opt_key(inv.market))
        .bind(inv.args.clone())
        .bind(!raw.ok())
        .execute(&mut *tx)
        .await
        .context("invocations")?;
    }
    Ok(())
}

/// Markets of the mints that pool-less venue events name.
async fn mint_markets(
    tx: &mut PgConnection,
    d: &Decoded,
) -> anyhow::Result<HashMap<Pubkey, Pubkey>> {
    let mut mints: Vec<String> = d
        .venue_events
        .iter()
        .filter(|v| v.market.is_none() && v.event.pool.is_none())
        .filter_map(|v| v.event.mint)
        .map(|m| m.to_string())
        .collect();
    mints.sort();
    mints.dedup();
    let mut out = HashMap::new();
    if mints.is_empty() {
        return Ok(out);
    }
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT mint::text, market::text FROM markets WHERE mint = ANY($1)")
            .bind(mints)
            .fetch_all(&mut *tx)
            .await?;
    for (mint, market) in rows {
        out.insert(
            crate::numeric::parse_key(&mint)?,
            crate::numeric::parse_key(&market)?,
        );
    }
    Ok(out)
}

/// Markets of the `(pool, mint)` pairs that other people's trades (and venue events naming a
/// pool) trade: only a pool of the mint's market counts (`sw_is_market_pool`, M8-SEC-6). An
/// attacker's pool that names a listed mint stays without a market.
async fn pool_markets(
    tx: &mut PgConnection,
    d: &Decoded,
) -> anyhow::Result<HashMap<(Pubkey, Pubkey), Pubkey>> {
    let mut pairs: Vec<(Pubkey, Pubkey)> = d
        .trades
        .iter()
        .filter(|t| t.market.is_none())
        .filter_map(|t| t.trade.mint.map(|m| (t.trade.pool, m)))
        .chain(
            d.venue_events
                .iter()
                .filter(|v| v.market.is_none())
                .filter_map(|v| v.event.pool.zip(v.event.mint)),
        )
        .collect();
    pairs.sort();
    pairs.dedup();
    let mut out = HashMap::new();
    if pairs.is_empty() {
        return Ok(out);
    }
    let (pools, mints): (Vec<String>, Vec<String>) = pairs
        .iter()
        .map(|(p, m)| (p.to_string(), m.to_string()))
        .unzip();
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT x.pool, x.mint, m.market::text \
         FROM unnest($1::text[], $2::text[]) AS x(pool, mint) \
         JOIN markets m ON m.mint = x.mint \
         WHERE sw_is_market_pool(m.market, x.pool)",
    )
    .bind(pools)
    .bind(mints)
    .fetch_all(&mut *tx)
    .await
    .context("the markets of venue pools")?;
    for (pool, mint, market) in rows {
        out.insert(
            (
                crate::numeric::parse_key(&pool)?,
                crate::numeric::parse_key(&mint)?,
            ),
            crate::numeric::parse_key(&market)?,
        );
    }
    Ok(out)
}

async fn insert_events(
    tx: &mut PgConnection,
    raw: &RawTx,
    d: &Decoded,
) -> anyhow::Result<(Vec<EventIn>, Option<i64>)> {
    let mut out = Vec::with_capacity(d.events.len());
    let mut max_id = None;
    let sig = raw.signature.to_string();
    for e in &d.events {
        let market = e.market;
        let key = ChainKey {
            slot: slot_i64(raw.slot),
            tx_index: i32::try_from(raw.tx_index).unwrap_or(i32::MAX),
            ix_index: i16_idx(e.ix_index),
            ev_source: e.ev_source as i16,
            ev_seq: i32_of(e.ev_seq),
        };
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO events (signature, ix_index, ev_source, ev_seq, slot, tx_index, block_time, \
               program, invocation, kind, market, position, actor, ev_slot, ev_ts, data, derived) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) \
             ON CONFLICT DO NOTHING RETURNING id",
        )
        .bind(&sig)
        .bind(key.ix_index)
        .bind(key.ev_source)
        .bind(key.ev_seq)
        .bind(key.slot)
        .bind(key.tx_index)
        .bind(raw.block_time)
        .bind(e.program.to_string())
        .bind(&e.invocation)
        .bind(e.kind)
        .bind(opt_key(market))
        .bind(opt_key(e.position))
        .bind(opt_key(e.actor))
        .bind(slot_i64(e.ev_slot))
        .bind(e.ev_ts)
        .bind(&e.data)
        .bind(e.derived.clone())
        .fetch_optional(&mut *tx)
        .await
        .with_context(|| format!("events ({})", e.kind))?;
        if let Some(id) = id {
            max_id = Some(max_id.map_or(id, |m: i64| m.max(id)));
        }
        out.push(EventIn {
            signature: sig.clone(),
            key,
            block_time: raw.block_time,
            fee_payer: raw.fee_payer,
            invocation: e.invocation.clone(),
            kind: e.kind.to_string(),
            market,
            ev_ts: e.ev_ts,
            data: e.data.clone(),
            derived: e.derived.clone(),
        });
    }
    Ok((out, max_id))
}

async fn insert_quarantine(tx: &mut PgConnection, raw: &RawTx, d: &Decoded) -> anyhow::Result<()> {
    for q in &d.quarantine {
        tracing::warn!(
            signature = %raw.signature,
            reason = %q.reason,
            applied = q.applied,
            "quarantined payload"
        );
        sqlx::query(
            "INSERT INTO ingest_quarantine (signature, ix_index, ev_source, ev_seq, program, reason, \
               raw, applied, market) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT DO NOTHING",
        )
        .bind(raw.signature.to_string())
        .bind(i16_idx(q.ix_index))
        .bind(q.ev_source as i16)
        .bind(i32_of(q.ev_seq))
        .bind(q.program.to_string())
        .bind(&q.reason)
        .bind(&q.raw)
        .bind(q.applied)
        .bind(opt_key(q.market))
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

async fn insert_venue_rows(
    tx: &mut PgConnection,
    raw: &RawTx,
    d: &Decoded,
    market_of_mint: &HashMap<Pubkey, Pubkey>,
    market_of_pool: &HashMap<(Pubkey, Pubkey), Pubkey>,
) -> anyhow::Result<u64> {
    let sig = raw.signature.to_string();
    let ts = raw.block_time.unwrap_or(0);
    let mut n = 0;
    for t in &d.trades {
        let Some(mint) = t.trade.mint else { continue };
        let market = t
            .market
            .or_else(|| market_of_pool.get(&(t.trade.pool, mint)).copied());
        sqlx::query(
            "INSERT INTO venue_trades (signature, ix_index, ev_source, ev_seq, slot, tx_index, ts, \
               venue, program, pool, mint, market, side, token_amount, sol_amount, fee_lp, \
               fee_protocol, fee_creator, fee_creator_lamports, creator, user_key, is_shortwin, \
               shortwin_invocation, pool_token_reserve, pool_sol_reserve, price_wad) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14::numeric, \
               $15::numeric, $16::numeric, $17::numeric, $18::numeric, $19::numeric, $20, $21, $22, \
               $23, $24::numeric, $25::numeric, $26::numeric) ON CONFLICT DO NOTHING",
        )
        .bind(&sig)
        .bind(i16_idx(t.ix_index))
        .bind(t.ev_source as i16)
        .bind(i32_of(t.ev_seq))
        .bind(slot_i64(raw.slot))
        .bind(i32::try_from(raw.tx_index).unwrap_or(i32::MAX))
        .bind(ts)
        .bind(t.trade.venue)
        .bind(t.trade.program.to_string())
        .bind(t.trade.pool.to_string())
        .bind(mint.to_string())
        .bind(opt_key(market))
        .bind(t.trade.side.as_str())
        .bind(t.trade.token_amount.to_string())
        .bind(t.trade.sol_amount.to_string())
        .bind(t.trade.fee_lp.map(|v| v.to_string()))
        .bind(t.trade.fee_protocol.map(|v| v.to_string()))
        .bind(t.trade.fee_creator.map(|v| v.to_string()))
        .bind(t.trade.fee_creator_lamports.map(|v| v.to_string()))
        .bind(opt_key(t.trade.creator))
        .bind(t.trade.user.to_string())
        .bind(t.is_shortwin)
        .bind(t.shortwin_invocation.clone())
        .bind(t.trade.pool_token_reserve.map(|v| v.to_string()))
        .bind(t.trade.pool_sol_reserve.map(|v| v.to_string()))
        .bind(t.trade.price_wad().map(|v| v.to_string()))
        .execute(&mut *tx)
        .await
        .context("venue_trades")?;
        n += 1;
    }
    for (pool, mint, market, sol_is_coin) in &d.learned_pools {
        sqlx::query(
            "INSERT INTO venue_pools (pool, venue, program, mint, market, sol_is_first, first_seen_slot) \
             VALUES ($1, 'raydium_v4', $2, $3, $4, $5, $6) \
             ON CONFLICT (pool) DO UPDATE SET sol_is_first = COALESCE(venue_pools.sol_is_first, EXCLUDED.sol_is_first), \
               market = COALESCE(venue_pools.market, EXCLUDED.market), \
               first_seen_slot = LEAST(venue_pools.first_seen_slot, EXCLUDED.first_seen_slot), \
               updated_at = now()",
        )
        .bind(pool.to_string())
        .bind(venue::RAYDIUM_V4.to_string())
        .bind(mint.to_string())
        .bind(opt_key(*market))
        .bind(*sol_is_coin)
        .bind(slot_i64(raw.slot))
        .execute(&mut *tx)
        .await?;
    }
    for v in &d.venue_events {
        let market = v.market.or_else(|| match (v.event.pool, v.event.mint) {
            (Some(pool), Some(mint)) => market_of_pool.get(&(pool, mint)).copied(),
            (None, Some(mint)) => market_of_mint.get(&mint).copied(),
            _ => None,
        });
        sqlx::query(
            "INSERT INTO venue_events (signature, ix_index, ev_source, ev_seq, slot, tx_index, ts, \
               venue, program, kind, pool, mint, market, data) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) ON CONFLICT DO NOTHING",
        )
        .bind(&sig)
        .bind(i16_idx(v.ix_index))
        .bind(v.ev_source as i16)
        .bind(i32_of(v.ev_seq))
        .bind(slot_i64(raw.slot))
        .bind(i32::try_from(raw.tx_index).unwrap_or(i32::MAX))
        .bind(ts)
        .bind(v.event.venue)
        .bind(v.event.program.to_string())
        .bind(v.event.kind)
        .bind(opt_key(v.event.pool))
        .bind(opt_key(v.event.mint))
        .bind(opt_key(market))
        .bind(&v.event.data)
        .execute(&mut *tx)
        .await
        .context("venue_events")?;
    }
    for u in &d.upgrades {
        sqlx::query(
            "INSERT INTO venue_program_upgrades (signature, ix_index, inner_index, venue, program, \
               programdata, slot, ts) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
        )
        .bind(&sig)
        .bind(i16_idx(u.ix_index))
        .bind(u.inner_index.map_or(-1, i16_idx))
        .bind(i16::from(u.upgrade.venue))
        .bind(u.upgrade.program.to_string())
        .bind(u.upgrade.programdata.to_string())
        .bind(slot_i64(raw.slot))
        .bind(raw.block_time)
        .execute(&mut *tx)
        .await?;
    }
    Ok(n)
}

/// `ingest_state` (§3.5): the chain clock and the watermarks a finalized source can advance.
async fn update_state(tx: &mut PgConnection, raw: &RawTx) -> anyhow::Result<()> {
    let finalized = raw.commitment == Commitment::Finalized;
    sqlx::query(
        "UPDATE ingest_state SET \
           latest_block_time = GREATEST(COALESCE(latest_block_time, $1), $1), \
           confirmed_slot = GREATEST(confirmed_slot, $2), \
           finalized_slot = CASE WHEN $3 AND NOT EXISTS ( \
               SELECT 1 FROM ingest_txs WHERE commitment = 'confirmed' AND slot <= $2) \
             THEN GREATEST(finalized_slot, $2) ELSE finalized_slot END, \
           updated_at = now() WHERE id",
    )
    .bind(raw.block_time)
    .bind(slot_i64(raw.slot))
    .bind(finalized)
    .execute(&mut *tx)
    .await
    .context("ingest_state")?;
    Ok(())
}

async fn advance_cursor(
    tx: &mut PgConnection,
    c: &CursorAdvance,
    raw: &RawTx,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO ingest_cursors (stream, address, newest_signature, newest_slot, \
           oldest_signature, oldest_slot) VALUES ($1, $2, $3, $4, $3, $4) \
         ON CONFLICT (stream) DO UPDATE SET \
           newest_signature = CASE WHEN ingest_cursors.newest_slot IS NULL \
               OR EXCLUDED.newest_slot >= ingest_cursors.newest_slot \
             THEN EXCLUDED.newest_signature ELSE ingest_cursors.newest_signature END, \
           newest_slot = GREATEST(COALESCE(ingest_cursors.newest_slot, EXCLUDED.newest_slot), EXCLUDED.newest_slot), \
           oldest_signature = CASE WHEN ingest_cursors.oldest_slot IS NULL \
               OR EXCLUDED.oldest_slot < ingest_cursors.oldest_slot \
             THEN EXCLUDED.oldest_signature ELSE ingest_cursors.oldest_signature END, \
           oldest_slot = LEAST(COALESCE(ingest_cursors.oldest_slot, EXCLUDED.oldest_slot), EXCLUDED.oldest_slot), \
           updated_at = now()",
    )
    .bind(&c.stream)
    .bind(c.address.to_string())
    .bind(raw.signature.to_string())
    .bind(slot_i64(raw.slot))
    .execute(&mut *tx)
    .await
    .context("ingest_cursors")?;
    // The stream's membership (its newest finalized transaction bounds the next listing; a
    // rollback resets the cursor to it, M8-SEC-5).
    sqlx::query(
        "INSERT INTO ingest_tx_streams (signature, stream, slot, tx_index) VALUES ($1, $2, $3, $4) \
         ON CONFLICT DO NOTHING",
    )
    .bind(raw.signature.to_string())
    .bind(&c.stream)
    .bind(slot_i64(raw.slot))
    .bind(i32::try_from(raw.tx_index).unwrap_or(i32::MAX))
    .execute(&mut *tx)
    .await
    .context("ingest_tx_streams")?;
    Ok(())
}

/// The source names (`ingest_txs.source`) checked against the schema's vocabulary.
pub fn source_name(s: &str) -> anyhow::Result<&'static str> {
    crate::model::SOURCES
        .iter()
        .copied()
        .find(|x| *x == s)
        .with_context(|| format!("unknown ingest source {s}"))
}
