//! The chain-client abstraction (ARCHITECTURE §7.1 "Stack"): everything the keeper reads from
//! and sends to a cluster, behind one trait with two backends — [`crate::rpc::RpcChain`] for a
//! real cluster (JSON-RPC) and the in-process LiteSVM backend of the scenario tests
//! (`services/keeper-litesvm`, TEST-ONLY).
//!
//! The trait is deliberately small and backend-neutral: raw accounts in, versioned transactions
//! out. Transaction *failures* are values ([`TxOutcome::err`]); only transport problems are
//! `Err`. A backend reports the `emit_cpi!` inner instructions of every top-level instruction
//! ([`TxOutcome::inner`]) so the keeper can decode events from a simulation exactly as from a
//! landed transaction.
//!
//! Bundles (U22): [`Chain::send_bundle`] and [`Chain::simulate_bundle`] have refusing defaults;
//! the JSON-RPC backend sends through a Jito block engine when one is configured. No keeper duty
//! sends a bundle since the straddle bundle's retirement (pump-final §7, owner D2); the transport
//! stays for duty 3's Jito path (U8, not built).

use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_error::TransactionError;

/// A fetched account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountData {
    pub lamports: u64,
    pub owner: Pubkey,
    pub executable: bool,
    pub data: Vec<u8>,
}

/// The cluster clock the next transaction executes at (slot and `Clock::unix_timestamp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainClock {
    pub slot: u64,
    pub unix_timestamp: i64,
}

/// A `getProgramAccounts` byte filter: `data[offset..offset + bytes.len()] == bytes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Memcmp {
    pub offset: usize,
    pub bytes: Vec<u8>,
}

/// Why a transaction failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxError {
    /// Top-level instruction `index` failed with custom program error `code`.
    Custom { index: u8, code: u32 },
    /// Top-level instruction `index` failed with another instruction error (its debug text).
    Instruction { index: u8, detail: String },
    /// Any other execution error (its debug text).
    Execution(String),
    /// The cluster refused the transaction before executing it (unsupported version, too
    /// large, sanitize failure, blockhash): nothing ran. The v1 → v0 fallback keys on this.
    Rejected(String),
    /// Sent, but not seen confirmed before the backend's timeout (dropped, or not landed in
    /// time): the keeper treats the attempt as a miss and retries with an escalated price.
    Dropped(String),
}

impl TxError {
    /// Maps a runtime `TransactionError`: instruction errors keep their index; errors raised
    /// before anything ran (blockhash, sanitize, version, locks) are [`TxError::Rejected`].
    pub fn from_transaction_error(err: TransactionError) -> Self {
        use solana_instruction::error::InstructionError;
        match err {
            TransactionError::InstructionError(index, InstructionError::Custom(code)) => {
                Self::Custom { index, code }
            }
            TransactionError::InstructionError(index, other) => Self::Instruction {
                index,
                detail: format!("{other:?}"),
            },
            TransactionError::BlockhashNotFound
            | TransactionError::AlreadyProcessed
            | TransactionError::SanitizeFailure
            | TransactionError::UnsupportedVersion
            | TransactionError::TooManyAccountLocks
            | TransactionError::AccountLoadedTwice => Self::Rejected(format!("{err:?}")),
            other => Self::Execution(format!("{other:?}")),
        }
    }

