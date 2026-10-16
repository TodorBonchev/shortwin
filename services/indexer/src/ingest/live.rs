//! The live source (INDEXER.md §2.3). Owner: INGEST.
//!
//! Default (`ingest.live_source = "logs_subscribe"`): JSON-RPC PubSub `logsSubscribe({mentions:
//! [<address>]}, {commitment: "confirmed"})` — one subscription per tracked address (the PubSub
//! API takes one address per subscription) — plus `slotSubscribe`. Each notified signature is
//! fetched with `getTransaction` at `confirmed` (through `rpc::to_raw_tx`) with its block
//! position (§2.2); a slot's transactions are released to the writer in block order once a later
//! slot has been notified or the slot has been quiet for [`SLOT_QUIET`].
//!
//! Start-up and every reconnect (§3.5): (1) subscribe and buffer; (2) gap-fill every stream from
//! its newest finalized transaction with the backfill (listed at `confirmed`; what the node
//! reports finalized is written at `finalized`, M8-SEC-1); (3) drain the buffer (duplicates are
//! no-ops); (4) stream. Reconnects back off exponentially (1 s → 30 s); a failed gap-fill counts
//! as a failed connection, and the budget resets only after a gap-fill completes, so a gap-fill
//! that can never succeed ends the process (M8-SEC-5). A notified signature the node does not
//! serve is kept in `ingest_missing` and asked for again at every finality tick until it is
//! ingested or proven abandoned (M8-SEC-2). The finality tracker (`ingest::finality`) and the
//! account refresher (`ingest::refresher`) run on the same task between notifications (one
//! writer, §3.1). Yellowstone gRPC (§2.4) is not built in M8 (IX-4): selecting it is a start-up
//! error. URLs and errors are logged through the endpoint's redactor (M8-SEC-15).

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use solana_signature::Signature;
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::Message;

use super::config::BackfillConfig;
use super::finality::{self, BlockCheck, StatusSource, TxStatus};
use super::raw::{Commitment, RawTx};
use super::refresher::Refresher;
use super::rpc::{self, BackfillSource, BlockLookup, Redactor, Rpc, Stream};
use super::source::{SourceItem, TxSource};
use super::writer::Writer;
use crate::IndexerConfig;

/// How long a slot's buffered notifications wait for more of the same slot.
pub const SLOT_QUIET: Duration = Duration::from_millis(1_500);

/// A parsed PubSub message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notification {
    /// `logsNotification`: a transaction mentioning a tracked address landed in `slot`.
    Logs { slot: u64, signature: String },
    /// `slotNotification`.
    Slot { slot: u64, root: u64 },
    /// A subscription confirmation or anything else.
    Other,
}

/// Parses one PubSub text frame.
pub fn parse_notification(text: &str) -> Notification {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Notification::Other;
    };
    match v.get("method").and_then(Value::as_str) {
        Some("logsNotification") => {
            let r = &v["params"]["result"];
            match (
                r["context"]["slot"].as_u64(),
                r["value"]["signature"].as_str(),
            ) {
                (Some(slot), Some(sig)) => Notification::Logs {
                    slot,
                    signature: sig.to_string(),
                },
                _ => Notification::Other,
            }
        }
        Some("slotNotification") => {
            let r = &v["params"]["result"];
            Notification::Slot {
                slot: r["slot"].as_u64().unwrap_or(0),
                root: r["root"].as_u64().unwrap_or(0),
            }
        }
        _ => Notification::Other,
    }
}

/// The subscription requests for `addresses` (one `logsSubscribe` each) and `slotSubscribe`.
pub fn subscribe_requests(addresses: &[solana_pubkey::Pubkey]) -> Vec<String> {
    let mut out: Vec<String> = addresses
        .iter()
        .enumerate()
        .map(|(i, a)| {
            json!({
                "jsonrpc": "2.0",
                "id": i + 1,
                "method": "logsSubscribe",
                "params": [{"mentions": [a.to_string()]}, {"commitment": "confirmed"}],
            })
            .to_string()
        })
        .collect();
    out.push(
        json!({"jsonrpc": "2.0", "id": addresses.len() + 1, "method": "slotSubscribe"}).to_string(),
    );
    out
}

