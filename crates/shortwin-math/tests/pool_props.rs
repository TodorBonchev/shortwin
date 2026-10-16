//! Property tests for `shortwin_math::pool` (v0.4.2): random operation sequences (dust to
//! u64-extreme amounts, time passing, `rent_protocol_bps` governance changes, full wipes
//! with post-wipe sweeps, deep rebases, runs, churn, repeated claims, covers, lumps,
//! shortfalls; lazy or touch-all lenders; lump and lock parameters at their compiled
//! extremes) with the ECON §13 invariants I-P1–I-P7 / ARCH §8.3 I8 asserted after every
//! operation — I-P4 also on the untouched state — plus the calendar's own invariants, the
//! per-operation ghost checks of `pool_model.fuzz`, atomicity of refusals and the claim
//! side of the liveness wind-down (I-P6). KI-65: lazy conversions through chained rebase
//! records in any order and in `sync_lender` steps of any size never orphan a share.

mod common;
#[path = "pool_harness.rs"]
mod pool_harness;

use pool_harness::{DAY, Market, Model, S3_TAIL_S};
use proptest::prelude::*;
use shortwin_math::num::{WAD, mul_div, mul_div_i128};
use shortwin_math::pool::{self, Lender, NO_LOCK_EPOCH, Pool, ShareEpoch, Standing, SweepSource};
use shortwin_math::{MathError, Rounding};

#[derive(Clone, Debug)]
enum Op {
    Deposit(usize, u64, bool),
    /// user, how (0 random, 1 at allowance, 2 all shares, 3 one share), seed
    Withdraw(usize, u8, u64),
    /// C0-9: withdraw at the allowance in a loop
    Run(usize, u8),
    /// amount seed, duration index
    Borrow(u64, u8),
    /// seconds, index growth
    Accrue(u16, u128),
    /// time passes: index into ADVANCES
    Advance(u8),
    /// governance: index into GOV_BPS
    Gov(u8),
    /// position seed, rent paid in full, shortfall seed, lender lump
    Repay(u64, bool, u64, u64),
    /// position seed, SOL in, lump seed
    Settle(u64, u64, u64),
    /// borrow what the cap allows, settle everything
    Wipe(u64),
    /// leave 1..=3 raw units of claims: a deep rebase
    Deep(u64, u64),
    /// V2-2: claim, then claim again `n` times
    Claim(usize, u8),
    Cover(usize, u64),
    Sweep(u64, bool),
    /// V2-3: another position opens and closes
    Churn(u64),
    Touch(usize),
    /// KI-66: a third party's `sync_lender` (the pot-only touch), of a lender behind a rebase
    /// record if there is one
    TouchPot(usize),
    /// KI-66 full: `sync_lender(n)` with `n` records, signed by the owner (a partial conversion
    /// that may stop inside a forward chain) or a third party's (the chain walked `n` records),
    /// of a lender with a chain if there is one
    Sync(usize, u8, bool),
    /// R4A-2 / R4B-3: the allowances as the program sees them (each lender touched alone)
    View,
}

const ADVANCES: [i64; 7] = [60, 3_600, 6 * 3_600, DAY, 2 * DAY, 9 * DAY, 31 * DAY];
const DURATIONS: [i64; 5] = [3_600, DAY, 7 * DAY, 30 * DAY, 90 * DAY];
const GOV_BPS: [u32; 5] = [0, 1_000, 1_500, 3_000, 5_000];
/// `(lump_vest_s, lock_grace_s)`: defaults and compiled extremes (ECON §15).
const PARAMS: [(u32, u32); 3] = [(21_600, 86_400), (3_600, 3_600), (604_800, 604_800)];

#[derive(Clone, Debug)]
struct Cfg {
    cap_bps: u64,
    touch_all: bool,
    params: (u32, u32),
    /// S3 tail of every position: default or compiled maximum (3 d).
    tail: i64,
}

fn tokens() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => 1u64..40,
        4 => 1u64..10_000_000_000_000,
        2 => (1u64 << 60)..=u64::MAX,
        1 => Just(u64::MAX),
    ]
}

fn lamports() -> impl Strategy<Value = u64> {
    prop_oneof![2 => 0u64..12, 3 => 0u64..1_000_000_000_000, 1 => 0u64..100_000_000_000_000]
}

fn op(users: usize) -> impl Strategy<Value = Op> {
    let u = 0..users;
    prop_oneof![
        4 => (u.clone(), tokens(), any::<bool>()).prop_map(|(u, a, r)| Op::Deposit(u, a, r)),
        4 => (u.clone(), 0u8..4, any::<u64>()).prop_map(|(u, h, s)| Op::Withdraw(u, h, s)),
        2 => (u.clone(), 1u8..25).prop_map(|(u, n)| Op::Run(u, n)),
        3 => (any::<u64>(), 0u8..5).prop_map(|(s, d)| Op::Borrow(s, d)),
        2 => (0u16..600, prop_oneof![0u128..1_000_000_000_000_000, 0u128..10_000_000_000_000_000_000_000]).prop_map(|(t, d)| Op::Accrue(t, d)),
        2 => (0u8..7).prop_map(Op::Advance),
        1 => (0u8..5).prop_map(Op::Gov),
        3 => (any::<u64>(), prop::bool::weighted(0.8), any::<u64>(), lamports()).prop_map(|(k, f, s, l)| Op::Repay(k, f, s, l)),
        3 => (any::<u64>(), lamports(), any::<u64>()).prop_map(|(k, s, l)| Op::Settle(k, s, l)),
        1 => lamports().prop_map(Op::Wipe),
        1 => (1u64..4, lamports()).prop_map(|(r, s)| Op::Deep(r, s)),
        3 => (u.clone(), 0u8..6).prop_map(|(u, n)| Op::Claim(u, n)),
        1 => (u.clone(), lamports()).prop_map(|(u, x)| Op::Cover(u, x)),
        2 => (lamports(), any::<bool>()).prop_map(|(x, r)| Op::Sweep(x, r)),
        2 => any::<u64>().prop_map(Op::Churn),
        1 => u.clone().prop_map(Op::Touch),
        2 => u.clone().prop_map(Op::TouchPot),
        2 => (u, 1u8..5, any::<bool>()).prop_map(|(u, n, s)| Op::Sync(u, n, s)),
        1 => Just(Op::View),
    ]
}

