//! Session recording from LiteSVM (INDEXER.md §11.3). Owner: ANALYTICS + REPLAY.
//!
//! The recording hook in `tests/litesvm/src/record.rs` (`Env::start_recording` /
//! `Env::take_recording`, fed by `Env::send` and by the keeper backend's `send_with_hooks` and
//! bundle path through `Env::execute_recorded`) hands this module each executed
//! `VersionedTransaction`, its loaded lookup-table addresses, the `Clock` it ran at, its
//! position in the slot and LiteSVM's `TransactionMetadata` (or the failure);
//! [`session_tx`] renders that exactly as JSON-RPC `getTransaction(sig, {encoding: "base64",
//! maxSupportedTransactionVersion: 1})` would return it:
//!
//! - `transaction: [<base64 of the wire bytes>, "base64"]` (legacy, v0 and v1 alike), `version`;
//! - `meta`: `err` / `status`, `fee`, `preBalances` / `postBalances` (every key, static then
//!   loaded), `innerInstructions` (only the top-level instructions that have some, each inner
//!   instruction compiled against the full key list with base58 data and its `stackHeight`),
//!   `logMessages`, `loadedAddresses`, `computeUnitsConsumed`, `returnData` (when set), empty
//!   token balances and rewards;
//! - `slot`, `blockTime` (the clock's unix time), `transactionIndex` (the per-slot counter).
//!
//! [`to_session`] adds the header and the account checkpoints (each after the transaction it
//! follows); [`write`] / [`write_to_dir`] store the JSON Lines file of
//! `shortwin_indexer::ingest::session`.

use std::path::{Path, PathBuf};

use anchor_lang::prelude::Pubkey;
use anyhow::{Context, anyhow};
use base64::Engine;
use shortwin_indexer::ingest::session::{
    AccountsCheckpoint, SESSION_VERSION, Session, SessionAccount, SessionHeader, SessionLine,
    SessionTx, SessionWriter,
};
use shortwin_litesvm_tests::record::{AccountCheckpoint, RecordedTx, Recording};
use solana_message::v0::LoadedAddresses;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status_client_types::option_serializer::OptionSerializer;
use solana_transaction_status_client_types::{
    EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction,
    EncodedTransactionWithStatusMeta, InnerInstruction, InnerInstructions,
    TransactionBinaryEncoding, TransactionStatusMeta, UiReturnDataEncoding,
    UiTransactionReturnData, UiTransactionStatusMeta,
};

/// `SessionHeader.recorder` of every session this crate writes.
pub const RECORDER: &str = "litesvm";

/// The environment variable that makes the session tests also write their sessions into a
/// directory (INDEXER.md §11.3: `SHORTWIN_RECORD_SESSIONS=<dir>` regenerates the committed set).
pub const RECORD_SESSIONS_ENV: &str = "SHORTWIN_RECORD_SESSIONS";

/// A session header for `scenario` on the program build `build` (`"test-venue"` or
/// `"release"`), with the program id this repository builds.
pub fn header(scenario: &str, build: &str, notes: Option<&str>) -> SessionHeader {
    SessionHeader {
        version: SESSION_VERSION,
        program_id: shortwin::ID.to_string(),
        recorder: RECORDER.to_string(),
        scenario: scenario.to_string(),
        build: build.to_string(),
        notes: notes.map(str::to_string),
    }
}

/// The transaction's wire bytes, base64 (the encoding a JSON-RPC client sends and
/// `getTransaction` returns: wincode of the `VersionedTransaction`, v1 included).
pub fn wire_base64(tx: &VersionedTransaction) -> anyhow::Result<String> {
    let params = shortwin_services_common::rpc::send_bundle_params(std::slice::from_ref(tx))?;
    params[0][0]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no encoded transaction in {params}"))
}