/// Notified signatures waiting for their slot to be released (§2.3).
#[derive(Debug, Default)]
pub struct SlotBuffer {
    slots: BTreeMap<u64, (Instant, Vec<String>)>,
    seen: HashSet<String>,
    newest: u64,
}

impl SlotBuffer {
    /// Buffers a notification (a repeated signature is ignored).
    pub fn push(&mut self, slot: u64, signature: String, now: Instant) {
        self.newest = self.newest.max(slot);
        if !self.seen.insert(signature.clone()) {
            return;
        }
        let e = self.slots.entry(slot).or_insert((now, Vec::new()));
        e.0 = now;
        e.1.push(signature);
    }

    /// The slots ready for release, oldest first: below the newest notified slot, or quiet for
    /// `quiet`.
    pub fn take_ready(&mut self, now: Instant, quiet: Duration) -> Vec<(u64, Vec<String>)> {
        let ready: Vec<u64> = self
            .slots
            .iter()
            .filter(|(slot, (at, _))| **slot < self.newest || now.duration_since(*at) >= quiet)
            .map(|(s, _)| *s)
            .collect();
        let mut out = Vec::new();
        for s in ready {
            if let Some((_, sigs)) = self.slots.remove(&s) {
                out.push((s, sigs));
            }
        }
        if self.seen.len() > 100_000 {
            self.seen.clear();
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// The finality tracker's view of the cluster through JSON-RPC.
pub struct RpcStatus<'a> {
    pub rpc: &'a Rpc,
}

impl StatusSource for RpcStatus<'_> {
    async fn finalized_slot(&self) -> anyhow::Result<u64> {
        self.rpc
            .call("getSlot", || {
                self.rpc.client().get_slot_with_commitment(
                    solana_commitment_config::CommitmentConfig::finalized(),
                )
            })
            .await
    }

    async fn statuses(&self, sigs: &[Signature]) -> anyhow::Result<Vec<Option<TxStatus>>> {
        // With the history: the status cache alone covers ~300 slots, and "no status" there
        // says nothing about an older transaction (M8-SEC-1).
        let res = self
            .rpc
            .call("getSignatureStatuses", || {
                self.rpc.client().get_signature_statuses_with_history(sigs)
            })
            .await?;
        Ok(res
            .value
            .into_iter()
            .map(|s| {
                s.map(|s| TxStatus {
                    slot: s.slot,
                    finalized: matches!(
                        s.confirmation_status,
                        Some(solana_transaction_status_client_types::TransactionConfirmationStatus::Finalized)
                    ),
                })
            })
            .collect())
    }

    async fn fetch_finalized(&self, sig: &Signature) -> anyhow::Result<Option<RawTx>> {
        self.rpc.raw_tx(sig, Commitment::Finalized).await
    }

    async fn block_check(&self, slot: u64, sig: &Signature) -> anyhow::Result<BlockCheck> {
        Ok(
            match self
                .rpc
                .block_signatures_with(slot, Commitment::Finalized, 2)
                .await?
            {
                BlockLookup::Signatures(s) => {
                    let sig = sig.to_string();
                    if s.contains(&sig) {
                        BlockCheck::Has
                    } else {
                        BlockCheck::Lacks
                    }
                }
                BlockLookup::Skipped => BlockCheck::Lacks,
                BlockLookup::Unavailable => BlockCheck::Unknown,
            },
        )
    }
}

/// Notified signatures the node did not serve at `confirmed` (M8-SEC-2): kept until ingested.
async fn record_missing(
    pool: &PgPool,
    signature: &str,
    slot: u64,
    reason: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO ingest_missing (signature, slot, reason) VALUES ($1, $2, $3) \
         ON CONFLICT (signature) DO UPDATE SET attempts = ingest_missing.attempts + 1, \
           last_attempt_at = now(), reason = EXCLUDED.reason",
    )
    .bind(signature)
    .bind(i64::try_from(slot).unwrap_or(i64::MAX))
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// What [`retry_missing`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MissingReport {
    pub ingested: u64,
    pub abandoned: u64,
    pub pending: u64,
}