    /// The custom error code, if the failure was one.
    pub fn custom_code(&self) -> Option<u32> {
        match self {
            Self::Custom { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The failing top-level instruction, if the failure was an instruction's.
    pub fn instruction(&self) -> Option<usize> {
        match self {
            Self::Custom { index, .. } | Self::Instruction { index, .. } => {
                Some(usize::from(*index))
            }
            _ => None,
        }
    }
}

/// One inner instruction (a CPI at any depth) of a top-level instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InnerIx {
    pub program_id: Pubkey,
    pub data: Vec<u8>,
}

/// The result of a simulation or of a landed transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TxOutcome {
    /// The transaction's signature (default for a simulation).
    pub signature: Signature,
    /// `None` on success.
    pub err: Option<TxError>,
    pub logs: Vec<String>,
    /// Compute units the whole transaction consumed.
    pub units_consumed: u64,
    /// Lamports the fee payer paid (0 for a simulation when the backend does not report it).
    pub fee: u64,
    /// The inner instructions of each top-level instruction, in order (index = top-level
    /// instruction index; empty lists where there were none).
    pub inner: Vec<Vec<InnerIx>>,
    /// The slot it comes from: a simulation's context slot (the bank it ran on), a landed
    /// transaction's slot; 0 when unknown (refused before it ran, dropped, or a backend that does
    /// not say).
    pub slot: u64,
}

impl TxOutcome {
    /// `true` if the transaction succeeded.
    pub fn ok(&self) -> bool {
        self.err.is_none()
    }
}

/// What became of a bundle ([`Chain::send_bundle`]; ARCH §5.4, U22): a Jito bundle executes its
/// transactions in order and all-or-nothing, so either every transaction landed or none did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BundleOutcome {
    /// The block engine's bundle id (empty for an in-process backend).
    pub id: String,
    /// Every transaction landed, in order.
    pub landed: bool,
    /// Each transaction's outcome, in order: the landed transactions' metas when `landed`;
    /// otherwise what is known — an in-process backend reports the transactions up to and
    /// including the failing one, a block engine nothing (the bundle was dropped or refused:
    /// [`BundleOutcome::detail`]).
    pub txs: Vec<TxOutcome>,
    /// Why the bundle did not land (the failing transaction's index and error, a refusal, a
    /// timeout); empty when it landed.
    pub detail: String,
}

/// What the keeper needs from a cluster (module docs).
///
/// `async fn` in a trait: the keeper is generic over its backend and runs on one task, so the
/// futures need not be `Send` (the LiteSVM backend is single-threaded).
#[allow(async_fn_in_trait)]
pub trait Chain {
    /// The clock the next transaction executes at.
    async fn clock(&self) -> anyhow::Result<ChainClock>;

    /// The accounts at `keys`, in order (`None` for a missing account).
    async fn accounts(&self, keys: &[Pubkey]) -> anyhow::Result<Vec<Option<AccountData>>>;

    /// The accounts at `keys` with only their first `len` data bytes (`None` for a missing
    /// account): the venue ProgramData headers of the upgrade watcher (ARCH §7.1 duty 10), whose
    /// accounts are up to 10 MiB. The default reads whole accounts and truncates; a backend
    /// that can slice server-side (JSON-RPC `dataSlice`) overrides it.
    async fn account_headers(
        &self,
        keys: &[Pubkey],
        len: usize,
    ) -> anyhow::Result<Vec<Option<AccountData>>> {
        let mut out = self.accounts(keys).await?;
        for a in out.iter_mut().flatten() {
            a.data.truncate(len);
        }
        Ok(out)
    }

    /// [`Chain::account_headers`] as of slot `min_context_slot` or later: a node of an RPC pool
    /// that has not reached it refuses the read (an error) rather than answer from an older
    /// state — the keeper's AIR-18 check reads the ProgramData of a program refused in a slot,
    /// which only a read at or after that slot shows. The default ignores it: an in-process
    /// backend is never behind.
    async fn account_headers_at(
        &self,
        keys: &[Pubkey],
        len: usize,
        min_context_slot: u64,
    ) -> anyhow::Result<Vec<Option<AccountData>>> {
        let _ = min_context_slot;
        self.account_headers(keys, len).await
    }

    /// Every account of `program` matching all `filters`.
    async fn program_accounts(
        &self,
        program: &Pubkey,
        filters: &[Memcmp],
    ) -> anyhow::Result<Vec<(Pubkey, AccountData)>>;

