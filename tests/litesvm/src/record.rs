//! Session recording (docs/design/INDEXER.md §11.3; M8 REPLAY track): what LiteSVM executed, as
//! the chain would have committed it, for the indexer's replay harness
//! (`services/indexer-litesvm`, which renders a [`Recording`] as a session file).
//!
//! - [`Env::start_recording`] turns the hook on; from then on every transaction that reaches the
//!   chain through [`Env::execute_recorded`] — [`Env::send`] (both its hooked and unhooked paths),
//!   `Env::send_v0` (the Raydium fork's v0 sends) and the keeper backend's `send_with_hooks` /
//!   bundle path in `services/keeper-litesvm` — is
//!   appended as a [`RecordedTx`]: the transaction as sent, the lookup-table addresses it loaded,
//!   the clock it ran at, its position in the slot (a per-slot counter from 0, the block
//!   position of INDEXER.md §3.3), the balances of its keys before and after, and LiteSVM's
//!   metadata (or the failure). The prefix replays the invariant hooks run on VM clones are not
//!   chain transactions and are never recorded; neither are simulations.
//! - Only transactions a cluster would commit are recorded: successes and the failures that
//!   are charged and committed (an instruction error, rent, a restricted program). A
//!   transaction rejected before execution (signature, blockhash, fee payer) never reaches a
//!   block, so it is not part of the session.
//! - [`Env::account_checkpoint`] snapshots every account short.win owns (configuration, guard,
//!   extras, oracle cache, markets, vaults, positions, lenders, share epochs, exposures) plus
//!   the keys given to [`Env::record_track`], and lists the tracked keys that no longer exist
//!   (closed positions, lenders). [`CheckpointPolicy`] decides the automatic ones; the
//!   recording always ends with one ([`Env::take_recording`]).
//! - A failed bundle commits nothing: the keeper backend rolls the recording back to the mark
//!   it took before the bundle ([`Env::recording_mark`], [`Env::recording_rollback`]).

use std::collections::BTreeSet;

use anchor_lang::prelude::Pubkey;
use anchor_lang::{Discriminator, event::EVENT_IX_TAG_LE};
use litesvm::LiteSVM;
use litesvm::types::TransactionMetadata;
use solana_account::Account;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_error::TransactionError;

use crate::env::{Env, TxResult};

/// The harness's key types, for the crates that script recorded sessions on it
/// (`services/indexer-litesvm` links neither `solana-keypair` nor `solana-signer` itself).
pub use solana_keypair::Keypair;
pub use solana_signer::Signer;

/// Size of an address lookup table's header (`LOOKUP_TABLE_META_SIZE`); the addresses follow.
const ALT_META_SIZE: usize = 56;

/// When [`Env`] takes checkpoints by itself while recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointPolicy {
    /// Only [`Env::account_checkpoint`] (and the final one).
    Manual,
    /// After every recorded transaction.
    EveryTx,
    /// After every successful transaction that emitted a short.win `emit_cpi!` event other than
    /// `PriceCranked` (the accounting, lender, position, pool and admin events) — so every
    /// position is seen right after its `Open` and every change of a market's accounting has its
    /// chain state.
    AfterEvents,
}

/// One executed transaction (INDEXER.md §11.3).
#[derive(Clone, Debug)]
pub struct RecordedTx {
    /// The transaction as sent (legacy, v0 or v1).
    pub tx: VersionedTransaction,
    /// Addresses loaded through lookup tables (v0), writable then read-only.
    pub loaded_writable: Vec<Pubkey>,
    pub loaded_readonly: Vec<Pubkey>,
    /// The clock the transaction ran at.
    pub slot: u64,
    pub unix_timestamp: i64,
    /// Position among the slot's recorded transactions (from 0).
    pub tx_index: u32,
    /// Lamports of every account key (static, then loaded) before and after.
    pub pre_balances: Vec<u64>,
    pub post_balances: Vec<u64>,
    /// LiteSVM's metadata, or the failure.
    pub result: TxResult,
}

