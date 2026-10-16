//! Lender-pool invariants (ARCH §8.3 I1 claims side, I8 = ECON I-P1–I-P7), on the state the
//! program sees, after every transaction.
//!
//! Everything here is computed with `shortwin_math::pool` on copies of the stored accounts
//! (`Market` pool group, `MarketVault`, every `LenderPosition`, every `ShareEpoch` record of
//! the market): nothing is written back. The checks follow `pool_model.Pool.check()` v0.4.2:
//!
//! - [`check_market`] (per market, after every transaction):
//!   - records: one `ShareEpoch` per rolled epoch, consecutive, the live wipe record a full
//!     wipe, `conv_rem < 1,000^k` on a rebase record and 0 on a full wipe (KI-65); `wipe_live ⇒ S = 0`; `S = 0 ⇒ vest_bal = 0` (the vest is flushed whenever `S`
//!     reaches 0); `S = 0 ⇒ ΣB_Q = 0` (the last shares never leave while tokens are borrowed,
//!     ARCH §9 M0b note 4, M1-SEC-3);
//!   - **the forward chain** (KI-66 full) of every stored lender ([`fwd_state`]): none
//!     (`fwd_epoch`, `fwd_lag`, `fwd_shares` 0), or behind the pool with `fwd_epoch ≤ share
//!     epoch`, `FWD_HOLD` with `epoch < fwd_epoch` and no lag, `FWD_OPEN` with `epoch ≤
//!     fwd_epoch` and a lag of 0 or 1;
//!   - **I-P4 on the untouched state** (v0.4.1, R4A-2/R4B-3): Σ over lenders of
//!     `pool::allowance_view` (each lender touched alone, everyone else as stored) ≤ `T` +
//!     number of lenders;
//!   - then every lender is touched in turn (as `check()` does; the touches share one copy of
//!     the records, whose `conv_rem` each conversion advances, as the program's transactions
//!     would), and on that state: I1 claims side, **both directions** (SPEC §10's equality up
//!     to rounding, M1-FID-1): `Σ claims ≤ T + ΣB_Q` and `T + ΣB_Q − Σ claims ≤
//!     ⌈C·(VS + S − Σ shares)/(S + VS)⌉ + n` with `claims = ⌊shares·C/(S + VS)⌋`, `C = T + ΣB_Q +
//!     VA` over the current epoch (the virtual shares' part and one unit of rounding per
//!     lender); **`Σ shares = S`** (KI-65: conversions round cumulatively through each rebase
//!     record, so no share is orphaned; a conversion the program did not persist would leave
//!     one share too many or too few); I-P2 per lender `claimed ≤ real`,
//!     `real − claimed ≤ max(pending, 0)`, and `Σ(real − claimed) + unmatched + vest_bal ≤
//!     lender_sol`; the entitlement side `Σ pending ≤ lender_sol − vest_bal + receivable`
//!     (signed); I-P4 again, every lender
//!     touched; no lock effective at or after its `lock_until`; the calendar: `lock_norm` ≥ Σ
//!     of the live locks' terms, equal up to one unit per lender per rebase record;
//!   - **liveness** (I-P6) when nothing is borrowed and nothing is receivable: on copies,
//!     release the whole vest, touch everyone, claim twice in sequence; then either every
//!     positive entitlement is paid, or what is left is exactly the realised SOL that no
//!     lender was allocated (rounding), which must stay below [`liveness_dust_cap`].
//! - [`snapshot`] + [`check_ix`] (instruction level, against the state just before the
//!   instruction; M1-FID-3, M1-FID-4): every withdrawal is within the pro-rata allowance the
//!   lender had just before it, computed with only that lender touched (I-P4, C0-9), and never
//!   leaves a lender whose shares fell with a negative `pending` (C0-13); the SOL `claim_sol`
//!   pays never exceeds the lender's own entitlement just before it (I-P5; `claim_sol ×N` in
//!   one transaction is checked claim by claim, V2-2); token conservation of deposits and
//!   withdrawals (`T`, `S` and the lender's shares move by exactly the event's amounts, minted
//!   = `⌊amount·(S + VS)/C⌋`, paid = `⌊shares·C/(S + VS)⌋` on the pre-state; M1-FID-1); a
//!   market left with `S = 0` holds no more `T + ΣB_Q` than before, or than the virtual shares'
//!   part `⌈C·VS/(S + VS)⌉` when its last shares just left (M1-FID-1); and **locks never
//!   decrease** while `now < lock_until` and `Π`, its epoch and the share epoch are unchanged
//!   (the withdrawing lender's down to its remaining shares at most; ARCH §8.3 I8, V2-3, R3B-1;
//!   M1-FID-9); and **a `LenderPosition` disappears only in `withdraw` or `claim_sol`** of its
//!   owner, with a post-state `pool::closable` recomputed on the pre-state (a withdrawal's
//!   after its handler's prologue), its rent going to the owner, and a claim or withdrawal
//!   whose recomputed post-state is closable closed it (KI-67; [`closes_only_when_empty`]);
//!   and **every vest flush is reported** (AIR-07, the indexer's `vest_bal`): an instruction
//!   that takes a running vest to 0 before its end emits `SolSettle.vest_flushed > 0` or a
//!   lender event with `s_after = 0` ([`vest_flushes_are_reported`]).

use std::cell::Cell;

use anchor_lang::AccountDeserialize;
use anchor_lang::prelude::Pubkey;
use litesvm::LiteSVM;
use shortwin::engine::prologue;
use shortwin::state::{GlobalConfig, LenderPosition, Market, MarketVault, ShareEpoch};
use shortwin_math::pool::{
    self, Lender, NO_LOCK_EPOCH, Pool, ShareEpoch as MathShareEpoch, Standing, Vault,
};
use solana_clock::Clock;

use super::{Violation, ensure, fail, lenders_of};
use crate::read;

thread_local! {
    static DUST_CAP: Cell<u64> = const { Cell::new(DEFAULT_LIVENESS_DUST_CAP) };
    static SHARELESS_BORROW: Cell<bool> = const { Cell::new(false) };
}

