//! TEST-ONLY: the keeper's in-process chain backend on LiteSVM, and the scenario driver.
//!
//! [`LiteSvmChain`] implements [`Chain`] over the program-test harness's [`Env`]
//! (`tests/litesvm`): short.win's **test-venue** build and the mock venue (A49). Every
//! transaction the keeper sends runs the harness's **invariant hooks after every instruction**
//! (ARCH §8.3), exactly as `Env::send` does for the harness's own legacy transactions — but on
//! the keeper's versioned (v1 / v0) transaction as built, so formats, sizes and compute budgets
//! are the real ones:
//!
//! 1. every existing `Market` the transaction names is tracked;
//! 2. a transaction of several instructions is replayed prefix by prefix on a clone of the VM
//!    (signature checks off on the clone), and after prefix `k` instruction `k` is checked
//!    (`invariants::check_ix`) against the state after prefix `k − 1`, and every tracked market
//!    (`invariants::check_all`);
//! 3. the transaction itself runs; its last instruction is checked against the state after
//!    all the others; a failed transaction is checked for committing nothing.
//!
//! A violation panics with the transaction's logs. Knobs for the scenarios: refuse v1
//! transactions ([`LiteSvmChain::set_v1`], to exercise the v0 fallback), the recent
//! priority fees the backend reports ([`LiteSvmChain::set_priority_fees`]), whole-account
//! reads of a key that fail ([`LiteSvmChain::fail_reads_of`]), simulations that fail
//! ([`LiteSvmChain::fail_simulations`]), exit simulations short.win refuses with a given error
//! ([`LiteSvmChain::refuse_exit_simulations`], or only those its own account parsing passes:
//! [`LiteSvmChain::refuse_parsed_exit_simulations`]), header reads of a node lagging the last
//! deploy ([`LiteSvmChain::lag_headers_of`]) and simulations — all, or the exits' — served by a
//! node that stays behind ([`LiteSvmChain::lag_simulations`],
//! [`LiteSvmChain::lag_exit_simulations`]); every read and send is logged
//! ([`LiteSvmChain::rpc_log`], M4-SEC-7: the keeper's RPC work per tick), the blockhash,
//! priority-fee and simulation calls counted ([`LiteSvmChain::other_calls`]). **Timing**
//! (M7-SEC-1, M7-SEC-6): the keeper's
//! transactions can land seconds after the clock it read
//! ([`LiteSvmChain::set_landing_delays`]), its clock can lag the chain's
//! ([`LiteSvmChain::set_clock_lag`]), and a trade can land between its read and its transaction
//! ([`LiteSvmChain::before_next_send`]); [`Harness::second_slow`] models a tick that waited for
//! its confirmations (the next tick's clock a second later than the tick's own).
//!
//! **No bundles** (pump-final §7, owner D2): the keeper sends none since the straddle bundle's
//! retirement, so this backend keeps the trait's refusing `send_bundle` / `simulate_bundle` (its
//! in-process Jito executor and the `complete-curve` helper's loader left with the straddle).
//!
//! [`Harness`] couples the environment, a funded keeper and the clock: one keeper tick per
//! slot or per second.

#![allow(clippy::result_large_err)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use litesvm::types::TransactionMetadata;
use shortwin::state::Market;
use shortwin_keeper::{Keeper, KeeperConfig, TickReport};
use shortwin_litesvm_tests::{Env, invariants, read};
use shortwin_services_common::tx::{self, TxFormat};
use shortwin_services_common::{
    AccountData, Chain, ChainClock, InnerIx, Memcmp, TxError, TxOutcome,
};
use solana_hash::Hash;
use solana_keypair::Keypair;
use solana_message::{AddressLookupTableAccount, Message, VersionedMessage};
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

/// Lamports the keeper's fee payer starts with.
pub const KEEPER_FUNDING: u64 = 100_000_000_000;

