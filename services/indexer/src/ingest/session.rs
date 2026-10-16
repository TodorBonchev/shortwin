//! Recorded sessions (INDEXER.md §11.3). FOUNDATION (frozen): the REPLAY track's recorder writes
//! them from LiteSVM runs, the INGEST track's replay source reads them, the replay harness
//! compares the indexed database with their account checkpoints.
//!
//! A session is a JSON Lines file:
//!
//! 1. one [`SessionHeader`] line;
//! 2. [`SessionTx`] lines in execution order, each holding the transaction **exactly as
//!    JSON-RPC `getTransaction` returns it** (`encoding: "base64"`,
//!    `maxSupportedTransactionVersion: 1`) — so a replay goes through the same conversion as the
//!    RPC backfill (`ingest::rpc`), the strongest end-to-end check available without a cluster;
//! 3. any number of [`AccountsCheckpoint`] lines, each after the transaction it follows: every
//!    account the recorder tracks, as the chain held it then (the last one is the session's end
//!    state).
//!
//! Slots and block times are the recorder's clock (LiteSVM's `Clock`); `tx_index` counts the
//! transactions of one slot from 0 in execution order.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use solana_transaction_status_client_types::EncodedConfirmedTransactionWithStatusMeta;

/// The format version this module reads and writes.
pub const SESSION_VERSION: u32 = 1;

/// One line of a session file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionLine {
    Header(SessionHeader),
    Tx(Box<SessionTx>),
    Accounts(AccountsCheckpoint),
}

/// The first line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHeader {
    /// [`SESSION_VERSION`].
    pub version: u32,
    /// short.win's program id in the recorded run (base58). (Sessions recorded before the
    /// straddle helper's retirement also carry a `complete_curve_id`; it is ignored: design
    /// pump-final §9, owner decision D2.)
    pub program_id: String,
    /// What recorded it (`"litesvm"`, `"surfpool"`).
    pub recorder: String,
    /// The scenario's name (e.g. `"keeper-litesvm/liquidate::cr_liquidation"`).
    pub scenario: String,
    /// The program build the run loaded (e.g. `"test-venue"` or `"release"`).
    pub build: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// A recorded transaction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionTx {
    pub slot: u64,
    /// Position among the slot's transactions.
    pub tx_index: u32,
    /// The transaction as `getTransaction` returns it — the flat RPC shape: `slot`,
    /// `blockTime`, `transactionIndex` (= the fields above), `transaction: [<base64>,
    /// "base64"]`, `meta`, `version`.
    pub transaction: EncodedConfirmedTransactionWithStatusMeta,
}

/// The tracked accounts after a transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsCheckpoint {
    /// Signature of the transaction this checkpoint follows (base58); `None` before the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    pub slot: u64,
    pub unix_timestamp: i64,
    pub accounts: Vec<SessionAccount>,
    /// Tracked keys that no longer exist (closed positions, lenders, exposures).
    #[serde(default)]
    pub closed: Vec<String>,
}

/// An account as the chain held it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAccount {
    pub pubkey: String,
    pub owner: String,
    pub lamports: u64,
    /// Base64 of the account data.
    pub data: String,
    pub executable: bool,
}

/// A whole session, read into memory.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub header: SessionHeader,
    /// Transactions in execution order.
    pub txs: Vec<SessionTx>,
    /// Checkpoints with the index of the first transaction **after** them (`txs[..k]` were
    /// executed when the checkpoint was taken).
    pub checkpoints: Vec<(usize, AccountsCheckpoint)>,
}

impl Session {
    /// The last checkpoint (the end state), if any.
    pub fn end_state(&self) -> Option<&AccountsCheckpoint> {
        self.checkpoints.last().map(|(_, c)| c)
    }
}

/// Reads a session file.
pub fn read(path: &Path) -> anyhow::Result<Session> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut header = None;
    let mut txs = Vec::new();
    let mut checkpoints = Vec::new();
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let parsed: SessionLine = serde_json::from_str(&line)
            .with_context(|| format!("{}:{}: not a session line", path.display(), n + 1))?;
        match parsed {
            SessionLine::Header(h) => {
                if header.is_some() || n != 0 {
                    bail!(
                        "{}:{}: the header must be the first line",
                        path.display(),
                        n + 1
                    );
                }
                if h.version != SESSION_VERSION {
                    bail!(
                        "{}: session version {} (this build reads {SESSION_VERSION})",
                        path.display(),
                        h.version
                    );
                }
                header = Some(h);
            }
            SessionLine::Tx(tx) => txs.push(*tx),
            SessionLine::Accounts(c) => checkpoints.push((txs.len(), c)),
        }
    }
    let header = header.with_context(|| format!("{}: no header line", path.display()))?;
    Ok(Session {
        header,
        txs,
        checkpoints,
    })
}

/// Writes a session file line by line (the recorder's side).
pub struct SessionWriter {
    out: BufWriter<std::fs::File>,
}

impl SessionWriter {
    /// Creates `path` and writes the header.
    pub fn create(path: &Path, header: &SessionHeader) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file =
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let mut w = Self {
            out: BufWriter::new(file),
        };
        w.line(&SessionLine::Header(header.clone()))?;
        Ok(w)
    }

    /// Appends one line.
    pub fn line(&mut self, line: &SessionLine) -> anyhow::Result<()> {
        serde_json::to_writer(&mut self.out, line)?;
        self.out.write_all(b"\n")?;
        Ok(())
    }

    /// Flushes and closes the file.
    pub fn finish(mut self) -> anyhow::Result<()> {
        self.out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_round_trips() {
        // The workspace's target/tmp (on disk; created if missing).
        let dir = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../target/tmp"));
        let path = dir.join(format!("session-rt-{}.jsonl", std::process::id()));
        let header = SessionHeader {
            version: SESSION_VERSION,
            program_id: shortwin::ID.to_string(),
            recorder: "unit-test".into(),
            scenario: "round-trip".into(),
            build: "none".into(),
            notes: None,
        };
        // The flat shape `getTransaction` returns.
        let tx: EncodedConfirmedTransactionWithStatusMeta =
            serde_json::from_value(serde_json::json!({
                "slot": 7,
                "blockTime": 1_790_000_000,
                "transactionIndex": 0,
                "transaction": ["AQ==", "base64"],
                "meta": null,
                "version": 0
            }))
            .expect("an RPC-shaped transaction");
        let mut w = SessionWriter::create(&path, &header).expect("create");
        w.line(&SessionLine::Tx(Box::new(SessionTx {
            slot: 7,
            tx_index: 0,
            transaction: tx.clone(),
        })))
        .expect("tx");
        w.line(&SessionLine::Accounts(AccountsCheckpoint {
            after: None,
            slot: 7,
            unix_timestamp: 1_790_000_000,
            accounts: vec![],
            closed: vec![],
        }))
        .expect("checkpoint");
        w.finish().expect("finish");
        let s = read(&path).expect("read");
        std::fs::remove_file(&path).ok();
        assert_eq!(s.header, header);
        assert_eq!(s.txs.len(), 1);
        assert_eq!(s.txs[0].transaction, tx);
        assert_eq!(s.checkpoints.len(), 1);
        assert_eq!(s.checkpoints[0].0, 1);
    }
}