/// Relaxes `S = 0 ⇒ ΣB_Q = 0` for this test thread: only the `pool_model` trace replays, whose
/// injected borrower side borrows orphan tokens while `S = 0` (the model has no such rule;
/// M2's `open_short` must refuse it). Every lender instruction is still held to the program's
/// own `LastSharesBorrowed` rule.
pub fn allow_shareless_borrow(on: bool) {
    SHARELESS_BORROW.with(|c| c.set(on));
}

fn shareless_borrow_allowed() -> bool {
    SHARELESS_BORROW.with(Cell::get)
}

/// Default bound on the realised SOL no lender is allocated after a wind-down (lamports):
/// 0.01 SOL, the "seeds above 0.01 SOL" line of the R3A-2 stranding table (ECON §9.4).
/// Rounding leaves a few lamports per touch (since KI-65 no share is orphaned at a rebase, so
/// nothing is credited to shares nobody holds); the `pool_model` trace replays point the cap at
/// the model's own figure.
pub const DEFAULT_LIVENESS_DUST_CAP: u64 = 10_000_000;

/// The current cap (per test thread).
pub fn liveness_dust_cap() -> u64 {
    DUST_CAP.with(Cell::get)
}

/// Sets the cap for this test thread (the trace replays: the model's own wind-down figure).
pub fn set_liveness_dust_cap(cap: u64) {
    DUST_CAP.with(|c| c.set(cap));
}

/// A market's lender-pool state as the program sees it.
#[derive(Clone)]
pub struct PoolState {
    pub market: Pubkey,
    pub pool: Pool,
    pub vault: Box<Vault>,
    /// Every `LenderPosition` of the market, by key.
    pub lenders: Vec<(Pubkey, LenderPosition)>,
    /// Every `ShareEpoch` record of the market, by epoch.
    pub records: Vec<MathShareEpoch>,
    pub open_positions: u32,
    pub now: i64,
}

impl PoolState {
    /// Loads the state of `market` (`None` if it has no market or vault).
    pub fn load(svm: &LiteSVM, market: &Pubkey) -> Option<Self> {
        let m = read::zc::<Market>(svm, market)?;
        let v = read::zc::<MarketVault>(svm, &m.market_vault)?;
        Some(Self::from_accounts(svm, market, &m, &v))
    }

    /// The state from already-read accounts.
    pub fn from_accounts(svm: &LiteSVM, market: &Pubkey, m: &Market, v: &MarketVault) -> Self {
        let mut lenders = lenders_of(svm, market);
        lenders.sort_by_key(|(k, _)| *k);
        Self {
            market: *market,
            pool: m.pool(),
            vault: v.vault(),
            lenders,
            records: epoch_records(svm, market),
            open_positions: m.open_positions,
            now: svm.get_sysvar::<Clock>().unix_timestamp,
        }
    }

    /// The records from `epoch` on (what a lender at `epoch` needs).
    pub fn records_from(&self, epoch: u32) -> &[MathShareEpoch] {
        let i = self
            .records
            .iter()
            .position(|r| r.epoch >= epoch)
            .unwrap_or(self.records.len());
        &self.records[i..]
    }

    /// The records from `epoch` on, mutable: a conversion writes `conv_rem` (KI-65), so
    /// touches on copies thread one copy of the records through every lender, as the program's
    /// transactions would.
    pub fn records_from_mut(records: &mut [MathShareEpoch], epoch: u32) -> &mut [MathShareEpoch] {
        let i = records
            .iter()
            .position(|r| r.epoch >= epoch)
            .unwrap_or(records.len());
        &mut records[i..]
    }

    /// The lender of `owner`, if any.
    pub fn lender_of(&self, owner: &Pubkey) -> Option<&LenderPosition> {
        self.lenders
            .iter()
            .map(|(_, l)| l)
            .find(|l| l.owner == *owner)
    }
}

/// Whether the conversion that took a lender from `before` to `after` with `records` read a
/// rebase record, by the math crate alone: it moved the lender's epoch, and given only the
/// records before the first rebase record the conversion stops `Behind` (it needed that
/// record). Only the owner's `sync_lender` may do that (KI65-REV-SEC1); anyone else's is the
/// pot-only touch (KI-66), which never does. The program's own test is
/// `engine::lender::reads_rebase`.
pub fn reads_rebase(p: &Pool, before: &Lender, after: &Lender, records: &[MathShareEpoch]) -> bool {
    let first = records
        .iter()
        .position(|r| !r.is_full_wipe())
        .unwrap_or(records.len());
    let mut cut = *before;
    after.epoch > before.epoch
        && pool::convert(p, &mut cut, &mut records[..first].to_vec()) == Ok(Standing::Behind)
}

/// What a third party's `sync_lender(n)` of `owner`'s position leaves (KI-66: the pot-only
/// touch `pool::touch_pot` with the `n` records from the lender's epoch — KI-66 full: from its
/// forward chain's `fwd_epoch` once it has one — on the pre-state): the lender and the vault.
/// `None` without a position or if the math crate refuses.
pub fn third_party_sync(st: &PoolState, owner: &Pubkey, n: usize) -> Option<(Lender, Vault)> {
    let mut l = st.lender_of(owner)?.lender();
    let from = if l.fwd == pool::FWD_NONE {
        l.epoch
    } else {
        l.fwd_epoch
    };
    let recs: Vec<MathShareEpoch> = st.records_from(from).iter().take(n).copied().collect();
    let mut v = *st.vault;
    pool::touch_pot(&st.pool, &mut v, &mut l, &recs, st.now).ok()?;
    Some((l, v))
}