/// The in-process chain (module docs).
pub struct LiteSvmChain {
    env: Rc<RefCell<Env>>,
    v1: Cell<bool>,
    fees: RefCell<Vec<u64>>,
    sent: RefCell<Vec<SentTx>>,
    rpc: RefCell<Vec<Rpc>>,
    /// Seconds the keeper's clock read lags the chain's (M7-SEC-1: a `confirmed` read on mainnet
    /// is 1–2 slots behind the tip its transactions land at).
    clock_lag: Cell<i64>,
    /// Runs before the next `send` executes (a trade landing between the keeper's read and its
    /// transaction; M7-SEC-2 (b)).
    before_send: RefCell<Option<BeforeSend>>,
    /// Landing latency (M7-SEC-1, M7-SEC-6): before each `send` executes, the chain's clock
    /// advances by the next value of this source (seconds; 0: the second the keeper read), so
    /// the keeper's transaction lands later than the clock it read — as a `confirmed` read plus
    /// 1–3 slots of send latency do on mainnet.
    landing: RefCell<Option<LandingDelays>>,
    /// Whole-account reads naming this key fail this many more times
    /// ([`LiteSvmChain::fail_reads_of`]).
    fail_reads: Cell<Option<(Pubkey, usize)>>,
    /// The next this many simulations fail ([`LiteSvmChain::fail_simulations`]).
    fail_sims: Cell<usize>,
    /// Header reads naming no context slot answer this key with this deploy slot this many more
    /// times ([`LiteSvmChain::lag_headers_of`]).
    lag_headers: Cell<Option<(Pubkey, u64, usize)>>,
    /// Simulations answered from a copy of the VM ([`LiteSvmChain::lag_simulations`]): the copy,
    /// its slot, how many more, and whether only exits' ([`LiteSvmChain::lag_exit_simulations`]).
    stale_sims: RefCell<Option<(litesvm::LiteSVM, u64, usize, bool)>>,
    /// The custom error short.win's `liquidate` / `expire` simulations are refused with, and
    /// whether only where its own account parsing passes
    /// ([`LiteSvmChain::refuse_exit_simulations`],
    /// [`LiteSvmChain::refuse_parsed_exit_simulations`]).
    refuse_exits: Cell<Option<(u32, bool)>>,
    /// The calls [`Rpc`] does not log ([`LiteSvmChain::other_calls`]).
    other: Cell<OtherCalls>,
}

/// The backend calls the [`Rpc`] log leaves out (it lists the reads and sends M4-SEC-7 measures),
/// counted: AIR-01's cost of the flag's simulations before the exits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OtherCalls {
    /// `getLatestBlockhash`.
    pub blockhashes: usize,
    /// `getRecentPrioritizationFees`.
    pub priority_fees: usize,
    /// `simulateTransaction`.
    pub simulations: usize,
}

impl OtherCalls {
    /// The calls made since `earlier` (a count taken before).
    pub fn since(self, earlier: Self) -> Self {
        Self {
            blockhashes: self.blockhashes - earlier.blockhashes,
            priority_fees: self.priority_fees - earlier.priority_fees,
            simulations: self.simulations - earlier.simulations,
        }
    }
}

/// A source of landing delays (seconds) for [`LiteSvmChain::set_landing_delays`].
pub type LandingDelays = Box<dyn FnMut() -> i64>;

/// What runs when the keeper's next send reaches the chain, before it executes (a trade landing
/// between the keeper's read and its transaction).
type BeforeSend = Box<dyn FnOnce(&mut Env)>;

/// One backend call, as a JSON-RPC backend would make it (M4-SEC-7's measurements).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rpc {
    /// `getMultipleAccounts` of these keys (a JSON-RPC backend splits them into calls of 100).
    Accounts(Vec<Pubkey>),
    /// `getProgramAccounts`.
    ProgramAccounts,
    /// `sendTransaction` of a transaction naming these short.win instruction count.
    Send { instructions: usize },
}

impl Rpc {
    /// The JSON-RPC calls this stands for (`getMultipleAccounts` takes 100 keys a call).
    pub fn calls(&self) -> usize {
        match self {
            Self::Accounts(keys) => keys.len().div_ceil(100).max(1),
            Self::ProgramAccounts | Self::Send { .. } => 1,
        }
    }
}

/// A transaction the keeper sent (for assertions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentTx {
    pub format: Option<TxFormat>,
    pub bytes: usize,
    pub accounts: usize,
    /// short.win instructions in it (ComputeBudget excluded).
    pub instructions: usize,
    pub ok: bool,
    /// The slot it was sent in (T-CASCADE's per-slot accounting).
    pub slot: u64,
    /// Signatures (the fee payer, and the `coverage_authority` of a `set_coverage_pause`).
    pub signatures: usize,
    /// The compute-unit limit it requested (v1 config / v0 `SetComputeUnitLimit`; `None`:
    /// the runtime default), which a leader charges against each writable account's block
    /// budget (C0-17).
    pub cu_limit: Option<u32>,
    /// Compute units it consumed.
    pub units: u64,
    /// The accounts it write-locks.
    pub writable: Vec<Pubkey>,
}

/// The compute-unit limit `tx` requests: a v1 message's config, a v0 or legacy message's
/// `SetComputeUnitLimit` (ComputeBudget instruction 2, `u32` LE).
pub fn requested_cu_limit(tx: &VersionedTransaction, ixs: &[Instruction]) -> Option<u32> {
    if let VersionedMessage::V1(m) = &tx.message {
        return m.config.compute_unit_limit;
    }
    let cb: Pubkey = "ComputeBudget111111111111111111111111111111"
        .parse()
        .expect("ComputeBudget id");
    ixs.iter()
        .filter(|ix| ix.program_id == cb && ix.data.first() == Some(&2) && ix.data.len() >= 5)
        .find_map(|ix| ix.data[1..5].try_into().ok().map(u32::from_le_bytes))
}

