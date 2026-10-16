//! Owner: pool track. Lender pool: shares, epochs, pro-rata withdrawal with expiring locks and the lock calendar, SOL entitlement, realised claims, the pot, lump vesting (ECON §9; ARCH §3.3; normative `sim/pool_model.py` v0.4.2).
//!
//! A port of `pool_model.Pool` v0.4.2 with its default switches (option (a) of the
//! round-3 brief, owner decision O16: continuous rent entitlement, a pot for realised
//! surplus, vested lumps), exact to the lamport and the share (differential vectors in
//! `tests/vectors/pool/`). The state is split the way the program stores it:
//!
//! - [`Pool`]: the lender-pool group of `Market` (ARCH §2.3.4), including the vest,
//!   `max_release` and the post-wipe flags (`wipe_live` is `Market.flags` bit 5, the two
//!   [`WIPE_TRADER_SWEPT`] / [`WIPE_REWARDS_CLAIMED`] bits are bits 7 and 8).
//! - [`Vault`]: the lender-pool part of `MarketVault` (ARCH §2.3.6): `lender_sol`, the pot
//!   `unmatched`, `protocol_fees`, the lock state (`Π`, `pi_epoch`, `lock_norm`, `b_min`)
//!   and the lock expiry calendar (`cal_day`, [`LOCK_BUCKETS`] day buckets).
//! - [`Lender`]: a `LenderPosition` (ARCH §2.3.6); [`ShareEpoch`]: one share-epoch record
//!   (ARCH §2.3.6, A39).
//!
//! `touch`, `claim` and `cover` take `&Pool`: they write only the lender and the vault,
//! so `claim_sol`, `cover_deficit` and `sync_lender` run `pool_model`'s full touch without
//! write-locking `Market` (C0-17, A70). Math types are not account layouts (ARCH §2.2):
//! the program converts to and from its `PodU128` layouts. Every struct here still has an
//! identical layout on every target (all 128-bit fields at 16-byte offsets, all padding
//! explicit, sizes and offsets asserted at compile time), so a byte copy never differs
//! between SBF (`align_of::<u128>() == 8`), x86-64 and wasm32 (16).
//!
//! The rent index, positions and the rent aggregate `A_j` belong to `rent`: `accrue`,
//! `repay` and `settle` take the lender credit and the per-position true-up (`credited`,
//! `rent::credited_for`) that `rent` computes. The utilisation cap is `caps::util_ok`'s.
//!
//! **Time.** Every function that releases the vest or reads a lock takes `now` (unix
//! seconds, `Clock::unix_timestamp`); `pool_model` keeps it as `Pool.now`.
//!
//! **Epoch records.** Functions that touch a lender take `records`: consecutive
//! [`ShareEpoch`] records (by `epoch`) covering the epochs the lender is behind,
//! `records[0]` at or before `lender.epoch`; extra records are ignored. None are needed
//! for a lender in the current epoch or one without shares, lock and lock term (it jumps,
//! V2-12); a lender whose shares and lock floored to 0 but whose term did not needs the
//! record of its epoch, which scales the term before the jump (as `pool_model`).
//! The program key-checks the accounts. `sync_lender(max_epochs)` signed by the lender's owner
//! is [`convert`] with the records at hand, then [`touch`] once the lender is no longer
//! [`Standing::Behind`]; sent by anyone else it is [`touch_pot`], which never converts through
//! a rebase record but still moves the surplus accrued up to it, and along the lender's forward
//! chain after it, to the pot (KI65-REV-SEC1, KI-66, KI-66 full); for a lender with a chain its
//! records start at or before `fwd_epoch`. [`closable`] is when a position closes (KI-67).
//! The records are `&mut`: a conversion through a rebase record rounds cumulatively and
//! writes the record's `conv_rem` (KI-65, no rebase orphans; see [`convert`]), so every
//! caller persists the records whose `conv_rem` changed together with the lender (the
//! program writes them back, `engine::lender::store_records`). A refused call writes
//! nothing; [`allowance_view`] only reads.
//!
//! **Errors and atomicity.** Every mutating function either succeeds or leaves all its
//! arguments unchanged. A refusal by a pool rule has its own [`MathError`] variant, one per
//! program error of ARCH §3.11 (`pool_model`'s messages in brackets):
//! [`MathError::DepositTooSmall`] ("deposit too small"), [`MathError::WipeSweepPending`]
//! ("WipeSweepPending"), [`MathError::PoolDrained`] ("PoolDrained", AIR-07, appended after
//! ARCH §3.11's group), [`MathError::NegativePending`] ("negative SOL balance: cover it
//! first"), [`MathError::InsufficientShares`] ("not enough shares"),
//! [`MathError::InsufficientInventory`] ("insufficient unborrowed inventory"),
//! [`MathError::AboveAllowance`] ("above pro-rata share of unborrowed inventory") and
//! [`MathError::LenderBehind`] (share-epoch records missing). `InvalidInput` is left for
//! malformed arguments (records out of order, a lender ahead of the pool, a missing wipe
//! record, a lock day outside the calendar). `Overflow` / `Underflow` mean a result does
//! not fit the account field (ARCH §2.3.4, §2.3.6).
//!
//! Rounding follows ECON §2.1: shares minted, tokens paid, SOL credits, claims, vest
//! releases, lock terms, the aggregate `locked` and (AIR-02) the value-weighted vest end
//! down; a lender's own lock, `Π`, `lock_until` and the rent true-up (`credited`, computed
//! by the caller) up.

use core::mem::{offset_of, size_of};

use crate::consts::MAX_RELEASE_SPAN_S;
use crate::error::MathError;
use crate::num::{BPS, Rounding, U256, WAD, mul_div, mul_div_i128, to_i128, to_u64};

/// `VS`: virtual shares (decimals offset 3; ECON §9.2, §15).
pub const VIRTUAL_SHARES: u128 = 1_000;
/// `VA`: virtual token units (ECON §9.2, §15).
pub const VIRTUAL_ASSETS: u128 = 1;
/// Shares per raw token unit above which an epoch rolls with a rebase (ECON §9.5, C0-6).
pub const R_MAX: u128 = 1_000_000;
/// Rebase factor: a rebase divides shares by `REBASE^k` (ECON §9.5).
pub const REBASE: u128 = 1_000;
/// A settlement that leaves at most `1/VEST_FLUSH_RATIO` of the token claims it found
/// (`T + ΣB_Q` before it) flushes the unreleased vest to the shares that held them, as a full
/// wipe does (AIR-07; ECON §9.5: "the wiped lenders keep their unreleased lumps"). A run
/// leaves the allowance's floor (1 raw token) and a dust deposit before the last settlement
/// leaves its own, so `T + ΣB_Q = 0` alone missed the wipes that leave dust, and the vest went
/// to whoever deposited next. A deposit before the settlement keeps its own tokens as claims,
/// so above `1/(VEST_FLUSH_RATIO − 1)` of the claims it would stop the flush and leave the vest
/// to a later deposit at the collapsed price (AIR-07's residual): [`deposit`] is therefore
/// refused while the pool is drained (`S > 0`, `ΣB_Q > 0` and `VEST_FLUSH_RATIO·T ≤ T + ΣB_Q`, the
/// only states a flushing settlement can start from; `MathError::PoolDrained`). A rebase test would
/// not do: a pool near the `R_MAX` bound rebases after a small settlement.
pub const VEST_FLUSH_RATIO: u128 = 1_000;
/// `lock_epoch` of a lender without a lock (`pool_model.Lender`: −1).
pub const NO_LOCK_EPOCH: u32 = u32::MAX;
/// `Lender::fwd`: no forward chain (KI-66 full; `pool_model.FWD_NONE`).
pub const FWD_NONE: u8 = 0;
/// `Lender::fwd`: the pot-only touch held the lender at the rebase record of its `epoch` (accrued
/// to the record's ends); the chain covers the epochs after it up to `fwd_epoch`.
pub const FWD_HOLD: u8 = 1;
/// `Lender::fwd`: the owner converted part of the way into the chain; nothing accrued on
/// `shares` in `epoch` yet; the chain (base `shares − fwd_lag`) covers `epoch ..= fwd_epoch`.
pub const FWD_OPEN: u8 = 2;
/// The lock calendar's bucket length and ring size (ECON §9.3, §15; ARCH §2.3.6, A75),
/// defined with the other compile-time constants.
pub use crate::consts::{LOCK_BUCKET_S, LOCK_BUCKETS};
/// `Pool::wipe_swept` bit: `sweep_trader` ran since `S` last reached 0 — a full wipe, or since
/// AIR-08 the last lender's exit (`Market.flags` bit 7 `wipe_trader_swept`; R4A-7).
pub const WIPE_TRADER_SWEPT: u8 = 1;
/// `Pool::wipe_swept` bit: `claim_venue_rewards` ran since `S` last reached 0 — a full wipe,
/// or since AIR-08 the last lender's exit (`Market.flags` bit 8 `wipe_rewards_claimed`; R4A-7).
pub const WIPE_REWARDS_CLAIMED: u8 = 2;

const LOCK_BUCKETS_I64: i64 = LOCK_BUCKETS as i64;
/// Largest `den_log1000` whose `1,000^k` fits `u128`.
const MAX_DEN_LOG: u8 = 12;
const WAD_I: i128 = 1_000_000_000_000_000_000;

/// Market-level lender-pool state: the lender-pool group and the wipe flags of `Market`
/// (ARCH §2.3.4; `pool_model.Pool`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pool {
    /// `S`: shares outstanding in the current share epoch.
    pub total_shares: u128,
    /// `acc`: SOL entitlement per share (WAD lamports per share, signed; ECON §9.4).
    pub acc: i128,
    /// Undistributed remainder of `acc` credits, lamports·WAD, in `[0, S)`.
    pub carry: i128,
    /// `acc_r`: realised lender SOL allocated per share (WAD; V2-2).
    pub acc_r: u128,
    /// Undistributed remainder of `acc_r` allocations, lamports·WAD.
    pub carry_r: u128,
    /// Carries dropped at epoch rolls, lamports·WAD (stay in the vault, unowned).
    pub dust: i128,
    /// `T`: unborrowed tokens (internal counter; donations ignored).
    pub tokens_free: u64,
    /// `ΣB_Q`: tokens lent out.
    pub borrowed_q: u64,
    /// Rent credited to lenders but not yet paid in.
    pub receivable: u64,
    /// Latest `deadline + S3 tail` of any position opened so far (R4A-8, R4B-8).
    pub max_release: i64,
    /// Unreleased lump SOL (already in `lender_sol`; R3B-2).
    pub vest_bal: u64,
    /// Time of the last vest release.
    pub vest_last: i64,
    /// When the whole `vest_bal` is released (value-weighted, R4B-5).
    pub vest_end: i64,
    /// Current share epoch (C0-6).
    pub share_epoch: u32,
    /// Non-zero while the latest record is a full wipe still open for top-ups (V2-12).
    pub wipe_live: u8,
    /// Sweeps done since `S` last reached 0 (a full wipe, or since AIR-08 the last lender's
    /// exit): [`WIPE_TRADER_SWEPT`] | [`WIPE_REWARDS_CLAIMED`] (R4A-7). Read only while `S = 0`
    /// (a deposit into a pool without shares waits for them).
    pub wipe_swept: u8,
    /// Explicit padding (zero).
    pub _pad: [u8; 2],
}

/// The lender-pool part of `MarketVault` (ARCH §2.3.6; `pool_model.Pool`'s `sol_vault`,
/// `unmatched`, `protocol`, lock state and `lock_cal`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Vault {
    /// Lock expiry calendar: bucket `d mod LOCK_BUCKETS` holds the lock terms whose
    /// `lock_until` ends day `d`, for the days `cal_day .. cal_day + LOCK_BUCKETS`.
    pub lock_cal: [u128; LOCK_BUCKETS],
    /// `Π`: borrowed-inventory survival index (WAD; ECON §9.3, C0-9); back to `WAD` at a `Π`
    /// epoch roll and, since AIR-12, at the first lock of every lock era.
    pub pi: u128,
    /// Σ of the live calendar buckets (the aggregate lock term).
    pub lock_norm: u128,
    /// `lender_sol` (`pool_model.sol_vault`): realised lender SOL, including the pot and
    /// the unreleased vest.
    pub lender_sol: u64,
    /// The pot: realised SOL beyond its receivers' entitlement (R3A-2).
    pub unmatched: u64,
    /// Protocol fees. The pool adds lender credits that find no lender (`S = 0`, no live
    /// wipe record, R3A-10); every other protocol income is added by the program.
    pub protocol_fees: u64,
    /// Running minimum of `ΣB_Q` since the first lock of the lock era — the first lock taken
    /// while no lock is live; it restarts `Π` at `WAD` too (V2-3, AIR-12).
    pub b_min: u64,
    /// Oldest calendar day not yet dropped.
    pub cal_day: i64,
    /// `Π` epoch: every lock and bucket clears when it rolls.
    pub pi_epoch: u32,
    /// Explicit padding (zero).
    pub _pad: [u8; 4],
}

/// Lender state (`LenderPosition`, ARCH §2.3.6; `pool_model.Lender`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Lender {
    /// Shares, in units of `epoch`.
    pub shares: u128,
    /// `acc` at the last touch.
    pub snap: i128,
    /// `acc_r` at the last touch.
    pub snap_r: u128,
    /// Locked shares at `lock_pi` (ECON §9.3).
    pub lock: u128,
    /// `Π` when the lock was set.
    pub lock_pi: u128,
    /// This lender's term in the calendar bucket of `lock_until`: `⌊lock·WAD/lock_pi⌋`.
    pub lock_term: u128,
    /// SOL entitlement not yet paid (negative after a shortfall debit; ECON §9.4).
    pub pending: i64,
    /// Cumulative realised SOL allocated to this lender, including pot draws (V2-2).
    pub real: u64,
    /// Cumulative SOL paid to this lender (V2-2).
    pub claimed: u64,
    /// When the lock expires (a calendar day end; R3B-1, R4A-2).
    pub lock_until: i64,
    /// Share epoch of `shares`.
    pub epoch: u32,
    /// `Π` epoch of `lock` ([`NO_LOCK_EPOCH`] without a lock).
    pub lock_epoch: u32,
    /// KI-66 full: the epoch the forward chain reached (0 with [`FWD_NONE`]).
    pub fwd_epoch: u32,
    /// KI-66 full: [`FWD_NONE`], [`FWD_HOLD`] or [`FWD_OPEN`]. While a chain is active, `snap`
    /// and `snap_r` are the chain's snapshots in `fwd_epoch`; the lender's own are implicit
    /// (the record's ends in [`FWD_HOLD`], 0 in [`FWD_OPEN`]).
    pub fwd: u8,
    /// KI-66 full, [`FWD_OPEN`]: `shares` minus the chain's shares in `epoch`, 0 or 1 (each
    /// record's rounding unit beyond the floor; 0 otherwise).
    pub fwd_lag: u8,
    /// Explicit padding (zero).
    pub _pad: [u8; 2],
    /// KI-66 full: the forward chain's shares in `fwd_epoch`: the floor of the lender's shares
    /// through the records, which no conversion order can lower (0 with [`FWD_NONE`]).
    pub fwd_shares: u128,
}

/// Share-epoch record (`ShareEpoch`, ARCH §2.3.6, A39; `pool_model.Pool.epochs[e]`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShareEpoch {
    /// `acc` at the roll (a live full-wipe record is topped up, V2-12).
    pub acc_end: i128,
    /// `acc_r` at the roll.
    pub acc_r_end: u128,
    /// `S` at the roll (> 0).
    pub s_old: u128,
    /// Remainder of `acc_end` top-ups, lamports·WAD.
    pub carry: i128,
    /// Remainder of `acc_r_end` top-ups, lamports·WAD.
    pub carry_r: u128,
    /// Conversion remainder (KI-65): the old-epoch shares converted through this record so far,
    /// modulo `1,000^den_log1000`, in `[0, den)`. A lender's conversion mints
    /// `⌊(conv_rem + shares)/den⌋` and leaves the remainder here, so the conversions of every
    /// holder of the old epoch sum to `⌊s_old/den⌋` = the new `S` exactly (no rebase orphans).
    /// 0 for a full wipe.
    pub conv_rem: u128,
    /// The epoch this record closes.
    pub epoch: u32,
    /// Conversion numerator: 0 (full wipe) or 1.
    pub num: u8,
    /// `k` of the conversion denominator `1,000^k` (0 for a full wipe).
    pub den_log1000: u8,
    /// Explicit padding (zero).
    pub _pad: [u8; 10],
}

// Layouts (see the module docs): every 128-bit field at a multiple of 16, all padding
// explicit, sizes multiples of 16, so the layout is the same whether `u128` is 8- or
// 16-aligned.
const _: () = {
    assert!(size_of::<Pool>() == 160);
    assert!(offset_of!(Pool, total_shares) == 0);
    assert!(offset_of!(Pool, acc) == 16);
    assert!(offset_of!(Pool, carry) == 32);
    assert!(offset_of!(Pool, acc_r) == 48);
    assert!(offset_of!(Pool, carry_r) == 64);
    assert!(offset_of!(Pool, dust) == 80);
    assert!(offset_of!(Pool, tokens_free) == 96);
    assert!(offset_of!(Pool, borrowed_q) == 104);
    assert!(offset_of!(Pool, receivable) == 112);
    assert!(offset_of!(Pool, max_release) == 120);
    assert!(offset_of!(Pool, vest_bal) == 128);
    assert!(offset_of!(Pool, vest_last) == 136);
    assert!(offset_of!(Pool, vest_end) == 144);
    assert!(offset_of!(Pool, share_epoch) == 152);
    assert!(offset_of!(Pool, wipe_live) == 156);
    assert!(offset_of!(Pool, wipe_swept) == 157);
    assert!(offset_of!(Pool, _pad) == 158);

    assert!(size_of::<Vault>() == 2_128);
    assert!(offset_of!(Vault, lock_cal) == 0);
    assert!(offset_of!(Vault, pi) == 2_048);
    assert!(offset_of!(Vault, lock_norm) == 2_064);
    assert!(offset_of!(Vault, lender_sol) == 2_080);
    assert!(offset_of!(Vault, unmatched) == 2_088);
    assert!(offset_of!(Vault, protocol_fees) == 2_096);
    assert!(offset_of!(Vault, b_min) == 2_104);
    assert!(offset_of!(Vault, cal_day) == 2_112);
    assert!(offset_of!(Vault, pi_epoch) == 2_120);
    assert!(offset_of!(Vault, _pad) == 2_124);

    assert!(size_of::<Lender>() == 160);
    assert!(offset_of!(Lender, shares) == 0);
    assert!(offset_of!(Lender, snap) == 16);
    assert!(offset_of!(Lender, snap_r) == 32);
    assert!(offset_of!(Lender, lock) == 48);
    assert!(offset_of!(Lender, lock_pi) == 64);
    assert!(offset_of!(Lender, lock_term) == 80);
    assert!(offset_of!(Lender, pending) == 96);
    assert!(offset_of!(Lender, real) == 104);
    assert!(offset_of!(Lender, claimed) == 112);
    assert!(offset_of!(Lender, lock_until) == 120);
    assert!(offset_of!(Lender, epoch) == 128);
    assert!(offset_of!(Lender, lock_epoch) == 132);
    assert!(offset_of!(Lender, fwd_epoch) == 136);
    assert!(offset_of!(Lender, fwd) == 140);
    assert!(offset_of!(Lender, fwd_lag) == 141);
    assert!(offset_of!(Lender, _pad) == 142);
    assert!(offset_of!(Lender, fwd_shares) == 144);

    assert!(size_of::<ShareEpoch>() == 112);
    assert!(offset_of!(ShareEpoch, acc_end) == 0);
    assert!(offset_of!(ShareEpoch, acc_r_end) == 16);
    assert!(offset_of!(ShareEpoch, s_old) == 32);
    assert!(offset_of!(ShareEpoch, carry) == 48);
    assert!(offset_of!(ShareEpoch, carry_r) == 64);
    assert!(offset_of!(ShareEpoch, conv_rem) == 80);
    assert!(offset_of!(ShareEpoch, epoch) == 96);
    assert!(offset_of!(ShareEpoch, num) == 100);
    assert!(offset_of!(ShareEpoch, den_log1000) == 101);
    assert!(offset_of!(ShareEpoch, _pad) == 102);
};