/// KI-66 full: the forward-chain fields of a stored lender (ARCH §2.3.6 `LenderPosition`): no
/// chain (`fwd_epoch`, `fwd_lag` and `fwd_shares` 0), or one the pot-only touch started at a
/// rebase record (`FWD_HOLD`: `epoch < fwd_epoch`, no lag) or the owner's conversion entered
/// (`FWD_OPEN`: `epoch ≤ fwd_epoch`, lag 0 or 1), on a lender behind the pool, never ahead of
/// it (`fwd_epoch ≤ share_epoch`).
pub fn fwd_state(p: &Pool, l: &Lender) -> bool {
    match l.fwd {
        pool::FWD_NONE => l.fwd_epoch == 0 && l.fwd_lag == 0 && l.fwd_shares == 0,
        pool::FWD_HOLD => {
            l.epoch < p.share_epoch
                && l.fwd_epoch <= p.share_epoch
                && l.epoch < l.fwd_epoch
                && l.fwd_lag == 0
        }
        pool::FWD_OPEN => {
            l.epoch < p.share_epoch
                && l.fwd_epoch <= p.share_epoch
                && l.epoch <= l.fwd_epoch
                && l.fwd_lag <= 1
        }
        _ => false,
    }
}

/// Every `ShareEpoch` record of `market` at its own address `["share_epoch", market, epoch]`
/// (the only ones the program accepts), sorted by epoch.
pub fn epoch_records(svm: &LiteSVM, market: &Pubkey) -> Vec<MathShareEpoch> {
    let mut out: Vec<MathShareEpoch> = read::all_of::<ShareEpoch>(svm)
        .into_iter()
        .filter_map(|(k, a)| {
            let r = ShareEpoch::try_deserialize(&mut a.data.as_slice()).ok()?;
            (r.market == *market && record_address(market, r.epoch) == k).then(|| r.record())
        })
        .collect();
    out.sort_by_key(|r| r.epoch);
    out
}

