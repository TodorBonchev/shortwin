//! The RPC backfill source and the `getTransaction` → [`RawTx`](super::raw::RawTx) conversion
//! (INDEXER.md §2.2). Owner: INGEST.
//!
//! - Streams (§3.5): `program:<short.win>`, `programdata:<venue>` for the six venue ProgramData
//!   accounts (the latch's raw cause, §7.3), and `pool:<pubkey>` for
//!   tracked pools when third-party trades are ingested (§7.1).
//! - Per stream: `getSignaturesForAddress(address, {before, until, limit: page_limit,
//!   commitment})` pages from the newest back to the cursor (the forward gap), and — while the
//!   stream's history is incomplete — from its oldest frontier back to the history's start (or
//!   `backfill.until_signature`).
//! - The streams' signatures are merged (a transaction reached through two streams is written
//!   once, §3.1), fetched with `getTransaction(sig, {encoding: "base64",
//!   maxSupportedTransactionVersion: 1, commitment})` — v1 (SIMD-0385) keeper transactions need
//!   version 1 — with `fetch_concurrency` in flight and at most `max_rps` requests per second, and
//!   written in chain order `(slot, tx_index)`; each write advances the cursors of the streams
//!   that listed the transaction, in the same Postgres transaction.
//! - `tx_index` (§3.3): `transactionIndex` when the node returns it, else the signature's index
//!   in `getBlock(slot, {transactionDetails: "signatures"})`, cached per slot.
//! - RPC errors are retried with exponential backoff (0.5 s → 30 s, 8 attempts) unless the node
//!   answered with a permanent error code ([`PERMANENT_CODES`]); a source gives up only after its
//!   retry budget and the process exits non-zero (§3.6). Every error text passes through the
//!   endpoint's [`Redactor`]: provider credentials in the URL never reach a log (M8-SEC-15).
//! - **No signature is skipped** (M8-SEC-2): `getTransaction` → `None` only for a JSON `null`
//!   result; a listed signature the node does not serve is asked for again with backoff, and
//!   when the budget is spent the chunk fails before anything of it is written, so no cursor
//!   moves past an unfetched transaction.
//! - Paging runs until an empty page (M8-SEC-11: a provider's page cap below `page_limit` is
//!   never read as the start of history). A bound the node does not know (`-32020`, e.g. a
//!   rolled-back or pruned signature) falls back to slot bounds, with an error log (M8-SEC-5).
//! - The live gap-fill lists at `confirmed` but fetches and writes every signature the node
//!   reports `finalized` at `finalized`: only the head stays at `confirmed` (M8-SEC-1).
//!
//! [`to_raw_tx`] is shared with the replay source (`ingest::replay`) and the live source: a
//! recorded session holds exactly what `getTransaction` returns.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use futures::StreamExt;
use serde_json::json;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_rpc_client_api::client_error::{Error as ClientError, ErrorKind as ClientErrorKind};
use solana_rpc_client_api::config::{RpcBlockConfig, RpcTransactionConfig};
use solana_rpc_client_api::custom_error as codes;
use solana_rpc_client_api::request::{RpcError as ClientRpcError, RpcRequest};
use solana_signature::Signature;
use solana_transaction_status_client_types::option_serializer::OptionSerializer;
use solana_transaction_status_client_types::{
    EncodedConfirmedTransactionWithStatusMeta, TransactionConfirmationStatus, TransactionDetails,
    UiInstruction, UiTransactionEncoding,
};
use sqlx::PgPool;
use tokio::sync::Mutex;

use super::IngestReport;
use super::config::BackfillConfig;
use super::raw::{Commitment, RawIx, RawTopIx, RawTx};
use super::source::{SourceItem, TxSource};
use super::writer::{CursorAdvance, Writer};
use crate::IndexerConfig;

/// Converts a `getTransaction` result into a [`RawTx`]: decodes the base64 transaction
/// (legacy, v0 with `meta.loadedAddresses`, or v1), resolves every instruction's accounts
/// (static keys, then the loaded writable, then the loaded read-only addresses), groups
/// `meta.innerInstructions` (base58 data, `stackHeight`) under their top-level index, and copies
/// fee, compute units, logs and the error. `tx_index` is the block position (§3.3).
pub fn to_raw_tx(
    tx: &EncodedConfirmedTransactionWithStatusMeta,
    tx_index: u32,
    commitment: Commitment,
) -> anyhow::Result<RawTx> {
    let vt = tx
        .transaction
        .transaction
        .decode()
        .context("the transaction does not decode (base64 legacy / v0 / v1 expected)")?;
    let meta = tx
        .transaction
        .meta
        .as_ref()
        .context("no transaction meta")?;
    let mut keys: Vec<Pubkey> = vt.message.static_account_keys().to_vec();
    if let OptionSerializer::Some(la) = &meta.loaded_addresses {
        for k in la.writable.iter().chain(la.readonly.iter()) {
            keys.push(
                k.parse()
                    .map_err(|e| anyhow!("loaded address {k}: {e:?}"))?,
            );
        }
    } else if vt
        .message
        .address_table_lookups()
        .is_some_and(|l| !l.is_empty())
    {
        bail!("a v0 transaction without meta.loadedAddresses");
    }
    let key = |i: u8| -> anyhow::Result<Pubkey> {
        keys.get(usize::from(i))
            .copied()
            .with_context(|| format!("account index {i} out of {} keys", keys.len()))
    };
    let mut instructions = Vec::with_capacity(vt.message.instructions().len());
    for ix in vt.message.instructions() {
        instructions.push(RawTopIx {
            ix: RawIx {
                program_id: key(ix.program_id_index)?,
                accounts: ix
                    .accounts
                    .iter()
                    .map(|&a| key(a))
                    .collect::<anyhow::Result<_>>()?,
                data: ix.data.clone(),
                stack_height: 1,
            },
            inner: Vec::new(),
        });
    }
    if let OptionSerializer::Some(inner) = &meta.inner_instructions {
        for group in inner {
            let top = instructions
                .get_mut(usize::from(group.index))
                .with_context(|| {
                    format!(
                        "inner instructions of a missing instruction {}",
                        group.index
                    )
                })?;
            for ui in &group.instructions {
                let UiInstruction::Compiled(c) = ui else {
                    bail!("a parsed inner instruction (fetch with encoding base64)");
                };
                top.inner.push(RawIx {
                    program_id: key(c.program_id_index)?,
                    accounts: c
                        .accounts
                        .iter()
                        .map(|&a| key(a))
                        .collect::<anyhow::Result<_>>()?,
                    data: bs58::decode(&c.data)
                        .into_vec()
                        .with_context(|| format!("inner instruction data {}", c.data))?,
                    stack_height: c
                        .stack_height
                        .context("an inner instruction without stackHeight (unsupported node)")?,
                });
            }
        }
    }
    // A response without `logMessages` (a node that omits them) cannot show the `emit!` log
    // events (`CoverageGap`, exit-path `MintSafetyTripped`): it is read as a cut log stream, the
    // runtime's own marker, so `ingest_txs.logs_truncated` tells (M8-SEC-13).
    let logs = match &meta.log_messages {
        OptionSerializer::Some(l) => l.clone(),
        _ => vec![LOG_TRUNCATED.to_string()],
    };
    let compute_units = match meta.compute_units_consumed {
        OptionSerializer::Some(c) => Some(c),
        _ => None,
    };
    let err = meta
        .err
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .context("the transaction error")?;
    Ok(RawTx {
        signature: *vt.signatures.first().context("an unsigned transaction")?,
        slot: tx.slot,
        tx_index,
        block_time: tx.block_time,
        commitment,
        fee_payer: *keys.first().context("no fee payer")?,
        fee: meta.fee,
        compute_units,
        err,
        account_keys: keys,
        instructions,
        logs,
    })
}