/// Where a lender stands after [`convert`] / [`touch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Standing {
    /// In the current share epoch (and, after `touch`, accrued).
    Current,
    /// On the live full-wipe record (V2-12): no current shares, so `withdraw` refuses;
    /// `pending` and `real` follow the record's top-ups.
    Parked,
    /// More [`ShareEpoch`] records are needed (`sync_lender` in steps, ARCH §3.3).
    Behind,
}

/// Which post-wipe sweep a [`sweep`] is (ARCH §3.3, §3.9; R4A-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SweepSource {
    /// `sweep_trader`: SOL left on the trader PDA.
    Trader,
    /// `claim_venue_rewards`: venue cashback.
    VenueRewards,
}

impl SweepSource {
    const fn bit(self) -> u8 {
        match self {
            Self::Trader => WIPE_TRADER_SWEPT,
            Self::VenueRewards => WIPE_REWARDS_CLAIMED,
        }
    }
}

/// A position's resolution as the pool sees it: `repay` (close, liquidation, expiry) or
/// `settle` (S1/S2/S3) of `q` borrowed tokens (ARCH §3.3 "borrower-side pool calls").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Resolution {
    /// Tokens the position borrowed.
    pub q: u64,
    /// Lender rent already credited continuously on the position's behalf
    /// (`rent::credited_for`, ceil; `pool_model._credited_for`).
    pub credited: u64,
    /// Lender lump that vests: the lender part of the penalty or the expiry fee
    /// (`repay`); the part of a settlement's take above `⌊Q·P_ema/WAD⌋ + R_due` (`settle`,
    /// `waterfall::settle`; S1, S2 and S3, S1 since AIR-21).
    pub lump: u64,
}

impl Pool {
    /// Pool of a new market: no shares (`pool_model.Pool()`).
    pub const fn new() -> Self {
        Self {
            total_shares: 0,
            acc: 0,
            carry: 0,
            acc_r: 0,
            carry_r: 0,
            dust: 0,
            tokens_free: 0,
            borrowed_q: 0,
            receivable: 0,
            max_release: 0,
            vest_bal: 0,
            vest_last: 0,
            vest_end: 0,
            share_epoch: 0,
            wipe_live: 0,
            wipe_swept: WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED,
            _pad: [0; 2],
        }
    }

    /// Whether the latest record is a full wipe still open for top-ups (V2-12).
    pub const fn is_wipe_live(&self) -> bool {
        self.wipe_live != 0
    }

    /// `T + ΣB_Q`: all lender token claims.
    pub fn token_claims(&self) -> Result<u128, MathError> {
        add_u128(u128::from(self.tokens_free), u128::from(self.borrowed_q))
    }
}

impl Default for Pool {
    fn default() -> Self {
        Self::new()
    }
}

impl Vault {
    /// The vault of a new market: `Π = WAD`, empty calendar (`pool_model.Pool()`).
    pub const fn new() -> Self {
        Self {
            lock_cal: [0; LOCK_BUCKETS],
            pi: WAD,
            lock_norm: 0,
            lender_sol: 0,
            unmatched: 0,
            protocol_fees: 0,
            b_min: 0,
            cal_day: 0,
            pi_epoch: 0,
            _pad: [0; 4],
        }
    }

    /// The live calendar buckets `(day, Σ lock terms)` in day order, non-zero only
    /// (`pool_model.Pool.lock_cal`).
    pub fn buckets(&self) -> impl Iterator<Item = (i64, u128)> + '_ {
        (0..LOCK_BUCKETS_I64).filter_map(move |i| {
            let day = self.cal_day.checked_add(i)?;
            let v = *self.lock_cal.get(slot(day).ok()?)?;
            (v != 0).then_some((day, v))
        })
    }
}

impl Default for Vault {
    fn default() -> Self {
        Self::new()
    }
}

impl Lender {
    /// A new lender at the pool's current state (`pool_model.Lender(acc, acc_r, epoch)`).
    pub const fn new(pool: &Pool) -> Self {
        Self {
            shares: 0,
            snap: pool.acc,
            snap_r: pool.acc_r,
            lock: 0,
            lock_pi: WAD,
            lock_term: 0,
            pending: 0,
            real: 0,
            claimed: 0,
            lock_until: 0,
            epoch: pool.share_epoch,
            lock_epoch: NO_LOCK_EPOCH,
            fwd_epoch: 0,
            fwd: FWD_NONE,
            fwd_lag: 0,
            _pad: [0; 2],
            fwd_shares: 0,
        }
    }

    /// Drops the forward chain (KI-66 full): settled by the owner's conversion, a jump or a park.
    pub const fn clear_fwd(&mut self) {
        self.fwd = FWD_NONE;
        self.fwd_epoch = 0;
        self.fwd_lag = 0;
        self.fwd_shares = 0;
    }
}

impl ShareEpoch {
    /// `true` for a full wipe (`num = 0`: the old shares keep only their SOL).
    pub const fn is_full_wipe(&self) -> bool {
        self.num == 0
    }

    /// Conversion denominator `1,000^den_log1000`.
    pub fn den(&self) -> Result<u128, MathError> {
        den_of(self.den_log1000)
    }

    /// `(num, den)`, validated: `num ∈ {0, 1}`, `den = 1` for a full wipe.
    fn ratio(&self) -> Result<(u128, u128), MathError> {
        match (self.num, self.den_log1000) {
            (0, 0) => Ok((0, 1)),
            (1, k) => Ok((1, den_of(k)?)),
            _ => Err(MathError::InvalidInput),
        }
    }
}

// ---- helpers -----------------------------------------------------------------------------

fn ovf<T>(x: Option<T>) -> Result<T, MathError> {
    x.ok_or(MathError::Overflow)
}

fn add_u128(a: u128, b: u128) -> Result<u128, MathError> {
    ovf(a.checked_add(b))
}

fn sub_u128(a: u128, b: u128) -> Result<u128, MathError> {
    a.checked_sub(b).ok_or(MathError::Underflow)
}

fn add_u64(a: u64, b: u64) -> Result<u64, MathError> {
    ovf(a.checked_add(b))
}

fn sub_u64(a: u64, b: u64) -> Result<u64, MathError> {
    a.checked_sub(b).ok_or(MathError::Underflow)
}

fn add_i128(a: i128, b: i128) -> Result<i128, MathError> {
    ovf(a.checked_add(b))
}

fn sub_i128(a: i128, b: i128) -> Result<i128, MathError> {
    ovf(a.checked_sub(b))
}

fn add_i64(a: i64, b: i64) -> Result<i64, MathError> {
    ovf(a.checked_add(b))
}

fn den_of(k: u8) -> Result<u128, MathError> {
    if k > MAX_DEN_LOG {
        return Err(MathError::Overflow);
    }
    ovf(REBASE.checked_pow(u32::from(k)))
}

/// A signed value into an unsigned field: `Underflow` below 0, `Overflow` above the max.
fn to_unsigned<T: TryFrom<i128>>(x: i128) -> Result<T, MathError> {
    if x < 0 {
        return Err(MathError::Underflow);
    }
    T::try_from(x).map_err(|_| MathError::Overflow)
}

fn to_i64(x: i128) -> Result<i64, MathError> {
    i64::try_from(x).map_err(|_| MathError::Overflow)
}

/// `a − b` of two unsigned accumulators, signed.
fn diff_u128(a: u128, b: u128) -> Result<i128, MathError> {
    if a >= b {
        to_i128(sub_u128(a, b)?)
    } else {
        ovf(to_i128(sub_u128(b, a)?)?.checked_neg())
    }
}

/// `⌊shares·Δacc/WAD⌋` (toward −∞, like the model's `//`).
fn accrued(shares: u128, delta: i128) -> Result<i128, MathError> {
    mul_div_i128(delta, shares, WAD, Rounding::Floor)
}

/// Python's `divmod(num, d)` for `d > 0`: quotient toward −∞, remainder in `[0, d)`.
fn divmod_floor(num: i128, d: u128) -> Result<(i128, u128), MathError> {
    if d == 0 {
        return Err(MathError::DivByZero);
    }
    match i128::try_from(d) {
        Ok(di) => {
            let q = ovf(num.checked_div_euclid(di))?;
            let r = ovf(num.checked_rem_euclid(di))?;
            Ok((q, to_unsigned(r)?))
        }
        // d > i128::MAX ≥ |num| (except i128::MIN, whose magnitude is 2^127 < d).
        Err(_) if num >= 0 => Ok((0, num.unsigned_abs())),
        Err(_) => Ok((-1, sub_u128(d, num.unsigned_abs())?)),
    }
}

/// `⌈a/b⌉` for `b > 0` (Python's `cdiv`).
fn ceil_div_i64(a: i64, b: i64) -> Result<i64, MathError> {
    let q = ovf(a.checked_div_euclid(b))?;
    let r = ovf(a.checked_rem_euclid(b))?;
    if r == 0 { Ok(q) } else { add_i64(q, 1) }
}

/// Calendar day of a timestamp: `⌊t/LOCK_BUCKET_S⌋`.
fn day_of(t: i64) -> Result<i64, MathError> {
    ovf(t.checked_div_euclid(LOCK_BUCKET_S))
}

/// Ring slot of a calendar day: `day mod LOCK_BUCKETS`.
fn slot(day: i64) -> Result<usize, MathError> {
    let s = ovf(day.checked_rem_euclid(LOCK_BUCKETS_I64))?;
    usize::try_from(s).map_err(|_| MathError::Overflow)
}

/// `T + ΣB_Q + VA`.
fn claims_va(p: &Pool) -> Result<u128, MathError> {
    add_u128(p.token_claims()?, VIRTUAL_ASSETS)
}

/// `S + VS`.
fn shares_vs(p: &Pool) -> Result<u128, MathError> {
    add_u128(p.total_shares, VIRTUAL_SHARES)
}

/// The index of the record for `epoch` in a consecutive slice, `None` if the slice does not
/// reach it.
fn record_idx(records: &[ShareEpoch], epoch: u32) -> Result<Option<usize>, MathError> {
    let Some(first) = records.first() else {
        return Ok(None);
    };
    let Some(idx) = epoch.checked_sub(first.epoch) else {
        return Ok(None);
    };
    let idx = usize::try_from(idx).map_err(|_| MathError::Overflow)?;
    match records.get(idx) {
        None => Ok(None),
        Some(r) if r.epoch == epoch => Ok(Some(idx)),
        Some(_) => Err(MathError::InvalidInput),
    }
}

/// The epoch records a conversion reads and, on commit, writes `conv_rem` back to (KI-65).
/// A conversion reads each record at most once, before it writes that record, so the
/// read-only pass and the commit pass see the same remainders and compute the same lender.
trait Recs {
    /// Whether the conversion accrues `pending` and `real` through the records (the commit
    /// pass only replays the path: shares, locks and epochs, which no accrual decides).
    const ACCRUE: bool;
    /// Whether the conversion stops at a rebase record instead of converting through it (the
    /// pot-only touch, [`touch_pot`], KI-66).
    const HOLD: bool = false;
    /// The record of `epoch` and its index, `None` if the slice does not reach it.
    fn get(&self, epoch: u32) -> Result<Option<(usize, ShareEpoch)>, MathError>;
    /// The conversion through record `idx` left `rem` (called only when it changed).
    fn set_rem(&mut self, idx: usize, rem: u128) -> Result<(), MathError>;
    /// A conversion stopped with the lender at `(epoch, shares)`.
    fn converted(&mut self, epoch: u32, shares: u128);
}

/// The read-only pass every mutating function computes on copies first; remembers where
/// its first conversion stopped, which the commit pass must reproduce.
struct ReadOnly<'a> {
    recs: &'a [ShareEpoch],
    first: Option<(u32, u128)>,
}

impl<'a> ReadOnly<'a> {
    const fn new(recs: &'a [ShareEpoch]) -> Self {
        Self { recs, first: None }
    }
}

impl Recs for ReadOnly<'_> {
    const ACCRUE: bool = true;
    fn get(&self, epoch: u32) -> Result<Option<(usize, ShareEpoch)>, MathError> {
        Ok(record_idx(self.recs, epoch)?.and_then(|i| self.recs.get(i).map(|r| (i, *r))))
    }
    fn set_rem(&mut self, _idx: usize, _rem: u128) -> Result<(), MathError> {
        Ok(())
    }
    fn converted(&mut self, epoch: u32, shares: u128) {
        if self.first.is_none() {
            self.first = Some((epoch, shares));
        }
    }
}

/// The commit pass: writes the conversion remainders.
struct Commit<'a> {
    recs: &'a mut [ShareEpoch],
    end: Option<(u32, u128)>,
}

impl Recs for Commit<'_> {
    const ACCRUE: bool = false;
    fn get(&self, epoch: u32) -> Result<Option<(usize, ShareEpoch)>, MathError> {
        Ok(record_idx(self.recs, epoch)?.and_then(|i| self.recs.get(i).map(|r| (i, *r))))
    }
    fn set_rem(&mut self, idx: usize, rem: u128) -> Result<(), MathError> {
        self.recs.get_mut(idx).ok_or(MathError::Invariant)?.conv_rem = rem;
        Ok(())
    }
    fn converted(&mut self, epoch: u32, shares: u128) {
        self.end = Some((epoch, shares));
    }
}

/// The pot-only touch's pass ([`touch_pot`], KI-66): it stops at the first rebase record (the
/// hold, which starts the forward chain), so it converts through full-wipe records only, which
/// keep their `conv_rem`; a remainder to write would be a bug (`Invariant`).
struct Hold<'a>(&'a [ShareEpoch]);

impl Recs for Hold<'_> {
    const ACCRUE: bool = true;
    const HOLD: bool = true;
    fn get(&self, epoch: u32) -> Result<Option<(usize, ShareEpoch)>, MathError> {
        Ok(record_idx(self.0, epoch)?.and_then(|i| self.0.get(i).map(|r| (i, *r))))
    }
    fn set_rem(&mut self, _idx: usize, _rem: u128) -> Result<(), MathError> {
        Err(MathError::Invariant)
    }
    fn converted(&mut self, _epoch: u32, _shares: u128) {}
}

/// A lender's shares through a record whose validated ratio is `(num, den)` (KI-65, ECON
/// §9.5): 0 at a full wipe (`conv_rem` is not touched); else `⌊(conv_rem + shares)/den⌋` with
/// the new remainder `(conv_rem + shares) mod den`, so at least `⌊shares/den⌋` and at most
/// one more. One u128 division; `(shares mod den) + conv_rem < 2·den ≤ 2·10^36` cannot
/// overflow.
///
/// # Errors
/// `InvalidInput` if `conv_rem ≥ den` (a malformed record).
fn convert_shares(
    shares: u128,
    conv_rem: u128,
    num: u128,
    den: u128,
) -> Result<(u128, u128), MathError> {
    if num == 0 {
        return Ok((0, conv_rem));
    }
    if conv_rem >= den {
        return Err(MathError::InvalidInput);
    }
    let q = ovf(shares.checked_div(den))?;
    let r = add_u128(sub_u128(shares, ovf(q.checked_mul(den))?)?, conv_rem)?;
    if r >= den {
        Ok((add_u128(q, 1)?, sub_u128(r, den)?))
    } else {
        Ok((q, r))
    }
}

// ---- lock expiry calendar (ECON §9.3; ARCH §2.3.6, A75; R4A-2, R4B-3) ---------------------

/// `pool_model.Pool._cal_advance`: drops every bucket whose day has ended (`day ≤
/// ⌊now/LOCK_BUCKET_S⌋`) from `lock_norm`, so an expired lock leaves the aggregate at its
/// `lock_until` without its holder being touched; then `cal_day = ⌊now/LOCK_BUCKET_S⌋ + 1`.
/// At most [`LOCK_BUCKETS`] buckets are visited (a longer gap clears the ring). The
/// prologue of every lock read or write, including [`resolve`] and [`touch`].
///
/// # Errors
/// `Overflow` only for timestamps near `i64::MAX`.
pub fn cal_advance(vault: &mut Vault, now: i64) -> Result<(), MathError> {
    let today = day_of(now)?;
    if today < vault.cal_day {
        return Ok(());
    }
    let next = add_i64(today, 1)?;
    let span = ovf(next.checked_sub(vault.cal_day))?;
    if span >= LOCK_BUCKETS_I64 {
        for b in &mut vault.lock_cal {
            vault.lock_norm = vault.lock_norm.saturating_sub(*b);
            *b = 0;
        }
    } else {
        let mut d = vault.cal_day;
        while d < next {
            let b = vault
                .lock_cal
                .get_mut(slot(d)?)
                .ok_or(MathError::Invariant)?;
            vault.lock_norm = vault.lock_norm.saturating_sub(*b);
            *b = 0;
            d = add_i64(d, 1)?;
        }
    }
    vault.cal_day = next;
    Ok(())
}

/// `lock_norm` as [`cal_advance`] at `now` would leave it, without changing the vault.
fn norm_at(vault: &Vault, now: i64) -> Result<u128, MathError> {
    let today = day_of(now)?;
    if today < vault.cal_day {
        return Ok(vault.lock_norm);
    }
    let next = add_i64(today, 1)?;
    let span = ovf(next.checked_sub(vault.cal_day))?;
    let mut norm = vault.lock_norm;
    if span >= LOCK_BUCKETS_I64 {
        for b in &vault.lock_cal {
            norm = norm.saturating_sub(*b);
        }
    } else {
        let mut d = vault.cal_day;
        while d < next {
            norm = norm.saturating_sub(*vault.lock_cal.get(slot(d)?).ok_or(MathError::Invariant)?);
            d = add_i64(d, 1)?;
        }
    }
    Ok(norm)
}

/// The ring slot of a live day (`cal_day ≤ day < cal_day + LOCK_BUCKETS`), else `None`.
fn live_slot(vault: &Vault, day: i64) -> Result<Option<usize>, MathError> {
    let end = add_i64(vault.cal_day, LOCK_BUCKETS_I64)?;
    if day < vault.cal_day || day >= end {
        return Ok(None);
    }
    slot(day).map(Some)
}