/// Asks for every `ingest_missing` signature again (at most `limit`, oldest slot first): one the
/// node now serves is written (at `finalized` when it is, else at `confirmed`) and leaves the
/// table; one whose finalized block does not carry it (an abandoned fork) leaves it with a
/// warning; the others stay (an alert while they do).
pub async fn retry_missing(
    writer: &mut Writer,
    rpc: &Rpc,
    src: &impl StatusSource,
    streams: &[Stream],
    limit: i64,
) -> anyhow::Result<MissingReport> {
    let pool = writer.pool().clone();
    // Each row backs off (2^attempts s, at most 5 min), so signatures a node never serves do
    // not take the live loop's RPC budget.
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT signature::text, slot FROM ingest_missing \
         WHERE last_attempt_at <= now() - LEAST(interval '1 second' * power(2, LEAST(attempts, 12)), \
                                                 interval '5 minutes') \
         ORDER BY slot, signature LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&pool)
    .await?;
    let mut report = MissingReport::default();
    if rows.is_empty() {
        return Ok(report);
    }
    let finalized = src.finalized_slot().await?;
    for (s, slot) in rows {
        let sig: Signature = s
            .parse()
            .map_err(|e| anyhow::anyhow!("signature {s}: {e:?}"))?;
        let slot = u64::try_from(slot).unwrap_or(0);
        let got = match rpc.raw_tx(&sig, Commitment::Finalized).await? {
            Some(t) => Some(t),
            None => rpc.raw_tx(&sig, Commitment::Confirmed).await?,
        };
        if let Some(t) = got {
            rpc::apply_item(writer, streams, SourceItem::Tx(Box::new(t))).await?;
            sqlx::query("DELETE FROM ingest_missing WHERE signature = $1")
                .bind(&s)
                .execute(&pool)
                .await?;
            report.ingested += 1;
            continue;
        }
        if slot <= finalized && src.block_check(slot, &sig).await? == BlockCheck::Lacks {
            tracing::warn!(signature = %s, slot, "a notified signature's fork was abandoned");
            sqlx::query("DELETE FROM ingest_missing WHERE signature = $1")
                .bind(&s)
                .execute(&pool)
                .await?;
            report.abandoned += 1;
            continue;
        }
        sqlx::query(
            "UPDATE ingest_missing SET attempts = attempts + 1, last_attempt_at = now() \
             WHERE signature = $1",
        )
        .bind(&s)
        .execute(&pool)
        .await?;
        report.pending += 1;
    }
    writer.end_batch().await?;
    Ok(report)
}

/// The live PubSub source (module docs) as a [`TxSource`]: after each (re)connection it first
/// yields the gap-fill (the backfill machinery at `confirmed`, chain order), then the buffered
/// and streamed notifications, a released slot at a time in block order, each followed by a
/// `Watermark { slot, Confirmed }`; a slot is released once a later slot is notified or it has
/// been quiet for [`SLOT_QUIET`], and a heartbeat watermark comes at least every [`HEARTBEAT`]
/// however busy the subscriptions are, so its driver can run the finality tracker and the
/// refresher (a stream whose notifications never pause for a whole heartbeat used to stall both
/// the releases and the tracker). It never returns
/// `None`; connection failures are retried with backoff (1 s → 30 s) and surface only after
/// [`RECONNECTS`] failures in a row.
pub struct LiveSource<'a> {
    rpc: &'a Rpc,
    pool: PgPool,
    ws_url: String,
    ws_redactor: Redactor,
    streams: Vec<Stream>,
    backfill: BackfillConfig,
    rx: Option<tokio::sync::mpsc::UnboundedReceiver<Notification>>,
    reader: Option<tokio::task::JoinHandle<()>>,
    gap: Option<BackfillSource<'a>>,
    buffer: SlotBuffer,
    ready: VecDeque<SourceItem>,
    newest_slot: u64,
    last_heartbeat: Instant,
    failures: u32,
    backoff: Duration,
}