    /// Every account of `program` of exactly `data_size` bytes matching all `filters`: the
    /// Raydium pool discovery (ARCH §7.1 duty 12), whose programs hold one account layout of
    /// each size the program verifies (AMM v4 `AmmInfo` 752 B, CPMM `PoolState` 637 B). The
    /// default filters [`Chain::program_accounts`]' result by length; a JSON-RPC backend
    /// overrides it with a `dataSize` filter, so the node compares bytes only on accounts of
    /// that size.
    async fn program_accounts_sized(
        &self,
        program: &Pubkey,
        data_size: usize,
        filters: &[Memcmp],
    ) -> anyhow::Result<Vec<(Pubkey, AccountData)>> {
        let mut out = self.program_accounts(program, filters).await?;
        out.retain(|(_, a)| a.data.len() == data_size);
        Ok(out)
    }

    /// A recent blockhash for new transactions.
    async fn latest_blockhash(&self) -> anyhow::Result<Hash>;

    /// Simulates `tx` against the current state (signatures are not verified).
    async fn simulate(&self, tx: &VersionedTransaction) -> anyhow::Result<TxOutcome>;

    /// Sends `tx` and waits until it landed (or failed, or was refused).
    async fn send(&self, tx: &VersionedTransaction) -> anyhow::Result<TxOutcome>;

    /// Recent prioritization fees (micro-lamports per CU) of transactions that write-locked
    /// any of `writable` (ARCH §7.1 duty 14); empty if the backend has none.
    async fn recent_priority_fees(&self, writable: &[Pubkey]) -> anyhow::Result<Vec<u64>>;

    /// Sends `txs` as one **Jito bundle** (ARCH §5.4, U22: at most 5 transactions, executed in
    /// order and all-or-nothing, a tip to a Jito tip account in one of them) and waits until it
    /// landed or is known not to have. The default refuses: the backend has no block engine.
    async fn send_bundle(&self, txs: &[VersionedTransaction]) -> anyhow::Result<BundleOutcome> {
        let _ = txs;
        anyhow::bail!("this chain backend sends no bundles (no block engine configured)")
    }

    /// Simulates `txs` in order, each on the state the ones before it left (a bundle's
    /// pre-flight); stops at the first failure. The default refuses: a plain JSON-RPC node
    /// simulates one transaction at a time, so a bundle whose later transactions depend on the
    /// earlier ones cannot be pre-flighted there.
    async fn simulate_bundle(
        &self,
        txs: &[VersionedTransaction],
    ) -> anyhow::Result<Vec<TxOutcome>> {
        let _ = txs;
        anyhow::bail!("this chain backend cannot simulate a bundle")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_instruction::error::InstructionError;

    #[test]
    fn transaction_errors_map_to_their_kind() {
        let e = TxError::from_transaction_error(TransactionError::InstructionError(
            2,
            InstructionError::Custom(6_071),
        ));
        assert_eq!(
            e,
            TxError::Custom {
                index: 2,
                code: 6_071
            }
        );
        assert_eq!((e.custom_code(), e.instruction()), (Some(6_071), Some(2)));
        let e = TxError::from_transaction_error(TransactionError::InstructionError(
            1,
            InstructionError::InvalidAccountData,
        ));
        assert_eq!((e.custom_code(), e.instruction()), (None, Some(1)));
        // AIR-18: the loader's "Program is not deployed" (a program deployed, upgraded or
        // extended in this slot) keeps its name, which the keeper tells transient by.
        assert_eq!(
            TxError::from_transaction_error(TransactionError::InstructionError(
                0,
                InstructionError::UnsupportedProgramId,
            )),
            TxError::Instruction {
                index: 0,
                detail: "UnsupportedProgramId".into(),
            }
        );
        for pre in [
            TransactionError::BlockhashNotFound,
            TransactionError::UnsupportedVersion,
            TransactionError::SanitizeFailure,
        ] {
            assert!(matches!(
                TxError::from_transaction_error(pre),
                TxError::Rejected(_)
            ));
        }
        assert!(matches!(
            TxError::from_transaction_error(TransactionError::InsufficientFundsForFee),
            TxError::Execution(_)
        ));
    }
}