/// `pool_model.Pool._cal_add`: adds a lock term to the bucket of `until`'s day and to
/// `lock_norm`. A term of 0 changes nothing.
///
/// # Errors
/// `InvalidInput` if `term > 0` and `until`'s day is not live (`cal_day ≤ day <
/// cal_day + LOCK_BUCKETS`: run [`cal_advance`] first; a new lock's `lock_until` from
/// [`lock_until_for`] is always inside); `Overflow` if the bucket or `lock_norm` leaves
/// `u128`.
pub fn cal_add(vault: &mut Vault, until: i64, term: u128) -> Result<(), MathError> {
    if term == 0 {
        return Ok(());
    }
    let i = live_slot(vault, day_of(until)?)?.ok_or(MathError::InvalidInput)?;
    let norm = add_u128(vault.lock_norm, term)?;
    let b = vault.lock_cal.get_mut(i).ok_or(MathError::Invariant)?;
    *b = add_u128(*b, term)?;
    vault.lock_norm = norm;
    Ok(())
}

/// `pool_model.Pool._cal_sub`: removes a live lock's term from the bucket of `until`'s
/// day (at most what the bucket holds) and from `lock_norm`. The caller checks that the
/// lock is live (`now < until`, current `Π` epoch); a day outside the ring holds nothing.
///
/// # Errors
/// `Overflow` only for timestamps near `i64::MAX`.
pub fn cal_sub(vault: &mut Vault, until: i64, term: u128) -> Result<(), MathError> {
    let Some(i) = live_slot(vault, day_of(until)?)? else {
        return Ok(());
    };
    let b = vault.lock_cal.get_mut(i).ok_or(MathError::Invariant)?;
    let x = term.min(*b);
    *b = sub_u128(*b, x)?;
    vault.lock_norm = vault.lock_norm.saturating_sub(x);
    Ok(())
}

/// `pool_model.Pool.lock_until_for`: `lock_until` of a new or extended lock,
/// `⌈(max(now, max_release) + lock_grace_s)/LOCK_BUCKET_S⌉·LOCK_BUCKET_S` (ECON §2.1,
/// §9.3; R4A-8, R4B-8): the latest release of any position opened so far plus the grace,
/// rounded up to its calendar day's end.
///
/// # Errors
/// `Overflow` for timestamps near `i64::MAX`.
pub fn lock_until_for(pool: &Pool, now: i64, lock_grace_s: u32) -> Result<i64, MathError> {
    let x = add_i64(now.max(pool.max_release), i64::from(lock_grace_s))?;
    ovf(ceil_div_i64(x, LOCK_BUCKET_S)?.checked_mul(LOCK_BUCKET_S))
}

// ---- vesting (ECON §9.4; R3B-2, R4B-5) ---------------------------------------------------

/// Release of the vest due at `now`; the caller credits it.
fn release_in(p: &mut Pool, now: i64) -> Result<u64, MathError> {
    if p.vest_bal == 0 {
        p.vest_last = now;
        return Ok(0);
    }
    if now < p.vest_last {
        return Err(MathError::InvalidInput);
    }
    let rel = if now >= p.vest_end {
        p.vest_bal
    } else {
        let elapsed =
            u128::try_from(ovf(now.checked_sub(p.vest_last))?).map_err(|_| MathError::Underflow)?;
        let span = u128::try_from(ovf(p.vest_end.checked_sub(p.vest_last))?)
            .map_err(|_| MathError::Underflow)?;
        to_u64(mul_div(
            u128::from(p.vest_bal),
            elapsed,
            span,
            Rounding::Floor,
        )?)?
    };
    p.vest_last = now;
    if rel != 0 {
        if p.total_shares == 0 {
            // Unreachable: the vest is flushed whenever S reaches 0 (wipe, last lender).
            return Err(MathError::Invariant);
        }
        p.vest_bal = sub_u64(p.vest_bal, rel)?;
        credit_pos(p, i128::from(rel), true)?;
    }
    Ok(rel)
}

/// `pool_model.Pool._release` (inside `accrue`, ECON §9.4): releases
/// `⌊vest_bal·(now − vest_last)/(vest_end − vest_last)⌋` (all of it once `now ≥ vest_end`)
/// into `acc` and `acc_r` together, and sets `vest_last = now`. Idempotent at a given
/// `now`. Returns the lamports released.
///
/// # Errors
/// `InvalidInput` if `now < vest_last` while a vest is running; `Invariant` if a vest runs
/// while `S = 0` (the pool flushes it whenever `S` reaches 0); `Overflow` if an
/// accumulator leaves its type.
pub fn release(pool: &mut Pool, now: i64) -> Result<u64, MathError> {
    let mut p = *pool;
    let rel = release_in(&mut p, now)?;
    *pool = p;
    Ok(rel)
}

fn vest_in(
    p: &mut Pool,
    v: &mut Vault,
    wipe: Option<&mut ShareEpoch>,
    now: i64,
    lump_vest_s: u32,
    lamports: u64,
) -> Result<(), MathError> {
    if lamports == 0 {
        return Ok(());
    }
    release_in(p, now)?;
    if p.total_shares == 0 {
        // Nobody to vest for: the live wipe record or the protocol.
        return credit_in(p, v, wipe, i128::from(lamports), true);
    }
    let x = u128::from(lamports);
    let span: u128 = if p.vest_bal != 0 && p.vest_end > now {
        // Value-weighted end (R4B-5): the end moves by ⌊x·(lump_vest_s − left)/(bal + x)⌋
        // s. Never earlier while `lump_vest_s ≥ left` (absent a governance decrease of
        // `lump_vest_s` below the remaining span; after one, a lump pulls the end earlier,
        // as in pool_model) (M0B-A-2). Rounded down (AIR-02): a dust lump against a running
        // vest (`x·(lump_vest_s − left) < bal + x`) leaves the end where it is; rounded up,
        // every 1-lamport `sweep_trader` moved it 1 s, any number of times per transaction.
        let left =
            u128::try_from(ovf(p.vest_end.checked_sub(now))?).map_err(|_| MathError::Underflow)?;
        let bal = u128::from(p.vest_bal);
        let num = add_u128(
            ovf(bal.checked_mul(left))?,
            ovf(x.checked_mul(u128::from(lump_vest_s)))?,
        )?;
        mul_div(num, 1, add_u128(bal, x)?, Rounding::Floor)?
    } else {
        u128::from(lump_vest_s)
    };
    let span = i64::try_from(span).map_err(|_| MathError::Overflow)?;
    p.vest_bal = add_u64(p.vest_bal, lamports)?;
    p.vest_last = now;
    p.vest_end = add_i64(now, span)?;
    Ok(())
}

/// `pool_model.Pool._vest` (ECON §9.4; ARCH §3.3 `vest`): a lump of `lamports`, already in
/// `lender_sol`, released linearly: first the release due at `now`; then, if `S = 0`, a
/// realised credit (to the live wipe record or the protocol, see [`credit`]); otherwise
/// `vest_end = now + ⌊(vest_bal·(vest_end − now) + x·lump_vest_s)/(vest_bal + x)⌋` while a
/// vest runs (value-weighted, R4B-5; floor, AIR-02), else `now + lump_vest_s`, and
/// `vest_bal += x`, `vest_last = now`. Nothing happens for 0 lamports (not even the release).
///
/// A lump moves a running vest's end by `⌊x·(lump_vest_s − left)/(vest_bal + x)⌋` seconds
/// (`left = vest_end − now`): never earlier while `lump_vest_s ≥ left`, which holds unless
/// governance lowered `lump_vest_s` below the remaining span; after such a decrease a small
/// lump pulls the end earlier (by days at the compiled extremes), exactly as `pool_model`.
/// A dust lump (`x·(lump_vest_s − left) < vest_bal + x`) leaves the end where it is, so
/// repeated 1-lamport sweeps cannot hold the end `lump_vest_s` away (AIR-02).
///
/// # Errors
/// As [`release`] and [`credit`]; `Overflow` if `vest_bal` or `vest_end` leave their types.
pub fn vest(
    pool: &mut Pool,
    vault: &mut Vault,
    wipe_record: Option<&mut ShareEpoch>,
    now: i64,
    lump_vest_s: u32,
    lamports: u64,
) -> Result<(), MathError> {
    let (mut p, mut v) = (*pool, *vault);
    let mut rec = wipe_record.as_deref().copied();
    vest_in(&mut p, &mut v, rec.as_mut(), now, lump_vest_s, lamports)?;
    commit_record(wipe_record, rec);
    *pool = p;
    *vault = v;
    Ok(())
}

fn commit_record(target: Option<&mut ShareEpoch>, rec: Option<ShareEpoch>) {
    if let (Some(t), Some(r)) = (target, rec) {
        *t = r;
    }
}

// ---- SOL crediting (ECON §9.4, §9.5) -------------------------------------------------------

/// Credit while `S > 0`: `acc += ⌊(x·WAD + carry)/S⌋` (remainder carried) and, if
/// `realised` and `x > 0`, the same allocation into `acc_r`.
fn credit_pos(p: &mut Pool, lamports: i128, realised: bool) -> Result<(), MathError> {
    let num = add_i128(ovf(lamports.checked_mul(WAD_I))?, p.carry)?;
    let (d, r) = divmod_floor(num, p.total_shares)?;
    p.carry = to_i128(r)?;
    p.acc = add_i128(p.acc, d)?;
    if realised && lamports > 0 {
        credit_r_in(p, to_unsigned(lamports)?)?;
    }
    Ok(())
}

fn credit_in(
    p: &mut Pool,
    v: &mut Vault,
    wipe: Option<&mut ShareEpoch>,
    lamports: i128,
    realised: bool,
) -> Result<(), MathError> {
    if lamports == 0 {
        return Ok(());
    }
    if p.total_shares != 0 {
        return credit_pos(p, lamports, realised);
    }
    if p.is_wipe_live() {
        // V2-12: top up the full-wipe record for the wiped lenders.
        let rec = wipe.ok_or(MathError::InvalidInput)?;
        *rec = top_up(p, *rec, lamports, realised)?;
        return Ok(());
    }
    // R3A-10: no lender to credit: the protocol (signed, like the model's `protocol`).
    if lamports > 0 {
        let x: u64 = to_unsigned(lamports)?;
        v.lender_sol = sub_u64(v.lender_sol, x)?;
        v.protocol_fees = add_u64(v.protocol_fees, x)?;
    } else {
        let x = u64::try_from(lamports.unsigned_abs()).map_err(|_| MathError::Overflow)?;
        v.protocol_fees = sub_u64(v.protocol_fees, x)?;
        v.lender_sol = add_u64(v.lender_sol, x)?;
    }
    Ok(())
}

/// `pool_model.Pool._credit` (ECON §9.4, §9.5; ARCH §3.3 `credit`): credits `lamports`
/// (negative: a debit) of entitlement pro-rata to the current shares,
/// `acc += ⌊(lamports·WAD + carry)/S⌋` (toward −∞, remainder carried); with `realised`
/// it also allocates positive amounts as realised SOL ([`credit_r`]). While `S = 0`: after
/// a full wipe (`wipe_live`) the amount tops up the wipe record (`wipe_record`, both
/// accumulators if `realised`, V2-12); otherwise the SOL has no lender and moves from
/// `lender_sol` to `protocol_fees` (R3A-10).
///
/// # Errors
/// `InvalidInput` if a top-up is due and `wipe_record` is not the live wipe record;
/// `Underflow`/`Overflow` if a counter leaves its type.
pub fn credit(
    pool: &mut Pool,
    vault: &mut Vault,
    wipe_record: Option<&mut ShareEpoch>,
    lamports: i128,
    realised: bool,
) -> Result<(), MathError> {
    let (mut p, mut v) = (*pool, *vault);
    let mut rec = wipe_record.as_deref().copied();
    credit_in(&mut p, &mut v, rec.as_mut(), lamports, realised)?;
    commit_record(wipe_record, rec);
    *pool = p;
    *vault = v;
    Ok(())
}

/// Top-up of the live full-wipe record by `lamports` (V2-12).
fn top_up(
    pool: &Pool,
    mut rec: ShareEpoch,
    lamports: i128,
    realised: bool,
) -> Result<ShareEpoch, MathError> {
    if !rec.is_full_wipe() || rec.epoch.checked_add(1) != Some(pool.share_epoch) {
        return Err(MathError::InvalidInput);
    }
    let lw = ovf(lamports.checked_mul(WAD_I))?;
    let (d, r) = divmod_floor(add_i128(lw, rec.carry)?, rec.s_old)?;
    rec.carry = to_i128(r)?;
    rec.acc_end = add_i128(rec.acc_end, d)?;
    if realised {
        let (d, r) = divmod_floor(add_i128(lw, to_i128(rec.carry_r)?)?, rec.s_old)?;
        rec.carry_r = r;
        rec.acc_r_end = to_unsigned(add_i128(to_i128(rec.acc_r_end)?, d)?)?;
    }
    Ok(rec)
}

fn credit_r_in(p: &mut Pool, lamports: u64) -> Result<(), MathError> {
    if lamports == 0 || p.total_shares == 0 {
        return Ok(());
    }
    let num = add_u128(ovf(u128::from(lamports).checked_mul(WAD))?, p.carry_r)?;
    let d = ovf(num.checked_div(p.total_shares))?;
    let carry_r = ovf(num.checked_rem(p.total_shares))?;
    p.acc_r = add_u128(p.acc_r, d)?;
    p.carry_r = carry_r;
    Ok(())
}

/// `pool_model.Pool._credit_r` (V2-2; ECON §9.4): allocates `lamports` of realised
/// lender SOL to the current shares, `acc_r += ⌊(lamports·WAD + carry_r)/S⌋`. No-op for
/// 0 or while `S = 0`.
///
/// # Errors
/// `Overflow` if `acc_r` leaves `u128`.
pub fn credit_r(pool: &mut Pool, lamports: u64) -> Result<(), MathError> {
    let mut p = *pool;
    credit_r_in(&mut p, lamports)?;
    *pool = p;
    Ok(())
}

// ---- lender touch (ECON §9.4, §9.5; ARCH §3.3 `touch`) ------------------------------------

/// Epoch conversion as far as `records` reach (the loop of `pool_model.Pool._touch`),
/// accumulating `pending` and `real` in `i128`; tells `records` where it stopped.
fn convert_in<R: Recs>(
    pool: &Pool,
    l: &mut Lender,
    records: &mut R,
    pending: &mut i128,
    real: &mut i128,
) -> Result<Standing, MathError> {
    let st = convert_steps(pool, l, records, pending, real)?;
    records.converted(l.epoch, l.shares);
    Ok(st)
}

fn convert_steps<R: Recs>(
    pool: &Pool,
    l: &mut Lender,
    records: &mut R,
    pending: &mut i128,
    real: &mut i128,
) -> Result<Standing, MathError> {
    if l.epoch > pool.share_epoch {
        return Err(MathError::InvalidInput);
    }
    loop {
        if l.epoch == pool.share_epoch {
            return Ok(Standing::Current);
        }
        let parks = pool.is_wipe_live() && l.epoch.checked_add(1) == Some(pool.share_epoch);
        if l.shares == 0 && l.lock == 0 && l.lock_term == 0 && !parks && l.fwd == FWD_NONE {
            // V2-12 jump for an empty lender, without reading a record: the model reads
            // one, adds 0, scales nothing and jumps to the same state. A lender whose
            // shares and lock floored to 0 can still hold a term (`⌊lock·WAD/Π⌋ > lock`
            // once Π < WAD): the model scales it by this record before jumping, so such a
            // lender reads the record below (M0B-A-1).
            l.epoch = pool.share_epoch;
            l.snap = pool.acc;
            l.snap_r = pool.acc_r;
            continue;
        }
        let Some((idx, rec)) = records.get(l.epoch)? else {
            return Ok(Standing::Behind);
        };
        if R::ACCRUE {
            match l.fwd {
                // KI-66 full: the pot-only touch accrued this epoch to the record's ends.
                FWD_HOLD => {}
                FWD_OPEN => {
                    // The chain took its part of this epoch; the shares' own accrual less it.
                    let q = sub_u128(l.shares, u128::from(l.fwd_lag))?;
                    if l.epoch < l.fwd_epoch {
                        // The whole epoch: only the lag's part remains (one product).
                        *pending = add_i128(*pending, lag_part(q, l.fwd_lag, rec.acc_end)?)?;
                        *real = add_i128(*real, lag_part(q, l.fwd_lag, to_i128(rec.acc_r_end)?)?)?;
                    } else {
                        let (fp, fr) = fwd_part(l, l.epoch, q, rec.acc_end, rec.acc_r_end)?;
                        *pending =
                            add_i128(*pending, sub_i128(accrued(l.shares, rec.acc_end)?, fp)?)?;
                        *real =
                            add_i128(*real, sub_i128(accrued_r(l.shares, rec.acc_r_end)?, fr)?)?;
                    }
                }
                _ => {
                    *pending =
                        add_i128(*pending, accrued(l.shares, sub_i128(rec.acc_end, l.snap)?)?)?;
                    *real = add_i128(
                        *real,
                        accrued(l.shares, diff_u128(rec.acc_r_end, l.snap_r)?)?,
                    )?;
                }
            }
        }
        if parks {
            // V2-12: the full-wipe record is still open for top-ups; stay on it (a chain that
            // ended here is settled by the accrual above).
            l.snap = rec.acc_end;
            l.snap_r = rec.acc_r_end;
            l.clear_fwd();
            return Ok(Standing::Parked);
        }
        let (num, den) = rec.ratio()?;
        // `⌊x·num/den⌋` with `num ∈ {0, 1}` (`ratio`): a division, no 256-bit product.
        let scale = |x: u128| -> Result<u128, MathError> {
            if num == 0 {
                Ok(0)
            } else {
                ovf(x.checked_div(den))
            }
        };
        if R::HOLD && num != 0 {
            // KI-66: the pot-only touch accrued the lender up to the values the rebase record
            // froze for its epoch and stays on it, unconverted: no share, lock or `conv_rem`
            // moves, so it orders no conversion (KI65-REV-SEC1). KI-66 full: from here the
            // forward chain (`fwd_walk`) accrues the floor of its shares through the later
            // records, which no conversion order can lower.
            l.fwd = FWD_HOLD;
            l.fwd_epoch = ovf(l.epoch.checked_add(1))?;
            l.fwd_shares = scale(l.shares)?;
            l.fwd_lag = 0;
            l.snap = 0;
            l.snap_r = 0;
            return Ok(Standing::Behind);
        }
        // KI-66 full: the chain's shares in the next epoch, and whether this epoch settled it.
        let chain = if l.fwd == FWD_NONE {
            None
        } else {
            let base = if l.fwd == FWD_OPEN {
                sub_u128(l.shares, u128::from(l.fwd_lag))?
            } else {
                l.shares
            };
            Some((scale(base)?, l.epoch >= l.fwd_epoch))
        };
        // KI-65: cumulative rounding through the record, so the conversions of every holder
        // of the old epoch sum to the new S (no orphan shares).
        let (shares, rem) = convert_shares(l.shares, rec.conv_rem, num, den)?;
        if rem != rec.conv_rem {
            records.set_rem(idx, rem)?;
        }
        l.shares = shares;
        l.lock = scale(l.lock)?;
        // v0.4.1: the term scales with the bucket it sits in (the roll scaled the buckets).
        l.lock_term = scale(l.lock_term)?;
        l.epoch = ovf(l.epoch.checked_add(1))?;
        match chain {
            Some((q, false)) => {
                // Inside the chain: nothing accrued on the new shares yet; `snap` / `snap_r`
                // stay the chain's snapshots in `fwd_epoch`.
                l.fwd = FWD_OPEN;
                l.fwd_lag =
                    u8::try_from(sub_u128(l.shares, q)?).map_err(|_| MathError::Invariant)?;
                // The lag lemma: `shares ≤ chain + 1` stays so through a record (`conv_rem < den`).
                if l.fwd_lag > 1 || (l.epoch == l.fwd_epoch && q != l.fwd_shares) {
                    return Err(MathError::Invariant);
                }
            }
            _ => {
                l.clear_fwd();
                l.snap = 0;
                l.snap_r = 0;
            }
        }
        if l.shares == 0 && l.lock == 0 {
            // V2-12 jump: nothing left to convert.
            l.epoch = pool.share_epoch;
            l.snap = pool.acc;
            l.snap_r = pool.acc_r;
            l.clear_fwd();
        }
    }
}