thread_local! {
    static RECORD_KEYS: std::cell::RefCell<std::collections::HashMap<(Pubkey, u32), Pubkey>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// `["share_epoch", market, epoch]`, memoised per test thread (a bump search per record per
/// transaction dominates long trace replays).
fn record_address(market: &Pubkey, epoch: u32) -> Pubkey {
    RECORD_KEYS.with(|m| {
        *m.borrow_mut()
            .entry((*market, epoch))
            .or_insert_with(|| crate::pda::share_epoch(market, epoch).0)
    })
}

/// `⌊shares·(T + ΣB_Q + VA)/(S + VS)⌋` in u128 (the math crate's `tokens_for_shares`).
fn token_value(p: &Pool, shares: u128) -> Result<u128, Violation> {
    pool::tokens_for_shares(p, shares).map_err(|e| Violation {
        invariant: "I1",
        detail: format!("tokens_for_shares({shares}) failed: {e:?}"),
    })
}

/// A lender touched on copies, as `pool::touch` would leave it (and the vault).
fn touch(
    st: &PoolState,
    records: &mut [MathShareEpoch],
    vault: &mut Vault,
    l: &LenderPosition,
    who: &Pubkey,
) -> Result<Lender, Violation> {
    let mut x = l.lender();
    let recs = PoolState::records_from_mut(records, l.epoch);
    pool::touch(&st.pool, vault, &mut x, recs, st.now).map_err(|e| Violation {
        invariant: "I8",
        detail: format!(
            "market {}: lender {who} cannot be touched with the stored records: {e:?}",
            st.market
        ),
    })?;
    Ok(x)
}

/// The per-market lender invariants (see the module docs).
pub fn check_market(
    svm: &LiteSVM,
    market: &Pubkey,
    m: &Market,
    v: &MarketVault,
) -> Result<(), Violation> {
    let st = PoolState::from_accounts(svm, market, m, v);
    check_state(&st)
}

/// [`check_market`] on a loaded state.
pub fn check_state(st: &PoolState) -> Result<(), Violation> {
    let p = &st.pool;
    let n = st.lenders.len() as u128;
    let market = st.market;

    // Records and the wipe / vest states.
    for (i, r) in st.records.iter().enumerate() {
        ensure(r.epoch as usize == i, "I8", || {
            format!(
                "market {market}: ShareEpoch records not one per epoch from 0 (#{i} is epoch {})",
                r.epoch
            )
        })?;
        // KI-65: a rebase record's conversion remainder stays below its denominator; a full
        // wipe's is never written.
        let rem_ok = if r.is_full_wipe() {
            r.conv_rem == 0
        } else {
            r.den().is_ok_and(|d| r.conv_rem < d)
        };
        ensure(rem_ok, "I8", || {
            format!(
                "market {market}: ShareEpoch {} conv_rem {} out of range (num {}, k {})",
                r.epoch, r.conv_rem, r.num, r.den_log1000
            )
        })?;
    }
    ensure(st.records.len() as u32 == p.share_epoch, "I8", || {
        format!(
            "market {market}: {} ShareEpoch records for share epoch {}",
            st.records.len(),
            p.share_epoch
        )
    })?;
    if p.is_wipe_live() {
        ensure(p.total_shares == 0, "I8", || {
            format!(
                "market {market}: wipe record live with S = {}",
                p.total_shares
            )
        })?;
        let last = st.records.last();
        ensure(last.is_some_and(MathShareEpoch::is_full_wipe), "I8", || {
            format!("market {market}: wipe live but the latest record is not a full wipe")
        })?;
    }
    ensure(p.total_shares != 0 || p.vest_bal == 0, "I8", || {
        format!(
            "market {market}: S = 0 with an unreleased vest {}",
            p.vest_bal
        )
    })?;
    // ARCH §9 M0b note 4 (M1-SEC-3): no borrowed tokens without shares.
    ensure(
        p.total_shares != 0 || p.borrowed_q == 0 || shareless_borrow_allowed(),
        "I8",
        || {
            format!(
                "market {market}: S = 0 while ΣB_Q = {} is borrowed",
                p.borrowed_q
            )
        },
    )?;

    // KI-66 full: every stored forward chain is consistent.
    for (key, l) in &st.lenders {
        ensure(fwd_state(p, &l.lender()), "I8", || {
            format!(
                "market {market}: lender {key} forward chain fwd {} fwd_epoch {} fwd_lag {} \
                 fwd_shares {} at epoch {} (share epoch {})",
                l.fwd, l.fwd_epoch, l.fwd_lag, l.fwd_shares, l.epoch, p.share_epoch
            )
        })?;
    }

    // I-P4 on the untouched state: each lender's allowance with only that lender touched.
    let mut seen: u128 = 0;
    for (key, l) in &st.lenders {
        let a = pool::allowance_view(p, &st.vault, &l.lender(), st.records_from(l.epoch), st.now)
            .map_err(|e| Violation {
            invariant: "I8",
            detail: format!("market {market}: allowance_view of {key} failed: {e:?}"),
        })?;
        seen += u128::from(a);
    }
    ensure(seen <= u128::from(p.tokens_free) + n, "I8", || {
        format!(
            "market {market}: I-P4 (untouched state): Σ allowances {seen} > T {} + {n}",
            p.tokens_free
        )
    })?;

    // Touch every lender in turn (pool_model.check(), which advances the calendar first).
    let mut vault = st.vault.clone();
    pool::cal_advance(&mut vault, st.now).map_err(|e| Violation {
        invariant: "I8",
        detail: format!("market {market}: cal_advance failed: {e:?}"),
    })?;
    let mut touched: Vec<(Pubkey, Lender)> = Vec::with_capacity(st.lenders.len());
    let mut recs = st.records.clone();
    for (key, l) in &st.lenders {
        touched.push((*key, touch(st, &mut recs, &mut vault, l, key)?));
    }

    // I1 claims side and share conservation, over the current epoch.
    let mut claims: u128 = 0;
    let mut cur_shares: u128 = 0;
    for (_, l) in touched.iter().filter(|(_, l)| l.epoch == p.share_epoch) {
        claims += token_value(p, l.shares)?;
        cur_shares += l.shares;
    }
    let assets = u128::from(p.tokens_free) + u128::from(p.borrowed_q);
    ensure(claims <= assets, "I1", || {
        format!("market {market}: token claims {claims} > T + ΣB_Q = {assets}")
    })?;
    // KI-65: conversions round cumulatively through each record, so once every lender is
    // touched every share has a holder: Σ shares = S exactly.
    ensure(cur_shares == p.total_shares, "I1", || {
        format!(
            "market {market}: Σ lender shares {cur_shares} != S {}",
            p.total_shares
        )
    })?;
    // I1's other direction (SPEC §10 is an equality; M1-FID-1): what no lender claims is at
    // most the virtual shares' part plus one unit per lender.
    if p.total_shares > 0 {
        let unclaimed = assets.saturating_sub(claims);
        let bound = unclaimed_bound(p, cur_shares).map_err(|e| Violation {
            invariant: "I1",
            detail: format!("market {market}: unclaimed bound failed: {e:?}"),
        })? + n;
        ensure(unclaimed <= bound, "I1", || {
            format!(
                "market {market}: T + ΣB_Q = {assets} but lenders claim only {claims} \
                 ({unclaimed} unclaimed > bound {bound}: S {}, Σ shares {cur_shares})",
                p.total_shares
            )
        })?;
    }

    // I-P2 claim side (strict, per lender) and the entitlement side (signed aggregate).
    let mut own: u128 = 0;
    let (mut pos, mut neg) = (0i128, 0i128);
    for (key, l) in &touched {
        ensure(l.claimed <= l.real, "I8", || {
            format!("lender {key}: claimed {} > real {}", l.claimed, l.real)
        })?;
        let unclaimed = l.real - l.claimed;
        ensure(
            i128::from(unclaimed) <= i128::from(l.pending.max(0)),
            "I8",
            || {
                format!(
                    "lender {key}: own allocation {unclaimed} above its entitlement {} after a touch",
                    l.pending
                )
            },
        )?;
        own += u128::from(unclaimed);
        pos += i128::from(l.pending.max(0));
        neg += i128::from((-l.pending).max(0));
    }
    let claim_side = own + u128::from(vault.unmatched) + u128::from(p.vest_bal);
    ensure(claim_side <= u128::from(vault.lender_sol), "I8", || {
        format!(
            "market {market}: Σ(real − claimed) {own} + unmatched {} + vest_bal {} > lender_sol {}",
            vault.unmatched, p.vest_bal, vault.lender_sol
        )
    })?;
    let backing = i128::from(vault.lender_sol) - i128::from(p.vest_bal) + i128::from(p.receivable);
    ensure(pos - neg <= backing, "I8", || {
        format!(
            "market {market}: Σ pending {pos} − {neg} > lender_sol {} − vest_bal {} + receivable {}",
            vault.lender_sol, p.vest_bal, p.receivable
        )
    })?;

    // I-P4 with everyone touched; locks never effective at or after lock_until.
    let mut total: u128 = 0;
    let mut live_terms: u128 = 0;
    for (key, l) in &touched {
        let a = pool::allowance(p, &vault, l, st.now).map_err(|e| Violation {
            invariant: "I8",
            detail: format!("market {market}: allowance of {key} failed: {e:?}"),
        })?;
        total += u128::from(a);
        let lock = pool::lock_of(&vault, l, st.now).unwrap_or(0);
        ensure(lock == 0 || st.now < l.lock_until, "I8", || {
            format!(
                "lender {key}: lock {lock} effective at {} ≥ lock_until {}",
                st.now, l.lock_until
            )
        })?;
        ensure(l.lock == 0 || l.lock_epoch != NO_LOCK_EPOCH, "I8", || {
            format!("lender {key}: lock {} without a Π epoch", l.lock)
        })?;
        let live =
            l.epoch == p.share_epoch && l.lock_epoch == vault.pi_epoch && st.now < l.lock_until;
        if live {
            live_terms += l.lock_term;
        }
    }
    ensure(total <= u128::from(p.tokens_free) + n, "I8", || {
        format!(
            "market {market}: I-P4 (all touched): Σ allowances {total} > T {} + {n}",
            p.tokens_free
        )
    })?;
    // The calendar: lock_norm (as of now) holds every live lock's term; rebases floor each
    // bucket once and each lender's term once, which leaves at most one unit per lender per
    // rebase record in the aggregate.
    let buckets: u128 = vault.lock_cal.iter().sum();
    ensure(buckets == vault.lock_norm, "I8", || {
        format!(
            "market {market}: lock_norm {} != Σ buckets {buckets}",
            vault.lock_norm
        )
    })?;
    let rebases = st.records.iter().filter(|r| r.num == 1).count() as u128;
    ensure(
        live_terms <= vault.lock_norm && vault.lock_norm <= live_terms + n * rebases,
        "I8",
        || {
            format!(
                "market {market}: calendar lock_norm {} vs Σ live lock terms {live_terms} \
                 ({rebases} rebases, {n} lenders)",
                vault.lock_norm
            )
        },
    )?;

    // Liveness (I-P6), when nothing is borrowed or receivable.
    if p.borrowed_q == 0 && p.receivable == 0 && st.open_positions == 0 {
        liveness(st)?;
    }
    Ok(())
}

/// `⌈C·(VS + S − held)/(S + VS)⌉`, `C = T + ΣB_Q + VA`: the token value of the virtual shares
/// and of the `S − held` shares no lender holds (I1's lower side, M1-FID-1; `held = S` since
/// KI-65, so only the virtual shares' part).
pub fn unclaimed_bound(p: &Pool, held: u128) -> Result<u128, shortwin_math::MathError> {
    use shortwin_math::num::{Rounding, mul_div};
    let c = u128::from(p.tokens_free) + u128::from(p.borrowed_q) + pool::VIRTUAL_ASSETS;
    let unheld = pool::VIRTUAL_SHARES + p.total_shares.saturating_sub(held);
    mul_div(
        c,
        unheld,
        p.total_shares + pool::VIRTUAL_SHARES,
        Rounding::Ceil,
    )
}

/// What a wind-down leaves (I-P6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindDown {
    /// SOL paid by the sequential claims.
    pub paid: u64,
    /// `lender_sol` left.
    pub stranded: u64,
    /// Realised SOL no lender was allocated before the claims (`lender_sol − Σ(real −
    /// claimed) − unmatched` after the release and the touches).
    pub unallocated: u64,
    /// Σ positive `pending` left unpaid.
    pub unpaid: u128,
}