/// The `getTransaction` meta of a recorded transaction.
pub fn status_meta(r: &RecordedTx) -> UiTransactionStatusMeta {
    let meta = r.meta();
    let inner: Vec<InnerInstructions> = meta
        .inner_instructions
        .iter()
        .enumerate()
        .filter(|(_, list)| !list.is_empty())
        .map(|(i, list)| InnerInstructions {
            index: u8::try_from(i).unwrap_or(u8::MAX),
            instructions: list
                .iter()
                .map(|ii| InnerInstruction {
                    instruction: ii.instruction.clone(),
                    stack_height: Some(u32::from(ii.stack_height)),
                })
                .collect(),
        })
        .collect();
    let status = match &r.result {
        Ok(_) => Ok(()),
        Err(f) => Err(f.err.clone()),
    };
    let mut ui = UiTransactionStatusMeta::from(TransactionStatusMeta {
        status,
        fee: meta.fee,
        pre_balances: r.pre_balances.clone(),
        post_balances: r.post_balances.clone(),
        inner_instructions: Some(inner),
        log_messages: Some(meta.logs.clone()),
        pre_token_balances: Some(Vec::new()),
        post_token_balances: Some(Vec::new()),
        rewards: Some(Vec::new()),
        loaded_addresses: LoadedAddresses {
            writable: r.loaded_writable.clone(),
            readonly: r.loaded_readonly.clone(),
        },
        return_data: None,
        compute_units_consumed: Some(meta.compute_units_consumed),
        cost_units: None,
    });
    if !meta.return_data.data.is_empty() {
        ui.return_data = OptionSerializer::Some(UiTransactionReturnData {
            program_id: meta.return_data.program_id.to_string(),
            data: (
                base64::engine::general_purpose::STANDARD.encode(&meta.return_data.data),
                UiReturnDataEncoding::Base64,
            ),
        });
    }
    ui
}

/// A recorded transaction as `getTransaction` returns it (module docs).
pub fn session_tx(r: &RecordedTx) -> anyhow::Result<SessionTx> {
    let transaction = EncodedConfirmedTransactionWithStatusMeta {
        slot: r.slot,
        transaction: EncodedTransactionWithStatusMeta {
            transaction: EncodedTransaction::Binary(
                wire_base64(&r.tx)?,
                TransactionBinaryEncoding::Base64,
            ),
            meta: Some(status_meta(r)),
            version: Some(r.tx.version()),
        },
        block_time: Some(r.unix_timestamp),
        transaction_index: Some(r.tx_index),
    };
    Ok(SessionTx {
        slot: r.slot,
        tx_index: r.tx_index,
        transaction,
    })
}

/// A checkpoint in the session format; `after` is the signature of the transaction it follows.
pub fn session_checkpoint(c: &AccountCheckpoint, after: Option<String>) -> AccountsCheckpoint {
    let engine = base64::engine::general_purpose::STANDARD;
    AccountsCheckpoint {
        after,
        slot: c.slot,
        unix_timestamp: c.unix_timestamp,
        accounts: c
            .accounts
            .iter()
            .map(|(k, a)| SessionAccount {
                pubkey: k.to_string(),
                owner: a.owner.to_string(),
                lamports: a.lamports,
                data: engine.encode(&a.data),
                executable: a.executable,
            })
            .collect(),
        closed: c.closed.iter().map(Pubkey::to_string).collect(),
    }
}

/// Whether `r` mentions short.win: what the indexer's program signature stream returns
/// (`getSignaturesForAddress(program)`, INDEXER.md §2.2). A venue swap or an LP action of the
/// scenario's own actors does not, and is left out of the session (its block position stays:
/// `tx_index` keeps the gap a real block would show).
pub fn mentions_programs(r: &RecordedTx) -> bool {
    r.account_keys().contains(&shortwin::ID)
}

/// A whole recording as a session (module docs): the transactions [`mentions_programs`]
/// keeps, and every checkpoint after the kept transaction it follows.
pub fn to_session(rec: &Recording, header: SessionHeader) -> anyhow::Result<Session> {
    to_session_with(rec, header, mentions_programs)
}