/// `pool_model.Pool._to_pot` (R3A-2): realised allocation beyond the lender's
/// entitlement, `surplus = (real − claimed) − max(pending, 0)`, moves to the pot.
fn to_pot(v: &mut Vault, claimed: u64, pending: i128, real: &mut i128) -> Result<(), MathError> {
    let surplus = sub_i128(sub_i128(*real, i128::from(claimed))?, pending.max(0))?;
    if surplus > 0 {
        *real = sub_i128(*real, surplus)?;
        v.unmatched = add_u64(v.unmatched, to_unsigned(surplus)?)?;
    }
    Ok(())
}

/// KI-66 full: `⌊(q + lag)·x/WAD⌋ − ⌊q·x/WAD⌋` (floor) with one product: with `q·x = WAD·h + r`,
/// `r ∈ [0, WAD)` (Euclid), it is `⌊(r + lag·x)/WAD⌋`. 0 without a lag. The owner's accrual of an
/// epoch the forward chain accrued whole, less the chain's part, at the cost of one accrual.
fn lag_part(q: u128, lag: u8, x: i128) -> Result<i128, MathError> {
    if lag == 0 || x == 0 {
        return Ok(0);
    }
    let w = to_i128(WAD)?;
    let mag = x.unsigned_abs();
    let r_abs = match q.checked_mul(mag) {
        Some(p) => p % WAD,
        None => {
            U256::from_product(q, mag)
                .div_rem_u128(WAD)
                .ok_or(MathError::DivByZero)?
                .1
        }
    };
    let r = to_i128(r_abs)?;
    let r = if x < 0 && r != 0 { sub_i128(w, r)? } else { r };
    match i128::from(lag)
        .checked_mul(x)
        .and_then(|lx| lx.checked_add(r))
    {
        Some(n) => Ok(ovf(n.checked_div_euclid(w))?),
        None => sub_i128(accrued(add_u128(q, u128::from(lag))?, x)?, accrued(q, x)?),
    }
}

/// `⌊shares·x/WAD⌋` of an unsigned accumulator value (floor).
fn accrued_r(shares: u128, x: u128) -> Result<i128, MathError> {
    to_i128(mul_div(x, shares, WAD, Rounding::Floor)?)
}

/// KI-66 full: what the forward chain accrued in epoch `j` on its `q` shares there: the whole
/// epoch (to the record's ends) before `fwd_epoch`, up to the chain's snapshots in it. The chain
/// accrues by cumulative floor (`⌊q·end⌋ − ⌊q·snap⌋`), so its total over any number of pot-only
/// touches is this single floor, which the owner's conversion takes back.
fn fwd_part(
    l: &Lender,
    j: u32,
    q: u128,
    end: i128,
    end_r: u128,
) -> Result<(i128, i128), MathError> {
    if j < l.fwd_epoch {
        Ok((accrued(q, end)?, accrued_r(q, end_r)?))
    } else {
        Ok((accrued(q, l.snap)?, accrued_r(q, l.snap_r)?))
    }
}

/// KI-66 full: the pot-only touch's forward chain from `fwd_epoch` with `fwd_shares`, as far as
/// `records` reach: each record's ends, then the current `acc` / `acc_r` (cumulative floor); it
/// parks on a live wipe record and jumps to the current epoch once the chain holds 0 shares.
/// Writes only `pending`, `real`, the snapshots, `fwd_epoch` and `fwd_shares`.
fn fwd_walk(
    pool: &Pool,
    l: &mut Lender,
    records: &[ShareEpoch],
    pending: &mut i128,
    real: &mut i128,
) -> Result<(), MathError> {
    let (mut j, mut q) = (l.fwd_epoch, l.fwd_shares);
    loop {
        if q == 0 {
            j = pool.share_epoch;
            l.snap = pool.acc;
            l.snap_r = pool.acc_r;
            break;
        }
        let (end, end_r) = if j == pool.share_epoch {
            (pool.acc, pool.acc_r)
        } else {
            match record_idx(records, j)?.and_then(|i| records.get(i)) {
                Some(r) => (r.acc_end, r.acc_r_end),
                None => break,
            }
        };
        *pending = add_i128(*pending, sub_i128(accrued(q, end)?, accrued(q, l.snap)?)?)?;
        *real = add_i128(
            *real,
            sub_i128(accrued_r(q, end_r)?, accrued_r(q, l.snap_r)?)?,
        )?;
        l.snap = end;
        l.snap_r = end_r;
        if j == pool.share_epoch
            || (pool.is_wipe_live() && j.checked_add(1) == Some(pool.share_epoch))
        {
            break;
        }
        let rec = records
            .get(record_idx(records, j)?.ok_or(MathError::Invariant)?)
            .ok_or(MathError::Invariant)?;
        let (num, den) = rec.ratio()?;
        q = if num == 0 {
            0
        } else {
            ovf(q.checked_div(den))?
        };
        j = ovf(j.checked_add(1))?;
        l.snap = 0;
        l.snap_r = 0;
    }
    if j > pool.share_epoch {
        return Err(MathError::Invariant);
    }
    l.fwd_epoch = j;
    l.fwd_shares = q;
    Ok(())
}

fn touch_in(
    pool: &Pool,
    v: &mut Vault,
    l: &mut Lender,
    records: &mut ReadOnly<'_>,
    now: i64,
) -> Result<Standing, MathError> {
    cal_advance(v, now)?;
    let mut pending = i128::from(l.pending);
    let mut real = i128::from(l.real);
    let st = convert_in(pool, l, records, &mut pending, &mut real)?;
    touch_tail(pool, v, l, st, false, pending, real, now)?;
    Ok(st)
}

/// The rest of `pool_model.Pool._touch` once the conversion stopped at `st` (the calendar
/// already advanced), with the conversion's `pending` and `real`: a lender parked on a live
/// wipe record, or held at a rebase record by the pot-only touch (`held`, [`touch_pot`], its
/// forward chain walked), moves its surplus to the pot and stops there; a current lender
/// accrues the current epoch (with a forward chain ending there, [`FWD_OPEN`], less the
/// chain's part, then drops the chain), moves its surplus and drops its own expired lock.
/// `LenderBehind` for a lender neither current, parked nor held.
#[allow(clippy::too_many_arguments)]
fn touch_tail(
    pool: &Pool,
    v: &mut Vault,
    l: &mut Lender,
    st: Standing,
    held: bool,
    mut pending: i128,
    mut real: i128,
    now: i64,
) -> Result<(), MathError> {
    match st {
        Standing::Behind if !held => return Err(MathError::LenderBehind),
        Standing::Behind | Standing::Parked => to_pot(v, l.claimed, pending, &mut real)?,
        Standing::Current => {
            if l.fwd == FWD_NONE {
                pending = add_i128(pending, accrued(l.shares, sub_i128(pool.acc, l.snap)?)?)?;
                real = add_i128(real, accrued(l.shares, diff_u128(pool.acc_r, l.snap_r)?)?)?;
            } else {
                // KI-66 full: the chain ended in this epoch; the shares' accrual less its part.
                let q = sub_u128(l.shares, u128::from(l.fwd_lag))?;
                if l.fwd != FWD_OPEN || l.fwd_epoch != l.epoch || q != l.fwd_shares {
                    return Err(MathError::Invariant);
                }
                let (fp, fr) = fwd_part(l, l.epoch, q, pool.acc, pool.acc_r)?;
                pending = add_i128(pending, sub_i128(accrued(l.shares, pool.acc)?, fp)?)?;
                real = add_i128(real, sub_i128(accrued_r(l.shares, pool.acc_r)?, fr)?)?;
                l.clear_fwd();
            }
            l.snap = pool.acc;
            l.snap_r = pool.acc_r;
            to_pot(v, l.claimed, pending, &mut real)?;
            // R3B-1 / v0.4.1: the lender's own expired lock; the calendar already dropped
            // its term from the aggregate at `lock_until`.
            if l.lock != 0 && now >= l.lock_until && l.lock_epoch == v.pi_epoch {
                l.lock = 0;
                l.lock_term = 0;
                l.lock_epoch = NO_LOCK_EPOCH;
            }
        }
    }
    l.pending = to_i64(pending)?;
    l.real = to_unsigned(real)?;
    Ok(())
}

/// Lazy share-epoch conversion only, as far as `records` reach (the loop of
/// `pool_model.Pool._touch`; ECON §9.5, V2-12 jump and parking; ARCH §3.3
/// `sync_lender`). Does not accrue the current epoch, move a surplus to the pot or read
/// the calendar: follow with [`touch`].
///
/// **No rebase orphans (KI-65).** Through a rebase record (`1/den`) the lender's `s` old
/// shares become `⌊(conv_rem + s)/den⌋` and the record keeps `(conv_rem + s) mod den`
/// (written back into `records`: the caller persists every record whose `conv_rem`
/// changed). The quotients telescope: whatever the order, the holders of the old epoch
/// receive `⌊C/den⌋` new shares once they have converted `C` old ones, so once all of them
/// have, they hold exactly the new `S = ⌊s_old/den⌋` (by induction from `S = 0`: every
/// share of `s_old` was held). Each lender gets `⌊s/den⌋` or one more; the order only
/// decides who receives the record's at most `n − 1` rounding units (one per position: a
/// party with several positions, or a tuned residue, can receive several; the program lets
/// only a position's owner convert it through a rebase record, KI65-REV-SEC1, so a party
/// orders only its own conversions; anyone else's sync is [`touch_pot`]). Hence ECON §13
/// I-P1′, recursive over the records `0..N` behind the current epoch `N`:
/// `S = Σ current shares + L_{N−1}`, where `L_e = ⌊(conv_rem_e + U_e)/den_e⌋` through a
/// rebase record (0 through a full wipe), `U_e` = the epoch-`e` shares of the lenders still
/// at epoch `e` + `L_{e−1}` (`L_{−1} = 0`): only the latest record's term, its `U` carrying
/// the latent units of the earlier records (a sum of every record's term would count a
/// lender two records behind twice, in two epochs' units). `S` reaches 0 only when no
/// position holds or will hold a share.
///
/// # Errors
/// `InvalidInput` if the lender is ahead of the pool, the records are not consecutive
/// or a record is malformed (`conv_rem ≥ den` included); `Overflow`/`Underflow` if
/// `pending`/`real` leave their field types; `Invariant` if the commit pass diverges from
/// the read-only pass (unreachable). A refusal leaves the lender and `records` unchanged;
/// a partial conversion ([`Standing::Behind`]) writes only the records it passed.
///
/// **The forward chain (KI-66 full).** A lender with a chain ([`touch_pot`]) accrues in each
/// epoch only what the chain did not ([`FWD_HOLD`]: 0 at its own epoch; [`FWD_OPEN`]: the lag's
/// part, or at `fwd_epoch` its shares' accrual less the chain's up to its snapshots), keeps
/// [`FWD_OPEN`] while it stops inside the chain (`Invariant` if `shares − chain` leaves `{0, 1}`
/// or the chain's shares at `fwd_epoch` differ from `fwd_shares`; unreachable) and drops the
/// chain once it passes `fwd_epoch`, parks or jumps; [`touch`] drops it at the current epoch.
pub fn convert(
    pool: &Pool,
    lender: &mut Lender,
    records: &mut [ShareEpoch],
) -> Result<Standing, MathError> {
    let mut l = *lender;
    let mut pending = i128::from(l.pending);
    let mut real = i128::from(l.real);
    let mut ro = ReadOnly::new(records);
    let st = convert_in(pool, &mut l, &mut ro, &mut pending, &mut real)?;
    l.pending = to_i64(pending)?;
    l.real = to_unsigned(real)?;
    let seen = ro.first;
    commit_rems(pool, lender, records, seen)?;
    *lender = l;
    Ok(st)
}

/// The commit pass (KI-65): replays `before`'s conversion on the same records and writes
/// each record's new `conv_rem`. It runs only after the read-only pass succeeded, from the
/// same lender; the conversion's path (shares, lock, lock term, epoch, the jump and the
/// parking) reads only the lender, the records and the pool's `share_epoch` and `wipe_live`
/// (which a vest release does not move), so it takes the same path. It skips the accruals of
/// `pending` and `real` (no path decision reads them) and must stop where the read-only pass's
/// first conversion stopped (`seen`: epoch and shares), else `Invariant`. Records the lender
/// does not convert through are left as they are.
fn commit_rems(
    pool: &Pool,
    before: &Lender,
    records: &mut [ShareEpoch],
    seen: Option<(u32, u128)>,
) -> Result<(), MathError> {
    if before.epoch == pool.share_epoch {
        return Ok(());
    }
    let mut l = *before;
    let (mut pending, mut real) = (i128::from(before.pending), i128::from(before.real));
    let mut c = Commit {
        recs: records,
        end: None,
    };
    convert_in(pool, &mut l, &mut c, &mut pending, &mut real).map_err(|_| MathError::Invariant)?;
    if c.end.is_none() || c.end != seen {
        return Err(MathError::Invariant);
    }
    Ok(())
}

/// `pool_model.Pool._touch(who)` (ECON §9.4, §9.5; ARCH §3.3 `touch`): [`cal_advance`];
/// convert; accrue `pending` and `real` to the current `acc`/`acc_r` (floor); move a
/// surplus of realised SOL over entitlement to the pot `unmatched` (R3A-2); clear the
/// lender's own lock if it expired (`now ≥ lock_until`; the aggregate is not touched, the
/// calendar dropped it). A lender parked on a live wipe record moves its surplus to the
/// pot and stops there. Writes only the lender and the vault. Idempotent at a given `now`.
/// Returns [`Standing::Current`] or [`Standing::Parked`]. The conversion writes the
/// `conv_rem` of the rebase records it passes back into `records` ([`convert`], KI-65).
///
/// # Errors
/// `LenderBehind` if records are missing (the lender would stay [`Standing::Behind`]);
/// otherwise as [`convert`]; `Overflow` if the pot leaves `u64`.
pub fn touch(
    pool: &Pool,
    vault: &mut Vault,
    lender: &mut Lender,
    records: &mut [ShareEpoch],
    now: i64,
) -> Result<Standing, MathError> {
    let (mut v, mut l) = (*vault, *lender);
    let mut ro = ReadOnly::new(records);
    let st = touch_in(pool, &mut v, &mut l, &mut ro, now)?;
    let seen = ro.first;
    commit_rems(pool, lender, records, seen)?;
    *vault = v;
    *lender = l;
    Ok(st)
}

/// The pot-only touch (KI-66, KI-66 full; ECON §9.4 R3A-2, §9.5; ARCH §3.3 `sync_lender` without
/// the owner's signature; `pool_model.Pool._touch_pot`): [`touch`], except that the lender is
/// never converted through a rebase record (`1/1,000^k`; KI65-REV-SEC1).
///
/// **The hold.** A lender whose conversion reaches one (holding shares, a lock or a lock term at
/// that record's epoch `E`, with the record at hand) accrues `pending` and `real` up to the
/// record's `acc_end` / `acc_r_end` (the values it froze for that epoch; floor, as the
/// conversion would) and starts its forward chain ([`FWD_HOLD`]: `fwd_epoch = E + 1`,
/// `fwd_shares = ⌊s/den_E⌋`, snapshots 0).
///
/// **The forward chain.** It then accrues what the chain's shares earn in the later epochs, as
/// far as `records` reach (they may start at or before `fwd_epoch`): each record's ends, then the
/// current `acc` / `acc_r`, on `q_{j+1} = ⌊q_j·num_j/den_j⌋` (0 through a full wipe) — the floor
/// of the lender's shares through the records, which no conversion order can lower — by
/// cumulative floor (`⌊q·end/WAD⌋ − ⌊q·snap/WAD⌋`), so any number of touches sums to one floor.
/// It parks on a live wipe record and, once the chain holds 0 shares, jumps to the current
/// epoch. A lender with a chain continues it from `fwd_epoch` (no hold again). Then the surplus
/// (R3A-2) moves to the pot. The shares, lock, lock term, `lock_until` and epoch stay, and no
/// record is written, so it orders no conversion. Returns [`Standing::Behind`] for a held or
/// chained lender.
///
/// **No double count.** Idempotent: a second pot-only touch accrues 0 and finds no surplus. The
/// owner's later conversion ([`convert`], [`touch`]) takes the chain's accruals back exactly: it
/// adds 0 at `E` in [`FWD_HOLD`]; through a record inside the chain it stores [`FWD_OPEN`] with
/// `fwd_lag = shares − chain`, 0 or 1 (`shares ≤ chain + 1` stays so through a record, as
/// `conv_rem < den`); at an epoch the chain accrued whole it adds only the lag's part
/// (`⌊(q + lag)·A⌋ − ⌊q·A⌋`), and at `fwd_epoch` `⌊shares·A⌋ − ⌊q·snap⌋`, then drops the
/// chain. So `pending`, the lender's shares and every record end as after a [`touch`] alone,
/// and `real + unmatched` too (only the split between the two can differ, as between touches at
/// two times). What stays with the owner is the latent rounding unit: its shares beyond the
/// chain's floor (0 or 1 per record) earn in the later epochs only through its conversion.
///
/// Otherwise as [`convert`] followed by [`touch`]: plain catch-up through full-wipe records, the
/// V2-12 jump, the parking on a live wipe record and the touch of a current lender; a lender
/// left [`Standing::Behind`] because records are missing before any hold keeps the partial
/// conversion (`pending` and `real` accrued through the records passed), as [`convert`] does,
/// without the calendar or the pot.
///
/// # Errors
/// As [`convert`] and [`touch`] (never `LenderBehind`); `Invariant` if the conversion would
/// write a record (unreachable: a full wipe keeps its `conv_rem`) or the chain would pass the
/// current epoch. A refusal changes nothing.
pub fn touch_pot(
    pool: &Pool,
    vault: &mut Vault,
    lender: &mut Lender,
    records: &[ShareEpoch],
    now: i64,
) -> Result<Standing, MathError> {
    let (mut v, mut l) = (*vault, *lender);
    let mut pending = i128::from(l.pending);
    let mut real = i128::from(l.real);
    let (st, held) = if l.fwd == FWD_NONE {
        let mut hold = Hold(records);
        let st = convert_steps(pool, &mut l, &mut hold, &mut pending, &mut real)?;
        (st, st == Standing::Behind && l.fwd == FWD_HOLD)
    } else {
        (Standing::Behind, true)
    };
    if held {
        fwd_walk(pool, &mut l, records, &mut pending, &mut real)?;
    }
    if st == Standing::Behind && !held {
        l.pending = to_i64(pending)?;
        l.real = to_unsigned(real)?;
    } else {
        cal_advance(&mut v, now)?;
        touch_tail(pool, &mut v, &mut l, st, held, pending, real, now)?;
    }
    *vault = v;
    *lender = l;
    Ok(st)
}