/// The wind-down of ECON §9.4 on copies: release the whole vest, touch every lender, claim
/// twice in sequence. Requires nothing borrowed (every position resolved).
pub fn wind_down(st: &PoolState) -> Result<WindDown, Violation> {
    let market = st.market;
    let mut p = st.pool;
    let mut vault = st.vault.clone();
    let end = st.now.max(p.vest_end);
    pool::release(&mut p, end).map_err(|e| Violation {
        invariant: "I8",
        detail: format!("market {market}: wind-down release failed: {e:?}"),
    })?;
    let st2 = PoolState {
        pool: p,
        now: end,
        ..st.clone()
    };
    let mut ls: Vec<Lender> = Vec::with_capacity(st.lenders.len());
    let mut recs = st2.records.clone();
    for (key, l) in &st.lenders {
        ls.push(touch(&st2, &mut recs, &mut vault, l, key)?);
    }
    let own: u128 = ls.iter().map(|l| u128::from(l.real - l.claimed)).sum();
    let unallocated = u128::from(vault.lender_sol)
        .checked_sub(own + u128::from(vault.unmatched))
        .ok_or_else(|| Violation {
            invariant: "I8",
            detail: format!("market {market}: allocations exceed lender_sol at the wind-down"),
        })?;
    let mut paid = 0u64;
    for _ in 0..2 {
        for (i, l) in ls.iter_mut().enumerate() {
            let from = PoolState::records_from_mut(&mut recs, st.lenders[i].1.epoch);
            paid += pool::claim(&p, &mut vault, l, from, end).map_err(|e| Violation {
                invariant: "I8",
                detail: format!("market {market}: wind-down claim failed: {e:?}"),
            })?;
        }
    }
    let unpaid: u128 = ls.iter().map(|l| l.pending.max(0) as u128).sum();
    Ok(WindDown {
        paid,
        stranded: vault.lender_sol,
        unallocated: u64::try_from(unallocated).unwrap_or(u64::MAX),
        unpaid,
    })
}

/// I-P6 on the wound-down copies ([`wind_down`]). Both checks bind only while some
/// entitlement is unpaid: with nothing owed, any `lender_sol` left passes, however much of it
/// is allocated to no lender. Over-stranding with every lender paid is the fuzzer's to catch —
/// its wind-down's second claim pass bounds `lender_sol` by the dust cap (`tests/fuzz`;
/// FUZZ-M10-24H-A's rebase-orphan credit is gone since KI-65).
fn liveness(st: &PoolState) -> Result<(), Violation> {
    let w = wind_down(st)?;
    let market = st.market;
    ensure(w.unpaid == 0 || w.stranded <= w.unallocated, "I8", || {
        format!(
            "market {market}: liveness: {} lamports stranded with {} of entitlement unpaid, \
             but only {} were unallocated",
            w.stranded, w.unpaid, w.unallocated
        )
    })?;
    let cap = liveness_dust_cap();
    ensure(w.unpaid == 0 || w.unallocated <= cap, "I8", || {
        format!(
            "market {market}: liveness: {} lamports of realised SOL allocated to no lender \
             (cap {cap}) while {} of entitlement is unpaid",
            w.unallocated, w.unpaid
        )
    })
}

// ---- transaction level ---------------------------------------------------------------------

/// The lender state of the tracked markets before a transaction.
#[derive(Clone, Default)]
pub struct LenderSnapshot {
    pub markets: Vec<PoolState>,
    /// Per `LenderPosition` of those markets: `(market, owner, the position's lamports, the
    /// owner's lamports)` (KI-67's close hook).
    pub rents: Vec<(Pubkey, Pubkey, u64, u64)>,
    /// The `Market` accounts of those markets: a withdrawal's prologue (params sync and rent
    /// accrual) replayed by KI-67's close hook.
    pub accounts: Vec<(Pubkey, Market)>,
}