/// The accounts `ixs` write-lock (every writable meta, the fee payer included).
fn writable_keys(ixs: &[Instruction], payer: Pubkey) -> Vec<Pubkey> {
    let mut v = vec![payer];
    for m in ixs.iter().flat_map(|ix| ix.accounts.iter()) {
        if m.is_writable && !v.contains(&m.pubkey) {
            v.push(m.pubkey);
        }
    }
    v
}

/// The record of `tx` (decompiled as `ixs`) with its outcome `out`, sent in `slot`.
fn sent_record(
    tx: &VersionedTransaction,
    ixs: &[Instruction],
    out: &TxOutcome,
    slot: u64,
) -> SentTx {
    let payer = tx
        .message
        .static_account_keys()
        .first()
        .copied()
        .unwrap_or_default();
    SentTx {
        format: tx::format_of(tx),
        bytes: tx::size(tx),
        accounts: tx::account_count(tx),
        instructions: ixs.iter().filter(|i| i.program_id == shortwin::ID).count(),
        ok: out.ok(),
        slot,
        signatures: tx.signatures.len(),
        cu_limit: requested_cu_limit(tx, ixs),
        units: out.units_consumed,
        writable: writable_keys(ixs, payer),
    }
}

impl LiteSvmChain {
    /// A backend on `env` (shared with the test driving the market).
    pub fn new(env: Rc<RefCell<Env>>) -> Self {
        Self {
            env,
            v1: Cell::new(true),
            fees: RefCell::new(Vec::new()),
            sent: RefCell::new(Vec::new()),
            rpc: RefCell::new(Vec::new()),
            clock_lag: Cell::new(0),
            before_send: RefCell::new(None),
            landing: RefCell::new(None),
            fail_reads: Cell::new(None),
            fail_sims: Cell::new(0),
            lag_headers: Cell::new(None),
            stale_sims: RefCell::new(None),
            refuse_exits: Cell::new(None),
            other: Cell::new(OtherCalls::default()),
        }
    }

    /// While `code` is set, every simulation (`simulate`) of a transaction carrying short.win's
    /// `liquidate` or `expire` is answered as the program's refusal of that instruction with
    /// custom error `code` (Anchor's error log, `custom program error`), the VM untouched: what a
    /// short.win build whose account parsing neither of the keeper's curve TradeExtras lists
    /// matches answers, or a market whose ReadSet or VenueExtras fails that parsing (pump-final
    /// §7). `None` turns it off. Other simulations run as usual.
    pub fn refuse_exit_simulations(&self, code: Option<u32>) {
        self.refuse_exits.set(code.map(|c| (c, false)));
    }

    /// [`Self::refuse_exit_simulations`] only where short.win's own account parsing passes the
    /// exit's lists: a simulation the program refuses on account parsing itself
    /// (`BadRemainingAccounts`, `VenueAccountMismatch`) is answered as it ran. What a market whose
    /// VenueExtras fail `parse_extras` — after the TradeExtras parser — answers (pump-final §7):
    /// the list the deployed build takes is refused with `code`, the other by that parser. `None`
    /// turns it off.
    pub fn refuse_parsed_exit_simulations(&self, code: Option<u32>) {
        self.refuse_exits.set(code.map(|c| (c, true)));
    }

    /// The next `times` simulations (`simulate`) are answered from the VM as it is now, with
    /// this slot as their context slot — a node of an RPC pool that stays behind the chain
    /// (AIR-01: the flag's simulation served from a bank before the deploy the keeper's header
    /// read showed). The copy checks no signature and no blockhash (the keeper's blockhash is
    /// newer than it).
    pub fn lag_simulations(&self, times: usize) {
        self.lag(times, false);
    }

    /// [`Self::lag_simulations`] for the next `times` simulations of a transaction carrying
    /// short.win's `liquidate` or `expire` only (pump-final §7: a curve exit simulated on a bank
    /// before short.win's upgrade the keeper's header read showed); the others run as usual.
    pub fn lag_exit_simulations(&self, times: usize) {
        self.lag(times, true);
    }

    fn lag(&self, times: usize, exits_only: bool) {
        let env = self.env.borrow();
        let svm = env
            .svm
            .clone()
            .with_sigverify(false)
            .with_blockhash_check(false);
        *self.stale_sims.borrow_mut() = Some((svm, env.clock().slot, times, exits_only));
    }

    /// `tx` simulated on the VM, or on the copy of [`Self::lag_simulations`] while it answers.
    fn run_simulation(&self, tx: &VersionedTransaction) -> TxOutcome {
        let env = self.env.borrow();
        let alts = lookup_tables(&env, tx);
        if let Some((svm, slot, times, exits_only)) = self.stale_sims.borrow_mut().as_mut()
            && *times > 0
            && (!*exits_only || exit_index(&env, tx).is_some())
        {
            *times -= 1;
            return simulated(svm, tx, &alts, *slot);
        }
        simulated(&env.svm, tx, &alts, env.clock().slot)
    }