fn scenario() -> impl Strategy<Value = (Cfg, Vec<Op>)> {
    (
        prop::bool::weighted(0.6),
        any::<bool>(),
        0usize..3,
        any::<bool>(),
        2usize..8,
    )
        .prop_flat_map(|(full, touch_all, params, tail_max, users)| {
            let cfg = Cfg {
                cap_bps: if full { 10_000 } else { 9_000 },
                touch_all,
                params: PARAMS[params],
                tail: if tail_max { 3 * DAY } else { S3_TAIL_S },
            };
            (Just(cfg), prop::collection::vec(op(users), 1..120))
        })
}

struct Driver {
    cfg: Cfg,
    s: Model,
}

/// KI-66: `after` is `before` with `u` touched by `pool::touch_pot` (standing `st`). It wrote no
/// record; a lender left behind (with every record at hand: held at a rebase record) kept its
/// shares, lock, lock term and epoch, accrued `pending` and `real` once to the record's ends and
/// moved exactly R3A-2's surplus of that state to the pot; a second pot-only touch changes
/// nothing; and the owner's touch afterwards ends where the owner's touch alone ends — the lender
/// (`real` aside), the records, `pending` — with the same `real + unmatched` (the pot-only touch
/// counts nothing twice). The touch after it may refuse only where the touch alone does, or with `Overflow`
/// (its pot is the fuller one).
fn pot_only_touch_is_sound(
    before: &Market,
    after: &Market,
    u: usize,
    st: Standing,
) -> Result<(), TestCaseError> {
    prop_assert_eq!(
        &after.records,
        &before.records,
        "a pot-only touch writes no record"
    );
    let a = *after.lender(u).expect("lender");
    let held = |b: &Lender| {
        b.fwd != pool::FWD_NONE
            || (b.epoch < before.pool.share_epoch
                && before.records[b.epoch as usize].num == 1
                && (b.shares != 0 || b.lock != 0 || b.lock_term != 0))
    };
    if let (Standing::Behind, Some(b)) = (st, before.lender(u).filter(|b| held(b))) {
        prop_assert_eq!(
            (a.shares, a.lock, a.lock_term, a.epoch, a.lock_until),
            (b.shares, b.lock, b.lock_term, b.epoch, b.lock_until)
        );
        // KI-66 full, recomputed independently: the hold to the record's ends on the shares,
        // then the floor chain through every later record (cumulative floor), parking on a
        // live wipe record, to the current `acc`; exactly R3A-2's surplus to the pot.
        let fl = |x: i128, q: u128| mul_div_i128(x, q, WAD, Rounding::Floor).expect("accrual");
        let flr = |x: u128, q: u128| {
            i128::try_from(mul_div(x, q, WAD, Rounding::Floor).expect("accrual")).expect("i128")
        };
        let (mut pending, mut real) = (i128::from(b.pending), i128::from(b.real));
        let (mut j, mut q, mut sn, mut snr) = (b.fwd_epoch, b.fwd_shares, b.snap, b.snap_r);
        if b.fwd == pool::FWD_NONE {
            let r = before.records[b.epoch as usize];
            pending += fl(r.acc_end - b.snap, b.shares);
            let d_r = if r.acc_r_end >= b.snap_r {
                i128::try_from(r.acc_r_end - b.snap_r)
            } else {
                i128::try_from(b.snap_r - r.acc_r_end).map(|d| -d)
            };
            real += fl(d_r.expect("Δacc_r"), b.shares);
            (j, q, sn, snr) = (b.epoch + 1, b.shares / r.den().expect("den"), 0, 0);
        }
        let p = &before.pool;
        loop {
            if q == 0 {
                (j, sn, snr) = (p.share_epoch, p.acc, p.acc_r);
                break;
            }
            let (end, end_r) = if j == p.share_epoch {
                (p.acc, p.acc_r)
            } else {
                let r = before.records[j as usize];
                (r.acc_end, r.acc_r_end)
            };
            pending += fl(end, q) - fl(sn, q);
            real += flr(end_r, q) - flr(snr, q);
            (sn, snr) = (end, end_r);
            if j == p.share_epoch || (p.is_wipe_live() && j + 1 == p.share_epoch) {
                break;
            }
            let r = before.records[j as usize];
            q = if r.num == 0 {
                0
            } else {
                q / r.den().expect("den")
            };
            (j, sn, snr) = (j + 1, 0, 0);
        }
        prop_assert_eq!(
            (a.fwd_epoch, a.fwd_shares, a.snap, a.snap_r),
            (j, q, sn, snr)
        );
        prop_assert!(a.fwd == pool::FWD_HOLD || a.fwd == b.fwd);
        let surplus = (real - i128::from(b.claimed) - pending.max(0)).max(0);
        prop_assert_eq!(i128::from(a.pending), pending, "pending: hold + chain");
        prop_assert_eq!(
            i128::from(after.vault.unmatched) - i128::from(before.vault.unmatched),
            surplus,
            "the surplus accrued through the chain moved to the pot"
        );
        prop_assert_eq!(i128::from(a.real), real - surplus);
        prop_assert!(
            i128::from(a.real) - i128::from(a.claimed) <= i128::from(a.pending).max(0),
            "no surplus left with the held lender"
        );
    }
    let mut again = after.clone();
    again.touch_pot(u).expect("a second pot-only touch");
    prop_assert_eq!(&again, after, "idempotent");
    let (mut alone, mut later) = (before.clone(), after.clone());
    match (alone.touch(u), later.touch(u)) {
        (Ok(_), Ok(_)) => {
            let (x, y) = (*alone.lender(u).expect("x"), *later.lender(u).expect("y"));
            prop_assert_eq!(Lender { real: 0, ..x }, Lender { real: 0, ..y });
            prop_assert_eq!(&alone.records, &later.records);
            prop_assert_eq!(
                u128::from(x.real) + u128::from(alone.vault.unmatched),
                u128::from(y.real) + u128::from(later.vault.unmatched)
            );
            prop_assert!(later.vault.unmatched >= alone.vault.unmatched);
        }
        (Err(_), Err(_)) | (Ok(_), Err(MathError::Overflow)) => {}
        (x, y) => prop_assert!(false, "touch alone {x:?}, after the pot-only touch {y:?}"),
    }
    Ok(())
}