/// Whether a `LenderPosition` may close and return its rent (KI-67; ARCH §3.3 `withdraw`,
/// `claim_sol`): current, no shares, nothing owed either way (`pending == 0`, so `real ==
/// claimed` after the touch's surplus move), no lock and no forward chain (KI-66 full). A
/// `lock_term` without a `lock` is not read again (a lock of 0 is no lock, and the calendar
/// keeps its own copy of the term until `lock_until`), so it does not keep a position open.
#[must_use]
pub const fn closable(pool: &Pool, l: &Lender) -> bool {
    l.epoch == pool.share_epoch
        && l.shares == 0
        && l.pending == 0
        && l.lock == 0
        && l.fwd == FWD_NONE
}

// ---- share value ---------------------------------------------------------------------------

/// Tokens a burn of `shares` pays: `⌊shares·(T + ΣB_Q + VA)/(S + VS)⌋` (ECON §9.2).
///
/// # Errors
/// `Overflow` if the result exceeds `u128`.
pub fn tokens_for_shares(pool: &Pool, shares: u128) -> Result<u128, MathError> {
    mul_div(shares, claims_va(pool)?, shares_vs(pool)?, Rounding::Floor)
}

/// Shares a deposit of `amount` mints: `⌊amount·(S + VS)/(T + ΣB_Q + VA)⌋` (ECON §9.2).
///
/// # Errors
/// `Overflow` if the result exceeds `u128`.
pub fn shares_for_tokens(pool: &Pool, amount: u64) -> Result<u128, MathError> {
    mul_div(
        u128::from(amount),
        shares_vs(pool)?,
        claims_va(pool)?,
        Rounding::Floor,
    )
}

/// Token claim of a converted lender, 0 unless it is in the current epoch
/// (`pool_model.token_value`; I-P1).
///
/// # Errors
/// As [`tokens_for_shares`].
pub fn token_value(pool: &Pool, lender: &Lender) -> Result<u128, MathError> {
    if lender.epoch != pool.share_epoch {
        return Ok(0);
    }
    tokens_for_shares(pool, lender.shares)
}

// ---- pro-rata withdrawal (ECON §9.3; C0-9, V2-3, R3B-1, R4A-2) ------------------------------

/// `pool_model.Pool._lock_of`: `lock_i(now) = min(⌈lock·Π/lock_Π⌉, shares)` if the lock
/// is from this `Π` epoch and `now < lock_until`, else 0 (ECON §9.3).
///
/// # Errors
/// `DivByZero` if `lock_pi` is 0; `Overflow` if the product exceeds `u128`.
pub fn lock_of(vault: &Vault, lender: &Lender, now: i64) -> Result<u128, MathError> {
    if lender.lock_epoch != vault.pi_epoch || lender.lock == 0 || now >= lender.lock_until {
        return Ok(0);
    }
    Ok(mul_div(lender.lock, vault.pi, lender.lock_pi, Rounding::Ceil)?.min(lender.shares))
}

/// `pool_model.Pool.locked_total`: `locked = min(⌊lock_norm·Π/WAD⌋, S)` (floor, v0.4.1)
/// with `lock_norm` as the calendar stands at `now` (ECON §9.3). Does not change the
/// vault.
///
/// # Errors
/// `Overflow` if the product exceeds `u128`.
pub fn locked_total(pool: &Pool, vault: &Vault, now: i64) -> Result<u128, MathError> {
    Ok(mul_div(norm_at(vault, now)?, vault.pi, WAD, Rounding::Floor)?.min(pool.total_shares))
}

/// Tokens the lender may withdraw now (`pool_model.Pool.free_allowance` after its
/// touch; ECON §9.3, SPEC §2.4): with `u = shares − lock_i(now)` and
/// `su = max(S − locked, u)`, `min(⌊u·T/su⌋, ⌊shares·(T + ΣB_Q + VA)/(S + VS)⌋, T)`;
/// 0 if `su = 0` or the lender is parked on a live wipe record. Call [`touch`] first;
/// the vault is only read (the calendar as it stands at `now`).
///
/// # Errors
/// `LenderBehind` if the lender has not been converted to the current epoch.
pub fn allowance(pool: &Pool, vault: &Vault, lender: &Lender, now: i64) -> Result<u64, MathError> {
    if lender.epoch != pool.share_epoch {
        let parked = pool.is_wipe_live() && lender.epoch.checked_add(1) == Some(pool.share_epoch);
        return if parked {
            Ok(0)
        } else {
            Err(MathError::LenderBehind)
        };
    }
    let u = sub_u128(lender.shares, lock_of(vault, lender, now)?)?;
    let su = sub_u128(pool.total_shares, locked_total(pool, vault, now)?)?.max(u);
    if su == 0 {
        return Ok(0);
    }
    let t = u128::from(pool.tokens_free);
    let by_free = mul_div(u, t, su, Rounding::Floor)?;
    let by_value = tokens_for_shares(pool, lender.shares)?;
    to_u64(by_free.min(by_value).min(t))
}

/// `pool_model.Pool.allowance_view`: the lender's [`allowance`] exactly as the program
/// computes it when only this lender is touched and every other lender is left as it is
/// (the state ECON §13 I-P4 must hold on, v0.4.1; T-POT). Pure: nothing changes, the
/// records' `conv_rem` included (the conversion is computed, not written).
///
/// # Errors
/// As [`touch`] and [`allowance`].
pub fn allowance_view(
    pool: &Pool,
    vault: &Vault,
    lender: &Lender,
    records: &[ShareEpoch],
    now: i64,
) -> Result<u64, MathError> {
    let (mut v, mut l) = (*vault, *lender);
    touch_in(pool, &mut v, &mut l, &mut ReadOnly::new(records), now)?;
    allowance(pool, &v, &l, now)
}

fn withdraw_in(
    p: &mut Pool,
    v: &mut Vault,
    l: &mut Lender,
    records: &mut ReadOnly<'_>,
    now: i64,
    lock_grace_s: u32,
    shares: u128,
) -> Result<u64, MathError> {
    release_in(p, now)?;
    let st = touch_in(p, v, l, records, now)?;
    if l.pending < 0 {
        return Err(MathError::NegativePending);
    }
    if st != Standing::Current || shares == 0 || shares > l.shares {
        return Err(MathError::InsufficientShares);
    }
    let claims = claims_va(p)?;
    let amount = tokens_for_shares(p, shares)?;
    let t = u128::from(p.tokens_free);
    if amount > t {
        return Err(MathError::InsufficientInventory);
    }
    let allow = u128::from(allowance(p, v, l, now)?);
    if amount > allow {
        return Err(MathError::AboveAllowance);
    }
    let cur = lock_of(v, l, now)?;
    let u = sub_u128(l.shares, cur)?;
    let burn_max = if allow > 0 {
        mul_div(allow, shares_vs(p)?, claims, Rounding::Ceil)?
    } else {
        0
    };
    let add = if p.borrowed_q > 0 && burn_max > 0 && u > burn_max {
        mul_div(
            shares.min(burn_max),
            sub_u128(u, burn_max)?,
            burn_max,
            Rounding::Ceil,
        )?
    } else {
        0
    };
    let remaining = sub_u128(l.shares, shares)?;
    let new_lock = add_u128(cur, add)?.min(remaining);
    if new_lock != 0 && v.lock_norm == 0 {
        // The first lock of a lock era: no lock is live (every term has left the calendar), so
        // none reads `Π`. `b_min` restarts at `ΣB_Q` and, since AIR-12, `Π` at `WAD` with it:
        // kept, every era's decay multiplied into the next (`Π` fell to 1 across eras while
        // `ΣB_Q` stayed above 0, and locks stopped decaying).
        v.b_min = p.borrowed_q;
        v.pi = WAD;
    }
    if l.lock_epoch == v.pi_epoch && cur != 0 {
        // A live lock leaves its bucket (an expired one's bucket is already gone).
        cal_sub(v, l.lock_until, l.lock_term)?;
    }
    // v0.4.1: floor, so the aggregate never exceeds the lenders' own (ceil) locks.
    let term = if new_lock != 0 {
        mul_div(new_lock, WAD, v.pi, Rounding::Floor)?
    } else {
        0
    };
    let until = if add != 0 {
        let prev = if cur != 0 { l.lock_until } else { 0 };
        prev.max(lock_until_for(p, now, lock_grace_s)?)
    } else {
        l.lock_until
    };
    cal_add(v, until, term)?;
    l.lock = new_lock;
    l.lock_pi = v.pi;
    l.lock_epoch = v.pi_epoch;
    l.lock_term = term;
    l.lock_until = until;
    if shares == p.total_shares && p.vest_bal != 0 {
        // R3B-2 / R4B-4: the last lender is paid the unreleased vest first. With cumulative
        // conversion (KI-65) the burn takes S to 0 only when no other position holds or will
        // hold a share of this epoch (I-P1′): a lender that has not converted yet still counts.
        let vb = p.vest_bal;
        p.vest_bal = 0;
        credit_pos(p, i128::from(vb), true)?;
        touch_in(p, v, l, records, now)?;
    }
    l.shares = sub_u128(l.shares, shares)?;
    p.total_shares = sub_u128(p.total_shares, shares)?;
    if p.total_shares == 0 {
        // AIR-08: the market has no lender again; SOL that reaches the trader from now on is
        // the protocol's (KI-10), so the next deposit waits for the sweeps, as after a wipe.
        p.wipe_swept = 0;
    }
    let amount = to_u64(amount)?;
    p.tokens_free = sub_u64(p.tokens_free, amount)?;
    Ok(amount)
}

/// `pool_model.Pool.withdraw` (ECON §9.3; ARCH §3.3 `withdraw`): releases the vest,
/// touches the lender, burns `shares` for `⌊shares·(T + ΣB_Q + VA)/(S + VS)⌋` tokens and
/// returns them. Refused unless `pending ≥ 0` (C0-13), `0 < shares ≤ lender.shares` in
/// the current epoch, and the tokens are `≤ T` and `≤` [`allowance`]. The borrowed part
/// of the withdrawn fraction becomes a lock: `lock_i + ⌈min(shares, b_max)·(u −
/// b_max)/b_max⌉` (capped at the remaining shares; none while `ΣB_Q = 0`), `b_max =
/// ⌈allowance·(S + VS)/(T + ΣB_Q + VA)⌉`, `u = shares_i − lock_i`; the first lock taken
/// while no lock is live (`lock_norm = 0`: a new `Π` epoch, or every lock expired) sets
/// `b_min = ΣB_Q` (V2-3) and `Π = WAD` (AIR-12). A live lock's term leaves its calendar bucket;
/// the new term `⌊new_lock·WAD/Π⌋` (floor) enters the bucket of `lock_until`, which a
/// lock increase moves to `max(lock_until, lock_until_for(now))` (R3B-1, R4A-2, R4A-8).
/// If the burn takes `S` to 0 the unreleased vest is released to the lender first (R3B-2);
/// the burn takes `S` to 0 only when no other position holds or will hold a share (no
/// rebase orphans, KI-65: a lender behind still counts in `S`, see [`convert`]), and it clears
/// the sweep bits, so the next deposit waits for the sweeps (AIR-08, see [`deposit`]). The
/// program pays SOL with [`claim`] afterwards. The lender's conversion writes `conv_rem`
/// into `records` ([`convert`]).
///
/// # Errors
/// In the model's order: `NegativePending`, `InsufficientShares`,
/// `InsufficientInventory`, `AboveAllowance`; `Overflow` if a lock term `⌊lock·WAD/Π⌋` or
/// `lock_norm` exceeds `u128` (possible only while `Π` is tiny within one lock era; the
/// withdrawal is then refused until `ΣB_Q` reaches 0 or every lock has expired, ARCH §9 M0b
/// note 1, AIR-12); as [`touch`] and [`release`].
pub fn withdraw(
    pool: &mut Pool,
    vault: &mut Vault,
    lender: &mut Lender,
    records: &mut [ShareEpoch],
    now: i64,
    lock_grace_s: u32,
    shares: u128,
) -> Result<u64, MathError> {
    let (mut p, mut v, mut l) = (*pool, *vault, *lender);
    let mut ro = ReadOnly::new(records);
    let amount = withdraw_in(&mut p, &mut v, &mut l, &mut ro, now, lock_grace_s, shares)?;
    let seen = ro.first;
    commit_rems(pool, lender, records, seen)?;
    *pool = p;
    *vault = v;
    *lender = l;
    Ok(amount)
}

fn resolve_in(p: &Pool, v: &mut Vault, now: i64, q: u64) -> Result<(), MathError> {
    cal_advance(v, now)?;
    if q >= p.borrowed_q {
        v.pi_epoch = ovf(v.pi_epoch.checked_add(1))?;
        v.pi = WAD;
        v.lock_norm = 0;
        v.lock_cal = [0; LOCK_BUCKETS];
        v.b_min = 0;
        return Ok(());
    }
    let bn = sub_u64(p.borrowed_q, q)?;
    if v.lock_norm > 0 && bn < v.b_min {
        v.pi = mul_div(v.pi, u128::from(bn), u128::from(v.b_min), Rounding::Ceil)?;
        v.b_min = bn;
    }
    Ok(())
}

/// `pool_model.Pool._resolve` (A37 as amended by A57, A75, V2-3; ECON §9.3): `q` borrowed
/// tokens are resolved (returned or SOL-settled). Call before `ΣB_Q` is reduced
/// ([`repay`] and [`settle`] do). [`cal_advance`] first; then, with `B' = ΣB_Q − q`: if
/// `B' = 0` (or `q ≥ ΣB_Q`) the `Π` epoch rolls and every lock and calendar bucket
/// clears; else if locks exist and `B' < b_min`, `Π ← ⌈Π·B'/b_min⌉` and `b_min = B'`.
///
/// # Errors
/// `Overflow` if the `Π` epoch counter overflows.
pub fn resolve(pool: &Pool, vault: &mut Vault, now: i64, q: u64) -> Result<(), MathError> {
    let mut v = *vault;
    resolve_in(pool, &mut v, now, q)?;
    *vault = v;
    Ok(())
}

// ---- deposits, claims, covers ---------------------------------------------------------------

fn deposit_in(
    p: &mut Pool,
    v: &mut Vault,
    l: &mut Lender,
    records: &mut ReadOnly<'_>,
    now: i64,
    amount: u64,
    rewards_required: bool,
) -> Result<u128, MathError> {
    release_in(p, now)?;
    touch_in(p, v, l, records, now)?;
    let minted = shares_for_tokens(p, amount)?;
    if minted == 0 {
        return Err(MathError::DepositTooSmall);
    }
    let total = add_u128(p.total_shares, minted)?;
    if p.total_shares != 0
        && p.borrowed_q != 0
        && ovf(u128::from(p.tokens_free).checked_mul(VEST_FLUSH_RATIO))? <= p.token_claims()?
    {
        // AIR-07's residual: from a drained pool a settlement can leave at most
        // 1/VEST_FLUSH_RATIO of the claims and flush the vest to the shares that held them. A
        // deposit now keeps its tokens as claims, so above 1/999 of them it would stop the flush
        // and a later deposit at the collapsed price would take the vest: none while drained.
        // Without shares there is no vest to keep (credits go to the protocol, R3A-10), and a
        // deposit is what brings `S` back (ARCH §9 M0b note 4), so `S = 0` is not drained.
        return Err(MathError::PoolDrained);
    }
    if p.total_shares == 0 {
        // R4A-7, and since AIR-08 also after the last lender's exit: SOL that reaches the
        // trader while `S = 0` is the wiped lenders' (the live wipe record) or the protocol's
        // (KI-10, R3A-10). A first deposit would make the sweep vest it to the new shares (and
        // the last-lender flush pay it back at once), so the sweeps that book it come first.
        let need = if rewards_required {
            WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
        } else {
            WIPE_TRADER_SWEPT
        };
        if p.wipe_swept & need != need {
            return Err(MathError::WipeSweepPending);
        }
        if p.is_wipe_live() {
            // V2-12: the full-wipe record is final from now on; a parked depositor converts.
            p.wipe_live = 0;
            touch_in(p, v, l, records, now)?;
        }
    }
    l.shares = add_u128(l.shares, minted)?;
    p.total_shares = total;
    p.tokens_free = add_u64(p.tokens_free, amount)?;
    Ok(minted)
}

/// `pool_model.Pool.deposit` (ECON §9.2; ARCH §3.3 `deposit`): releases the vest, touches
/// the lender, mints `⌊amount·(S + VS)/(T + ΣB_Q + VA)⌋` shares (returned) and adds
/// `amount` to `T`. A deposit into a pool without shares — after a full wipe (R4A-7) or,
/// since AIR-08, after the last lender's exit — needs the sweeps since `S` reached 0: the
/// trader sweep and, if `rewards_required` (a pump venue that can execute), the venue-reward
/// claim (a new market starts with both bits set: nothing can have reached its trader). After
/// a full wipe it then finalises the wipe record (clears `wipe_live`) and touches again, so a
/// parked depositor converts through it. No deposit is taken while the pool is drained —
/// shares held, tokens lent out and at most `1/`[`VEST_FLUSH_RATIO`] of the token claims
/// unborrowed — the only states a settlement that flushes the vest can start from (AIR-07's
/// residual). The lender's conversion writes `conv_rem` into `records` ([`convert`]).
///
/// # Errors
/// `DepositTooSmall` if nothing would be minted; `Overflow` if `S` or `T` would leave
/// `u128` / `u64`; `PoolDrained` if `S > 0`, `ΣB_Q > 0` and `VEST_FLUSH_RATIO·T ≤ T + ΣB_Q`;
/// `WipeSweepPending` if `S = 0` and a required sweep has not run since `S` reached 0; as
/// [`touch`] and [`release`].
pub fn deposit(
    pool: &mut Pool,
    vault: &mut Vault,
    lender: &mut Lender,
    records: &mut [ShareEpoch],
    now: i64,
    amount: u64,
    rewards_required: bool,
) -> Result<u128, MathError> {
    let (mut p, mut v, mut l) = (*pool, *vault, *lender);
    let mut ro = ReadOnly::new(records);
    let minted = deposit_in(
        &mut p,
        &mut v,
        &mut l,
        &mut ro,
        now,
        amount,
        rewards_required,
    )?;
    let seen = ro.first;
    commit_rems(pool, lender, records, seen)?;
    *pool = p;
    *vault = v;
    *lender = l;
    Ok(minted)
}