/// [`to_session`] with another transaction filter (`|_| true` keeps every transaction).
pub fn to_session_with(
    rec: &Recording,
    header: SessionHeader,
    keep: impl Fn(&RecordedTx) -> bool,
) -> anyhow::Result<Session> {
    // kept[i] = how many kept transactions precede recorded transaction i.
    let mut kept_before = Vec::with_capacity(rec.txs.len() + 1);
    let mut txs = Vec::new();
    let mut last_kept: Vec<Option<String>> = Vec::with_capacity(rec.txs.len() + 1);
    let mut last: Option<String> = None;
    for r in &rec.txs {
        kept_before.push(txs.len());
        last_kept.push(last.clone());
        if keep(r) {
            txs.push(session_tx(r)?);
            last = Some(r.signature().to_string());
        }
    }
    kept_before.push(txs.len());
    last_kept.push(last);
    let checkpoints = rec
        .checkpoints
        .iter()
        .map(|(k, c)| {
            let at = (*k).min(rec.txs.len());
            (
                kept_before[at],
                session_checkpoint(c, last_kept[at].clone()),
            )
        })
        .collect();
    Ok(Session {
        header,
        txs,
        checkpoints,
    })
}

/// The session's lines in file order: the header, then the checkpoints and transactions
/// interleaved (each checkpoint after the transaction it follows).
pub fn lines(s: &Session) -> Vec<SessionLine> {
    let mut out = vec![SessionLine::Header(s.header.clone())];
    let mut cps = s.checkpoints.iter().peekable();
    for k in 0..=s.txs.len() {
        while let Some((_, c)) = cps.next_if(|(at, _)| *at == k) {
            out.push(SessionLine::Accounts(c.clone()));
        }
        if let Some(tx) = s.txs.get(k) {
            out.push(SessionLine::Tx(Box::new(tx.clone())));
        }
    }
    out
}

/// Writes `s` to `path` (JSON Lines, `shortwin_indexer::ingest::session`).
pub fn write(path: &Path, s: &Session) -> anyhow::Result<()> {
    let mut w = SessionWriter::create(path, &s.header)?;
    for line in lines(s).into_iter().skip(1) {
        w.line(&line)?;
    }
    w.finish()
}

/// Writes `s` as `<dir>/<name>.jsonl` and returns the path.
pub fn write_to_dir(dir: &Path, name: &str, s: &Session) -> anyhow::Result<PathBuf> {
    let path = dir.join(format!("{name}.jsonl"));
    write(&path, s).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Where the session tests keep their sessions: `$SHORTWIN_RECORD_SESSIONS` when set, else the
/// workspace's `target/tmp/sessions` (on disk).
pub fn sessions_dir() -> PathBuf {
    std::env::var_os(RECORD_SESSIONS_ENV).map_or_else(
        || {
            PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../target/tmp/sessions"
            ))
        },
        PathBuf::from,
    )
}

/// `s` with only its first and last checkpoints (the committed copies of
/// `services/indexer/tests/data/sessions/`: every transaction, the state before the first one
/// and the end state; the dense checkpoints are recorded afresh at test time).
pub fn thinned(s: &Session) -> Session {
    let mut checkpoints = Vec::new();
    if let Some(first) = s.checkpoints.first() {
        checkpoints.push(first.clone());
    }
    if s.checkpoints.len() > 1
        && let Some(last) = s.checkpoints.last()
    {
        checkpoints.push(last.clone());
    }
    Session {
        header: s.header.clone(),
        txs: s.txs.clone(),
        checkpoints,
    }
}

/// The committed session set (INDEXER.md §11.3: the INGEST track's decoder tests read it):
/// `services/indexer/tests/data/sessions/`.
pub fn committed_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../indexer/tests/data/sessions"
    ))
}

/// The sessions committed to [`committed_dir`] (thinned with [`thinned`]).
pub const COMMITTED: [&str; 2] = ["mock_lifecycle", "mock_admin"];

/// INGEST's fixture sessions (`services/indexer/tests/data/ingest/`) that this crate records
/// (thinned with [`thinned`]; `SHORTWIN_RECORD_SESSIONS=<dir>` writes them into `<dir>/ingest/`).
/// The other fixtures there have no recording procedure (INDEXER.md §11.3).
pub const INGEST_FIXTURES: [&str; 1] = ["pump_synthetic_exit"];

/// INGEST's fixture directory: `services/indexer/tests/data/ingest/`.
pub fn ingest_fixtures_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../indexer/tests/data/ingest"
    ))
}