/// Refusals are pool rules (their own variants) or on-chain field limits.
fn refusal(e: MathError) -> Result<(), TestCaseError> {
    prop_assert!(
        matches!(
            e,
            MathError::DepositTooSmall
                | MathError::WipeSweepPending
                | MathError::PoolDrained
                | MathError::NegativePending
                | MathError::InsufficientShares
                | MathError::InsufficientInventory
                | MathError::AboveAllowance
                | MathError::Overflow
                | MathError::Underflow
        ),
        "unexpected error {e:?}"
    );
    Ok(())
}

impl Driver {
    fn m(&self) -> &Market {
        &self.s.m
    }

    /// `pool_model.Pool.borrow`: above the utilisation cap the open is refused after the
    /// vest release (`None`); otherwise `pool::borrow`, whose own refusals (field limits)
    /// change nothing.
    fn borrow(&mut self, q: u64, duration: i64) -> Result<Option<u64>, TestCaseError> {
        let p = self.m().pool;
        let (t, b) = (u128::from(p.tokens_free), u128::from(p.borrowed_q));
        if q == 0
            || q > p.tokens_free
            || (b + u128::from(q)) * 10_000 > u128::from(self.cfg.cap_bps) * (t + b)
        {
            self.s
                .m
                .release()
                .map_err(|e| TestCaseError::fail(format!("release: {e}")))?;
            return Ok(None);
        }
        let before = self.s.m.clone();
        let deadline = self.m().now + duration;
        match self.s.m.borrow(q, deadline + self.cfg.tail) {
            Ok(()) => Ok(Some(self.s.rent.open(q, deadline))),
            Err(e) => {
                refusal(e)?;
                prop_assert_eq!(&self.s.m, &before);
                Ok(None)
            }
        }
    }

    /// `paid`: `None` pays the accrued rent; an even seed a shortfall below it, an odd seed
    /// the accrued rent plus an excess of up to 1 SOL (an exit within `min_rent_s` pays
    /// `R_min` above it; its lender part above the credited rent vests, M10-8C-1).
    fn repay(&mut self, id: u64, paid: Option<u64>, lump: u64) -> Result<(), TestCaseError> {
        let owed = self.s.owed(id);
        let paid = paid.map_or(owed, |s| {
            if s % 2 == 0 {
                s % (owed + 1)
            } else {
                owed.saturating_add((s >> 1) % 1_000_000_000)
            }
        });
        let before = self.s.m.clone();
        if let Err(e) = self.s.repay(id, paid, lump) {
            refusal(e)?;
            prop_assert_eq!(&self.s.m, &before);
        }
        Ok(())
    }

    fn settle(&mut self, id: u64, sol: u64, lump: u64) -> Result<(), TestCaseError> {
        self.settle_above(id, sol, lump, 0)
    }

    /// A settlement whose `R_due` is `excess` above the accrued rent (M10-8C-1).
    fn settle_above(
        &mut self,
        id: u64,
        sol: u64,
        lump: u64,
        excess: u64,
    ) -> Result<(), TestCaseError> {
        let before = self.s.m.clone();
        if let Err(e) = self.s.settle_above(id, sol, lump, excess) {
            refusal(e)?;
            prop_assert_eq!(&self.s.m, &before);
        }
        Ok(())
    }

    fn pick(&self, seed: u64) -> Option<u64> {
        let n = self.s.rent.positions.len() as u64;
        (n > 0).then(|| self.s.rent.positions[(seed % n) as usize].id)
    }

    /// The market after `user`'s release + touch (the committed first step of `d` / `w`).
    fn touched(&self, user: usize) -> Market {
        let mut t = self.s.m.clone();
        let _ = t.pre(user);
        t
    }

    /// Withdraws `shares` with the ghost checks of `pool_model.fuzz`: I-P2 (no share
    /// reduction with pending < 0) and I-P4 (at most the allowance just computed).
    fn withdraw(
        &mut self,
        user: usize,
        shares: u128,
        allow: u64,
    ) -> Result<Option<u64>, TestCaseError> {
        let pending = self.m().lender(user).map_or(0, |l| l.pending);
        let before = self.touched(user);
        match self.s.withdraw(user, shares) {
            Ok(got) => {
                prop_assert!(pending >= 0, "withdrew with pending {pending}");
                prop_assert!(got <= allow, "withdrew {got} above allowance {allow}");
                Ok(Some(got))
            }
            Err(e) => {
                refusal(e)?;
                prop_assert_eq!(&self.s.m, &before);
                Ok(None)
            }
        }
    }