impl RecordedTx {
    /// The transaction's signature (its first one: the fee payer's).
    pub fn signature(&self) -> solana_transaction::Signature {
        self.tx.signatures.first().copied().unwrap_or_default()
    }

    /// The metadata (successful or not).
    pub fn meta(&self) -> &TransactionMetadata {
        match &self.result {
            Ok(m) => m,
            Err(f) => &f.meta,
        }
    }

    /// Every account key: static, then loaded writable, then loaded read-only.
    pub fn account_keys(&self) -> Vec<Pubkey> {
        let mut keys = self.tx.message.static_account_keys().to_vec();
        keys.extend_from_slice(&self.loaded_writable);
        keys.extend_from_slice(&self.loaded_readonly);
        keys
    }
}

/// The tracked accounts at one point of the recording.
#[derive(Clone, Debug)]
pub struct AccountCheckpoint {
    pub slot: u64,
    pub unix_timestamp: i64,
    /// Every tracked account that exists, by key.
    pub accounts: Vec<(Pubkey, Account)>,
    /// Tracked keys that no longer exist.
    pub closed: Vec<Pubkey>,
}

/// What [`Env::take_recording`] returns.
#[derive(Clone, Debug, Default)]
pub struct Recording {
    /// Transactions in execution order.
    pub txs: Vec<RecordedTx>,
    /// Checkpoints with the number of transactions executed before each (`txs[..k]`).
    pub checkpoints: Vec<(usize, AccountCheckpoint)>,
}

/// The recorder state [`Env`] carries while recording.
#[derive(Debug)]
pub struct Recorder {
    policy: CheckpointPolicy,
    rec: Recording,
    tracked: BTreeSet<Pubkey>,
    extra: BTreeSet<Pubkey>,
    /// (slot, next tx_index in it).
    slot_counter: Option<(u64, u32)>,
}

/// A position in the recording ([`Env::recording_mark`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingMark {
    txs: usize,
    checkpoints: usize,
    slot_counter: Option<(u64, u32)>,
}

/// The state captured before a transaction executes ([`Env::execute_recorded`]).
#[derive(Clone, Debug)]
struct PreTx {
    loaded_writable: Vec<Pubkey>,
    loaded_readonly: Vec<Pubkey>,
    keys: Vec<Pubkey>,
    pre_balances: Vec<u64>,
    slot: u64,
    unix_timestamp: i64,
}

/// The addresses a v0 transaction loads through its lookup tables, read from `svm` (writable,
/// read-only). Empty for legacy and v1 transactions; a missing or short table loads nothing (the
/// transaction then fails before execution and is not recorded).
pub fn loaded_addresses(svm: &LiteSVM, tx: &VersionedTransaction) -> (Vec<Pubkey>, Vec<Pubkey>) {
    let mut writable = Vec::new();
    let mut readonly = Vec::new();
    for l in tx.message.address_table_lookups().unwrap_or_default() {
        let Some(acc) = svm.get_account(&l.account_key) else {
            continue;
        };
        let addrs: Vec<Pubkey> = acc
            .data
            .get(ALT_META_SIZE..)
            .unwrap_or_default()
            .as_chunks::<32>()
            .0
            .iter()
            .map(|c| Pubkey::new_from_array(*c))
            .collect();
        let pick = |idx: &[u8], out: &mut Vec<Pubkey>| {
            for i in idx {
                if let Some(k) = addrs.get(usize::from(*i)) {
                    out.push(*k);
                }
            }
        };
        pick(&l.writable_indexes, &mut writable);
        pick(&l.readonly_indexes, &mut readonly);
    }
    (writable, readonly)
}

/// Whether a failure is one a cluster commits (the fee is charged and the transaction is in the
/// block): an instruction error, rent, a temporarily restricted program, an unbalanced
/// transaction. Everything else is rejected before execution.
pub fn failure_is_committed(err: &TransactionError) -> bool {
    matches!(
        err,
        TransactionError::InstructionError(..)
            | TransactionError::InsufficientFundsForRent { .. }
            | TransactionError::ProgramExecutionTemporarilyRestricted { .. }
            | TransactionError::UnbalancedTransaction
    )
}