/// How often an idle live source yields a heartbeat watermark.
pub const HEARTBEAT: Duration = Duration::from_millis(250);
/// Connection (or gap-fill) failures in a row before the live source gives up (the process
/// exits non-zero and its supervisor restarts it, §3.6).
pub const RECONNECTS: u32 = 20;
/// `getTransaction` attempts for a notified signature before it goes to `ingest_missing`.
const NOTIFIED_ATTEMPTS: u32 = 4;

impl<'a> LiveSource<'a> {
    pub fn new(
        rpc: &'a Rpc,
        pool: PgPool,
        ws_url: &str,
        streams: Vec<Stream>,
        backfill: BackfillConfig,
    ) -> Self {
        Self {
            rpc,
            pool,
            ws_url: ws_url.to_string(),
            ws_redactor: Redactor::new(ws_url),
            streams,
            backfill,
            rx: None,
            reader: None,
            gap: None,
            buffer: SlotBuffer::default(),
            ready: VecDeque::new(),
            newest_slot: 0,
            last_heartbeat: Instant::now(),
            failures: 0,
            backoff: Duration::from_secs(1),
        }
    }

    /// (1) Subscribes (the reader task buffers into the channel) and (2) prepares the gap-fill.
    async fn connect(&mut self) -> anyhow::Result<()> {
        let addresses: Vec<_> = self.streams.iter().map(|s| s.address).collect();
        let (ws, _) = tokio_tungstenite::connect_async(self.ws_url.as_str())
            .await
            .map_err(|e| anyhow::anyhow!(self.ws_redactor.redact(&e.to_string())))
            .with_context(|| format!("connecting to {}", self.ws_redactor.shown()))?;
        let (mut sink, mut stream) = ws.split();
        for req in subscribe_requests(&addresses) {
            sink.send(Message::text(req)).await?;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Notification>();
        let reader = tokio::spawn(async move {
            let _sink = sink; // keep the write half (and the connection) alive
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(Message::Text(t)) => {
                        let n = parse_notification(t.as_str());
                        if n != Notification::Other && tx.send(n).is_err() {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        self.rx = Some(rx);
        self.reader = Some(reader);
        self.gap = Some(BackfillSource::new(
            self.rpc,
            self.pool.clone(),
            self.streams.clone(),
            self.backfill.clone(),
            Commitment::Confirmed,
            false,
        ));
        Ok(())
    }

    fn disconnect(&mut self) {
        if let Some(r) = self.reader.take() {
            r.abort();
        }
        self.rx = None;
        self.gap = None;
    }

    /// Fetches the released slots' transactions into `ready`, in block order.
    async fn release(&mut self, slots: Vec<(u64, Vec<String>)>) -> anyhow::Result<()> {
        for (slot, sigs) in slots {
            let mut txs: Vec<RawTx> = Vec::new();
            for s in sigs {
                let sig: Signature = s
                    .parse()
                    .map_err(|e| anyhow::anyhow!("signature {s}: {e:?}"))?;
                // A just-confirmed transaction may take a moment to be served; one that is not
                // served is kept and asked for again (never dropped, M8-SEC-2).
                match self
                    .rpc
                    .raw_tx_patient(&sig, Commitment::Confirmed, NOTIFIED_ATTEMPTS)
                    .await?
                {
                    Some(t) => txs.push(t),
                    None => {
                        tracing::warn!(signature = %s, slot, "notified but not served at confirmed: kept in ingest_missing");
                        record_missing(
                            &self.pool,
                            &s,
                            slot,
                            "notified but not served at confirmed",
                        )
                        .await?;
                    }
                }
            }
            txs.sort_by_key(|t| t.tx_index);
            for t in txs {
                self.ready.push_back(SourceItem::Tx(Box::new(t)));
            }
            self.ready.push_back(SourceItem::Watermark {
                slot,
                commitment: Commitment::Confirmed,
            });
        }
        Ok(())
    }
}

impl LiveSource<'_> {
    /// One more failure in a row: back off, or give up once [`RECONNECTS`] are spent.
    async fn fail(&mut self, e: anyhow::Error, what: &str) -> anyhow::Result<()> {
        self.failures += 1;
        if self.failures >= RECONNECTS {
            return Err(e.context(format!(
                "the live source's reconnect budget is spent ({} failures in a row)",
                self.failures
            )));
        }
        tracing::warn!(error = %self.rpc.redactor().redact(&format!("{e:#}")), what, "retrying");
        tokio::time::sleep(self.backoff).await;
        self.backoff = (self.backoff * 2).min(Duration::from_secs(30));
        Ok(())
    }
}

impl TxSource for LiveSource<'_> {
    fn name(&self) -> &'static str {
        "ws_live"
    }

    async fn next(&mut self) -> anyhow::Result<Option<SourceItem>> {
        loop {
            if let Some(item) = self.ready.pop_front() {
                return Ok(Some(item));
            }
            if self.rx.is_none()
                && let Err(e) = self.connect().await
            {
                self.fail(e, "PubSub connection failed").await?;
                continue;
            }
            // (2) The gap-fill, before anything buffered. The failure budget resets only once
            // it completes: a gap-fill that can never succeed ends the process (M8-SEC-5).
            if let Some(gap) = self.gap.as_mut() {
                match gap.next().await {
                    Ok(Some(item)) => return Ok(Some(item)),
                    Ok(None) => {
                        self.gap = None;
                        self.failures = 0;
                        self.backoff = Duration::from_secs(1);
                        continue;
                    }
                    Err(e) => {
                        self.disconnect();
                        self.fail(e, "gap-fill failed; reconnecting").await?;
                        continue;
                    }
                }
            }
            // (3) Drain and (4) stream.
            let Some(rx) = self.rx.as_mut() else { continue };
            match tokio::time::timeout(HEARTBEAT, rx.recv()).await {
                Ok(Some(Notification::Logs { slot, signature })) => {
                    self.buffer.push(slot, signature, Instant::now());
                }
                Ok(Some(Notification::Slot { slot, .. })) => {
                    self.newest_slot = self.newest_slot.max(slot)
                }
                Ok(Some(Notification::Other)) | Err(_) => {}
                Ok(None) => {
                    tracing::warn!("PubSub connection closed; reconnecting");
                    let rest = self
                        .buffer
                        .take_ready(Instant::now() + SLOT_QUIET, SLOT_QUIET);
                    self.release(rest).await?;
                    self.disconnect();
                    continue;
                }
            }
            // Release what is ready — a later slot was notified, or the slot has been quiet —
            // whether or not notifications keep coming: a busy stream (or 400 ms slot
            // notifications) never leaves the channel silent for a whole heartbeat.
            let ready = self.buffer.take_ready(Instant::now(), SLOT_QUIET);
            if !ready.is_empty() {
                self.release(ready).await?;
                continue;
            }
            // A heartbeat at most every HEARTBEAT, busy or not: the driver's periodic work (the
            // finality tracker, the missing signatures, the refresher) runs.
            if self.last_heartbeat.elapsed() >= HEARTBEAT {
                self.last_heartbeat = Instant::now();
                return Ok(Some(SourceItem::Watermark {
                    slot: 0,
                    commitment: Commitment::Confirmed,
                }));
            }
        }
    }
}

/// `shortwin-indexer live` (see [`super::run_live`]): the live source, the writer, the
/// finality tracker and the refresher, on one task (one writer, §3.1).
pub async fn run(cfg: &IndexerConfig, pool: &PgPool) -> anyhow::Result<()> {
    match cfg.ingest.live_source.as_str() {
        "logs_subscribe" => {}
        "yellowstone" => bail!(
            "ingest.live_source = \"yellowstone\": the Yellowstone gRPC source is not built in M8 \
             (INDEXER.md §2.4, IX-4); use \"logs_subscribe\""
        ),
        other => bail!("unknown ingest.live_source {other:?}"),
    }
    cfg.ingest.check()?;
    let ids = rpc::program_ids(cfg)?;
    let rpc = Rpc::new(&cfg.ingest.rpc_url, cfg.ingest.backfill.max_rps);
    tracing::info!(
        rpc = rpc.redactor().shown(),
        ws = rpc::redact_url(&cfg.ingest.ws_url),
        "live ingest"
    );
    let mut writer = Writer::open(pool, ids, "ws_live").await?;
    let mut refresher = Refresher::new(
        &cfg.ingest.rpc_url,
        cfg.ingest.refresher.clone(),
        ids.shortwin,
    );
    let streams = rpc::streams(cfg, pool, &ids).await?;
    let mut source = LiveSource::new(
        &rpc,
        pool.clone(),
        &cfg.ingest.ws_url,
        streams.clone(),
        cfg.ingest.backfill.clone(),
    );
    let status = RpcStatus { rpc: &rpc };
    let finality_every = Duration::from_millis(cfg.ingest.finality.poll_ms.max(100));
    let mut last_finality = Instant::now();
    loop {
        let Some(item) = source.next().await? else {
            bail!("the live source ended");
        };
        let watermark = matches!(item, SourceItem::Watermark { .. });
        rpc::apply_item(&mut writer, &streams, item).await?;
        if watermark && last_finality.elapsed() >= finality_every {
            last_finality = Instant::now();
            let r =
                finality::run_once(&mut writer, &status, cfg.ingest.finality.status_batch).await?;
            if r.rolled_back > 0 || r.unresolved > 0 {
                tracing::warn!(
                    ?r,
                    "finality tracker rolled back or could not resolve transactions"
                );
            }
            let m = retry_missing(&mut writer, &rpc, &status, &streams, 16).await?;
            if m != MissingReport::default() {
                tracing::warn!(?m, "notified signatures the node had not served");
            }
            if let Err(e) = refresher.tick(pool).await {
                tracing::warn!(error = %rpc.redactor().redact(&format!("{e:#}")), "refresher");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_parse() {
        let n = parse_notification(
            r#"{"jsonrpc":"2.0","method":"logsNotification","params":{"result":{"context":{"slot":5},"value":{"signature":"abc","err":null,"logs":[]}},"subscription":1}}"#,
        );
        assert_eq!(
            n,
            Notification::Logs {
                slot: 5,
                signature: "abc".into()
            }
        );
        let s = parse_notification(
            r#"{"jsonrpc":"2.0","method":"slotNotification","params":{"result":{"parent":4,"root":1,"slot":5},"subscription":2}}"#,
        );
        assert_eq!(s, Notification::Slot { slot: 5, root: 1 });
        assert_eq!(
            parse_notification(r#"{"jsonrpc":"2.0","result":7,"id":1}"#),
            Notification::Other
        );
        let reqs = subscribe_requests(&[solana_pubkey::Pubkey::new_from_array([1; 32])]);
        assert_eq!(reqs.len(), 2);
        assert!(reqs[0].contains("logsSubscribe") && reqs[0].contains("confirmed"));
    }

    #[test]
    fn slots_release_in_order_once_superseded_or_quiet() {
        let t0 = Instant::now();
        let mut b = SlotBuffer::default();
        b.push(10, "a".into(), t0);
        b.push(10, "b".into(), t0);
        b.push(10, "a".into(), t0); // a duplicate notification
        assert!(b.take_ready(t0, SLOT_QUIET).is_empty());
        b.push(11, "c".into(), t0);
        let r = b.take_ready(t0, SLOT_QUIET);
        assert_eq!(r, vec![(10, vec!["a".to_string(), "b".to_string()])]);
        let r = b.take_ready(t0 + SLOT_QUIET, SLOT_QUIET);
        assert_eq!(r, vec![(11, vec!["c".to_string()])]);
        assert!(b.is_empty());
    }
}