// ---- endpoint credentials (M8-SEC-15) -----------------------------------------------------------

/// The runtime's own marker of a cut log stream (§5.1).
pub const LOG_TRUNCATED: &str = "Log truncated";

/// What replaces a credential in a log line or an error.
const REDACTED: &str = "REDACTED";

/// Removes provider credentials from text that may quote an endpoint URL: a provider key in the
/// URL's userinfo, path or query (`https://user:pass@host/…`, `https://host/<token>/`,
/// `https://host/?api-key=…`) never reaches a log or an error that is logged (M8-SEC-15).
/// reqwest's errors quote the whole URL, and the live source names its WebSocket endpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Redactor {
    shown: String,
    secrets: Vec<String>,
}

impl Redactor {
    /// The redactor of `url` (anything that is not `scheme://authority/path?query` is shown as
    /// `REDACTED` and redacted whole).
    pub fn new(url: &str) -> Self {
        let Some((scheme, rest)) = url.split_once("://") else {
            return Self {
                shown: REDACTED.to_string(),
                secrets: vec![url.to_string()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect(),
            };
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        let mut secrets = Vec::new();
        let host = match authority.rsplit_once('@') {
            Some((userinfo, host)) => {
                secrets.push(format!("{userinfo}@"));
                secrets.push(userinfo.to_string());
                if let Some((user, pass)) = userinfo.split_once(':') {
                    secrets.extend([user.to_string(), pass.to_string()]);
                }
                host
            }
            None => authority,
        };
        let tail = tail.split('#').next().unwrap_or_default();
        let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
        if !path.trim_matches('/').is_empty() {
            secrets.push(path.to_string());
            secrets.extend(path.split('/').filter(|s| s.len() >= 6).map(str::to_string));
        }
        if !query.is_empty() {
            secrets.push(format!("?{query}"));
            secrets.push(query.to_string());
            for kv in query.split('&') {
                let v = kv.split_once('=').map_or(kv, |(_, v)| v);
                if v.len() >= 6 {
                    secrets.push(v.to_string());
                }
            }
        }
        // Longest first, so a whole secret is replaced before its parts.
        secrets.retain(|s| !s.is_empty() && s != "/");
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        let shown = if secrets.is_empty() {
            format!("{scheme}://{host}{tail}")
        } else {
            format!("{scheme}://{host}/{REDACTED}")
        };
        Self { shown, secrets }
    }

    /// The URL without its credentials (for logs).
    pub fn shown(&self) -> &str {
        &self.shown
    }

    /// `text` with every credential of the URL replaced.
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for s in &self.secrets {
            if out.contains(s.as_str()) {
                out = out.replace(s.as_str(), REDACTED);
            }
        }
        out
    }
}

/// `url` without its credentials (for logs).
pub fn redact_url(url: &str) -> String {
    Redactor::new(url).shown().to_string()
}

// ---- the RPC client with rate limit and retries ------------------------------------------

/// A failed JSON-RPC call, its text redacted ([`Redactor`]); `code` is the node's JSON-RPC
/// error code when it answered with one (find it in an `anyhow` chain with [`rpc_code`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcFailure {
    pub what: String,
    pub code: Option<i64>,
    pub message: String,
}

impl fmt::Display for RpcFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.what, self.message)
    }
}

impl std::error::Error for RpcFailure {}

/// The JSON-RPC error code of the first [`RpcFailure`] in `e`'s chain.
pub fn rpc_code(e: &anyhow::Error) -> Option<i64> {
    e.chain()
        .find_map(|c| c.downcast_ref::<RpcFailure>())
        .and_then(|f| f.code)
}