    /// The calls the [`Rpc`] log leaves out, so far.
    pub fn other_calls(&self) -> OtherCalls {
        self.other.get()
    }

    fn count(&self, f: impl FnOnce(&mut OtherCalls)) {
        let mut c = self.other.get();
        f(&mut c);
        self.other.set(c);
    }

    /// The next `times` header reads naming no context slot (`account_headers`) answer
    /// ProgramData `key` as a node of an RPC pool still behind its last deploy would: with
    /// `stale` as its deploy slot. A read as of a context slot (`account_headers_at`) is answered
    /// by a node that has reached it: the account as it is (AIR-18).
    pub fn lag_headers_of(&self, key: Pubkey, stale: u64, times: usize) {
        self.lag_headers.set(Some((key, stale, times)));
    }

    /// The next `times` simulations (`simulate`) fail with a transport error — a node that
    /// could not simulate (AIR-01: the flag's simulation unable to judge a moved deploy slot).
    pub fn fail_simulations(&self, times: usize) {
        self.fail_sims.set(times);
    }

    /// The next `times` whole-account reads (`accounts`) naming `key` fail with a transport
    /// error; header reads (`account_headers`, JSON-RPC's `dataSlice`) are unaffected — AIR-01's
    /// whole ProgramData read failing while the latch's header read works.
    pub fn fail_reads_of(&self, key: Pubkey, times: usize) {
        self.fail_reads.set(Some((key, times)));
    }

    /// The accounts at `keys` as the VM holds them (one logged `getMultipleAccounts`).
    fn read(&self, keys: &[Pubkey]) -> Vec<Option<AccountData>> {
        self.rpc.borrow_mut().push(Rpc::Accounts(keys.to_vec()));
        let env = self.env.borrow();
        keys.iter()
            .map(|k| {
                env.svm.get_account(k).map(|a| AccountData {
                    lamports: a.lamports,
                    owner: a.owner,
                    executable: a.executable,
                    data: a.data,
                })
            })
            .collect()
    }

    /// The keeper reads a clock `s` seconds behind the chain's (its transactions still land at
    /// the chain's clock; M7-SEC-1).
    pub fn set_clock_lag(&self, s: i64) {
        self.clock_lag.set(s);
    }

    /// `f` runs on the chain right before the keeper's next `send` executes (M7-SEC-2 (b)).
    pub fn before_next_send(&self, f: impl FnOnce(&mut Env) + 'static) {
        *self.before_send.borrow_mut() = Some(Box::new(f));
    }

    /// Every `send` from now on lands `delays()` seconds after the clock the keeper read (the
    /// chain's clock advances by that much, two and a half slots a second, before the
    /// transaction executes); `None` turns it off. Simulations are unaffected.
    pub fn set_landing_delays(&self, delays: Option<LandingDelays>) {
        *self.landing.borrow_mut() = delays;
    }

    /// Every backend call so far, in order (M4-SEC-7).
    pub fn rpc_log(&self) -> Vec<Rpc> {
        self.rpc.borrow().clone()
    }

    /// Accept (or refuse, as a cluster without SIMD-0385) v1 transactions.
    pub fn set_v1(&self, accepted: bool) {
        self.v1.set(accepted);
    }

    /// The recent prioritization fees the backend reports.
    pub fn set_priority_fees(&self, fees: Vec<u64>) {
        *self.fees.borrow_mut() = fees;
    }

    /// Every transaction sent so far.
    pub fn sent(&self) -> Vec<SentTx> {
        self.sent.borrow().clone()
    }

    fn refused(&self, tx: &VersionedTransaction) -> Option<TxOutcome> {
        (matches!(tx.message, VersionedMessage::V1(_)) && !self.v1.get()).then(|| TxOutcome {
            err: Some(TxError::Rejected(
                "unsupported transaction version 1".into(),
            )),
            ..TxOutcome::default()
        })
    }
}

/// The lookup tables a v0 transaction names, read from the VM.
fn lookup_tables(env: &Env, tx: &VersionedTransaction) -> Vec<AddressLookupTableAccount> {
    tx.message
        .address_table_lookups()
        .unwrap_or_default()
        .iter()
        .filter_map(|l| {
            let a = env.svm.get_account(&l.account_key)?;
            tx::parse_lookup_table(l.account_key, &a.owner, &a.data).ok()
        })
        .collect()
}

/// The index of `tx`'s first short.win `liquidate` or `expire` instruction, if any.
fn exit_index(env: &Env, tx: &VersionedTransaction) -> Option<u8> {
    use anchor_lang::Discriminator;
    let alts = lookup_tables(env, tx);
    let ixs = tx::decompile(tx, &alts).ok()?;
    let i = ixs.iter().position(|ix| {
        ix.program_id == shortwin::ID
            && (ix
                .data
                .starts_with(shortwin::instruction::Liquidate::DISCRIMINATOR)
                || ix
                    .data
                    .starts_with(shortwin::instruction::Expire::DISCRIMINATOR))
    })?;
    u8::try_from(i).ok()
}