    fn step(&mut self, op: &Op) -> Result<(), TestCaseError> {
        match *op {
            Op::Deposit(u, a, rewards) => {
                if self.m().pool.total_shares == 0 && self.m().pool.wipe_swept == 0 && a % 3 != 0 {
                    // The depositor bundles the sweeps a deposit into a pool without shares
                    // waits for (after a full wipe, R4A-7; after the last exit, AIR-08).
                    self.s
                        .m
                        .sweep_from(a % 1_000, SweepSource::Trader)
                        .expect("sweep");
                    if rewards {
                        self.s
                            .m
                            .sweep_from(0, SweepSource::VenueRewards)
                            .expect("claim");
                    }
                }
                let before = self.touched(u);
                if let Err(e) = self.s.m.deposit_with(u, a, rewards) {
                    refusal(e)?;
                    prop_assert_eq!(&self.s.m, &before);
                }
            }
            Op::Withdraw(u, how, seed) => {
                if self.m().lender(u).is_none() {
                    return Ok(());
                }
                let allow = match self.s.free_allowance(u) {
                    Ok(a) => a,
                    Err(e) => return refusal(e),
                };
                let shares = self.s.shares(u);
                if shares == 0 {
                    return Ok(());
                }
                let s = match how {
                    0 => 1 + u128::from(seed) % shares,
                    1 => self.s.shares_at(allow).max(1).min(shares),
                    2 => shares,
                    _ => 1,
                };
                self.withdraw(u, s, allow)?;
            }
            Op::Run(u, n) => {
                // C0-9: a loop of withdrawals at the allowance takes at most the first
                // allowance, plus one unit of rounding per withdrawal.
                if self.m().lender(u).is_none() {
                    return Ok(());
                }
                let first = self.s.free_allowance(u).unwrap_or(0);
                let (mut took, mut k) = (0u128, 0u128);
                for _ in 0..n {
                    let allow = self.s.free_allowance(u).unwrap_or(0);
                    let shares = self.s.shares(u);
                    if allow == 0 || shares == 0 {
                        break;
                    }
                    match self.withdraw(u, self.s.shares_at(allow).max(1).min(shares), allow)? {
                        Some(got) => {
                            took += u128::from(got);
                            k += 1;
                        }
                        None => break,
                    }
                }
                prop_assert!(
                    took <= u128::from(first) + k,
                    "run took {took} > first allowance {first} + {k}"
                );
            }
            Op::Borrow(seed, d) => {
                let t = self.m().pool.tokens_free;
                if t > 0 {
                    self.borrow(1 + seed % t, DURATIONS[usize::from(d)])?;
                }
            }
            Op::Accrue(dt, d) => {
                // ARCH §9 M0b note (4): rent credited while S = 0 and ΣB_Q > 0 (after a dust
                // exit) reaches the protocol sink before any SOL arrived and breaks I-P2's
                // claim side in pool_model too (vectors `ext_scripted_no_shares`, where the
                // model's check() fails). The property lets only time pass there.
                if self.m().pool.total_shares == 0 && self.m().pool.borrowed_q > 0 {
                    self.s.advance(i64::from(dt), 0);
                    return Ok(());
                }
                // SOL amounts stay within the SOL supply: an accrual adds at most 10¹⁴
                // lamports of rent across the open positions (i64 `pending` holds them).
                let a = self.s.rent.a.max(1);
                let d = d.min(100_000_000_000_000 * shortwin_math::num::WAD * 10_000 / a);
                let before = self.s.clone();
                let c = self.s.rent.accrue(d);
                if let Err(e) = self.s.m.accrue(i64::from(dt), c) {
                    // Only a credit while S = 0 (ARCH §9 M0b note 4) can be refused.
                    refusal(e)?;
                    prop_assert_eq!(before.m.pool.total_shares, 0);
                    self.s = before;
                }
            }
            Op::Advance(i) => self.s.advance(ADVANCES[usize::from(i)], 0),
            Op::Gov(i) => self.s.rent.protocol_bps = GOV_BPS[usize::from(i)],
            Op::Repay(k, full, seed, lump) => {
                if let Some(id) = self.pick(k) {
                    self.repay(id, (!full).then_some(seed), lump)?;
                }
            }
            Op::Settle(k, sol, lump) => {
                if let Some(id) = self.pick(k) {
                    // One in four settlements pays R_due above the accrued rent (M10-8C-1).
                    let excess = if lump % 4 == 1 {
                        (lump >> 2) % 1_000_000_000
                    } else {
                        0
                    };
                    let lump = if lump % 3 == 0 { lump % (sol + 1) } else { 0 };
                    self.settle_above(id, sol, lump, excess)?;
                }
            }
            Op::Wipe(sol) => {
                let p = self.m().pool;
                if p.tokens_free > 0 && p.total_shares > 0 {
                    let q = u64::try_from(
                        u128::from(p.tokens_free) * u128::from(self.cfg.cap_bps) / 10_000,
                    )
                    .expect("q");
                    self.borrow(q, 30 * DAY)?;
                    for pos in self.s.rent.positions.clone() {
                        self.settle(pos.id, sol, 0)?;
                    }
                }
            }
            Op::Deep(r, sol) => {
                let p = self.m().pool;
                if p.tokens_free > 3 && p.total_shares > 0 {
                    self.borrow(p.tokens_free - r, 30 * DAY)?;
                    for pos in self.s.rent.positions.clone() {
                        self.settle(pos.id, sol, 0)?;
                    }
                }
            }
            Op::Claim(u, again) => {
                // I-P5: pays at most the lender's entitlement; again in the same state pays 0.
                let mut view = self.s.m.clone();
                if let Err(e) = view.touch(u) {
                    return refusal(e);
                }
                let bound = view.lender(u).expect("lender").pending.max(0);
                let paid = match self.s.claim(u) {
                    Ok(p) => p,
                    Err(e) => return refusal(e),
                };
                prop_assert!(
                    i128::from(paid) <= i128::from(bound),
                    "claim {paid} > {bound}"
                );
                for _ in 0..again {
                    prop_assert_eq!(self.s.claim(u), Ok(0), "repeated claim paid");
                }
            }
            Op::Cover(u, x) => {
                if self.m().lender(u).is_none() {
                    return Ok(());
                }
                if let Err(e) = self.s.m.touch(u) {
                    return refusal(e);
                }
                let pending = self.m().lender(u).expect("lender").pending;
                let x = if pending < 0 {
                    pending.unsigned_abs()
                } else {
                    x
                };
                let before = self.s.m.clone();
                match self.s.m.cover(u, x) {
                    Ok(()) if pending < 0 => {
                        prop_assert_eq!(self.m().lender(u).expect("lender").pending, 0);
                    }
                    Ok(()) => {}
                    Err(e) => {
                        refusal(e)?;
                        prop_assert_eq!(&self.s.m, &before);
                    }
                }
            }
            Op::Sweep(x, rewards) => {
                let before = self.s.m.clone();
                let src = if rewards {
                    SweepSource::VenueRewards
                } else {
                    SweepSource::Trader
                };
                if let Err(e) = self.s.m.sweep_from(x, src) {
                    refusal(e)?;
                    prop_assert_eq!(&self.s.m, &before);
                }
            }
            Op::Churn(seed) => {
                // I-P4 (V2-3): another position's open + close never reduces a lock.
                let p = self.m().pool;
                if p.tokens_free == 0 || p.borrowed_q == 0 {
                    return Ok(());
                }
                let users: Vec<usize> = self.m().lenders.iter().map(|(u, _)| *u).collect();
                let mut locks = Vec::new();
                for &u in &users {
                    if let Err(e) = self.s.m.touch(u) {
                        return refusal(e);
                    }
                    locks.push(self.s.m.lock_of(u).expect("lock"));
                }
                if let Some(id) = self.borrow(1 + seed % p.tokens_free, 3_600)? {
                    self.repay(id, Some(0), 0)?;
                }
                for (&u, &before) in users.iter().zip(&locks) {
                    self.s.m.touch(u).expect("touch");
                    let now = self.s.m.lock_of(u).expect("lock");
                    prop_assert!(
                        now >= before,
                        "churn released a lock of u{u}: {before} -> {now}"
                    );
                }
            }
            Op::Touch(u) => {
                let before = self.s.m.clone();
                match self.s.m.touch(u) {
                    Ok(st) => {
                        prop_assert!(st != Standing::Behind);
                        // Idempotent at one time.
                        let once = self.s.m.clone();
                        self.s.m.touch(u).expect("second touch");
                        prop_assert_eq!(&self.s.m, &once);
                    }
                    Err(e) => {
                        refusal(e)?;
                        prop_assert_eq!(&self.s.m, &before);
                    }
                }
            }
            Op::TouchPot(u) => {
                // Preferably a lender behind a rebase record (where the touch holds).
                let m = self.m();
                let behind: Vec<usize> = m
                    .lenders
                    .iter()
                    .filter(|(_, l)| {
                        l.epoch < m.pool.share_epoch && m.records[l.epoch as usize].num == 1
                    })
                    .map(|(w, _)| *w)
                    .collect();
                let u = behind.get(u % behind.len().max(1)).copied().unwrap_or(u);
                let before = self.s.m.clone();
                match self.s.m.touch_pot(u) {
                    Ok(st) => pot_only_touch_is_sound(&before, &self.s.m, u, st)?,
                    Err(e) => {
                        refusal(e)?;
                        prop_assert_eq!(&self.s.m, &before);
                    }
                }
            }
            Op::Sync(u, n, signed) => {
                // Preferably a lender with a forward chain, else one behind a rebase record.
                let m = self.m();
                let pick = |f: &dyn Fn(&Lender) -> bool| -> Vec<usize> {
                    m.lenders
                        .iter()
                        .filter(|(_, l)| f(l))
                        .map(|(w, _)| *w)
                        .collect()
                };
                let chained = pick(&|l| l.fwd != pool::FWD_NONE);
                let behind =
                    pick(&|l| l.epoch < m.pool.share_epoch && m.records[l.epoch as usize].num == 1);
                let from = if chained.is_empty() {
                    &behind
                } else {
                    &chained
                };
                let u = from.get(u % from.len().max(1)).copied().unwrap_or(u);
                let before = self.s.m.clone();
                match self.s.m.sync(u, usize::from(n), signed) {
                    Ok(()) if !signed => {
                        // KI65-REV-SEC1: a third party moves no share, lock, epoch or record.
                        prop_assert_eq!(&self.s.m.records, &before.records);
                        let (b, a) = (before.lender(u), self.s.m.lender(u).expect("lender"));
                        if let Some(b) =
                            b.filter(|b| b.epoch < before.pool.share_epoch && a.epoch == b.epoch)
                        {
                            prop_assert_eq!(
                                (a.shares, a.lock, a.lock_term, a.lock_until, a.claimed),
                                (b.shares, b.lock, b.lock_term, b.lock_until, b.claimed)
                            );
                        }
                    }
                    Ok(()) => {}
                    Err(e) => {
                        refusal(e)?;
                        prop_assert_eq!(&self.s.m, &before);
                    }
                }
            }
            Op::View => {
                // allowance_view is pure and equals the allowance after that lender's
                // own touch.
                let before = self.s.m.clone();
                for (u, _) in before.lenders.clone() {
                    let v = self.s.m.allowance_view(u);
                    prop_assert_eq!(&self.s.m, &before);
                    let mut t = before.clone();
                    prop_assert_eq!(v, t.allowance(u));
                }
            }
        }
        Ok(())
    }