/// Error codes that retrying cannot change: the node does not have what was asked for
/// (`getSignaturesForAddress`'s `before`/`until` not found, a skipped or cleaned-up slot, no
/// history), or the request itself is wrong.
pub const PERMANENT_CODES: [i64; 7] = [
    codes::JSON_RPC_SERVER_ERROR_FILTER_TRANSACTION_NOT_FOUND,
    codes::JSON_RPC_SERVER_ERROR_SLOT_SKIPPED,
    codes::JSON_RPC_SERVER_ERROR_LONG_TERM_STORAGE_SLOT_SKIPPED,
    codes::JSON_RPC_SERVER_ERROR_BLOCK_CLEANED_UP,
    codes::JSON_RPC_SERVER_ERROR_TRANSACTION_HISTORY_NOT_AVAILABLE,
    -32_601, // method not found
    -32_602, // invalid params
];

/// A block's transaction signatures, in block order (shared from the cache).
pub type BlockSignatures = Arc<Vec<String>>;

/// The JSON-RPC endpoint with the backfill's request budget (§2.2).
pub struct Rpc {
    client: RpcClient,
    redactor: Redactor,
    min_interval: Duration,
    next_at: Mutex<tokio::time::Instant>,
    /// Block signatures by `(slot, finalized)`: a confirmed block may be on a fork the cluster
    /// abandons, so it never answers a finalized question.
    block_cache: Mutex<HashMap<(u64, bool), BlockSignatures>>,
}

const RETRIES: u32 = 8;
/// `getTransaction` attempts for a listed signature that comes back `null` (≈ 30 s of backoff):
/// a lagging or load-balanced node catches up; past this the chunk fails (M8-SEC-2).
pub const UNSERVED_ATTEMPTS: u32 = 6;

/// What `getBlock` says about a slot (the finality tracker's check, M8-SEC-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockLookup {
    /// The block's transaction signatures, in block order.
    Signatures(Arc<Vec<String>>),
    /// No block at this slot on the chain the node follows at that commitment.
    Skipped,
    /// The node cannot tell (cleaned up, not stored, no history): nothing may be inferred.
    Unavailable,
}

impl Rpc {
    /// A client of `url` sending at most `max_rps` requests per second.
    pub fn new(url: &str, max_rps: u32) -> Self {
        Self {
            client: RpcClient::new_with_commitment(url.to_string(), CommitmentConfig::confirmed()),
            redactor: Redactor::new(url),
            min_interval: Duration::from_secs(1) / max_rps.max(1),
            next_at: Mutex::new(tokio::time::Instant::now()),
            block_cache: Mutex::new(HashMap::new()),
        }
    }

    /// The underlying client.
    pub fn client(&self) -> &RpcClient {
        &self.client
    }

    /// The endpoint's credential redactor.
    pub fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    async fn pace(&self) {
        let wait = {
            let mut next = self.next_at.lock().await;
            let now = tokio::time::Instant::now();
            let at = (*next).max(now);
            *next = at + self.min_interval;
            at
        };
        tokio::time::sleep_until(wait).await;
    }

    /// `e` as an [`RpcFailure`]: its code, and its text without the endpoint's credentials.
    pub fn failure(&self, what: &str, e: &ClientError) -> RpcFailure {
        let code = match e.kind() {
            ClientErrorKind::RpcError(ClientRpcError::RpcResponseError { code, .. }) => Some(*code),
            _ => None,
        };
        RpcFailure {
            what: what.to_string(),
            code,
            message: self.redactor.redact(&e.to_string()),
        }
    }

    /// Runs `f` with pacing and retries.
    pub async fn call<T, F, Fut>(&self, what: &str, f: F) -> anyhow::Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, ClientError>>,
    {
        self.call_with(what, RETRIES, f).await
    }

    /// [`Self::call`] with at most `attempts` attempts; a [`PERMANENT_CODES`] answer is returned
    /// at once.
    pub async fn call_with<T, F, Fut>(&self, what: &str, attempts: u32, f: F) -> anyhow::Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, ClientError>>,
    {
        let mut delay = Duration::from_millis(500);
        let mut attempt = 0;
        loop {
            self.pace().await;
            match f().await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let failure = self.failure(what, &e);
                    attempt += 1;
                    let permanent = failure.code.is_some_and(|c| PERMANENT_CODES.contains(&c));
                    if permanent || attempt >= attempts.max(1) {
                        return Err(failure.into());
                    }
                    tracing::warn!(what, attempt, error = %failure.message, "RPC error; retrying");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        }
    }