/// Snapshots the lender state of `markets`.
pub fn snapshot(svm: &LiteSVM, markets: &[Pubkey]) -> LenderSnapshot {
    let accounts = markets
        .iter()
        .filter_map(|m| read::zc::<Market>(svm, m).map(|a| (*m, a)))
        .collect();
    let markets: Vec<PoolState> = markets
        .iter()
        .filter_map(|m| PoolState::load(svm, m))
        .collect();
    let rents = markets
        .iter()
        .flat_map(|st| {
            st.lenders.iter().map(|(key, l)| {
                (
                    st.market,
                    l.owner,
                    read::lamports(svm, key),
                    read::lamports(svm, &l.owner),
                )
            })
        })
        .collect();
    LenderSnapshot {
        markets,
        rents,
        accounts,
    }
}

/// What an owner that is also the fee payer may pay besides (KI-67's rent check).
const FEE_SLACK: u64 = 100_000;

/// KI-67: a `LenderPosition` of `st` that no longer exists after the instruction was closed by
/// its owner's `withdraw` or `claim_sol`, with a closable post-state recomputed on the
/// pre-state — a claim's with `pool::touch` and `pool::claim`, a withdrawal's with its handler's
/// prologue on `account` (the pre-instruction `Market`: params sync, rent accrual), then
/// `pool::withdraw` of the event's shares and `pool::claim`; `pool::closable` in both — and its
/// rent went to the owner: the owner's lamports rose by exactly the position's rent plus the
/// SOL paid. Conversely a claim or withdrawal whose recomputed post-state is closable must have
/// closed the position.
pub fn closes_only_when_empty(
    svm: &LiteSVM,
    st: &PoolState,
    account: Option<&Market>,
    rents: &[(Pubkey, Pubkey, u64, u64)],
    ev: &super::IxEvents,
) -> Result<(), Violation> {
    let market = st.market;
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    let config = read::zc::<GlobalConfig>(svm, &crate::pda::config());
    for (_, pos) in &st.lenders {
        let owner = pos.owner;
        let gone =
            read::borsh::<LenderPosition>(svm, &crate::pda::lender(&market, &owner).0).is_none();
        let claim = ev
            .claims
            .iter()
            .find(|c| c.market == market && c.owner == owner);
        let withdraw = ev
            .withdraws
            .iter()
            .find(|w| w.market == market && w.owner == owner);
        let mut predicted = None;
        let expect_closed = match (claim, withdraw) {
            (Some(_), None) => {
                let (mut v, mut l, mut recs) = (st.vault.clone(), pos.lender(), st.records.clone());
                let recs = PoolState::records_from_mut(&mut recs, l.epoch);
                let ok = pool::touch(&st.pool, &mut v, &mut l, recs, now)
                    .and_then(|_| pool::claim(&st.pool, &mut v, &mut l, recs, now))
                    .is_ok_and(|_| pool::closable(&st.pool, &l));
                predicted = Some(l);
                ok
            }
            (None, Some(w)) => {
                // The handler's own steps on copies: the prologue (`market_now`, the params
                // sync, the rent accrual), `pool::withdraw` of the event's shares, `pool::claim`.
                let (Some(m), Some(config)) = (account, config.as_ref()) else {
                    return fail(
                        "I8",
                        format!("market {market}: no pre-instruction Market or config to replay"),
                    );
                };
                let mut m = *m;
                let t = prologue::market_now(&m, now);
                prologue::sync_market_params(config, &mut m, t);
                let (mut p, mut v, mut l, mut recs) =
                    (m.pool(), st.vault.clone(), pos.lender(), st.records.clone());
                let recs = PoolState::records_from_mut(&mut recs, l.epoch);
                let ok = prologue::accrue(&mut m, &mut p, &mut v, t).is_ok()
                    && pool::withdraw(
                        &mut p,
                        &mut v,
                        &mut l,
                        recs,
                        t,
                        config.params.lock_grace_s,
                        w.shares,
                    )
                    .and_then(|_| pool::claim(&p, &mut v, &mut l, recs, t))
                    .is_ok_and(|_| pool::closable(&p, &l));
                predicted = Some(l);
                ok
            }
            _ => false,
        };
        if !gone {
            ensure(!expect_closed, "I8", || {
                let stored =
                    read::borsh::<LenderPosition>(svm, &crate::pda::lender(&market, &owner).0)
                        .map(|p| p.lender());
                format!(
                    "market {market}: claim_sol / withdraw left {owner}'s empty position open \
                     (KI-67): predicted {predicted:?}, stored {stored:?}"
                )
            })?;
            continue;
        }
        ensure(expect_closed, "I8", || {
            format!(
                "market {market}: {owner}'s LenderPosition closed outside withdraw / claim_sol \
                 or with a post-state that is not closable (KI-67)"
            )
        })?;
        let Some(&(_, _, rent, before)) = rents
            .iter()
            .find(|(m, o, _, _)| *m == market && *o == owner)
        else {
            return fail(
                "I8",
                format!("market {market}: no rent snapshot for {owner}"),
            );
        };
        let paid = claim.map_or(0, |c| c.sol_paid) + withdraw.map_or(0, |w| w.sol_paid);
        let after = read::lamports(svm, &owner);
        // The owner may also pay the transaction's fee (a few signatures' worth).
        let due = before + rent + paid;
        ensure(after <= due && after + FEE_SLACK >= due, "I8", || {
            format!(
                "market {market}: {owner}'s position closed; the owner got {} lamports, not the \
                 rent {rent} + SOL {paid} (KI-67)",
                i128::from(after) - i128::from(before)
            )
        })?;
    }
    Ok(())
}