/// Whether `meta` (of a transaction with `keys`) holds a short.win `emit_cpi!` event other than
/// `PriceCranked` ([`CheckpointPolicy::AfterEvents`]).
fn emitted_state_event(meta: &TransactionMetadata, keys: &[Pubkey]) -> bool {
    meta.inner_instructions.iter().flatten().any(|ii| {
        keys.get(usize::from(ii.instruction.program_id_index)) == Some(&shortwin::ID)
            && ii
                .instruction
                .data
                .strip_prefix(EVENT_IX_TAG_LE)
                .is_some_and(|p| !p.starts_with(shortwin::events::PriceCranked::DISCRIMINATOR))
    })
}

impl Recorder {
    fn new(policy: CheckpointPolicy) -> Self {
        Self {
            policy,
            rec: Recording::default(),
            tracked: BTreeSet::new(),
            extra: BTreeSet::new(),
            slot_counter: None,
        }
    }

    fn checkpoint(&mut self, svm: &LiteSVM) {
        let clock = svm.get_sysvar::<solana_clock::Clock>();
        let mut accounts: Vec<(Pubkey, Account)> = svm.get_program_accounts(&shortwin::ID);
        for k in &self.extra {
            if !accounts.iter().any(|(a, _)| a == k)
                && let Some(acc) = svm.get_account(k)
            {
                accounts.push((*k, acc));
            }
        }
        accounts.sort_by_key(|(k, _)| *k);
        let present: BTreeSet<Pubkey> = accounts.iter().map(|(k, _)| *k).collect();
        let closed: Vec<Pubkey> = self.tracked.difference(&present).copied().collect();
        self.tracked.extend(present);
        self.rec.checkpoints.push((
            self.rec.txs.len(),
            AccountCheckpoint {
                slot: clock.slot,
                unix_timestamp: clock.unix_timestamp,
                accounts,
                closed,
            },
        ));
    }

    fn next_tx_index(&mut self, slot: u64) -> u32 {
        match self.slot_counter {
            Some((s, n)) if s == slot => {
                self.slot_counter = Some((s, n + 1));
                n
            }
            Some((s, _)) if slot < s => {
                panic!("recording: the clock went back from slot {s} to {slot}")
            }
            _ => {
                self.slot_counter = Some((slot, 1));
                0
            }
        }
    }
}

impl Env {
    /// Starts recording with [`CheckpointPolicy::AfterEvents`] (module docs); a recording
    /// already running is discarded. Takes the first checkpoint (the state before the first
    /// recorded transaction).
    pub fn start_recording(&mut self) {
        self.start_recording_with(CheckpointPolicy::AfterEvents);
    }

    /// Starts recording with `policy`.
    pub fn start_recording_with(&mut self, policy: CheckpointPolicy) {
        let mut r = Recorder::new(policy);
        r.checkpoint(&self.svm);
        self.recorder = Some(Box::new(r));
    }