/// short.win's refusal of top-level instruction `index` with custom error `code`, as a simulation
/// in `slot` reports it ([`LiteSvmChain::refuse_exit_simulations`]).
fn injected_refusal(index: u8, code: u32, slot: u64) -> TxOutcome {
    let id = shortwin::ID;
    TxOutcome {
        err: Some(TxError::Custom { index, code }),
        logs: vec![
            format!("Program {id} invoke [1]"),
            format!(
                "Program log: AnchorError thrown in keeper-litesvm (injected). Error Code: \
                 Injected. Error Number: {code}. Error Message: an injected refusal."
            ),
            format!("Program {id} failed: custom program error: {code:#x}"),
        ],
        slot,
        ..TxOutcome::default()
    }
}

/// Whether `out` is short.win's own refusal on account parsing (`BadRemainingAccounts`,
/// `VenueAccountMismatch`; [`LiteSvmChain::refuse_parsed_exit_simulations`]).
fn parse_refusal(out: &TxOutcome) -> bool {
    use shortwin::errors::ShortwinError as E;
    out.err
        .as_ref()
        .and_then(TxError::custom_code)
        .filter(|c| {
            *c == u32::from(E::BadRemainingAccounts) || *c == u32::from(E::VenueAccountMismatch)
        })
        .is_some_and(|c| {
            let failed = format!(
                "Program {} failed: custom program error: {c:#x}",
                shortwin::ID
            );
            out.logs.contains(&failed)
        })
}

/// `tx` simulated on `svm` as a node at `slot` answers it.
fn simulated(
    svm: &litesvm::LiteSVM,
    tx: &VersionedTransaction,
    alts: &[AddressLookupTableAccount],
    slot: u64,
) -> TxOutcome {
    match svm.simulate_transaction(tx.clone()) {
        Ok(info) => outcome(tx, alts, &info.meta, None, slot),
        Err(f) => outcome(
            tx,
            alts,
            &f.meta,
            Some(TxError::from_transaction_error(f.err)),
            slot,
        ),
    }
}

/// The outcome of `tx` run in `slot` (its metadata `meta`, its failure `err`).
fn outcome(
    tx: &VersionedTransaction,
    alts: &[AddressLookupTableAccount],
    meta: &TransactionMetadata,
    err: Option<TxError>,
    slot: u64,
) -> TxOutcome {
    let keys =
        tx::account_keys(tx, alts).unwrap_or_else(|_| tx.message.static_account_keys().to_vec());
    let n_top = tx.message.instructions().len();
    let mut inner = vec![Vec::new(); n_top];
    for (i, list) in meta.inner_instructions.iter().enumerate() {
        if let Some(slot) = inner.get_mut(i) {
            for ii in list {
                if let Some(program_id) = keys.get(usize::from(ii.instruction.program_id_index)) {
                    slot.push(InnerIx {
                        program_id: *program_id,
                        data: ii.instruction.data.clone(),
                    });
                }
            }
        }
    }
    TxOutcome {
        signature: meta.signature,
        err,
        logs: meta.logs.clone(),
        units_consumed: meta.compute_units_consumed,
        fee: meta.fee,
        inner,
        slot,
    }
}

/// Tracks every existing `Market` an instruction of `ixs` names (as `Env::send`).
fn track_named(env: &mut Env, ixs: &[Instruction]) {
    let named: Vec<Pubkey> = ixs
        .iter()
        .flat_map(|ix| ix.accounts.iter().map(|m| m.pubkey))
        .filter(|k| read::zc::<Market>(&env.svm, k).is_some())
        .collect();
    for k in named {
        env.track_market(k);
    }
}