    /// `getTransaction` (base64, version 1) at `commitment`; `None` only when the node answers
    /// `null` (it does not have the transaction at that commitment). Every error — a transport
    /// failure, an HTTP error, a JSON-RPC error whatever its text — is retried, then returned
    /// (M8-SEC-2: an error is never read as "not found").
    pub async fn transaction(
        &self,
        sig: &Signature,
        commitment: Commitment,
    ) -> anyhow::Result<Option<EncodedConfirmedTransactionWithStatusMeta>> {
        let cfg = RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::Base64),
            commitment: Some(commitment_config(commitment)),
            max_supported_transaction_version: Some(1),
        };
        let params = json!([sig.to_string(), cfg]);
        self.call("getTransaction", || {
            self.client
                .send::<Option<EncodedConfirmedTransactionWithStatusMeta>>(
                    RpcRequest::GetTransaction,
                    params.clone(),
                )
        })
        .await
    }

    /// `getBlock(slot, {transactionDetails: "signatures"})` at `commitment`, classified
    /// ([`BlockLookup`]); `Signatures` are cached per slot. A slot reported skipped below the
    /// node's first available block is `Unavailable` (a ledger jump, not a skip).
    pub async fn block_signatures(
        &self,
        slot: u64,
        commitment: Commitment,
    ) -> anyhow::Result<BlockLookup> {
        self.block_signatures_with(slot, commitment, RETRIES).await
    }

    /// [`Self::block_signatures`] with at most `attempts` attempts (the finality tracker's check
    /// asks twice: a block the node does not have stays unavailable).
    pub async fn block_signatures_with(
        &self,
        slot: u64,
        commitment: Commitment,
        attempts: u32,
    ) -> anyhow::Result<BlockLookup> {
        let key = (slot, commitment == Commitment::Finalized);
        if let Some(s) = self.block_cache.lock().await.get(&key).cloned() {
            return Ok(BlockLookup::Signatures(s));
        }
        let res = self
            .call_with("getBlock", attempts, || {
                self.client.get_block_with_config(
                    slot,
                    RpcBlockConfig {
                        encoding: None,
                        transaction_details: Some(TransactionDetails::Signatures),
                        rewards: Some(false),
                        commitment: Some(commitment_config(commitment)),
                        max_supported_transaction_version: Some(1),
                    },
                )
            })
            .await;
        let block = match res {
            Ok(b) => b,
            Err(e) => {
                return match rpc_code(&e) {
                    Some(
                        codes::JSON_RPC_SERVER_ERROR_SLOT_SKIPPED
                        | codes::JSON_RPC_SERVER_ERROR_LONG_TERM_STORAGE_SLOT_SKIPPED,
                    ) => {
                        let first = self
                            .call("getFirstAvailableBlock", || {
                                self.client.get_first_available_block()
                            })
                            .await?;
                        Ok(if slot >= first {
                            BlockLookup::Skipped
                        } else {
                            BlockLookup::Unavailable
                        })
                    }
                    Some(
                        codes::JSON_RPC_SERVER_ERROR_BLOCK_CLEANED_UP
                        | codes::JSON_RPC_SERVER_ERROR_BLOCK_NOT_AVAILABLE
                        | codes::JSON_RPC_SERVER_ERROR_TRANSACTION_HISTORY_NOT_AVAILABLE
                        | codes::JSON_RPC_SERVER_ERROR_LONG_TERM_STORAGE_UNREACHABLE
                        | codes::JSON_RPC_SERVER_ERROR_BLOCK_STATUS_NOT_AVAILABLE_YET,
                    ) => Ok(BlockLookup::Unavailable),
                    _ => Err(e),
                };
            }
        };
        let s = Arc::new(block.signatures.unwrap_or_default());
        let mut cache = self.block_cache.lock().await;
        if cache.len() > 256 {
            cache.clear();
        }
        cache.insert(key, s.clone());
        Ok(BlockLookup::Signatures(s))
    }

    /// The block position of `sig` in `slot` (`getBlock` signatures, cached per slot).
    pub async fn block_position(
        &self,
        slot: u64,
        sig: &str,
        commitment: Commitment,
    ) -> anyhow::Result<u32> {
        let sigs = match self.block_signatures(slot, commitment).await? {
            BlockLookup::Signatures(s) => s,
            other => bail!("the block of slot {slot} is not available ({other:?})"),
        };
        sigs.iter()
            .position(|s| s == sig)
            .and_then(|p| u32::try_from(p).ok())
            .with_context(|| format!("{sig} not in block {slot}"))
    }

    /// A transaction as a [`RawTx`] with its block position, or `None` if the node lacks it.
    pub async fn raw_tx(
        &self,
        sig: &Signature,
        commitment: Commitment,
    ) -> anyhow::Result<Option<RawTx>> {
        let Some(t) = self.transaction(sig, commitment).await? else {
            return Ok(None);
        };
        let index = match t.transaction_index {
            Some(i) => i,
            None => {
                self.block_position(t.slot, &sig.to_string(), commitment)
                    .await?
            }
        };
        Ok(Some(to_raw_tx(&t, index, commitment)?))
    }

    /// [`Self::raw_tx`] asked up to `attempts` times while the node answers `null` (0.5 s, 1 s,
    /// 2 s, … between attempts): a listed or notified signature a lagging node serves late.
    pub async fn raw_tx_patient(
        &self,
        sig: &Signature,
        commitment: Commitment,
        attempts: u32,
    ) -> anyhow::Result<Option<RawTx>> {
        let mut delay = Duration::from_millis(500);
        for attempt in 0..attempts.max(1) {
            if let Some(t) = self.raw_tx(sig, commitment).await? {
                return Ok(Some(t));
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(8));
            }
        }
        Ok(None)
    }
}

/// The RPC commitment of an ingest commitment.
pub fn commitment_config(c: Commitment) -> CommitmentConfig {
    match c {
        Commitment::Confirmed => CommitmentConfig::confirmed(),
        Commitment::Finalized => CommitmentConfig::finalized(),
    }
}

// ---- streams --------------------------------------------------------------------------------

/// A signature stream (§3.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub name: String,
    pub address: Pubkey,
}

/// The streams the indexer follows: short.win, the six venue ProgramData accounts and, with
/// third-party trades on, the tracked pools (by open interest, then TVL; at most `max_pools`).
/// (The retired straddle helper's `program:<complete_curve>` stream is gone with its ingestion,
/// design pump-final §9; a cursor row a database kept for it is never read again.)
pub async fn streams(
    cfg: &IndexerConfig,
    pool: &PgPool,
    ids: &super::ProgramIds,
) -> anyhow::Result<Vec<Stream>> {
    let mut out = vec![Stream {
        name: format!("program:{}", ids.shortwin),
        address: ids.shortwin,
    }];
    for pd in shortwin::constants::VENUE_PROGRAM_DATA {
        out.push(Stream {
            name: format!("programdata:{pd}"),
            address: pd,
        });
    }
    if cfg.ingest.venue_trades.third_party {
        let limit = i64::try_from(cfg.ingest.venue_trades.max_pools).unwrap_or(i64::MAX);
        let pools: Vec<String> = sqlx::query_scalar(
            "SELECT p FROM ( \
               SELECT COALESCE(m.ray_pool, m.venue_pool, m.bonding_curve)::text AS p, \
                 m.borrowed_q, m.lender_sol FROM markets m) x \
             WHERE p IS NOT NULL ORDER BY borrowed_q DESC, lender_sol DESC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(pool)
        .await?;
        for p in pools {
            let address = crate::numeric::parse_key(&p)?;
            out.push(Stream {
                name: format!("pool:{address}"),
                address,
            });
        }
    }
    Ok(out)
}

/// A listing bound: a signature and, when known, its slot (the fallback bound when the node
/// does not know the signature, M8-SEC-5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bound {
    pub signature: Signature,
    pub slot: Option<u64>,
}