/// `pool_model.Pool.claim_sol` (C0-13, V2-2, R3A-2; ARCH §3.3 `claim_sol`): touches the
/// lender (no vest release: `Market` stays read-only), draws from the pot up to its own
/// deficit, `draw = min(unmatched, max(0, pending − (real − claimed)))`, then pays
/// `min(pending, real − claimed, lender_sol)` if positive, else 0 (the draw stays). A
/// second call in the same state pays 0 (I-P5) and writes no record (the lender is
/// current). Returns the lamports to transfer.
///
/// # Errors
/// As [`touch`]; `Overflow` if `real` or `claimed` leave `u64`.
pub fn claim(
    pool: &Pool,
    vault: &mut Vault,
    lender: &mut Lender,
    records: &mut [ShareEpoch],
    now: i64,
) -> Result<u64, MathError> {
    let (mut v, mut l) = (*vault, *lender);
    let mut ro = ReadOnly::new(records);
    touch_in(pool, &mut v, &mut l, &mut ro, now)?;
    let seen = ro.first;
    let own = sub_i128(i128::from(l.real), i128::from(l.claimed))?;
    let need = sub_i128(i128::from(l.pending), own)?.max(0);
    let draw = u64::try_from(need.min(i128::from(v.unmatched))).map_err(|_| MathError::Overflow)?;
    if draw != 0 {
        v.unmatched = sub_u64(v.unmatched, draw)?;
        l.real = add_u64(l.real, draw)?;
    }
    let unclaimed = sub_i128(i128::from(l.real), i128::from(l.claimed))?;
    let pay = i128::from(l.pending)
        .min(unclaimed)
        .min(i128::from(v.lender_sol));
    let paid = if pay > 0 {
        let pay: u64 = to_unsigned(pay)?;
        l.pending = to_i64(sub_i128(i128::from(l.pending), i128::from(pay))?)?;
        l.claimed = add_u64(l.claimed, pay)?;
        v.lender_sol = sub_u64(v.lender_sol, pay)?;
        pay
    } else {
        0
    };
    commit_rems(pool, lender, records, seen)?;
    *vault = v;
    *lender = l;
    Ok(paid)
}

/// `pool_model.Pool.cover` (C0-13; ARCH §3.3 `cover_deficit`): touches the lender, then
/// the lender pays `lamports` into `lender_sol`: `pending += lamports`, `real +=
/// lamports`. What exceeds the lender's entitlement reaches the pot at its next touch.
/// The lender's conversion writes `conv_rem` into `records` ([`convert`]).
///
/// # Errors
/// `Overflow` if a counter leaves its type; as [`touch`].
pub fn cover(
    pool: &Pool,
    vault: &mut Vault,
    lender: &mut Lender,
    records: &mut [ShareEpoch],
    now: i64,
    lamports: u64,
) -> Result<(), MathError> {
    let (mut v, mut l) = (*vault, *lender);
    let mut ro = ReadOnly::new(records);
    touch_in(pool, &mut v, &mut l, &mut ro, now)?;
    let seen = ro.first;
    v.lender_sol = add_u64(v.lender_sol, lamports)?;
    l.pending = to_i64(add_i128(i128::from(l.pending), i128::from(lamports))?)?;
    l.real = add_u64(l.real, lamports)?;
    commit_rems(pool, lender, records, seen)?;
    *vault = v;
    *lender = l;
    Ok(())
}

/// Continuous rent crediting (`pool_model.Pool.advance(dt, d_index)`; ECON §7, §9.4;
/// ARCH §3.0 accrue): releases the vest due at `now`, then credits `lamports`, the
/// lenders' credit for the elapsed interval (`rent::lender_credit`, floor), as
/// entitlement (`receivable += lamports`). A credit while `S = 0` (only possible with a
/// dust position left after the last lender, ARCH §9 M0b note 4) goes to the protocol.
///
/// # Errors
/// As [`release`] and [`credit`] (a credit while a full-wipe record is live cannot occur:
/// `ΣB_Q = 0`, so it is refused with `InvalidInput`).
pub fn accrue(
    pool: &mut Pool,
    vault: &mut Vault,
    now: i64,
    lamports: u64,
) -> Result<(), MathError> {
    let (mut p, mut v) = (*pool, *vault);
    release_in(&mut p, now)?;
    if lamports != 0 {
        p.receivable = add_u64(p.receivable, lamports)?;
        credit_in(&mut p, &mut v, None, i128::from(lamports), false)?;
    }
    *pool = p;
    *vault = v;
    Ok(())
}

/// Trader sweep or venue cashback to lenders (`pool_model.Pool.sweep_sol`; ARCH §3.3,
/// §3.9; A44, V2-12, R3A-10, R4A-7): `lender_sol += lamports`; while `S = 0` (a live full-wipe
/// record or, since AIR-08, after the last lender's exit) the bit of `source` that a deposit
/// waits for is set (even for 0 lamports); then [`vest`] (with `S > 0` the SOL vests over
/// `lump_vest_s`; with `S = 0` it tops up the live wipe record — pass it — or goes to the
/// protocol).
///
/// # Errors
/// As [`vest`]; `Overflow` if `lender_sol` leaves `u64`.
pub fn sweep(
    pool: &mut Pool,
    vault: &mut Vault,
    wipe_record: Option<&mut ShareEpoch>,
    now: i64,
    lump_vest_s: u32,
    lamports: u64,
    source: SweepSource,
) -> Result<(), MathError> {
    let (mut p, mut v) = (*pool, *vault);
    let mut rec = wipe_record.as_deref().copied();
    v.lender_sol = add_u64(v.lender_sol, lamports)?;
    if p.total_shares == 0 {
        // R4A-7; AIR-08: with no live wipe record the SOL goes to the protocol (KI-10), and the
        // bit lets the next deposit in.
        p.wipe_swept |= source.bit();
    }
    vest_in(&mut p, &mut v, rec.as_mut(), now, lump_vest_s, lamports)?;
    commit_record(wipe_record, rec);
    *pool = p;
    *vault = v;
    Ok(())
}

// ---- borrower side (ECON §9.4; ARCH §3.3–§3.8) ---------------------------------------------

/// An open lends `q` tokens (`pool_model.Pool.borrow`; ARCH §3.3): releases the vest,
/// `T −= q`, `ΣB_Q += q`, `max_release = max(max_release, release_at)` with `release_at =
/// deadline + max_migration_pause_s + grad_grace_s + s3_wait_s` (the position's snapshot
/// values; R4A-8, R4B-8). The utilisation cap is `caps::util_ok`'s; the rent aggregate
/// (`A_j += q·coef_j·(10⁴ − rent_protocol_bps)`) is `rent::Aggr::add`'s.
///
/// # Errors
/// `InvalidInput` if `q = 0` or `release_at` lies beyond the compiled horizon (`now` plus
/// 90 d of duration and 3 d of S3 tail: a later lock day could not be kept in the
/// [`LOCK_BUCKETS`]-day calendar); `InsufficientInventory` if `q > T`; `Overflow` if
/// `ΣB_Q` leaves `u64`; as [`release`].
pub fn borrow(pool: &mut Pool, now: i64, q: u64, release_at: i64) -> Result<(), MathError> {
    let horizon = add_i64(now, MAX_RELEASE_SPAN_S)?;
    if q == 0 || release_at > horizon {
        return Err(MathError::InvalidInput);
    }
    if q > pool.tokens_free {
        return Err(MathError::InsufficientInventory);
    }
    let mut p = *pool;
    release_in(&mut p, now)?;
    p.borrowed_q = add_u64(p.borrowed_q, q)?;
    p.tokens_free = sub_u64(p.tokens_free, q)?;
    p.max_release = p.max_release.max(release_at);
    *pool = p;
    Ok(())
}

/// The shared head of `repay` and `settle` (`pool_model.Pool._close_pos`): vest release,
/// [`resolve`], `receivable −= min(credited, receivable)`.
fn close_pos(p: &mut Pool, v: &mut Vault, now: i64, res: &Resolution) -> Result<(), MathError> {
    if res.q == 0 || res.q > p.borrowed_q {
        return Err(MathError::InvalidInput);
    }
    release_in(p, now)?;
    resolve_in(p, v, now, res.q)?;
    p.receivable = sub_u64(p.receivable, res.credited.min(p.receivable))?;
    Ok(())
}

/// `pool_model.Pool._rent_top_up` (finding M10-8C-1; ECON §9.4): the part of an exit's
/// lender rent that was never credited while the position was open,
/// `max(0, min(principal, rent) − credited)`. It is positive when the exit pays more rent than
/// accrued to the lenders — `R_due = R_min` within `min_rent_s` of the open, or a settlement's
/// rent, which carries no protocol share — and it **vests like a lump** in [`repay`] and
/// [`settle`]: credited at once it would go to the shares holding at the exit, so a just-in-time
/// deposit bundled around the exit would take its pro-rata part atomically. `principal` is the
/// immediate SOL that carries the rent (`repay`: the lender rent itself; `settle`: the
/// settlement's immediate part, which pays the rent first). Never above `principal`.
#[must_use]
pub fn rent_top_up(principal: u64, rent: u64, credited: u64) -> u64 {
    principal.min(rent).saturating_sub(credited)
}

/// The rent true-up of an exit (`repay` and `settle`): `principal` lamports reached
/// `lender_sol`, `credited` of them were credited while the position was open. The top-up
/// above `credited` ([`rent_top_up`]) is returned for the vest; the rest is immediate: the
/// entitlement difference `immediate − credited` (signed: a shortfall is a debit, ≤ 0 here)
/// and the realised allocation `immediate`.
fn true_up(
    p: &mut Pool,
    v: &mut Vault,
    principal: u64,
    rent: u64,
    credited: u64,
) -> Result<u64, MathError> {
    let top = rent_top_up(principal, rent, credited);
    let immediate = sub_u64(principal, top)?;
    credit_in(
        p,
        v,
        None,
        sub_i128(i128::from(immediate), i128::from(credited))?,
        false,
    )?;
    credit_r_in(p, immediate)?;
    Ok(top)
}

/// `pool_model.Pool.repay` (close, liquidation, expiry; ECON §9.4; ARCH §3.3): releases
/// the vest, resolves the position's `q` ([`resolve`]; `receivable −= min(credited,
/// receivable)`), returns `q` to `T`, adds `lender_rent + lump` to `lender_sol` with
/// `lender_rent = rent_paid − ⌊rent_paid·rent_protocol_bps/10⁴⌋` (the position's snapshot
/// share; the protocol part is the program's), and trues the rent up against `credited`:
/// a shortfall `lender_rent − credited < 0` is debited at once, `min(lender_rent, credited)`
/// is allocated as realised, and the top-up above `credited` ([`rent_top_up`], e.g. the
/// `R_min` excess of an exit within `min_rent_s`; finding M10-8C-1) vests with the lump.
/// Returns `lender_rent`.
///
/// # Errors
/// `InvalidInput` if `q = 0`, `q > ΣB_Q` or `rent_protocol_bps > 10⁴`;
/// `Overflow`/`Underflow` if a counter leaves its type.
pub fn repay(
    pool: &mut Pool,
    vault: &mut Vault,
    now: i64,
    lump_vest_s: u32,
    res: &Resolution,
    rent_paid: u64,
    rent_protocol_bps: u32,
) -> Result<u64, MathError> {
    if u64::from(rent_protocol_bps) > BPS {
        return Err(MathError::InvalidInput);
    }
    let (mut p, mut v) = (*pool, *vault);
    close_pos(&mut p, &mut v, now, res)?;
    p.tokens_free = add_u64(p.tokens_free, res.q)?;
    p.borrowed_q = sub_u64(p.borrowed_q, res.q)?;
    let protocol = to_u64(mul_div(
        u128::from(rent_paid),
        u128::from(rent_protocol_bps),
        u128::from(BPS),
        Rounding::Floor,
    )?)?;
    let lender_rent = sub_u64(rent_paid, protocol)?;
    v.lender_sol = add_u64(v.lender_sol, add_u64(lender_rent, res.lump)?)?;
    let top = true_up(&mut p, &mut v, lender_rent, lender_rent, res.credited)?;
    vest_in(
        &mut p,
        &mut v,
        None,
        now,
        lump_vest_s,
        add_u64(res.lump, top)?,
    )?;
    *pool = p;
    *vault = v;
    Ok(lender_rent)
}

/// `pool_model.Pool.settle` (SOL settlement S1/S2/S3; ECON §8.6, §9.4, §9.5; ARCH §3.3):
/// the token debt `q` is extinguished ([`resolve`], `ΣB_Q −= q`; token claims fall
/// pro-rata), `sol_in` lamports reach `lender_sol`; the lump part `min(lump, sol_in)` vests
/// and the principal `sol_in − lump` is immediate but for its rent top-up: the principal
/// pays `rent` (the position's `R_due`, all of it the lenders' in a settlement) first, and
/// the part of that above `credited` ([`rent_top_up`]; finding M10-8C-1) vests with the
/// lump; the entitlement is trued up by `principal − top-up − credited` and that much is
/// allocated as realised SOL; if the settlement leaves at most `1/`[`VEST_FLUSH_RATIO`] of
/// the token claims it found (a full wipe included), the unreleased vest — this settlement's
/// part included — is released to the current shares at once (AIR-07); then [`maybe_roll`].
/// Returns [`Settled`]: the new share-epoch record if the epoch rolled (the program writes
/// it), the SOL vested and whether the vest was flushed.
///
/// # Errors
/// `InvalidInput` if `q = 0` or `q > ΣB_Q`; `Overflow`/`Underflow` if a counter leaves
/// its type.
pub fn settle(
    pool: &mut Pool,
    vault: &mut Vault,
    now: i64,
    lump_vest_s: u32,
    res: &Resolution,
    sol_in: u64,
    rent: u64,
) -> Result<Settled, MathError> {
    let (mut p, mut v) = (*pool, *vault);
    // AIR-07: the token claims the settlement finds.
    let claims = p.token_claims()?;
    close_pos(&mut p, &mut v, now, res)?;
    p.borrowed_q = sub_u64(p.borrowed_q, res.q)?;
    let lump = res.lump.min(sol_in);
    let principal = sub_u64(sol_in, lump)?;
    v.lender_sol = add_u64(v.lender_sol, sol_in)?;
    let top = true_up(&mut p, &mut v, principal, rent, res.credited)?;
    let vested = add_u64(lump, top)?;
    vest_in(&mut p, &mut v, None, now, lump_vest_s, vested)?;
    // AIR-07: the old shares' token claims are (nearly) wiped: they keep their unreleased
    // lumps, as at a full wipe (`T + ΣB_Q = 0`), instead of leaving them to the next depositor.
    let flushed =
        p.total_shares != 0 && ovf(p.token_claims()?.checked_mul(VEST_FLUSH_RATIO))? <= claims;
    if flushed {
        flush_vest_in(&mut p, now)?;
    }
    let rec = maybe_roll_in(&mut p, &mut v, now)?;
    *pool = p;
    *vault = v;
    Ok(Settled {
        rec,
        vested,
        flushed,
    })
}

/// What [`settle`] booked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct Settled {
    /// The new share-epoch record if the epoch rolled (the program writes it).
    pub rec: Option<ShareEpoch>,
    /// The SOL the settlement added to the vest: `min(lump, sol_in)` and the rent top-up
    /// ([`rent_top_up`]; finding M10-8C-1). A flush releases it with the vest before
    /// (`SolSettle.vest_flushed`, with or without a roll; `EpochRolled.vest_flushed` too when
    /// the epoch rolls).
    pub vested: u64,
    /// Whether the settlement left at most `1/`[`VEST_FLUSH_RATIO`] of the token claims it
    /// found — a full wipe, or since AIR-07 dust — and so released the whole unreleased vest
    /// to the current shares.
    pub flushed: bool,
}

/// The release due at `now`, then the rest of the unreleased vest to the current shares at
/// once (realised): the flush of a full wipe and of a settlement that leaves dust (AIR-07).
/// `S > 0` (the caller checks).
fn flush_vest_in(p: &mut Pool, now: i64) -> Result<(), MathError> {
    release_in(p, now)?;
    let vb = p.vest_bal;
    p.vest_bal = 0;
    if vb != 0 {
        credit_pos(p, i128::from(vb), true)?;
    }
    Ok(())
}

// ---- share epochs (ECON §9.5; C0-6) -----------------------------------------------------------

fn maybe_roll_in(p: &mut Pool, v: &mut Vault, now: i64) -> Result<Option<ShareEpoch>, MathError> {
    if p.total_shares == 0 {
        return Ok(None);
    }
    let claims = p.token_claims()?;
    let (num, k) = if claims == 0 {
        // The wiped epoch's lenders get the unreleased vest (in `settle` already flushed).
        flush_vest_in(p, now)?;
        (0u8, 0u8)
    } else {
        let cap = ovf(R_MAX.checked_mul(add_u128(claims, VIRTUAL_ASSETS)?))?;
        if shares_vs(p)? <= cap {
            return Ok(None);
        }
        let mut k: u8 = 1;
        while add_u128(ovf(p.total_shares.checked_div(den_of(k)?))?, VIRTUAL_SHARES)? > cap {
            k = ovf(k.checked_add(1))?;
        }
        (1, k)
    };
    let rec = ShareEpoch {
        acc_end: p.acc,
        acc_r_end: p.acc_r,
        s_old: p.total_shares,
        carry: 0,
        carry_r: 0,
        conv_rem: 0,
        epoch: p.share_epoch,
        num,
        den_log1000: k,
        _pad: [0; 10],
    };
    let den = den_of(k)?;
    let num = u128::from(num);
    p.share_epoch = ovf(p.share_epoch.checked_add(1))?;
    p.total_shares = mul_div(p.total_shares, num, den, Rounding::Floor)?;
    // v0.4.1: every calendar bucket scales; lock_norm is their sum.
    let mut norm: u128 = 0;
    for b in &mut v.lock_cal {
        *b = mul_div(*b, num, den, Rounding::Floor)?;
        norm = add_u128(norm, *b)?;
    }
    v.lock_norm = norm;
    p.dust = add_i128(add_i128(p.dust, p.carry)?, to_i128(p.carry_r)?)?;
    p.carry = 0;
    p.carry_r = 0;
    p.acc = 0;
    p.acc_r = 0;
    p.wipe_live = u8::from(p.total_shares == 0);
    if p.is_wipe_live() {
        // R4A-7: a deposit must wait for the post-wipe sweeps.
        p.wipe_swept = 0;
    }
    Ok(Some(rec))
}