    /// Whether a recording is running.
    pub fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }

    /// Stops recording and returns it, ending with a checkpoint of the current state. Empty if
    /// nothing was recording.
    pub fn take_recording(&mut self) -> Recording {
        let Some(mut r) = self.recorder.take() else {
            return Recording::default();
        };
        if r.rec.checkpoints.last().map(|(k, _)| *k) != Some(r.rec.txs.len()) {
            r.checkpoint(&self.svm);
        }
        r.rec
    }

    /// Snapshots the tracked accounts now (no-op unless recording; module docs).
    pub fn account_checkpoint(&mut self) {
        if let Some(r) = self.recorder.as_mut() {
            r.checkpoint(&self.svm);
        }
    }

    /// Adds `keys` (accounts short.win does not own: a pool, a curve) to every later
    /// checkpoint.
    pub fn record_track(&mut self, keys: &[Pubkey]) {
        if let Some(r) = self.recorder.as_mut() {
            r.extra.extend(keys.iter().copied());
        }
    }

    /// The recording's current end (for [`Env::recording_rollback`]); `None` unless recording.
    pub fn recording_mark(&self) -> Option<RecordingMark> {
        self.recorder.as_ref().map(|r| RecordingMark {
            txs: r.rec.txs.len(),
            checkpoints: r.rec.checkpoints.len(),
            slot_counter: r.slot_counter,
        })
    }

    /// Drops everything recorded after `mark` (a bundle that failed commits nothing).
    pub fn recording_rollback(&mut self, mark: Option<RecordingMark>) {
        if let (Some(r), Some(m)) = (self.recorder.as_mut(), mark) {
            r.rec.txs.truncate(m.txs);
            r.rec.checkpoints.truncate(m.checkpoints);
            r.slot_counter = m.slot_counter;
        }
    }

    /// Executes `tx` on the VM (no invariant hooks, no blockhash expiry) and, while recording,
    /// appends it if a cluster would have committed it (module docs). Every path that sends a
    /// chain transaction goes through here.
    pub fn execute_recorded(&mut self, tx: VersionedTransaction) -> TxResult {
        let Some(_) = self.recorder.as_ref() else {
            return self.svm.send_transaction(tx);
        };
        let (loaded_writable, loaded_readonly) = loaded_addresses(&self.svm, &tx);
        let mut keys = tx.message.static_account_keys().to_vec();
        keys.extend_from_slice(&loaded_writable);
        keys.extend_from_slice(&loaded_readonly);
        let clock = self.clock();
        let pre = PreTx {
            pre_balances: keys
                .iter()
                .map(|k| crate::read::lamports(&self.svm, k))
                .collect(),
            keys,
            loaded_writable,
            loaded_readonly,
            slot: clock.slot,
            unix_timestamp: clock.unix_timestamp,
        };
        let res = self.svm.send_transaction(tx.clone());
        self.record_executed(pre, tx, &res);
        res
    }

    fn record_executed(&mut self, pre: PreTx, tx: VersionedTransaction, res: &TxResult) {
        if let Err(f) = res
            && !failure_is_committed(&f.err)
        {
            return;
        }
        let post_balances = pre
            .keys
            .iter()
            .map(|k| crate::read::lamports(&self.svm, k))
            .collect();
        let state_event = res
            .as_ref()
            .is_ok_and(|meta| emitted_state_event(meta, &pre.keys));
        let Some(r) = self.recorder.as_mut() else {
            return;
        };
        let tx_index = r.next_tx_index(pre.slot);
        r.rec.txs.push(RecordedTx {
            tx,
            loaded_writable: pre.loaded_writable,
            loaded_readonly: pre.loaded_readonly,
            slot: pre.slot,
            unix_timestamp: pre.unix_timestamp,
            tx_index,
            pre_balances: pre.pre_balances,
            post_balances,
            result: res.clone(),
        });
        let take = match r.policy {
            CheckpointPolicy::Manual => false,
            CheckpointPolicy::EveryTx => true,
            CheckpointPolicy::AfterEvents => state_event,
        };
        if take {
            r.checkpoint(&self.svm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_committed_failures_are_recorded() {
        use solana_transaction::InstructionError;
        assert!(failure_is_committed(&TransactionError::InstructionError(
            0,
            InstructionError::Custom(6000)
        )));
        assert!(failure_is_committed(
            &TransactionError::InsufficientFundsForRent { account_index: 1 }
        ));
        for e in [
            TransactionError::BlockhashNotFound,
            TransactionError::AlreadyProcessed,
            TransactionError::SignatureFailure,
            TransactionError::InsufficientFundsForFee,
            TransactionError::AccountNotFound,
        ] {
            assert!(!failure_is_committed(&e), "{e:?}");
        }
    }
}