/// The instruction-level lender checks (see the module docs): `pre` is the state just before
/// the instruction, `svm` just after it, `ev` the instruction's events.
pub fn check_ix(
    svm: &LiteSVM,
    pre: &LenderSnapshot,
    ev: &super::IxEvents,
) -> Result<(), Violation> {
    let now = svm.get_sysvar::<Clock>().unix_timestamp;
    for st in &pre.markets {
        let market = st.market;
        let mine = |m: &Pubkey| *m == market;
        let st_now = PoolState { now, ..st.clone() };
        let post = PoolState::load(svm, &market);

        // Withdrawals: within the pro-rata allowance just before the instruction (C0-9;
        // M1-FID-4: every withdrawal, whatever else the transaction does).
        for w in ev.withdraws.iter().filter(|e| mine(&e.market)) {
            let l = st.lender_of(&w.owner).ok_or_else(|| Violation {
                invariant: "I8",
                detail: format!("withdraw by {} without a prior LenderPosition", w.owner),
            })?;
            let allow = pool::allowance_view(
                &st_now.pool,
                &st_now.vault,
                &l.lender(),
                st_now.records_from(l.epoch),
                now,
            )
            .unwrap_or(0);
            withdrawal_within_allowance(&market, w, allow)?;
            // No share reduction with a negative pending (post-state; C0-13).
            if let Some(l) =
                read::borsh::<LenderPosition>(svm, &crate::pda::lender(&market, &w.owner).0)
            {
                ensure(l.pending >= 0, "I8", || {
                    format!(
                        "lender {}: withdrew and left pending {} < 0",
                        w.owner, l.pending
                    )
                })?;
            }
        }
        // claim_sol: never above the lender's own entitlement just before it (I-P5, V2-2).
        for c in ev.claims.iter().filter(|e| mine(&e.market)) {
            let Some(l) = st.lender_of(&c.owner) else {
                return fail(
                    "I8",
                    format!("claim by {} without a LenderPosition", c.owner),
                );
            };
            let mut vault = st_now.vault.clone();
            let mut x = l.lender();
            let mut recs = st_now.records.clone();
            let standing = pool::touch(
                &st_now.pool,
                &mut vault,
                &mut x,
                PoolState::records_from_mut(&mut recs, l.epoch),
                now,
            );
            if matches!(standing, Ok(Standing::Current | Standing::Parked)) {
                let own = x.pending.max(0) as u128;
                ensure(u128::from(c.sol_paid) <= own, "I8", || {
                    format!(
                        "market {market}: {} claimed {} lamports, above its entitlement {own} \
                         (I-P5)",
                        c.owner, c.sol_paid
                    )
                })?;
            }
        }
        let account = pre
            .accounts
            .iter()
            .find(|(k, _)| *k == market)
            .map(|(_, a)| a);
        closes_only_when_empty(svm, st, account, &pre.rents, ev)?;
        let Some(post) = post else { continue };
        let deposits: Vec<_> = ev.deposits.iter().filter(|e| mine(&e.market)).collect();
        let withdraws: Vec<_> = ev.withdraws.iter().filter(|e| mine(&e.market)).collect();
        conservation(st, &post, &deposits, &withdraws)?;
        empty_pool_holds_no_new_tokens(st, &post)?;
        let withdrawer = withdraws.first().map(|w| w.owner);
        locks_never_decrease(st, &post, withdrawer.as_ref())?;
        vest_flushes_are_reported(st, &post, ev)?;
    }
    Ok(())
}

/// AIR-07's flush report (the indexer's `vest_bal`, INDEXER.md §5.3): an instruction that takes
/// a running vest to 0 before its end — a flush: a settlement that left at most 1/1,000 of the
/// token claims it found (a full wipe included), or the last shares leaving (R3B-2) — says so
/// in an event the indexer projects: `SolSettle.vest_flushed > 0` (with or without an epoch
/// roll), or a lender event with `s_after = 0`. A release that reaches the end leaves
/// `vest_last ≥ vest_end`; a flush leaves `vest_end` ahead of `vest_last`. Conversely a reported
/// flush leaves no vest, unless the settlement rolled the epoch (a rebase vests a pre-funded
/// record's lamports anew, ARCH §2.4.1).
pub fn vest_flushes_are_reported(
    pre: &PoolState,
    post: &PoolState,
    ev: &super::IxEvents,
) -> Result<(), Violation> {
    let market = pre.market;
    let (p0, p1) = (&pre.pool, &post.pool);
    let reported = ev
        .settlements
        .iter()
        .filter(|e| e.market == market)
        .map(|e| e.vest_flushed)
        .max()
        .unwrap_or(0);
    let last_shares = ev
        .withdraws
        .iter()
        .map(|e| (e.market, e.s_after))
        .chain(ev.claims.iter().map(|e| (e.market, e.s_after)))
        .chain(ev.deposits.iter().map(|e| (e.market, e.s_after)))
        .chain(ev.covers.iter().map(|e| (e.market, e.s_after)))
        .chain(ev.syncs.iter().map(|e| (e.market, e.s_after)))
        .any(|(m, s_after)| m == market && s_after == 0);
    let flushed = p0.vest_bal > 0 && p1.vest_bal == 0 && p1.vest_end > p1.vest_last;
    ensure(!flushed || reported > 0 || last_shares, "I8", || {
        format!(
            "market {market}: the vest ({} lamports, ending at {}) was flushed at {} and no \
             event reports it (AIR-07: SolSettle.vest_flushed, or a lender event's s_after = 0)",
            p0.vest_bal, p1.vest_end, p1.vest_last
        )
    })?;
    ensure(
        reported == 0 || p1.vest_bal == 0 || p1.share_epoch != p0.share_epoch,
        "I8",
        || {
            format!(
                "market {market}: SolSettle reports a flush of {reported} lamports and leaves a \
                 vest of {} without an epoch roll",
                p1.vest_bal
            )
        },
    )
}

/// C0-9 / I-P4: `w` took at most `allow`, the allowance its lender had just before it.
pub fn withdrawal_within_allowance(
    market: &Pubkey,
    w: &shortwin::events::Withdraw,
    allow: u64,
) -> Result<(), Violation> {
    ensure(w.amount <= allow, "I8", || {
        format!(
            "market {market}: {} withdrew {} tokens above its pro-rata allowance {allow}",
            w.owner, w.amount
        )
    })
}

