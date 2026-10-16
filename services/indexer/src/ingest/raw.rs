//! The normalised transaction every source produces (INDEXER.md §2.1). FOUNDATION (frozen).
//!
//! A source turns whatever it receives (RPC `getTransaction` JSON, a Yellowstone update, a
//! recorded session line) into a [`RawTx`]; the decoder sees nothing else, so all sources decode
//! identically.

use solana_pubkey::Pubkey;
use solana_signature::Signature;

/// Commitment of an ingested transaction (`ingest_txs.commitment`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Commitment {
    Confirmed,
    Finalized,
}

impl Commitment {
    /// The column value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Finalized => "finalized",
        }
    }
}

/// One instruction invocation: a top-level instruction or a CPI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawIx {
    pub program_id: Pubkey,
    /// The instruction's accounts, resolved to keys (lookup-table addresses included).
    pub accounts: Vec<Pubkey>,
    pub data: Vec<u8>,
    /// 1 for a top-level instruction, 2+ for CPIs (RPC `stackHeight`). Sources that cannot
    /// report it (none of the supported ones) must not produce a `RawTx`.
    pub stack_height: u32,
}

/// A top-level instruction and its inner instructions, in execution order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawTopIx {
    pub ix: RawIx,
    pub inner: Vec<RawIx>,
}

/// A transaction as the decoder sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct RawTx {
    pub signature: Signature,
    pub slot: u64,
    /// The transaction's position in its block (§3.3: always the block position).
    pub tx_index: u32,
    /// Block time (unix seconds) when known.
    pub block_time: Option<i64>,
    pub commitment: Commitment,
    pub fee_payer: Pubkey,
    pub fee: u64,
    pub compute_units: Option<u64>,
    /// `None` on success; the RPC `UiTransactionError` JSON otherwise.
    pub err: Option<serde_json::Value>,
    /// Static keys, then the loaded writable, then the loaded read-only addresses.
    pub account_keys: Vec<Pubkey>,
    pub instructions: Vec<RawTopIx>,
    pub logs: Vec<String>,
}

impl RawTx {
    /// `true` if the transaction succeeded (events are decoded only then).
    pub fn ok(&self) -> bool {
        self.err.is_none()
    }

    /// `true` if the runtime cut the log stream: `emit!` log events may be missing (§5.1).
    pub fn logs_truncated(&self) -> bool {
        self.logs.iter().any(|l| l == "Log truncated")
    }
}