    /// ECON §13 / I8 plus the calendar: `lock_norm` is the sum of the live buckets, which
    /// holds at least the live locks' terms (each within its own bucket).
    fn check(&mut self) -> Result<(), TestCaseError> {
        let b: u64 = self.s.rent.positions.iter().map(|p| p.q).sum();
        prop_assert_eq!(self.m().pool.borrowed_q, b, "ΣB_Q == Σ open q");
        prop_assert!(
            self.m().pool.max_release
                >= self
                    .s
                    .rent
                    .positions
                    .iter()
                    .map(|p| p.deadline + self.cfg.tail)
                    .max()
                    .unwrap_or(0),
            "I6: max_release covers every open position's release"
        );
        let r = if self.cfg.touch_all {
            self.s.m.check_touch_all()
        } else {
            self.s.m.invariants()
        };
        if let Err(e) = r {
            return Err(TestCaseError::fail(format!(
                "{e}\nstate {}",
                self.m().canonical()
            )));
        }
        let v = &self.m().vault;
        let sum: u128 = v.buckets().map(|(_, t)| t).sum();
        prop_assert_eq!(sum, v.lock_norm, "lock_norm = Σ buckets");
        let mut live = 0u128;
        for (u, l) in &self.m().lenders {
            let is_live = l.epoch == self.m().pool.share_epoch
                && l.lock != 0
                && l.lock_epoch == v.pi_epoch
                && self.m().now < l.lock_until;
            if is_live {
                prop_assert_eq!(l.lock_until % DAY, 0, "u{} lock_until is a day end", u);
                let day = l.lock_until / DAY;
                let bucket = v.buckets().find(|(d, _)| *d == day).map_or(0, |(_, t)| t);
                prop_assert!(l.lock_term <= bucket, "u{u}'s term outside its bucket");
                live += l.lock_term;
            }
            if l.lock_epoch == NO_LOCK_EPOCH {
                prop_assert_eq!(l.lock_term, 0);
            }
        }
        prop_assert!(live <= v.lock_norm, "Σ live terms {live} > lock_norm");
        Ok(())
    }