/// Sends `tx` on `env` with the invariant hooks after every instruction (module docs).
/// Panics on a violation.
pub fn send_with_hooks(
    env: &mut Env,
    tx: &VersionedTransaction,
) -> Result<TransactionMetadata, litesvm::types::FailedTransactionMetadata> {
    if !env.check_invariants {
        // A test that corrupts state on purpose turned the hooks off (as `Env::send`).
        // `execute_recorded`: the harness's session recording hook (INDEXER.md §11.3).
        let res = env.execute_recorded(tx.clone());
        env.svm.expire_blockhash();
        return res;
    }
    let alts = lookup_tables(env, tx);
    let ixs = tx::decompile(tx, &alts).expect("decompiling the keeper's transaction");
    let payer = *tx
        .message
        .static_account_keys()
        .first()
        .expect("a fee payer");
    track_named(env, &ixs);
    let admins = env.admin_keys();
    let mut pre = invariants::snapshot(&env.svm, env.tracked_markets());
    let mut prefix_ok = true;
    let mut prefix_failure = String::new();
    for k in 1..ixs.len() {
        let prefix = &ixs[..k];
        let mut svm = env.svm.clone().with_sigverify(false);
        let msg = Message::new_with_blockhash(prefix, Some(&payer), tx.message.recent_blockhash());
        let n_sig = usize::from(msg.header.num_required_signatures);
        let prefix_tx = VersionedTransaction {
            signatures: vec![Signature::default(); n_sig],
            message: VersionedMessage::Legacy(msg),
        };
        let meta = match svm.send_transaction(prefix_tx) {
            Ok(meta) => meta,
            Err(f) => {
                prefix_ok = false;
                prefix_failure = format!(
                    "prefix of {k} instructions: {:?}\n{}",
                    f.err,
                    f.meta.pretty_logs()
                );
                break;
            }
        };
        let fail = |v: invariants::Violation| -> ! {
            panic!(
                "{v}\n(after instruction {k} of {} of a keeper transaction)\nlogs:\n{}",
                ixs.len(),
                meta.pretty_logs()
            )
        };
        let ev = invariants::IxEvents::of_instruction(&meta, k - 1, &ixs[k - 1]);
        let admin = invariants::is_admin_signed(&ixs[k - 1], &admins);
        if let Err(v) = invariants::check_ix(&svm, &pre, admin, &ev) {
            fail(v);
        }
        let mut markets: Vec<Pubkey> = env.tracked_markets().to_vec();
        for key in prefix
            .iter()
            .flat_map(|ix| ix.accounts.iter().map(|m| m.pubkey))
            .filter(|key| read::zc::<Market>(&svm, key).is_some())
        {
            if !markets.contains(&key) {
                markets.push(key);
            }
        }
        for m in &markets {
            if let Err(v) = invariants::check_all(&svm, m) {
                fail(v);
            }
        }
        env.hook_checkpoints += 1;
        pre = invariants::snapshot(&svm, &markets);
    }

    // `execute_recorded`: the harness's session recording hook (INDEXER.md §11.3).
    let res = env.execute_recorded(tx.clone());
    env.svm.expire_blockhash();
    track_named(env, &ixs);
    env.hook_checkpoints += 1;
    let check_tracked = |env: &Env, meta: &TransactionMetadata| {
        for m in env.tracked_markets() {
            if let Err(v) = invariants::check_all(&env.svm, m) {
                panic!("{v}\nlogs:\n{}", meta.pretty_logs());
            }
        }
    };
    match &res {
        Ok(meta) => {
            assert!(
                prefix_ok,
                "a prefix of the keeper's transaction failed on the clone but the transaction \
                 succeeded\n{prefix_failure}\nlogs:\n{}",
                meta.pretty_logs()
            );
            let last = ixs.len().saturating_sub(1);
            if let Some(ix) = ixs.get(last) {
                let ev = invariants::IxEvents::of_instruction(meta, last, ix);
                let admin = invariants::is_admin_signed(ix, &admins);
                if let Err(v) = invariants::check_ix(&env.svm, &pre, admin, &ev) {
                    panic!(
                        "{v}\n(instruction {} of {} of a keeper transaction)\nlogs:\n{}",
                        last + 1,
                        ixs.len(),
                        meta.pretty_logs()
                    );
                }
            }
            check_tracked(env, meta);
        }
        Err(failed) => check_tracked(env, &failed.meta),
    }
    res
}

impl Chain for LiteSvmChain {
    async fn clock(&self) -> anyhow::Result<ChainClock> {
        let c = self.env.borrow().clock();
        Ok(ChainClock {
            slot: c.slot,
            unix_timestamp: c.unix_timestamp - self.clock_lag.get(),
        })
    }

    async fn accounts(&self, keys: &[Pubkey]) -> anyhow::Result<Vec<Option<AccountData>>> {
        if let Some((key, times)) = self.fail_reads.get()
            && times > 0
            && keys.contains(&key)
        {
            self.fail_reads.set(Some((key, times - 1)));
            self.rpc.borrow_mut().push(Rpc::Accounts(keys.to_vec()));
            anyhow::bail!("getMultipleAccounts: injected failure reading {key}");
        }
        Ok(self.read(keys))
    }

    async fn account_headers(
        &self,
        keys: &[Pubkey],
        len: usize,
    ) -> anyhow::Result<Vec<Option<AccountData>>> {
        let mut out = self.read(keys);
        for a in out.iter_mut().flatten() {
            a.data.truncate(len);
        }
        if let Some((key, stale, times)) = self.lag_headers.get()
            && times > 0
            && let Some(i) = keys.iter().position(|k| *k == key)
        {
            self.lag_headers.set(Some((key, stale, times - 1)));
            if let Some(slot) = out
                .get_mut(i)
                .and_then(Option::as_mut)
                .and_then(|a| a.data.get_mut(4..12))
            {
                slot.copy_from_slice(&stale.to_le_bytes());
            }
        }
        Ok(out)
    }