#[derive(Clone, Debug, Default)]
struct Cursor {
    newest: Option<Bound>,
    oldest: Option<Bound>,
    complete: bool,
    /// The stream's newest finalized transaction (`ingest_tx_streams`): the forward listing's
    /// bound. A finalized transaction is never rolled back, so the node keeps it (M8-SEC-5).
    finalized: Option<Bound>,
}

fn bound(sig: Option<String>, slot: Option<i64>) -> anyhow::Result<Option<Bound>> {
    sig.map(|s| {
        Ok(Bound {
            signature: parse_sig(&s)?,
            slot: slot.and_then(|v| u64::try_from(v).ok()),
        })
    })
    .transpose()
}

async fn load_cursor(pool: &PgPool, stream: &str) -> anyhow::Result<Cursor> {
    type Row = (
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<i64>,
        bool,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT newest_signature::text, newest_slot, oldest_signature::text, oldest_slot, \
           backfill_complete FROM ingest_cursors WHERE stream = $1",
    )
    .bind(stream)
    .fetch_optional(pool)
    .await?;
    let finalized: Option<(String, i64)> = sqlx::query_as(
        "SELECT s.signature::text, s.slot FROM ingest_tx_streams s \
         JOIN ingest_txs t ON t.signature = s.signature \
         WHERE s.stream = $1 AND t.commitment = 'finalized' \
         ORDER BY s.slot DESC, s.tx_index DESC LIMIT 1",
    )
    .bind(stream)
    .fetch_optional(pool)
    .await?;
    let (fs, fslot) = finalized.unzip();
    let finalized = bound(fs, fslot)?;
    Ok(match row {
        None => Cursor {
            finalized,
            ..Cursor::default()
        },
        Some((ns, nslot, os, oslot, complete)) => Cursor {
            newest: bound(ns, nslot)?,
            oldest: bound(os, oslot)?,
            complete,
            finalized,
        },
    })
}

/// A signature `getSignaturesForAddress` listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub signature: String,
    pub slot: u64,
    /// The node reported it `finalized`.
    pub finalized: bool,
}