    /// I-P6, claim side: after the wind-down nobody is owed while SOL allocated to it or
    /// in the pot remains (the vault drains to dust unless every entitlement is paid).
    fn liveness(&self) -> Result<(), TestCaseError> {
        let mut w = self.s.clone();
        for pos in w.rent.positions.clone() {
            let owed = w.rent.owed(&pos);
            if w.repay(pos.id, owed, 0).is_err() {
                return Ok(()); // a close refused while S = 0 (ARCH §9 M0b note 4)
            }
        }
        w.advance(i64::from(w.m.lump_vest_s) + 1, 0);
        let users: Vec<usize> = w.m.lenders.iter().map(|(u, _)| *u).collect();
        for &u in &users {
            w.touch(u);
        }
        for _ in 0..2 {
            for &u in &users {
                w.claim(u).expect("claim");
            }
        }
        for (u, l) in &w.m.lenders {
            if l.pending > 0 && w.m.vault.lender_sol > 0 {
                prop_assert!(
                    l.real == l.claimed && w.m.vault.unmatched == 0,
                    "I-P6: u{u} owed {} with real − claimed {} and pot {}",
                    l.pending,
                    l.real - l.claimed,
                    w.m.vault.unmatched
                );
            }
        }
        Ok(())
    }
}

proptest! {
    #![proptest_config(common::proptest_config())]

    /// ECON §13 I-P1–I-P7 and ARCH §8.3 I8 hold after every operation, I-P4 also on the
    /// untouched state; the wind-down is live at the end.
    #[test]
    fn invariants_hold_after_every_operation((cfg, ops) in scenario()) {
        let (lump, grace) = cfg.params;
        let mut d = Driver { s: Model::with_params(cfg.cap_bps, lump, grace), cfg };
        for (i, op) in ops.iter().enumerate() {
            d.step(op).map_err(|e| TestCaseError::fail(format!("op {i} {op:?}: {e}")))?;
            d.check().map_err(|e| TestCaseError::fail(format!("after op {i} {op:?}: {e}")))?;
        }
        d.liveness()?;
    }

    /// Share epochs keep `S` bounded (I-P3) through any number of full wipes (each followed
    /// by its post-wipe sweep) and deep rebases, and a lender wiped out long ago catches up
    /// with the records it needs.
    #[test]
    fn repeated_wipes_and_rebases(rounds in prop::collection::vec((1u64..u64::MAX / 4, 0u64..4, 0u64..1_000_000_000), 1..40)) {
        let mut m = Market::new();
        m.deposit(0, 1_000_000).expect("first deposit");
        for (i, &(amount, left, sol)) in rounds.iter().enumerate() {
            let user = 1 + i % 5;
            if m.pool.is_wipe_live() {
                prop_assert_eq!(m.deposit(user, amount), Err(MathError::WipeSweepPending));
                m.sweep(sol / 3).expect("post-wipe sweep");
            }
            if m.deposit(user, amount).is_err() {
                continue;
            }
            let t = m.pool.tokens_free;
            let q = t.saturating_sub(left).max(1);
            m.borrow(q, m.now + 30 * DAY).expect("borrow");
            m.settle(q, sol, 0, sol / 7, sol / 5).expect("settle");
            let p = m.pool;
            let claims = u128::from(p.tokens_free) + u128::from(p.borrowed_q);
            prop_assert!(p.total_shares == 0 || p.total_shares + pool::VIRTUAL_SHARES <= pool::R_MAX * (claims + pool::VIRTUAL_ASSETS));
            m.accrue(3_600, 0).expect("time");
            m.invariants().map_err(TestCaseError::fail)?;
        }
        // The first lender (never touched since) converts through every record at once.
        prop_assert!(m.touch(0).is_ok());
        let l = *m.lender(0).expect("lender 0");
        prop_assert!(l.epoch == m.pool.share_epoch || m.pool.is_wipe_live());
        m.invariants().map_err(TestCaseError::fail)?;
    }

    /// R4A-2 / R4B-3: an idle lender's expired lock leaves the aggregate at its
    /// `lock_until`, whoever is touched; the allowances as the program sees them (each
    /// lender touched alone) never sum above T + dust.
    #[test]
    fn expired_locks_leave_the_aggregate_untouched(
        deps in prop::collection::vec(1_000_000u64..10_000_000_000_000, 2..6),
        util in 10u64..90,
        dur in 0usize..5,
        wait in 0i64..100,
    ) {
        let mut s = Model::new(9_000);
        for (u, &a) in deps.iter().enumerate() {
            s.deposit(u, a).expect("deposit");
        }
        let q = s.m.pool.tokens_free / 100 * util;
        s.borrow(q, DURATIONS[dur], S3_TAIL_S).expect("open");
        let a = s.free_allowance(0).expect("allowance");
        let sh = s.shares_at(a).max(1).min(s.shares(0));
        s.withdraw(0, sh).expect("withdraw");
        let l0 = *s.m.lender(0).expect("lender");
        prop_assert_eq!(l0.lock_until % DAY, 0);
        prop_assert!(l0.lock_until >= DURATIONS[dur] + S3_TAIL_S + i64::from(pool_harness::LOCK_GRACE_S));
        // Nothing resolves (Π stays); only the calendar can release the lock.
        s.advance(wait * DAY / 3, 0);
        let mut total = 0u128;
        for u in 0..deps.len() {
            total += u128::from(s.m.allowance_view(u).expect("view"));
        }
        prop_assert!(total <= u128::from(s.m.pool.tokens_free) + deps.len() as u128);
        let locked = pool::locked_total(&s.m.pool, &s.m.vault, s.m.now).expect("locked");
        if s.m.now >= l0.lock_until {
            prop_assert_eq!(locked, 0, "an expired lock still counts for everyone");
            prop_assert_eq!(pool::lock_of(&s.m.vault, &l0, s.m.now), Ok(0));
        } else if l0.lock_term > 0 {
            // Π = WAD while nothing resolves: the aggregate is the term itself.
            prop_assert_eq!(locked, l0.lock_term.min(s.m.pool.total_shares));
        }
    }

    /// KI-66: the pot-only touch of a lender behind a rebase record, at random record ends,
    /// snapshots and balances (negative `pending` included, accruals near the field limits):
    /// held unconverted with its snapshots at the record's ends, the surplus moved to the pot,
    /// idempotent, and the owner's conversion afterwards counting nothing twice
    /// (`pot_only_touch_is_sound`); a refusal changes nothing.
    #[test]
    fn pot_only_touch_holds_at_a_rebase_record(
        shares in prop_oneof![3 => 1u128..1_000, 3 => 1u128..10u128.pow(15), 1 => 1u128..u128::MAX / 64],
        lock in prop_oneof![Just(0u128), 0u128..10u128.pow(15)],
        acc in prop_oneof![
            3 => (-10i128.pow(21)..10i128.pow(21), -10i128.pow(21)..10i128.pow(21)),
            1 => (-10i128.pow(30)..10i128.pow(30), -10i128.pow(30)..10i128.pow(30)),
        ],
        acc_r in prop_oneof![
            3 => (0u128..10u128.pow(21), 0u128..10u128.pow(21)),
            1 => (0u128..10u128.pow(30), 0u128..10u128.pow(30)),
        ],
        pending in prop_oneof![3 => -10i64.pow(15)..10i64.pow(15), 1 => any::<i64>()],
        (claimed, extra) in prop_oneof![
            3 => (0u64..10u64.pow(15), 0u64..10u64.pow(15)),
            1 => (0u64..u64::MAX / 2, 0u64..u64::MAX / 2),
        ],
        unmatched in prop_oneof![Just(0u64), 0u64..u64::MAX / 2],
        k in 1u8..=12,
        cur in (-10i128.pow(21)..10i128.pow(21), 0u128..10u128.pow(21)),
    ) {
        let rec = ShareEpoch {
            acc_end: acc.0,
            acc_r_end: acc_r.0,
            s_old: shares,
            carry: 0,
            carry_r: 0,
            conv_rem: 0,
            epoch: 0,
            num: 1,
            den_log1000: k,
            _pad: [0; 10],
        };
        let mut m = Market::new();
        m.pool.share_epoch = 1;
        m.pool.total_shares = shares / rec.den().expect("den");
        (m.pool.acc, m.pool.acc_r) = cur;
        m.vault.unmatched = unmatched;
        m.records.push(rec);
        let l = Lender {
            shares,
            lock,
            lock_term: lock,
            snap: acc.1,
            snap_r: acc_r.1.min(acc_r.0),
            pending,
            real: claimed + extra,
            claimed,
            epoch: 0,
            ..Lender::new(&m.pool)
        };
        m.lenders.push((0, l));
        let before = m.clone();
        match m.touch_pot(0) {
            Ok(st) => {
                prop_assert_eq!(st, Standing::Behind);
                pot_only_touch_is_sound(&before, &m, 0, st)?;
            }
            Err(e) => {
                refusal(e)?;
                prop_assert_eq!(&m, &before);
            }
        }
    }

    /// KI-65 (ECON §9.5): lenders holding random old shares convert through one to three
    /// chained rebase records (`1/1,000^k`, k up to 12) in a random interleaving of
    /// `sync_lender` steps of 1–3 records: every step lands within the floor chain and one
    /// more unit per record passed, and once every lender has converted they hold exactly
    /// the new `S` (no orphan share), whatever the order.
    #[test]
    fn conversion_order_and_steps_never_orphan_a_share(
        holdings in prop::collection::vec(
            prop_oneof![0u128..1_000, 0u128..10_000_000, 0u128..u128::MAX / 64],
            1..12,
        ),
        ks in prop::collection::vec(1u8..=12, 1..4),
        schedule in prop::collection::vec((any::<prop::sample::Index>(), 1usize..4), 0..40),
    ) {
        let mut s = holdings.iter().sum::<u128>();
        let mut records = Vec::new();
        for (e, &k) in ks.iter().enumerate() {
            let rec = ShareEpoch {
                acc_end: 0,
                acc_r_end: 0,
                s_old: s,
                carry: 0,
                carry_r: 0,
                conv_rem: 0,
                epoch: u32::try_from(e).expect("epoch"),
                num: 1,
                den_log1000: k,
                _pad: [0; 10],
            };
            s /= rec.den().expect("den");
            records.push(rec);
        }
        let mut p = Pool::new();
        p.share_epoch = u32::try_from(ks.len()).expect("epochs");
        p.total_shares = s;
        let mut ls: Vec<Lender> = holdings
            .iter()
            .map(|&h| Lender { shares: h, epoch: 0, snap: 0, snap_r: 0, ..Lender::new(&p) })
            .collect();
        let n_lenders = ls.len();
        let all: Vec<(usize, usize)> = (0..n_lenders).map(|i| (i, ks.len())).collect();
        let steps = schedule.iter().map(|(ix, n)| (ix.index(n_lenders), *n)).chain(all);
        for (i, n) in steps {
            let l = &mut ls[i];
            if l.epoch == p.share_epoch {
                continue;
            }
            let from = l.epoch as usize;
            let to = (from + n).min(records.len());
            let (mut lo, mut hi) = (l.shares, l.shares);
            for r in &records[from..to] {
                let den = r.den().expect("den");
                lo /= den;
                hi = hi / den + 1;
            }
            let st = pool::convert(&p, l, &mut records[from..to]).map_err(|e| TestCaseError::fail(format!("{e}")))?;
            prop_assert!(lo <= l.shares && l.shares <= hi, "{} not in [{lo}, {hi}]", l.shares);
            prop_assert_eq!(st == Standing::Behind, l.epoch != p.share_epoch);
            for r in &records {
                prop_assert!(r.conv_rem < r.den().expect("den"));
            }
        }
        prop_assert_eq!(ls.iter().map(|l| l.shares).sum::<u128>(), p.total_shares);
    }

    /// KI-66 full: third parties' windowed syncs (the pot-only touch walking the forward chain
    /// 1–4 records at a time from `fwd_epoch`) interleaved with the owners' own partial syncs
    /// (1–4 records: stops inside a chain, `FWD_OPEN`, lag 0 or 1), through chained records
    /// (rebases by `1/1,000^k` with random `conv_rem`, full wipes, a live wipe record) with random
    /// ends and lenders at random epochs, snapshots, balances and locks: no third-party sync
    /// moves a share, lock, epoch or record, the stored chain stays consistent, and once every
    /// owner has converted each lender is bit-equal to the run without the third parties
    /// (`real` aside), the records too, with `Σ real + unmatched` equal — nothing counted twice.
    #[test]
    fn windowed_syncs_equal_owner_alone(
        kinds in prop::collection::vec(prop_oneof![5 => 1u8..=3, 1 => Just(0u8)], 1..6),
        ends in prop::collection::vec((-10i128.pow(21)..10i128.pow(21), 0u128..10u128.pow(21), any::<u64>()), 6),
        cur in (-10i128.pow(21)..10i128.pow(21), 0u128..10u128.pow(21)),
        live in any::<bool>(),
        holdings in prop::collection::vec(
            (
                prop_oneof![0u128..1_000, 0u128..10_000_000, 0u128..10u128.pow(12)],
                prop_oneof![2 => Just(0u128), 1 => 0u128..10u128.pow(9)],
                any::<prop::sample::Index>(),
                (-10i128.pow(21)..10i128.pow(21), any::<u64>()),
                (-10i64.pow(12)..10i64.pow(12), 0u64..10u64.pow(12)),
            ),
            1..6,
        ),
        schedule in prop::collection::vec((any::<prop::sample::Index>(), 1usize..5, any::<bool>()), 0..40),
    ) {
        let n_rec = kinds.len();
        let mut records = Vec::new();
        for (e, &k) in kinds.iter().enumerate() {
            let mut rec = ShareEpoch {
                acc_end: ends[e].0,
                acc_r_end: ends[e].1,
                s_old: u128::MAX / 2,
                carry: 0,
                carry_r: 0,
                conv_rem: 0,
                epoch: u32::try_from(e).expect("epoch"),
                num: u8::from(k != 0),
                den_log1000: k,
                _pad: [0; 10],
            };
            rec.conv_rem = u128::from(ends[e].2) % rec.den().expect("den");
            records.push(rec);
        }
        let mut p = Pool::new();
        p.share_epoch = u32::try_from(n_rec).expect("epochs");
        (p.acc, p.acc_r) = cur;
        p.wipe_live = u8::from(live && kinds[n_rec - 1] == 0);
        let lenders: Vec<Lender> = holdings
            .iter()
            .map(|&(shares, lock, e, (snap, snap_r), (pending, real))| {
                let epoch = e.index(n_rec + 1);
                let r_end = records.get(epoch).map_or(cur.1, |r| r.acc_r_end);
                Lender {
                    shares,
                    lock: lock.min(shares),
                    lock_term: lock.min(shares),
                    lock_epoch: if lock == 0 { NO_LOCK_EPOCH } else { 0 },
                    lock_until: if lock == 0 { 0 } else { 30 * DAY },
                    epoch: u32::try_from(epoch).expect("epoch"),
                    snap,
                    snap_r: u128::from(snap_r).min(r_end),
                    pending,
                    real,
                    claimed: real / 2,
                    ..Lender::new(&p)
                }
            })
            .collect();
        let fail = |e: MathError| TestCaseError::fail(format!("{e:?}"));
        // sync_lender(n) as the program runs it (`pool_harness::Market::sync`).
        let sync = |v: &mut pool::Vault, l: &mut Lender, recs: &mut [ShareEpoch], n: usize, signed: bool| {
            let lo = if signed || l.fwd == pool::FWD_NONE { l.epoch } else { l.fwd_epoch } as usize;
            let (a, b) = (lo.min(n_rec), lo.saturating_add(n).min(n_rec));
            if signed {
                if pool::convert(&p, l, &mut recs[a..b])? != Standing::Behind {
                    pool::touch(&p, v, l, &mut recs[a..b], 0)?;
                }
            } else {
                pool::touch_pot(&p, v, l, &recs[a..b], 0)?;
            }
            Ok::<(), MathError>(())
        };
        let mut worlds = Vec::new();
        for third in [false, true] {
            let (mut v, mut ls, mut recs) = (pool::Vault::new(), lenders.clone(), records.clone());
            for &(ix, n, by_third) in &schedule {
                let i = ix.index(ls.len());
                if by_third && !third {
                    continue;
                }
                let b = ls[i];
                let before = recs.clone();
                sync(&mut v, &mut ls[i], &mut recs, n, !by_third).map_err(fail)?;
                let a = ls[i];
                pool_harness::fwd_state(&p, &a).map_err(TestCaseError::fail)?;
                if by_third {
                    prop_assert_eq!(&recs, &before, "a third party wrote a record");
                    if b.epoch < p.share_epoch && a.epoch == b.epoch {
                        prop_assert_eq!(
                            (a.shares, a.lock, a.lock_term, a.lock_until, a.claimed),
                            (b.shares, b.lock, b.lock_term, b.lock_until, b.claimed)
                        );
                    }
                }
            }
            for l in &mut ls {
                let from = l.epoch as usize;
                pool::touch(&p, &mut v, l, &mut recs[from.min(n_rec)..], 0).map_err(fail)?;
                prop_assert_eq!(l.fwd, pool::FWD_NONE);
            }
            worlds.push((v, ls, recs));
        }
        let ((va, la, ra), (vb, lb, rb)) = (&worlds[0], &worlds[1]);
        for (x, y) in la.iter().zip(lb) {
            prop_assert_eq!(Lender { real: 0, ..*x }, Lender { real: 0, ..*y });
        }
        prop_assert_eq!(ra, rb);
        let total = |v: &pool::Vault, ls: &[Lender]| {
            ls.iter().map(|l| u128::from(l.real)).sum::<u128>() + u128::from(v.unmatched)
        };
        prop_assert_eq!(total(va, la), total(vb, lb));
    }
}