    async fn account_headers_at(
        &self,
        keys: &[Pubkey],
        len: usize,
        _min_context_slot: u64,
    ) -> anyhow::Result<Vec<Option<AccountData>>> {
        let mut out = self.read(keys);
        for a in out.iter_mut().flatten() {
            a.data.truncate(len);
        }
        Ok(out)
    }

    async fn program_accounts(
        &self,
        program: &Pubkey,
        filters: &[Memcmp],
    ) -> anyhow::Result<Vec<(Pubkey, AccountData)>> {
        self.rpc.borrow_mut().push(Rpc::ProgramAccounts);
        let env = self.env.borrow();
        Ok(env
            .svm
            .get_program_accounts(program)
            .into_iter()
            .filter(|(_, a)| {
                filters.iter().all(|f| {
                    a.data
                        .get(f.offset..f.offset.saturating_add(f.bytes.len()))
                        .is_some_and(|b| b == f.bytes.as_slice())
                })
            })
            .map(|(k, a)| {
                (
                    k,
                    AccountData {
                        lamports: a.lamports,
                        owner: a.owner,
                        executable: a.executable,
                        data: a.data,
                    },
                )
            })
            .collect())
    }

    async fn latest_blockhash(&self) -> anyhow::Result<Hash> {
        self.count(|c| c.blockhashes += 1);
        Ok(self.env.borrow().svm.latest_blockhash())
    }

    async fn simulate(&self, tx: &VersionedTransaction) -> anyhow::Result<TxOutcome> {
        self.count(|c| c.simulations += 1);
        if let Some(r) = self.refused(tx) {
            return Ok(r);
        }
        if let Some(left) = self.fail_sims.get().checked_sub(1) {
            self.fail_sims.set(left);
            anyhow::bail!("simulateTransaction: injected failure");
        }
        let refusal = self.refuse_exits.get().and_then(|(code, after_parse)| {
            let env = self.env.borrow();
            exit_index(&env, tx)
                .map(|index| (injected_refusal(index, code, env.clock().slot), after_parse))
        });
        match refusal {
            Some((injected, false)) => Ok(injected),
            Some((injected, true)) => {
                let out = self.run_simulation(tx);
                Ok(if parse_refusal(&out) { out } else { injected })
            }
            None => Ok(self.run_simulation(tx)),
        }
    }

    async fn send(&self, tx: &VersionedTransaction) -> anyhow::Result<TxOutcome> {
        if let Some(r) = self.refused(tx) {
            return Ok(r);
        }
        let delay = self.landing.borrow_mut().as_mut().map_or(0, |f| f());
        if delay > 0 {
            self.env.borrow_mut().warp(delay);
        }
        if let Some(f) = self.before_send.borrow_mut().take() {
            f(&mut self.env.borrow_mut());
        }
        let (res, alts) = {
            let mut env = self.env.borrow_mut();
            let alts = lookup_tables(&env, tx);
            (send_with_hooks(&mut env, tx), alts)
        };
        let slot = self.env.borrow().clock().slot;
        let out = match &res {
            Ok(meta) => outcome(tx, &alts, meta, None, slot),
            Err(f) => outcome(
                tx,
                &alts,
                &f.meta,
                Some(TxError::from_transaction_error(f.err.clone())),
                slot,
            ),
        };
        let ixs = tx::decompile(tx, &alts).unwrap_or_default();
        self.rpc.borrow_mut().push(Rpc::Send {
            instructions: ixs.iter().filter(|i| i.program_id == shortwin::ID).count(),
        });
        self.sent
            .borrow_mut()
            .push(sent_record(tx, &ixs, &out, slot));
        Ok(out)
    }

    async fn recent_priority_fees(&self, _writable: &[Pubkey]) -> anyhow::Result<Vec<u64>> {
        self.count(|c| c.priority_fees += 1);
        Ok(self.fees.borrow().clone())
    }
}