/// Pages `getSignaturesForAddress(address)` from `before` back to `until` (both exclusive),
/// newest first, until an **empty page** (M8-SEC-11: a page shorter than `page_limit` is not the
/// end — a provider may cap pages lower); returns the listed signatures and whether the start of
/// the history was reached (no lower bound). A bound the node does not know (`-32020`: a
/// rolled-back signature, a pruned node) is replaced by its slot — the listing then starts at the
/// newest signature and keeps the slots at or below `before`'s, or stops below `until`'s — and
/// logged as an error (an operator alert, M8-SEC-5).
pub async fn list_signatures(
    rpc: &Rpc,
    address: &Pubkey,
    before: Option<Bound>,
    until: Option<Bound>,
    page_limit: usize,
    commitment: Commitment,
) -> anyhow::Result<(Vec<Listed>, bool)> {
    let mut out = Vec::new();
    let mut before_sig = before.map(|b| b.signature);
    let mut until_sig = until.map(|b| b.signature);
    let mut first_page = true;
    // Slot bounds that replace a signature bound the node refused.
    let mut max_slot: Option<u64> = None;
    let mut min_slot: Option<u64> = None;
    let (before_s, until_s) = (
        before.map(|b| b.signature.to_string()),
        until.map(|b| b.signature.to_string()),
    );
    loop {
        let res = rpc
            .call("getSignaturesForAddress", || {
                rpc.client.get_signatures_for_address_with_config(
                    address,
                    GetConfirmedSignaturesForAddress2Config {
                        before: before_sig,
                        until: until_sig,
                        limit: Some(page_limit),
                        commitment: Some(commitment_config(commitment)),
                    },
                )
            })
            .await;
        let page = match res {
            Ok(p) => p,
            Err(e)
                if rpc_code(&e)
                    == Some(codes::JSON_RPC_SERVER_ERROR_FILTER_TRANSACTION_NOT_FOUND) =>
            {
                if let (Some(u), Some(_), Some(slot)) =
                    (until, until_sig, until.and_then(|u| u.slot))
                {
                    tracing::error!(
                        %address,
                        until = %u.signature,
                        slot,
                        "the node does not know the listing's lower bound (rolled back or pruned); \
                         listing down to its slot instead"
                    );
                    until_sig = None;
                    min_slot = Some(slot);
                    continue;
                }
                if let (true, Some(b), Some(slot)) =
                    (first_page, before, before.and_then(|b| b.slot))
                    && before_sig == Some(b.signature)
                {
                    tracing::error!(
                        %address,
                        before = %b.signature,
                        slot,
                        "the node does not know the listing's upper bound (rolled back or pruned); \
                         listing from the newest signature down instead"
                    );
                    before_sig = None;
                    max_slot = Some(slot);
                    continue;
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        first_page = false;
        let Some(last) = page.last() else {
            break;
        };
        let next_before: Signature = last
            .signature
            .parse()
            .map_err(|e| anyhow!("signature: {e:?}"))?;
        let mut done = false;
        for s in &page {
            if min_slot.is_some_and(|m| s.slot < m)
                || until_s.as_deref() == Some(s.signature.as_str())
            {
                done = true;
                break;
            }
            if max_slot.is_some_and(|m| s.slot > m)
                || before_s.as_deref() == Some(s.signature.as_str())
            {
                continue;
            }
            out.push(Listed {
                signature: s.signature.clone(),
                slot: s.slot,
                finalized: commitment == Commitment::Finalized
                    || matches!(
                        s.confirmation_status,
                        Some(TransactionConfirmationStatus::Finalized)
                    ),
            });
        }
        if done {
            break;
        }
        before_sig = Some(next_before);
    }
    Ok((out, until.is_none() && min_slot.is_none()))
}

fn parse_sig(s: &str) -> anyhow::Result<Signature> {
    s.parse().map_err(|e| anyhow!("signature {s}: {e:?}"))
}

/// The streams whose cursor a transaction advances: those whose address it mentions (§3.5).
pub fn cursors_for(streams: &[Stream], raw: &RawTx) -> Vec<CursorAdvance> {
    streams
        .iter()
        .filter(|s| raw.account_keys.contains(&s.address))
        .map(|s| CursorAdvance {
            stream: s.name.clone(),
            address: s.address,
        })
        .collect()
}

/// The RPC backfill as a [`TxSource`]: lists every stream's forward gap (and, with `history`,
/// its incomplete past), merges the signatures (one transaction listed by two streams is
/// yielded once), then fetches them a chunk of whole slots at a time and yields them in chain
/// order `(slot, tx_index)`, each chunk followed by a `Watermark` (module docs). At `confirmed`
/// (the live gap-fill) a signature the node reports finalized is fetched and yielded at
/// `finalized` (M8-SEC-1).
pub struct BackfillSource<'a> {
    rpc: &'a Rpc,
    pool: PgPool,
    streams: Vec<Stream>,
    cfg: BackfillConfig,
    commitment: Commitment,
    history: bool,
    listed: bool,
    by_slot: BTreeMap<u64, Vec<(String, Commitment)>>,
    ready: std::collections::VecDeque<SourceItem>,
    /// Streams whose history start was reached: `(stream index, oldest slot listed)`.
    completed: Vec<(usize, Option<u64>)>,
    until: Option<Signature>,
}

impl<'a> BackfillSource<'a> {
    pub fn new(
        rpc: &'a Rpc,
        pool: PgPool,
        streams: Vec<Stream>,
        cfg: BackfillConfig,
        commitment: Commitment,
        history: bool,
    ) -> Self {
        Self {
            rpc,
            pool,
            streams,
            cfg,
            commitment,
            history,
            listed: false,
            by_slot: BTreeMap::new(),
            ready: std::collections::VecDeque::new(),
            completed: Vec::new(),
            until: None,
        }
    }

    /// The streams this source lists.
    pub fn streams(&self) -> &[Stream] {
        &self.streams
    }

    async fn list(&mut self) -> anyhow::Result<()> {
        self.cfg.check()?;
        let until_cfg = self
            .cfg
            .until_signature
            .as_deref()
            .map(parse_sig)
            .transpose()?;
        self.until = until_cfg;
        let until_cfg_bound = until_cfg.map(|signature| Bound {
            signature,
            slot: None,
        });
        let mut seen: HashMap<String, (u64, bool)> = HashMap::new();
        for (i, s) in self.streams.iter().enumerate() {
            let cur = load_cursor(&self.pool, &s.name).await?;
            // Forward: from the newest back to the stream's newest finalized transaction (never
            // rolled back), else its cursor; the confirmed head after it is listed again
            // (duplicates are no-ops, §3.1).
            let forward_until = cur.finalized.or(cur.newest).or(until_cfg_bound);
            let (fwd, start_reached) = list_signatures(
                self.rpc,
                &s.address,
                None,
                forward_until,
                self.cfg.page_limit,
                self.commitment,
            )
            .await?;
            let mut sigs = fwd;
            let mut complete = cur.newest.is_none() && (start_reached || until_cfg.is_some());
            if self.history && cur.newest.is_some() && !cur.complete {
                let (back, reached) = list_signatures(
                    self.rpc,
                    &s.address,
                    cur.oldest,
                    until_cfg_bound,
                    self.cfg.page_limit,
                    self.commitment,
                )
                .await?;
                sigs.extend(back);
                complete = reached || until_cfg.is_some();
            }
            if self.history && complete {
                self.completed.push((i, sigs.iter().map(|l| l.slot).min()));
            }
            for l in sigs {
                seen.entry(l.signature)
                    .and_modify(|e| e.1 |= l.finalized)
                    .or_insert((l.slot, l.finalized));
            }
        }
        for (sig, (slot, finalized)) in seen {
            let c = if finalized {
                Commitment::Finalized
            } else {
                self.commitment
            };
            self.by_slot.entry(slot).or_default().push((sig, c));
        }
        self.listed = true;
        Ok(())
    }

    /// Fetches the next chunk of whole slots into `ready`. A listed signature the node does not
    /// serve after [`UNSERVED_ATTEMPTS`] fails the chunk before any of it is yielded (M8-SEC-2):
    /// the cursors stay below it and the next run (or reconnect) lists it again.
    async fn fetch_chunk(&mut self) -> anyhow::Result<()> {
        let chunk = self.cfg.fetch_concurrency.max(1) * 4;
        let mut sigs: Vec<(String, Commitment)> = Vec::new();
        let mut last_slot = 0;
        while sigs.len() < chunk {
            let Some((&slot, _)) = self.by_slot.iter().next() else {
                break;
            };
            sigs.extend(self.by_slot.remove(&slot).unwrap_or_default());
            last_slot = slot;
        }
        let rpc = self.rpc;
        let fetched: Vec<anyhow::Result<Option<RawTx>>> = futures::stream::iter(sigs.clone())
            .map(|(s, c): (String, Commitment)| async move {
                let sig = parse_sig(&s)?;
                rpc.raw_tx_patient(&sig, c, UNSERVED_ATTEMPTS).await
            })
            .buffered(self.cfg.fetch_concurrency.max(1))
            .collect()
            .await;
        let mut txs: Vec<RawTx> = Vec::with_capacity(fetched.len());
        for ((s, c), r) in sigs.iter().zip(fetched) {
            match r? {
                Some(t) => txs.push(t),
                None => bail!(
                    "{s} was listed but getTransaction at {} returned null {UNSERVED_ATTEMPTS} \
                     times: stopping before it (no cursor moves past an unfetched transaction; \
                     the next run lists it again)",
                    c.as_str()
                ),
            }
        }
        txs.sort_by_key(|t| (t.slot, t.tx_index));
        for t in txs {
            self.ready.push_back(SourceItem::Tx(Box::new(t)));
        }
        self.ready.push_back(SourceItem::Watermark {
            slot: last_slot,
            commitment: self.commitment,
        });
        Ok(())
    }

    /// Records the streams whose history is complete (`backfill_complete`, `genesis_slot`).
    pub async fn finish(&self) -> anyhow::Result<()> {
        for (i, oldest_slot) in &self.completed {
            let s = &self.streams[*i];
            sqlx::query(
                "INSERT INTO ingest_cursors (stream, address, backfill_complete) VALUES ($1, $2, TRUE) \
                 ON CONFLICT (stream) DO UPDATE SET backfill_complete = TRUE, updated_at = now()",
            )
            .bind(&s.name)
            .bind(s.address.to_string())
            .execute(&self.pool)
            .await?;
            if *i == 0 && self.until.is_none() {
                sqlx::query(
                    "UPDATE ingest_state SET genesis_slot = $1 WHERE id AND genesis_slot IS NULL",
                )
                .bind(oldest_slot.and_then(|s| i64::try_from(s).ok()))
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }
}

impl TxSource for BackfillSource<'_> {
    fn name(&self) -> &'static str {
        "rpc_backfill"
    }

    async fn next(&mut self) -> anyhow::Result<Option<SourceItem>> {
        if !self.listed {
            self.list().await?;
        }
        if self.ready.is_empty() && !self.by_slot.is_empty() {
            self.fetch_chunk().await?;
        }
        Ok(self.ready.pop_front())
    }
}

/// Drives a [`TxSource`] into `writer` (module docs): each transaction with the cursors of the
/// streams it mentions, the batch ended at every watermark (late events rebuilt, §3.3), a
/// source's rollback applied (§3.4).
pub async fn drive(
    source: &mut impl TxSource,
    writer: &mut Writer,
    streams: &[Stream],
) -> anyhow::Result<()> {
    while let Some(item) = source.next().await? {
        apply_item(writer, streams, item).await?;
    }
    writer.end_batch().await?;
    Ok(())
}

/// Applies one source item through `writer`.
pub async fn apply_item(
    writer: &mut Writer,
    streams: &[Stream],
    item: SourceItem,
) -> anyhow::Result<()> {
    match item {
        SourceItem::Tx(t) => {
            let cursors = cursors_for(streams, &t);
            writer.write_with(&t, &cursors).await?;
        }
        SourceItem::Watermark { slot, commitment } => {
            writer.end_batch().await?;
            // A finalized source's rows advance `finalized_slot` as they are written.
            if commitment == Commitment::Confirmed {
                sqlx::query(
                    "UPDATE ingest_state SET confirmed_slot = GREATEST(confirmed_slot, $1), \
                       updated_at = now() WHERE id",
                )
                .bind(i64::try_from(slot).unwrap_or(i64::MAX))
                .execute(writer.pool())
                .await?;
            }
        }
        SourceItem::Rollback { slot } => {
            super::finality::rollback_from_slot(writer.pool(), slot, "source fork switch").await?;
            writer.end_batch_force().await?;
        }
    }
    Ok(())
}

/// Fills every stream's forward gap (and, with `history`, its incomplete past) through
/// `writer`, in chain order (module docs).
pub async fn backfill_streams(
    rpc: &Rpc,
    writer: &mut Writer,
    streams: &[Stream],
    cfg: &BackfillConfig,
    commitment: Commitment,
    history: bool,
) -> anyhow::Result<()> {
    let mut src = BackfillSource::new(
        rpc,
        writer.pool().clone(),
        streams.to_vec(),
        cfg.clone(),
        commitment,
        history,
    );
    drive(&mut src, writer, streams).await?;
    src.finish().await
}

/// The configured program id.
pub fn program_ids(cfg: &IndexerConfig) -> anyhow::Result<super::ProgramIds> {
    Ok(super::ProgramIds {
        shortwin: crate::numeric::parse_key(&cfg.ingest.program_id).context("ingest.program_id")?,
    })
}

/// `shortwin-indexer backfill` (see [`super::run_backfill`]).
pub async fn run_backfill(cfg: &IndexerConfig, pool: &PgPool) -> anyhow::Result<IngestReport> {
    cfg.ingest.check()?;
    let ids = program_ids(cfg)?;
    let rpc = Rpc::new(&cfg.ingest.rpc_url, cfg.ingest.backfill.max_rps);
    let mut writer = Writer::open(pool, ids, "rpc_backfill").await?;
    let streams = streams(cfg, pool, &ids).await?;
    let res = backfill_streams(
        &rpc,
        &mut writer,
        &streams,
        &cfg.ingest.backfill,
        Commitment::Finalized,
        true,
    )
    .await;
    let report = writer.close().await;
    res?;
    report
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// M8-SEC-15: provider credentials in the endpoint URL never reach a log line or an error.
    #[test]
    fn endpoint_credentials_are_redacted() {
        let r = Redactor::new(
            "https://user:hunter2@rpc.example.com:8899/v2/ALCHEMYKEY123?api-key=SECRET-HELIUS-KEY&x=1",
        );
        assert_eq!(r.shown(), "https://rpc.example.com:8899/REDACTED");
        let text = "error sending request for url \
            (https://user:hunter2@rpc.example.com:8899/v2/ALCHEMYKEY123?api-key=SECRET-HELIUS-KEY&x=1)";
        let out = r.redact(text);
        for secret in ["hunter2", "user:", "ALCHEMYKEY123", "SECRET-HELIUS-KEY"] {
            assert!(!out.contains(secret), "{secret} in {out}");
        }
        assert!(out.contains("rpc.example.com"));
        // reqwest normalises a bare host to `host/`: the query is still found.
        let r = Redactor::new("http://127.0.0.1:9?api-key=SECRET");
        assert!(
            !r.redact("http://127.0.0.1:9/?api-key=SECRET")
                .contains("SECRET")
        );
        assert_eq!(r.shown(), "http://127.0.0.1:9/REDACTED");
        // A URL without credentials is shown as is.
        assert_eq!(
            Redactor::new("ws://127.0.0.1:8900").shown(),
            "ws://127.0.0.1:8900"
        );
        assert_eq!(
            redact_url("wss://host/?token=abcdef"),
            "wss://host/REDACTED"
        );
    }

    /// M8-SEC-15, end to end: what `Rpc::call` returns (and logs) for a refused connection.
    #[tokio::test]
    async fn rpc_errors_carry_no_credentials() {
        let rpc = Rpc::new("http://127.0.0.1:9/?api-key=SECRET-HELIUS-KEY", 1_000);
        let e = rpc
            .call_with("getSlot", 1, || rpc.client().get_slot())
            .await
            .expect_err("nothing listens on :9");
        let text = format!("{e:#}");
        assert!(!text.contains("SECRET-HELIUS-KEY"), "{text}");
        assert!(text.contains("getSlot"), "{text}");
    }

    #[test]
    fn a_v0_transaction_resolves_loaded_addresses_and_groups_inner_instructions() {
        use solana_message::{MessageHeader, VersionedMessage, v0};
        use solana_transaction::versioned::VersionedTransaction;
        let payer = Pubkey::new_from_array([1; 32]);
        let program = Pubkey::new_from_array([2; 32]);
        let loaded_w = Pubkey::new_from_array([3; 32]);
        let loaded_r = Pubkey::new_from_array([4; 32]);
        let msg = v0::Message {
            header: MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 1,
            },
            account_keys: vec![payer, program],
            recent_blockhash: solana_hash::Hash::default(),
            instructions: vec![solana_message::compiled_instruction::CompiledInstruction {
                program_id_index: 1,
                accounts: vec![0, 2, 3],
                data: vec![9, 9],
            }],
            address_table_lookups: vec![solana_message::v0::MessageAddressTableLookup {
                account_key: Pubkey::new_from_array([7; 32]),
                writable_indexes: vec![0],
                readonly_indexes: vec![1],
            }],
        };
        let vt = VersionedTransaction {
            signatures: vec![Signature::from([5u8; 64])],
            message: VersionedMessage::V0(msg),
        };
        let bytes = wincode_bytes(&vt);
        let json = serde_json::json!({
            "slot": 42,
            "blockTime": 1_790_000_000,
            "transaction": [base64_of(&bytes), "base64"],
            "meta": {
                "err": {"InstructionError": [0, {"Custom": 6001}]},
                "status": {"Err": {"InstructionError": [0, {"Custom": 6001}]}},
                "fee": 5000,
                "preBalances": [], "postBalances": [],
                "innerInstructions": [{"index": 0, "instructions": [
                    {"programIdIndex": 3, "accounts": [2], "data": bs58::encode([1u8, 2]).into_string(), "stackHeight": 2}
                ]}],
                "logMessages": ["Program log: x"],
                "loadedAddresses": {"writable": [loaded_w.to_string()], "readonly": [loaded_r.to_string()]},
                "computeUnitsConsumed": 1234
            },
            "version": 0
        });
        let t: EncodedConfirmedTransactionWithStatusMeta =
            serde_json::from_value(json).expect("rpc shape");
        let raw = to_raw_tx(&t, 3, Commitment::Confirmed).expect("converts");
        assert_eq!(raw.tx_index, 3);
        assert_eq!(raw.account_keys, vec![payer, program, loaded_w, loaded_r]);
        assert_eq!(
            raw.instructions[0].ix.accounts,
            vec![payer, loaded_w, loaded_r]
        );
        assert_eq!(raw.instructions[0].inner[0].program_id, loaded_r);
        assert_eq!(raw.instructions[0].inner[0].data, vec![1, 2]);
        assert_eq!(raw.instructions[0].inner[0].stack_height, 2);
        assert_eq!(raw.compute_units, Some(1234));
        assert!(!raw.ok());
        let (ix, code) = crate::ingest::decode::instruction_error(raw.err.as_ref().expect("err"));
        assert_eq!((ix, code), (Some(0), Some(6001)));
    }

    /// The wire format (`EncodedTransaction::decode` reads it back): legacy and v0 put the
    /// signatures first (a short-vec count below 128 is one byte), then the message; v1
    /// (SIMD-0385) puts the message first, then the header's signatures without a count.
    pub(crate) fn wincode_bytes(
        vt: &solana_transaction::versioned::VersionedTransaction,
    ) -> Vec<u8> {
        if let solana_message::VersionedMessage::V1(_) = &vt.message {
            let mut out = vt.message.serialize();
            for s in &vt.signatures {
                out.extend_from_slice(s.as_ref());
            }
            return out;
        }
        let mut out = vec![u8::try_from(vt.signatures.len()).expect("< 128 signatures")];
        for s in &vt.signatures {
            out.extend_from_slice(s.as_ref());
        }
        out.extend(vt.message.serialize());
        out
    }

    fn base64_of(b: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(b)
    }
}