/// Token conservation of one instruction's deposit or withdrawal (I1, M1-FID-1): `T` and `S`
/// move by exactly the event's amounts, the lender's shares too (when it was on the current
/// epoch before), and the amounts are the pool formulas on the pre-state (the prologue moves
/// neither `S`, `T` nor `ΣB_Q`).
pub fn conservation(
    pre: &PoolState,
    post: &PoolState,
    deposits: &[&shortwin::events::Deposit],
    withdraws: &[&shortwin::events::Withdraw],
) -> Result<(), Violation> {
    let market = pre.market;
    let (p0, p1) = (&pre.pool, &post.pool);
    if p0.share_epoch != p1.share_epoch || p0.borrowed_q != p1.borrowed_q {
        return Ok(()); // a roll or a borrower-side move (M2): not a lender instruction alone
    }
    let (d_t, d_s, owner) = match (deposits, withdraws) {
        ([], []) => (0i128, 0i128, None),
        ([d], []) => {
            let minted = pool::shares_for_tokens(p0, d.amount).map_err(|e| Violation {
                invariant: "I1",
                detail: format!("market {market}: shares_for_tokens failed: {e:?}"),
            })?;
            ensure(d.shares == minted, "I1", || {
                format!(
                    "market {market}: deposit of {} minted {} shares, the pre-state gives {minted}",
                    d.amount, d.shares
                )
            })?;
            (
                i128::from(d.amount),
                i128::try_from(d.shares).unwrap_or(i128::MAX),
                Some((d.owner, i128::try_from(d.shares).unwrap_or(i128::MAX))),
            )
        }
        ([], [w]) => {
            let paid = pool::tokens_for_shares(p0, w.shares).map_err(|e| Violation {
                invariant: "I1",
                detail: format!("market {market}: tokens_for_shares failed: {e:?}"),
            })?;
            ensure(u128::from(w.amount) == paid, "I1", || {
                format!(
                    "market {market}: withdrawal of {} shares paid {} tokens, the pre-state \
                     gives {paid}",
                    w.shares, w.amount
                )
            })?;
            let burnt = i128::try_from(w.shares).unwrap_or(i128::MAX);
            (-i128::from(w.amount), -burnt, Some((w.owner, -burnt)))
        }
        _ => {
            return fail(
                "I1",
                format!("market {market}: several deposits / withdrawals in one instruction"),
            );
        }
    };
    let t = i128::from(p1.tokens_free) - i128::from(p0.tokens_free);
    let s_moved = i128::try_from(p1.total_shares).unwrap_or(i128::MAX)
        - i128::try_from(p0.total_shares).unwrap_or(i128::MAX);
    ensure(t == d_t && s_moved == d_s, "I1", || {
        format!(
            "market {market}: T moved by {t} and S by {s_moved}; the instruction's events say \
             {d_t} and {d_s}"
        )
    })?;
    if let Some((owner, d_shares)) = owner {
        let before = pre.lender_of(&owner);
        let current = before.is_none_or(|l| l.epoch == p0.share_epoch);
        let after = post.lender_of(&owner).map_or(0, |l| l.shares);
        if current {
            let held = i128::try_from(before.map_or(0, |l| l.shares)).unwrap_or(i128::MAX);
            let after = i128::try_from(after).unwrap_or(i128::MAX);
            ensure(after - held == d_shares, "I1", || {
                format!(
                    "market {market}: lender {owner}'s shares moved by {}, the event says \
                     {d_shares}",
                    after - held
                )
            })?;
        }
    }
    Ok(())
}

/// A market left with `S = 0` holds no more tokens than before the instruction or, when its
/// last shares just left, than the virtual shares' part of the pre-state (M1-FID-1: tokens
/// that no lender owns must not appear).
pub fn empty_pool_holds_no_new_tokens(pre: &PoolState, post: &PoolState) -> Result<(), Violation> {
    let market = pre.market;
    let (p0, p1) = (&pre.pool, &post.pool);
    if p1.total_shares != 0 {
        return Ok(());
    }
    let after = u128::from(p1.tokens_free) + u128::from(p1.borrowed_q);
    let before = u128::from(p0.tokens_free) + u128::from(p0.borrowed_q);
    let cap = if p0.total_shares == 0 {
        before
    } else {
        unclaimed_bound(p0, p0.total_shares).unwrap_or(u128::MAX)
    };
    ensure(after <= cap, "I1", || {
        format!(
            "market {market}: S = 0 with T + ΣB_Q = {after} (before: {before}, S {}; at most \
             {cap})",
            p0.total_shares
        )
    })
}

/// Locks never decrease (ARCH §8.3 I8; V2-3, R3B-1; M1-FID-9): while `now < lock_until`, with
/// `Π`, its epoch and the share epoch unchanged, every lender's effective lock `lock_i(now)`
/// after the instruction is at least what it was before — the withdrawing lender's at least
/// `min(before, its remaining shares)`. (`Π` falls only when borrowing does, a roll converts
/// locks and `lock_until` ends them: those are not decreases this check can judge.)
pub fn locks_never_decrease(
    pre: &PoolState,
    post: &PoolState,
    withdrawer: Option<&Pubkey>,
) -> Result<(), Violation> {
    let market = pre.market;
    let now = post.now;
    let (p0, p1) = (&pre.pool, &post.pool);
    let (v0, v1) = (&pre.vault, &post.vault);
    if p0.share_epoch != p1.share_epoch || v0.pi_epoch != v1.pi_epoch || v0.pi != v1.pi {
        return Ok(());
    }
    for (_, l0) in &pre.lenders {
        if l0.epoch != p0.share_epoch || now >= l0.lock_until {
            continue;
        }
        let before = pool::lock_of(v0, &l0.lender(), now).unwrap_or(0);
        if before == 0 {
            continue;
        }
        let l1 = post.lender_of(&l0.owner);
        let (after, shares) = l1.map_or((0, 0), |l| {
            (pool::lock_of(v1, &l.lender(), now).unwrap_or(0), l.shares)
        });
        let floor = if withdrawer == Some(&l0.owner) {
            before.min(shares)
        } else {
            before
        };
        ensure(after >= floor, "I8", || {
            format!(
                "market {market}: lender {}'s lock fell from {before} to {after} before its \
                 lock_until {} (Π and the epochs unchanged)",
                l0.owner, l0.lock_until
            )
        })?;
    }
    Ok(())
}