/// Writes an active address lookup table holding `addresses` at `key` (as the ALT program
/// leaves it after `extend_lookup_table` in an earlier slot): the protocol ALT of A28 for the
/// v0 fallback.
pub fn install_lookup_table(env: &mut Env, key: Pubkey, addresses: &[Pubkey]) {
    let mut data = vec![0u8; tx::ALT_META_SIZE];
    data[..4].copy_from_slice(&1u32.to_le_bytes()); // ProgramState::LookupTable
    data[4..12].copy_from_slice(&u64::MAX.to_le_bytes()); // deactivation_slot: active
    // last_extended_slot 0, start index 0, no authority (frozen), padding.
    for a in addresses {
        data.extend_from_slice(a.as_ref());
    }
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            key,
            solana_account::Account {
                lamports,
                data,
                owner: tx::ALT_PROGRAM_ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .expect("lookup table");
}

/// A test environment with a keeper (module docs).
pub struct Harness {
    pub env: Rc<RefCell<Env>>,
    pub keeper: Keeper<LiteSvmChain>,
    /// Every tick's report, in order.
    pub reports: Vec<TickReport>,
}

impl Harness {
    /// A keeper with `cfg` on `env`; its fee payer is funded with [`KEEPER_FUNDING`].
    ///
    /// Duty 11 (`crank_sol_usd`, `cfg.oracle`) is turned off when `env` holds no account at the
    /// configured Pyth feed address — most scenario environments plant none, and the keeper would
    /// otherwise raise `Alert::OracleUnreadable` in their first tick. The `oracle` scenarios plant
    /// the recorded `PriceUpdateV2` and turn the duty on ([`Keeper::config_mut`]).
    pub fn new(env: Env, mut cfg: KeeperConfig) -> Self {
        if env.svm.get_account(&env.config().pyth_sol_usd).is_none() {
            cfg.oracle.enabled = false;
        }
        let env = Rc::new(RefCell::new(env));
        let payer = Keypair::new();
        env.borrow_mut().fund(&payer.pubkey(), KEEPER_FUNDING);
        let chain = LiteSvmChain::new(env.clone());
        let keeper = Keeper::new(chain, payer, cfg).expect("keeper config");
        Self {
            env,
            keeper,
            reports: Vec::new(),
        }
    }

    /// Another keeper instance with `cfg` on the same chain (a restart, or a second region), with
    /// its own fee payer funded with [`KEEPER_FUNDING`] and a fresh state.
    pub fn another_keeper(&self, mut cfg: KeeperConfig) -> Keeper<LiteSvmChain> {
        let feed = self.env.borrow().config().pyth_sol_usd;
        if self.env.borrow().svm.get_account(&feed).is_none() {
            cfg.oracle.enabled = false;
        }
        let payer = Keypair::new();
        self.env.borrow_mut().fund(&payer.pubkey(), KEEPER_FUNDING);
        Keeper::new(LiteSvmChain::new(self.env.clone()), payer, cfg).expect("keeper config")
    }

    /// Replaces the keeper by a fresh instance with `cfg` (a restart: new state and fee payer,
    /// the same chain); the reports so far are kept.
    pub fn restart(&mut self, cfg: KeeperConfig) {
        self.keeper = self.another_keeper(cfg);
    }

    /// The keeper's backend.
    pub fn chain(&self) -> &LiteSvmChain {
        self.keeper.chain()
    }

    /// Runs `f` on the environment.
    pub fn with_env<R>(&self, f: impl FnOnce(&mut Env) -> R) -> R {
        f(&mut self.env.borrow_mut())
    }

    /// One keeper tick at the current clock (panics on a keeper error).
    pub fn tick(&mut self) -> &TickReport {
        let report = block_on(self.keeper.tick()).expect("keeper tick");
        self.reports.push(report);
        self.reports.last().expect("just pushed")
    }

    /// Advances one slot (same second) and ticks.
    pub fn slot(&mut self) -> &TickReport {
        {
            let mut env = self.env.borrow_mut();
            let c = env.clock();
            env.set_clock(c.unix_timestamp, c.slot + 1);
        }
        self.tick()
    }

    /// Advances one second (and 2 slots) and ticks.
    pub fn second(&mut self) -> &TickReport {
        self.env.borrow_mut().warp(1);
        self.tick()
    }

    /// Advances one second, ticks, and — when the tick sent anything (a crank, an exit, a
    /// coverage pause, a pool action, a sweep) — `extra` more seconds without a tick: on mainnet
    /// a tick that sends waits for `confirmed` (≈ 1 s), so the next tick reads a later second
    /// (M7-SEC-1 (b)). Returns the tick's report.
    pub fn second_slow(&mut self, extra: i64) -> TickReport {
        let r = self.second().clone();
        let sent = !r.cranked.is_empty()
            || !r.exits.is_empty()
            || !r.coverage_pauses.is_empty()
            || !r.sweeps.is_empty()
            || !r.pool_actions.is_empty();
        if sent && extra > 0 {
            self.idle(extra);
        }
        r
    }

    /// `n` seconds, one tick each.
    pub fn seconds(&mut self, n: u32) {
        for _ in 0..n {
            self.second();
        }
    }

    /// Advances `secs` without ticking (the keeper is down).
    pub fn idle(&mut self, secs: i64) {
        self.env.borrow_mut().warp(secs);
    }

    /// The current unix time.
    pub fn now(&self) -> i64 {
        self.env.borrow().now()
    }
}

/// Runs a future to completion on a current-thread runtime (the keeper's futures are not
/// `Send`; nothing in them awaits I/O on this backend).
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime")
        .block_on(f)
}