/// `pool_model.Pool._maybe_roll` (ECON §9.5): after a settlement, if `S > 0` and
/// `T + ΣB_Q = 0` the unreleased vest is released to the wiped shares and the epoch rolls
/// as a full wipe (`0/1`); else if `S + VS > R_MAX·(T + ΣB_Q + VA)` it rolls with the
/// smallest rebase `1/1,000^k` that restores the bound. A roll records `(acc, acc_r, S)`,
/// converts `S` and every calendar bucket (floor; `lock_norm` is their sum), moves the
/// carries to `dust`, restarts both accumulators, sets `wipe_live = (S = 0)` and, at a
/// full wipe, clears the post-wipe sweep bits. Returns the record to write.
///
/// # Errors
/// `Overflow` if a counter leaves its type; as [`release`].
pub fn maybe_roll(
    pool: &mut Pool,
    vault: &mut Vault,
    now: i64,
) -> Result<Option<ShareEpoch>, MathError> {
    let (mut p, mut v) = (*pool, *vault);
    let rec = maybe_roll_in(&mut p, &mut v, now)?;
    *pool = p;
    *vault = v;
    Ok(rec)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use core::mem::align_of;

    const D: i64 = LOCK_BUCKET_S;
    const VEST: u32 = 21_600;
    const GRACE: u32 = 86_400;

    #[test]
    fn layouts_are_target_independent() {
        for (size, align) in [
            (size_of::<Pool>(), align_of::<Pool>()),
            (size_of::<Vault>(), align_of::<Vault>()),
            (size_of::<Lender>(), align_of::<Lender>()),
            (size_of::<ShareEpoch>(), align_of::<ShareEpoch>()),
        ] {
            assert_eq!(size % 16, 0);
            assert!(align <= 16);
        }
    }

    #[test]
    fn divmod_matches_python() {
        let cases: [(i128, u128, i128, u128); 7] = [
            (7, 2, 3, 1),
            (-7, 2, -4, 1),
            (-8, 2, -4, 0),
            (0, 5, 0, 0),
            (i128::MIN, 1, i128::MIN, 0),
            (-1, u128::MAX, -1, u128::MAX - 1),
            (5, u128::MAX, 0, 5),
        ];
        for (n, d, q, r) in cases {
            assert_eq!(divmod_floor(n, d), Ok((q, r)), "{n} divmod {d}");
        }
        assert_eq!(divmod_floor(1, 0), Err(MathError::DivByZero));
        assert_eq!(
            divmod_floor(i128::MIN, i128::MAX.unsigned_abs() + 1),
            Ok((-1, 0))
        );
    }

    #[test]
    fn day_arithmetic_matches_python() {
        assert_eq!(ceil_div_i64(0, D), Ok(0));
        assert_eq!(ceil_div_i64(1, D), Ok(1));
        assert_eq!(ceil_div_i64(D, D), Ok(1));
        assert_eq!(ceil_div_i64(-1, D), Ok(0));
        assert_eq!(ceil_div_i64(-D - 1, D), Ok(-1));
        assert_eq!(day_of(-1), Ok(-1));
        assert_eq!(slot(-1), Ok(LOCK_BUCKETS - 1));
        assert_eq!(slot(128), Ok(0));
        let mut p = Pool::new();
        assert_eq!(lock_until_for(&p, 0, 3_600), Ok(D));
        assert_eq!(lock_until_for(&p, D - 3_600, 3_600), Ok(D));
        assert_eq!(lock_until_for(&p, D - 3_599, 3_600), Ok(2 * D));
        p.max_release = 30 * D + 4_800;
        assert_eq!(lock_until_for(&p, 5, GRACE), Ok(32 * D));
        assert_eq!(
            lock_until_for(&p, i64::MAX - 10, GRACE),
            Err(MathError::Overflow)
        );
    }

    #[test]
    fn den_bounds() {
        assert_eq!(den_of(0), Ok(1));
        assert_eq!(den_of(12), Ok(10u128.pow(36)));
        assert_eq!(den_of(13), Err(MathError::Overflow));
        let bad = ShareEpoch {
            num: 0,
            den_log1000: 1,
            ..rec0()
        };
        assert_eq!(bad.ratio(), Err(MathError::InvalidInput));
        let bad = ShareEpoch { num: 2, ..rec0() };
        assert_eq!(bad.ratio(), Err(MathError::InvalidInput));
    }

    fn rec0() -> ShareEpoch {
        ShareEpoch {
            acc_end: 0,
            acc_r_end: 0,
            s_old: 1,
            carry: 0,
            carry_r: 0,
            conv_rem: 0,
            epoch: 0,
            num: 0,
            den_log1000: 0,
            _pad: [0; 10],
        }
    }

    #[test]
    fn calendar_ring() {
        let mut v = Vault::new();
        // A day outside the ring is refused (advance first).
        assert_eq!(cal_add(&mut v, 200 * D, 5), Err(MathError::InvalidInput));
        cal_advance(&mut v, 10 * D + 7).unwrap();
        assert_eq!(v.cal_day, 11);
        assert_eq!(cal_add(&mut v, 10 * D, 5), Err(MathError::InvalidInput));
        assert_eq!(cal_add(&mut v, 11 * D, 0), Ok(()));
        cal_add(&mut v, 11 * D, 5).unwrap();
        cal_add(&mut v, 20 * D, 7).unwrap();
        cal_add(&mut v, 138 * D, 9).unwrap();
        assert_eq!(cal_add(&mut v, 139 * D, 1), Err(MathError::InvalidInput));
        assert_eq!(v.lock_norm, 21);
        assert_eq!(
            v.buckets().collect::<std::vec::Vec<_>>(),
            [(11, 5), (20, 7), (138, 9)]
        );
        // Removal takes at most the bucket; a dead day holds nothing.
        cal_sub(&mut v, 20 * D, 100).unwrap();
        cal_sub(&mut v, 5 * D, 100).unwrap();
        assert_eq!(v.lock_norm, 14);
        // The aggregate sees an expiry at its day end, without a touch.
        assert_eq!(norm_at(&v, 11 * D - 1), Ok(14));
        assert_eq!(norm_at(&v, 11 * D), Ok(9));
        cal_advance(&mut v, 11 * D).unwrap();
        assert_eq!((v.lock_norm, v.cal_day), (9, 12));
        // Idempotent, and never backwards.
        cal_advance(&mut v, 11 * D).unwrap();
        cal_advance(&mut v, 3).unwrap();
        assert_eq!((v.lock_norm, v.cal_day), (9, 12));
        // A gap of more than LOCK_BUCKETS days clears the ring.
        assert_eq!(norm_at(&v, 1_000 * D), Ok(0));
        cal_advance(&mut v, 1_000 * D).unwrap();
        assert_eq!((v.lock_norm, v.cal_day), (0, 1_001));
        assert!(v.lock_cal.iter().all(|b| *b == 0));
    }

    #[test]
    fn first_deposit_and_inflation_attack() {
        // ECON §9.2: internal counters ignore donations; a victim's 10⁹ redeems 10⁹.
        let (mut p, mut v) = (Pool::new(), Vault::new());
        let mut a = Lender::new(&p);
        let mut b = Lender::new(&p);
        assert_eq!(
            deposit(&mut p, &mut v, &mut a, &mut [], 0, 1, false),
            Ok(1_000)
        );
        assert_eq!(
            deposit(&mut p, &mut v, &mut b, &mut [], 0, 1_000_000_000, false),
            Ok(1_000_000_000_000)
        );
        assert_eq!(
            withdraw(&mut p, &mut v, &mut b, &mut [], 0, GRACE, 1_000_000_000_000),
            Ok(1_000_000_000)
        );
        let before = (p, v, b);
        assert_eq!(
            deposit(&mut p, &mut v, &mut b, &mut [], 0, 0, false),
            Err(MathError::DepositTooSmall)
        );
        assert_eq!(
            withdraw(&mut p, &mut v, &mut b, &mut [], 0, GRACE, 1),
            Err(MathError::InsufficientShares)
        );
        assert_eq!((p, v, b), before, "refusals change nothing");
        assert_eq!(
            borrow(&mut p, 0, 2, 0),
            Err(MathError::InsufficientInventory)
        );
        assert_eq!(borrow(&mut p, 0, 0, 0), Err(MathError::InvalidInput));
        assert_eq!(
            borrow(&mut p, 5, 1, 5 + 93 * D + 1),
            Err(MathError::InvalidInput)
        );
        assert_eq!(borrow(&mut p, 5, 1, 5 + 93 * D), Ok(()));
    }

    #[test]
    fn records_must_be_consecutive() {
        let (mut p, mut v) = (Pool::new(), Vault::new());
        let mut a = Lender::new(&p);
        deposit(&mut p, &mut v, &mut a, &mut [], 0, 1_000, false).unwrap();
        borrow(&mut p, 0, 1_000, D).unwrap();
        let res = Resolution {
            q: 1_000,
            credited: 0,
            lump: 0,
        };
        let r0 = settle(&mut p, &mut v, 0, VEST, &res, 5, 0)
            .unwrap()
            .rec
            .unwrap();
        assert!(r0.is_full_wipe() && p.is_wipe_live());
        assert_eq!(p.wipe_swept, 0);
        let (vc, mut stale) = (v, a);
        let mut vt = v;
        assert_eq!(
            touch(&p, &mut vt, &mut stale, &mut [], 0),
            Err(MathError::LenderBehind)
        );
        assert_eq!((vt, stale), (vc, a), "a refused touch changes nothing");
        assert_eq!(convert(&p, &mut stale, &mut []), Ok(Standing::Behind));
        let later = ShareEpoch { epoch: 7, ..r0 };
        assert_eq!(
            touch(&p, &mut vt, &mut stale, &mut [later], 0),
            Err(MathError::LenderBehind)
        );
        let mut gap = [r0, ShareEpoch { epoch: 2, ..r0 }];
        assert_eq!(convert(&p, &mut stale, &mut gap[1..]), Ok(Standing::Behind));
        let dup = [ShareEpoch { epoch: 0, ..r0 }, ShareEpoch { epoch: 0, ..r0 }];
        assert_eq!(
            record_idx(&dup, 1).map(|o| o.map(|i| &dup[i])),
            Err(MathError::InvalidInput)
        );
        assert_eq!(
            touch(&p, &mut v, &mut a, &mut [r0], 0),
            Ok(Standing::Parked)
        );
        assert_eq!(a.pending, 5);
        assert_eq!(allowance(&p, &v, &a, 0), Ok(0));
    }

    #[test]
    fn wipe_gate_needs_both_sweeps_on_pump_venues() {
        // R4A-7: after a full wipe a deposit waits for the post-wipe sweeps.
        let (mut p, mut v) = (Pool::new(), Vault::new());
        let mut old = Lender::new(&p);
        deposit(&mut p, &mut v, &mut old, &mut [], 0, 1_000, false).unwrap();
        borrow(&mut p, 0, 1_000, D).unwrap();
        let res = Resolution {
            q: 1_000,
            credited: 0,
            lump: 0,
        };
        let mut rec = settle(&mut p, &mut v, 0, VEST, &res, 50, 0)
            .unwrap()
            .rec
            .unwrap();
        let mut dust = Lender::new(&p);
        for req in [false, true] {
            assert_eq!(
                deposit(&mut p, &mut v, &mut dust, &mut [rec], 1, 2, req),
                Err(MathError::WipeSweepPending)
            );
        }
        sweep(
            &mut p,
            &mut v,
            Some(&mut rec),
            1,
            VEST,
            0,
            SweepSource::Trader,
        )
        .unwrap();
        assert_eq!(p.wipe_swept, WIPE_TRADER_SWEPT);
        assert_eq!(
            deposit(&mut p, &mut v, &mut dust, &mut [rec], 1, 2, true),
            Err(MathError::WipeSweepPending)
        );
        // 1 SOL of cashback tops up the wipe record, not the next depositor.
        sweep(
            &mut p,
            &mut v,
            Some(&mut rec),
            1,
            VEST,
            1_000_000_000,
            SweepSource::VenueRewards,
        )
        .unwrap();
        assert_eq!(v.lender_sol, 1_000_000_050);
        assert_eq!(
            deposit(&mut p, &mut v, &mut dust, &mut [rec], 1, 2, true),
            Ok(2_000)
        );
        assert!(!p.is_wipe_live());
        assert_eq!(
            touch(&p, &mut v, &mut old, &mut [rec], 2),
            Ok(Standing::Current)
        );
        assert_eq!(old.pending, 1_000_000_050);
        assert_eq!(claim(&p, &mut v, &mut dust, &mut [], 2), Ok(0));
    }

    #[test]
    fn vest_releases_linearly_with_a_value_weighted_end() {
        let (mut p, mut v) = (Pool::new(), Vault::new());
        let mut h = Lender::new(&p);
        deposit(&mut p, &mut v, &mut h, &mut [], 0, 1_000, false).unwrap();
        sweep(&mut p, &mut v, None, 0, VEST, 6_000, SweepSource::Trader).unwrap();
        assert_eq!((p.vest_bal, p.vest_end), (6_000, 21_600));
        assert_eq!(release(&mut p, 3_600), Ok(1_000));
        assert_eq!(release(&mut p, 3_600), Ok(0), "idempotent at one time");
        // A dust lump leaves the end where it is (floor, AIR-02), never earlier (lump_vest_s
        // unchanged, so lump_vest_s ≥ vest_end − now).
        sweep(&mut p, &mut v, None, 3_600, VEST, 1, SweepSource::Trader).unwrap();
        assert_eq!(p.vest_end, 21_600);
        // A lump as large as the balance moves the end half way.
        let bal = p.vest_bal;
        sweep(&mut p, &mut v, None, 3_600, VEST, bal, SweepSource::Trader).unwrap();
        assert_eq!(p.vest_end, 3_600 + 19_800); // ⌊(18,000 + 21,600)/2⌋
        assert_eq!(release(&mut p, 1_000_000), Ok(2 * bal));
        assert_eq!(p.vest_bal, 0);
        assert_eq!(release(&mut p, 5), Ok(0), "no vest: vest_last follows now");
        assert_eq!(p.vest_last, 5);
    }

    /// AIR-02 (I-02): `sweep_trader` is permissionless and has no minimum, and rent is checked
    /// per transaction, so `[1 lamport to the trader PDA, sweep_trader]` repeats freely. With the
    /// end rounded up, every 1-lamport lump against a running vest moved `vest_end` 1 s later:
    /// 1,000 in one transaction moved it 1,000 s, and one a second held it `lump_vest_s` away
    /// (the release decays exponentially, 63 % after 6 h: v0.4's behaviour that R4B-5 fixed).
    /// Rounded down, a dust lump moves the end 0 s and the 6 h window holds.
    #[test]
    fn dust_lumps_do_not_push_the_vest_end() {
        let (mut p, mut v) = (Pool::new(), Vault::new());
        let mut h = Lender::new(&p);
        deposit(&mut p, &mut v, &mut h, &mut [], 0, 1_000_000, false).unwrap();
        let lump = 10_000_000_000;
        sweep(&mut p, &mut v, None, 0, VEST, lump, SweepSource::Trader).unwrap();
        assert_eq!(p.vest_end, 21_600);
        for _ in 0..1_000 {
            sweep(&mut p, &mut v, None, 3_600, VEST, 1, SweepSource::Trader).unwrap();
        }
        assert_eq!(p.vest_end, 21_600, "1,000 dust lumps in one transaction");
        // One 1-lamport sweep a second until the original end: the whole lump (and the dust)
        // is released at 21,600; only the last second's lamport starts a new vest.
        let mut t = 3_600;
        while t < 21_600 {
            t += 1;
            sweep(&mut p, &mut v, None, t, VEST, 1, SweepSource::Trader).unwrap();
        }
        assert_eq!((p.vest_bal, p.vest_end), (1, 21_600 + i64::from(VEST)));
        // A real lump still moves a running vest's end later, never earlier (lump_vest_s ≥
        // vest_end − now), at most to now + lump_vest_s.
        sweep(&mut p, &mut v, None, t, VEST, lump, SweepSource::Trader).unwrap();
        let end = p.vest_end;
        sweep(
            &mut p,
            &mut v,
            None,
            t + 3_600,
            VEST,
            lump,
            SweepSource::Trader,
        )
        .unwrap();
        assert!(p.vest_end > end && p.vest_end <= t + 3_600 + i64::from(VEST));
    }

    /// M10-8C-1: the rent an exit pays above what was credited while the position was open
    /// vests like a lump — `repay` (the `R_min` excess of an exit within `min_rent_s`) and
    /// `settle` (whose immediate part pays `R_due` first) — so a depositor who joins in the
    /// exit's second claims 0 of it, while a shortfall is still debited at once.
    #[test]
    fn rent_top_up_vests_like_a_lump() {
        assert_eq!(rent_top_up(170, 170, 30), 140);
        assert_eq!(rent_top_up(850, 200, 30), 170, "settle: R_due's part only");
        assert_eq!(
            rent_top_up(20, 200, 30),
            0,
            "principal below the credited rent"
        );
        assert_eq!(rent_top_up(170, 170, 300), 0, "a shortfall");

        let setup = || {
            let (mut p, mut v) = (Pool::new(), Vault::new());
            let mut h = Lender::new(&p);
            deposit(&mut p, &mut v, &mut h, &mut [], 0, 1_000_000, false).unwrap();
            borrow(&mut p, 0, 1_000, D).unwrap();
            accrue(&mut p, &mut v, 60, 30).unwrap();
            let mut j = Lender::new(&p);
            deposit(&mut p, &mut v, &mut j, &mut [], 60, 9_000_000, false).unwrap();
            (p, v, h, j)
        };
        // repay: rent 200 at 15 %: lender rent 170, credited 30, top-up 140 vests with the lump.
        let (mut p, mut v, mut h, mut j) = setup();
        let (acc, acc_r) = (p.acc, p.acc_r);
        let res = Resolution {
            q: 1_000,
            credited: 30,
            lump: 50,
        };
        assert_eq!(repay(&mut p, &mut v, 60, VEST, &res, 200, 1_500), Ok(170));
        assert_eq!(p.acc, acc, "nothing above the credited rent at once");
        assert!(p.acc_r > acc_r, "the credited rent is realised");
        assert_eq!((p.vest_bal, p.vest_end), (190, 60 + i64::from(VEST)));
        assert_eq!(
            claim(&p, &mut v, &mut j, &mut [], 60),
            Ok(0),
            "the JIT lender"
        );
        assert!((29..=30).contains(&claim(&p, &mut v, &mut h, &mut [], 60).unwrap()));
        // A shortfall (rent 20: lender rent 17 < 30) is debited at once; nothing vests.
        let (mut p, mut v, _, _) = setup();
        let acc = p.acc;
        repay(
            &mut p,
            &mut v,
            60,
            VEST,
            &Resolution { lump: 0, ..res },
            20,
            1_500,
        )
        .unwrap();
        assert!(p.acc < acc);
        assert_eq!(p.vest_bal, 0);

        // settle: 900 in, lump 50; the principal 850 pays R_due = 200 first: 170 above the
        // credited 30 vests with the lump, the token part 650 is immediate.
        let (mut p, mut v, mut h, mut j) = setup();
        assert_eq!(
            settle(&mut p, &mut v, 60, VEST, &res, 900, 200),
            Ok(Settled {
                rec: None,
                vested: 220,
                flushed: false
            })
        );
        assert_eq!(p.vest_bal, 220, "the vest booked is the one returned");
        assert_eq!(p.borrowed_q, 0);
        let paid_j = claim(&p, &mut v, &mut j, &mut [], 60).unwrap();
        // The JIT lender's part of the immediate token value (650·9/10), none of the rent.
        assert!(
            paid_j <= 650 * 9 / 10 && paid_j + 2 >= 650 * 9 / 10,
            "{paid_j}"
        );
        let paid_h = claim(&p, &mut v, &mut h, &mut [], 60).unwrap();
        assert!(paid_h <= 30 + 65 && paid_h + 2 >= 30 + 65, "{paid_h}");
    }

    /// M0B-A-2 (review): "never earlier" needs `lump_vest_s ≥ vest_end − now`. After a
    /// governance decrease of `lump_vest_s` (compiled bounds 3,600..604,800 s), a
    /// 1-lamport lump pulls a running vest's end three days earlier; `pool_model._vest`
    /// with `lump_vest_s` changed the same way gives the same 347,400.
    #[test]
    fn vest_end_moves_earlier_after_a_governance_decrease() {
        let mut p = Pool::new();
        p.total_shares = 1_000_000;
        p.tokens_free = 1;
        let mut v = Vault::new();
        v.lender_sol = 10;
        vest(&mut p, &mut v, None, 0, 604_800, 1).unwrap();
        assert_eq!(p.vest_end, 604_800);
        vest(&mut p, &mut v, None, 86_400, 3_600, 1).unwrap();
        // ⌈(1·518,400 + 1·3,600)/2⌉ = 261,000 s after now: day ≈ 4 instead of day 7.
        assert_eq!(p.vest_end, 86_400 + 261_000);
        assert_eq!(p.vest_bal, 2);
        // With lump_vest_s at or above the remaining span the end never moves earlier.
        vest(&mut p, &mut v, None, 86_400, 261_000, 1).unwrap();
        assert_eq!(p.vest_end, 86_400 + 261_000);
    }

    /// M0B-A-1 (review): a lender whose shares and lock floored to 0 while its lock term
    /// did not (`⌊lock·WAD/Π⌋ > lock` once Π < WAD) reads the record of its epoch and scales
    /// the term before the V2-12 jump, as `pool_model._touch` (`lock_term·num//den`); a
    /// lender without a term still jumps without records.
    #[test]
    fn empty_lender_with_a_term_scales_it() {
        let mut p = Pool::new();
        p.share_epoch = 1;
        p.total_shares = 1_000_000;
        p.tokens_free = 1_000;
        let wipe = ShareEpoch {
            acc_end: 0,
            acc_r_end: 0,
            s_old: 2_000,
            carry: 0,
            carry_r: 0,
            conv_rem: 0,
            epoch: 0,
            num: 0,
            den_log1000: 0,
            _pad: [0; 10],
        };
        let mut v = Vault::new();
        let mut l = Lender::new(&p);
        l.epoch = 0;
        l.lock_term = 1;
        let before = (v, l);
        assert_eq!(
            touch(&p, &mut v, &mut l, &mut [], 0),
            Err(MathError::LenderBehind)
        );
        assert_eq!((v, l), before, "a refused touch changes nothing");
        assert_eq!(
            touch(&p, &mut v, &mut l, &mut [wipe], 0),
            Ok(Standing::Current)
        );
        // pool_model: lock_term = 1·0 // 1 = 0 (the crate kept 1 before the fix).
        assert_eq!((l.epoch, l.lock_term), (1, 0));
        // Two 1/1000 rebases: the first record scales the term, then the lender jumps.
        p.share_epoch = 2;
        let rebase = ShareEpoch {
            num: 1,
            den_log1000: 1,
            ..wipe
        };
        let mut l = Lender::new(&p);
        l.epoch = 0;
        l.lock_term = 123_456;
        assert_eq!(convert(&p, &mut l, &mut []), Ok(Standing::Behind));
        assert_eq!(convert(&p, &mut l, &mut [rebase]), Ok(Standing::Current));
        assert_eq!((l.epoch, l.lock_term, l.snap), (2, 123, p.acc));
        // Without a term the lender jumps without records (V2-12).
        let mut l = Lender::new(&p);
        l.epoch = 0;
        assert_eq!(convert(&p, &mut l, &mut []), Ok(Standing::Current));
        assert_eq!((l.epoch, l.lock_term), (2, 0));
    }

    #[test]
    fn credits_without_shares_go_to_the_protocol() {
        // R3A-10: every lender left (no wipe), then a sweep arrives.
        let (mut p, mut v) = (Pool::new(), Vault::new());
        let mut a = Lender::new(&p);
        let s = deposit(&mut p, &mut v, &mut a, &mut [], 0, 1_000_000_000_000, false).unwrap();
        withdraw(&mut p, &mut v, &mut a, &mut [], 0, GRACE, s).unwrap();
        sweep(
            &mut p,
            &mut v,
            None,
            0,
            VEST,
            1_000_000_000,
            SweepSource::Trader,
        )
        .unwrap();
        assert_eq!((v.lender_sol, v.protocol_fees), (0, 1_000_000_000));
        let mut d = Lender::new(&p);
        deposit(&mut p, &mut v, &mut d, &mut [], 0, 1_000_000, false).unwrap();
        assert_eq!(claim(&p, &mut v, &mut d, &mut [], 30_000), Ok(0));
        // A debit while S = 0 takes back from the protocol (signed, as the model).
        let (mut q, mut w) = (Pool::new(), Vault::new());
        assert_eq!(
            credit(&mut q, &mut w, None, -5, false),
            Err(MathError::Underflow)
        );
        w.protocol_fees = 7;
        credit(&mut q, &mut w, None, -5, false).unwrap();
        assert_eq!((w.protocol_fees, w.lender_sol), (2, 5));
    }

    fn rebase(epoch: u32, k: u8, s_old: u128) -> ShareEpoch {
        ShareEpoch {
            s_old,
            epoch,
            num: 1,
            den_log1000: k,
            ..rec0()
        }
    }

    /// A lender holding `shares` shares of an earlier `epoch`, not converted yet.
    fn behind(p: &Pool, epoch: u32, shares: u128) -> Lender {
        Lender {
            shares,
            snap: 0,
            snap_r: 0,
            epoch,
            ..Lender::new(p)
        }
    }

    /// KI-66 full: `lag_part` is the difference of the two floors, on edge and random values.
    #[test]
    fn lag_part_is_the_difference_of_two_floors() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let edge_q = [
            0u128,
            1,
            999,
            WAD - 1,
            WAD,
            WAD + 1,
            u128::MAX / 4,
            3 * 10u128.pow(38),
        ];
        let edge_x = [
            0i128,
            1,
            -1,
            10i128.pow(18),
            -(10i128.pow(18)),
            i128::MAX / 3,
            i128::MIN / 3,
        ];
        let mut cases = std::vec![(0u128, 0i128); 20_056];
        let mut n = 0;
        for &q in &edge_q {
            for &a in &edge_x {
                cases[n] = (q, a);
                n += 1;
            }
        }
        for c in cases.iter_mut().skip(n) {
            let q = (u128::from(next()) << (next() % 64)) >> (next() % 40);
            let a = (i128::from(next() as i64) << (next() % 60)) >> (next() % 30);
            *c = (q, a);
        }
        for (q, a) in cases {
            for lag in 0u8..=2 {
                let (Ok(hi), Ok(lo)) = (accrued(q + u128::from(lag), a), accrued(q, a)) else {
                    continue;
                };
                assert_eq!(
                    lag_part(q, lag, a).unwrap(),
                    hi - lo,
                    "q {q} lag {lag} x {a}"
                );
            }
        }
    }

    /// KI-66 full, the lag lemma: an owner's shares stay at most one above its forward chain's
    /// floor through any number of records, so `fwd_lag ∈ {0, 1}` (`u8`, `Invariant` above 1).
    /// Exhaustive over `s < 3·10⁶` through two chained `1/1,000` records with conv_rem samples
    /// (the extremes and the middle), then through the real path on a sample: the pot-only touch
    /// holds the lender and walks its chain to the current epoch, the owner converts one record
    /// at a time (`FWD_OPEN` with the lag the lemma gives, `epoch < fwd_epoch` then `epoch ==
    /// fwd_epoch`) and touches; it ends bit-equal to the owner converting alone (`pending`,
    /// shares, records) and `real + unmatched` equal.
    #[test]
    fn fwd_lag_at_most_one() {
        let rems = [0u128, 1, 500, 998, 999];
        for s in 0..3_000_000u128 {
            let q1 = s / 1_000;
            let q2 = q1 / 1_000;
            for &c1 in &rems {
                let (s1, _) = convert_shares(s, c1, 1, 1_000).unwrap();
                assert!(s1 >= q1 && s1 - q1 <= 1, "s {s} c1 {c1}");
                for &c2 in &[0u128, 999] {
                    let (s2, _) = convert_shares(s1, c2, 1, 1_000).unwrap();
                    assert!(s2 >= q2 && s2 - q2 <= 1, "s {s} c1 {c1} c2 {c2}");
                }
            }
        }
        let mut p = Pool::new();
        p.share_epoch = 2;
        p.total_shares = 3_000;
        p.tokens_free = 10;
        p.acc = 7_777_777_777_777_777_777;
        p.acc_r = 6_666_666_666_666_666_666;
        let recs0 = |c1: u128, c2: u128| {
            let mut r = [rebase(0, 1, 3_000_000), rebase(1, 1, 3_000)];
            (r[0].acc_end, r[0].acc_r_end, r[0].conv_rem) =
                (-3_333_333_333_333_333_337, 5_555_555_555_555_555_557, c1);
            (r[1].acc_end, r[1].acc_r_end, r[1].conv_rem) =
                (9_999_999_999_999_999_991, 4_444_444_444_444_444_443, c2);
            r
        };
        let mut lags = [0u32; 2];
        for s in (1..3_000_000u128)
            .step_by(997)
            .chain([999, 1_000, 999_999, 1_000_000, 1_999_999])
        {
            for &(c1, c2) in &[(0, 0), (999, 0), (999, 999), (1, 998), (500, 500)] {
                let mut l0 = behind(&p, 0, s);
                l0.pending = 1_000;
                l0.real = 10_000;
                // The owner converting alone.
                let (mut ra, mut va, mut la) = (recs0(c1, c2), Vault::new(), l0);
                touch(&p, &mut va, &mut la, &mut ra, 0).unwrap();
                // A third party's pot-only touch, then the owner one record at a time.
                let (mut rb, mut vb, mut lb) = (recs0(c1, c2), Vault::new(), l0);
                assert_eq!(
                    touch_pot(&p, &mut vb, &mut lb, &rb, 0),
                    Ok(Standing::Behind)
                );
                assert_eq!((lb.fwd, lb.fwd_epoch, lb.epoch), (FWD_HOLD, 2, 0), "s {s}");
                assert_eq!(rb, recs0(c1, c2), "the pot-only touch writes no record");
                let st = convert(&p, &mut lb, &mut rb[..1]);
                let open = |l: &Lender| (l.fwd, l.epoch, l.fwd_lag);
                if lb.shares == 0 && lb.lock == 0 {
                    assert_eq!(st, Ok(Standing::Current));
                    assert_eq!(
                        open(&lb),
                        (FWD_NONE, 2, 0),
                        "the V2-12 jump drops the chain"
                    );
                } else {
                    assert_eq!(st, Ok(Standing::Behind));
                    assert_eq!(
                        open(&lb),
                        (FWD_OPEN, 1, u8::try_from(lb.shares - s / 1_000).unwrap())
                    );
                    lags[0] += u32::from(lb.fwd_lag);
                    assert_eq!(convert(&p, &mut lb, &mut rb[1..]), Ok(Standing::Current));
                    if lb.fwd != FWD_NONE {
                        assert_eq!(
                            open(&lb),
                            (
                                FWD_OPEN,
                                2,
                                u8::try_from(lb.shares - lb.fwd_shares).unwrap()
                            )
                        );
                        lags[1] += u32::from(lb.fwd_lag);
                    }
                }
                touch(&p, &mut vb, &mut lb, &mut rb, 0).unwrap();
                assert_eq!(lb.fwd, FWD_NONE);
                assert_eq!(
                    (la.shares, la.epoch, la.pending, la.snap, la.snap_r),
                    (lb.shares, lb.epoch, lb.pending, lb.snap, lb.snap_r),
                    "s {s} c {c1} {c2}"
                );
                assert_eq!(ra, rb, "s {s}");
                assert_eq!(
                    u128::from(la.real) + u128::from(va.unmatched),
                    u128::from(lb.real) + u128::from(vb.unmatched),
                    "s {s}"
                );
            }
        }
        assert!(lags[0] > 1_000 && lags[1] > 1_000, "{lags:?}");
    }

    /// `convert_shares` through `rec`, as the conversion calls it.
    fn through(shares: u128, rec: &ShareEpoch) -> Result<(u128, u128), MathError> {
        let (num, den) = rec.ratio()?;
        convert_shares(shares, rec.conv_rem, num, den)
    }

    /// KI-65: `⌊(conv_rem + s)/den⌋` and the remainder, at least the floor and at most one
    /// more; a full wipe leaves `conv_rem` alone; a malformed remainder is refused; the
    /// widest denominator (1,000^12) cannot overflow.
    #[test]
    fn convert_shares_rounds_cumulatively() {
        let r = |rem: u128| ShareEpoch {
            conv_rem: rem,
            ..rebase(0, 1, 10_000)
        };
        assert_eq!(through(1_999, &r(0)), Ok((1, 999)), "floor");
        assert_eq!(through(1, &r(999)), Ok((1, 0)), "the unit, wrapping to 0");
        assert_eq!(through(1_001, &r(999)), Ok((2, 0)));
        assert_eq!(through(600, &r(600)), Ok((1, 200)), "dust lands the unit");
        assert_eq!(through(600, &r(0)), Ok((0, 600)), "dust below the unit");
        assert_eq!(through(0, &r(5)), Ok((0, 5)));
        assert_eq!(through(5, &r(999)), Ok((1, 4)));
        assert_eq!(through(5, &r(1_000)), Err(MathError::InvalidInput));
        assert_eq!(through(5, &r(u128::MAX)), Err(MathError::InvalidInput));
        let wipe = ShareEpoch {
            conv_rem: 7,
            ..rec0()
        };
        assert_eq!(through(1_234, &wipe), Ok((0, 7)), "a wipe keeps conv_rem");
        let den = 10u128.pow(36);
        let deep = ShareEpoch {
            conv_rem: den - 1,
            ..rebase(0, 12, u128::MAX)
        };
        let q = u128::MAX / den;
        let r0 = u128::MAX % den;
        assert_eq!(through(u128::MAX, &deep), Ok((q + 1, r0 - 1)));
        assert_eq!(
            through(
                den - 1,
                &ShareEpoch {
                    conv_rem: 0,
                    ..deep
                }
            ),
            Ok((0, den - 1))
        );
    }

    /// KI-65: whatever the conversion order, the old epoch's holders end with exactly the
    /// new `S`; the order moves only the record's rounding unit, never below a floor.
    #[test]
    fn conversion_order_moves_only_the_rounding_unit() {
        let (a, b) = (600u128, 7_199_600u128);
        let mut p = Pool::new();
        p.share_epoch = 1;
        p.total_shares = (a + b) / 1_000;
        let rec = rebase(0, 1, a + b);
        for (first, second, got_a, got_b) in [(a, b, 0, 7_200), (b, a, 1, 7_199)] {
            let mut recs = [rec];
            let mut v = Vault::new();
            let (mut x, mut y) = (behind(&p, 0, first), behind(&p, 0, second));
            touch(&p, &mut v, &mut x, &mut recs, 0).unwrap();
            touch(&p, &mut v, &mut y, &mut recs, 0).unwrap();
            assert_eq!(x.shares + y.shares, p.total_shares);
            let (sa, sb) = if first == a {
                (x.shares, y.shares)
            } else {
                (y.shares, x.shares)
            };
            assert_eq!((sa, sb), (got_a, got_b));
            assert!(sa >= a / 1_000 && sb >= b / 1_000);
            assert_eq!(recs[0].conv_rem, (a + b) % 1_000);
        }
        // A view computes the conversion without writing it.
        let recs = [rec];
        let l = behind(&p, 0, a);
        allowance_view(&p, &Vault::new(), &l, &recs, 0).unwrap();
        assert_eq!(recs, [rec]);
    }

    /// KI-65: a refused touch, deposit or withdrawal leaves the records byte-identical; a
    /// partial conversion (`Behind`) writes only the records it passed; a current lender
    /// writes none.
    #[test]
    fn refusals_write_no_record_and_partial_conversions_only_theirs() {
        let mut p = Pool::new();
        p.share_epoch = 2;
        p.total_shares = 7;
        p.tokens_free = 10;
        let recs0 = [rebase(0, 1, 7_654), rebase(1, 1, 7_654)];
        let mut v = Vault::new();
        let l0 = behind(&p, 0, 7_654);
        // Missing the second record: touch refuses and writes nothing.
        let (mut recs, mut l) = (recs0, l0);
        assert_eq!(
            touch(&p, &mut v, &mut l, &mut recs[..1], 0),
            Err(MathError::LenderBehind)
        );
        assert_eq!((recs, l), (recs0, l0));
        assert_eq!(
            deposit(&mut p.clone(), &mut v, &mut l, &mut recs, 0, 0, false),
            Err(MathError::DepositTooSmall)
        );
        assert_eq!((recs, l), (recs0, l0));
        assert_eq!(
            withdraw(&mut p.clone(), &mut v, &mut l, &mut recs, 0, GRACE, 1_000),
            Err(MathError::InsufficientShares)
        );
        assert_eq!((recs, l), (recs0, l0));
        // sync_lender(1): the first record only.
        assert_eq!(convert(&p, &mut l, &mut recs[..1]), Ok(Standing::Behind));
        assert_eq!((l.epoch, l.shares), (1, 7));
        assert_eq!((recs[0].conv_rem, recs[1].conv_rem), (654, 0));
        assert_eq!(convert(&p, &mut l, &mut recs[1..]), Ok(Standing::Current));
        assert_eq!((l.epoch, l.shares), (2, 0));
        assert_eq!(
            recs[1].conv_rem, 7,
            "the dust lender advances conv_rem and jumps"
        );
        // Current: claim writes nothing.
        let before = recs;
        claim(&p, &mut v, &mut l, &mut recs, 0).unwrap();
        assert_eq!(recs, before);
    }

    /// The commit pass must stop where the read-only pass stopped (unreachable otherwise).
    #[test]
    fn commit_pass_must_match_the_read_only_pass() {
        let mut p = Pool::new();
        p.share_epoch = 1;
        p.total_shares = 2;
        let mut recs = [rebase(0, 1, 2_500)];
        let l = behind(&p, 0, 2_500);
        assert_eq!(
            commit_rems(&p, &l, &mut recs, Some((1, 3))),
            Err(MathError::Invariant)
        );
        let mut recs = [rebase(0, 1, 2_500)];
        assert_eq!(commit_rems(&p, &l, &mut recs, Some((1, 2))), Ok(()));
        assert_eq!(recs[0].conv_rem, 500);
    }
}
