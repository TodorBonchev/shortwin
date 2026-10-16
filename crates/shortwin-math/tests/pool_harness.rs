//! Test harness for `shortwin_math::pool`, shared by `pool_vectors.rs`, `pool_props.rs`
//! and `pool_econ.rs` (`#[path = "pool_harness.rs"] mod pool_harness;`).
//!
//! [`Market`] holds what the program keeps in `Market`, `MarketVault`, `ShareEpoch` and
//! `LenderPosition` accounts plus the clock, and drives `pool::*` exactly as the
//! vocabulary of `tests/vectors/pool/gen_pool.py` does. [`Market::check_touch_all`] is
//! `pool_model.Pool.check` (ECON §13 I-P1–I-P7, I-P4 also on the untouched state;
//! ARCH §8.3 I8); [`Market::invariants`] runs it on a copy. [`Model`] adds `pool_model`'s
//! rent emulation and positions, so drivers can be written after the model's demos.
//! KI-65: a lender's conversion writes `conv_rem` into the records it passes, so every
//! lender operation takes the records `&mut` (the program writes them back), and I-P1 is
//! an equality once every lender is touched: `Σ shares = S`.
#![allow(dead_code)]

use std::fmt::Write as _;

use shortwin_math::num::{Rounding, WAD, mul_div};
use shortwin_math::pool::{
    self, Lender, Pool, R_MAX, Resolution, ShareEpoch, Standing, SweepSource, VIRTUAL_ASSETS,
    VIRTUAL_SHARES, Vault, WIPE_TRADER_SWEPT,
};
use shortwin_math::{MathError, U256};

/// `pool_model.BPS`.
pub const BPS: u128 = 10_000;
/// `pool_model.LUMP_VEST_S` (ECON §15 default).
pub const LUMP_VEST_S: u32 = 21_600;
/// `pool_model.LOCK_GRACE_S` (ECON §15 default).
pub const LOCK_GRACE_S: u32 = 86_400;
/// `pool_model.RENT_PROTOCOL_BPS` (SPEC default).
pub const RENT_PROTOCOL_BPS: u32 = 1_500;
/// `pool_model.S3_TAIL_S`: default `max_migration_pause_s + grad_grace_s + s3_wait_s`.
pub const S3_TAIL_S: i64 = 3_600 + 600 + 600;
/// One day.
pub const DAY: i64 = 86_400;

/// One market's lender side, as the program stores it, plus the clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Market {
    pub pool: Pool,
    pub vault: Vault,
    /// `Clock::unix_timestamp` (`pool_model.now`).
    pub now: i64,
    pub lump_vest_s: u32,
    pub lock_grace_s: u32,
    /// Ghost: lamports that ever reached `lender_sol` for lenders (`pool_model.inflow`).
    pub inflow: u128,
    /// Every `ShareEpoch` record; index = epoch.
    pub records: Vec<ShareEpoch>,
    /// `(user, lender)` in creation order.
    pub lenders: Vec<(usize, Lender)>,
}

impl Default for Market {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of one vocabulary operation, as the vectors write it.
pub fn show<T: ToString>(r: &Result<Option<T>, MathError>) -> String {
    match r {
        Ok(Some(v)) => format!("={}", v.to_string()),
        Ok(None) => String::new(),
        Err(e) => format!("!{}", code(*e)),
    }
}

/// Short error codes of the vectors.
pub fn code(e: MathError) -> &'static str {
    match e {
        MathError::Overflow => "overflow",
        MathError::Underflow => "underflow",
        MathError::DivByZero => "divzero",
        MathError::InvalidInput => "invalid",
        MathError::Undefined => "undefined",
        MathError::Invariant => "invariant",
        MathError::NegativePending => "negative_pending",
        MathError::InsufficientShares => "insufficient_shares",
        MathError::InsufficientInventory => "insufficient_inventory",
        MathError::AboveAllowance => "above_allowance",
        MathError::LenderBehind => "lender_behind",
        MathError::DepositTooSmall => "deposit_too_small",
        MathError::WipeSweepPending => "wipe_sweep_pending",
        MathError::PoolDrained => "pool_drained",
    }
}

fn records_from(records: &[ShareEpoch], epoch: u32) -> &[ShareEpoch] {
    &records[(epoch as usize).min(records.len())..]
}

/// The records from `epoch` on, mutable: a conversion writes their `conv_rem` (KI-65).
fn records_from_mut(records: &mut [ShareEpoch], epoch: u32) -> &mut [ShareEpoch] {
    let n = records.len();
    &mut records[(epoch as usize).min(n)..]
}

impl Market {
    pub fn new() -> Self {
        Self::with_params(LUMP_VEST_S, LOCK_GRACE_S)
    }

    pub fn with_params(lump_vest_s: u32, lock_grace_s: u32) -> Self {
        Self {
            pool: Pool::new(),
            vault: Vault::new(),
            now: 0,
            lump_vest_s,
            lock_grace_s,
            inflow: 0,
            records: Vec::new(),
            lenders: Vec::new(),
        }
    }

    /// Index of `user` in `lenders`, creating the lender (`pool::Lender::new`) if needed.
    pub fn slot(&mut self, user: usize) -> usize {
        if let Some(i) = self.lenders.iter().position(|(u, _)| *u == user) {
            return i;
        }
        self.lenders.push((user, Lender::new(&self.pool)));
        self.lenders.len() - 1
    }

    pub fn lender(&self, user: usize) -> Option<&Lender> {
        self.lenders
            .iter()
            .find(|(u, _)| *u == user)
            .map(|(_, l)| l)
    }

    pub fn touch(&mut self, user: usize) -> Result<Standing, MathError> {
        let i = self.slot(user);
        let l = &mut self.lenders[i].1;
        pool::touch(
            &self.pool,
            &mut self.vault,
            l,
            records_from_mut(&mut self.records, l.epoch),
            self.now,
        )
    }

    /// KI-66: the pot-only touch (a third party's `sync_lender`), with every record.
    pub fn touch_pot(&mut self, user: usize) -> Result<Standing, MathError> {
        let i = self.slot(user);
        let l = &mut self.lenders[i].1;
        pool::touch_pot(
            &self.pool,
            &mut self.vault,
            l,
            records_from(&self.records, l.epoch),
            self.now,
        )
    }

    /// KI-66 full: `sync_lender(n)` with the `n` records from where the walk starts: signed, the
    /// owner's `convert` then `touch` unless still behind (one unit); unsigned, `touch_pot` from the
    /// lender's epoch or, with a forward chain, from its `fwd_epoch`.
    pub fn sync(&mut self, user: usize, n: usize, signed: bool) -> Result<(), MathError> {
        let i = self.slot(user);
        let (mut v, mut l) = (self.vault, self.lenders[i].1);
        let lo = if signed || l.fwd == pool::FWD_NONE {
            l.epoch
        } else {
            l.fwd_epoch
        } as usize;
        let len = self.records.len();
        let (a, b) = (lo.min(len), lo.saturating_add(n).min(len));
        let mut recs = self.records.clone();
        if signed {
            let st = pool::convert(&self.pool, &mut l, &mut recs[a..b])?;
            if st != Standing::Behind {
                pool::touch(&self.pool, &mut v, &mut l, &mut recs[a..b], self.now)?;
            }
        } else {
            pool::touch_pot(&self.pool, &mut v, &mut l, &recs[a..b], self.now)?;
        }
        self.records = recs;
        self.vault = v;
        self.lenders[i].1 = l;
        Ok(())
    }

    /// The committed first step of `d` / `w`: release + touch, as one unit.
    pub fn pre(&mut self, user: usize) -> Result<(), MathError> {
        let i = self.slot(user);
        let (mut p, mut v, mut l) = (self.pool, self.vault, self.lenders[i].1);
        pool::release(&mut p, self.now)?;
        let recs = records_from_mut(&mut self.records, l.epoch);
        pool::touch(&p, &mut v, &mut l, recs, self.now)?;
        self.pool = p;
        self.vault = v;
        self.lenders[i].1 = l;
        Ok(())
    }

    /// `d`: release + touch (committed), then deposit (`rewards_required = false`).
    pub fn deposit(&mut self, user: usize, amount: u64) -> Result<u128, MathError> {
        self.deposit_with(user, amount, false)
    }

    pub fn deposit_with(
        &mut self,
        user: usize,
        amount: u64,
        rewards_required: bool,
    ) -> Result<u128, MathError> {
        self.pre(user)?;
        let i = self.slot(user);
        let l = &mut self.lenders[i].1;
        pool::deposit(
            &mut self.pool,
            &mut self.vault,
            l,
            records_from_mut(&mut self.records, l.epoch),
            self.now,
            amount,
            rewards_required,
        )
    }

    /// `w`: release + touch (committed), then withdraw.
    pub fn withdraw(&mut self, user: usize, shares: u128) -> Result<u64, MathError> {
        self.pre(user)?;
        let i = self.slot(user);
        let l = &mut self.lenders[i].1;
        pool::withdraw(
            &mut self.pool,
            &mut self.vault,
            l,
            records_from_mut(&mut self.records, l.epoch),
            self.now,
            self.lock_grace_s,
            shares,
        )
    }

    /// `a`: touch, then allowance (`pool_model.free_allowance`).
    pub fn allowance(&mut self, user: usize) -> Result<u64, MathError> {
        self.touch(user)?;
        pool::allowance(
            &self.pool,
            &self.vault,
            self.lender(user).expect("touched"),
            self.now,
        )
    }

    /// `A`: the allowance with only this lender touched (pure).
    pub fn allowance_view(&self, user: usize) -> Result<u64, MathError> {
        let l = self.lender(user).expect("allowance_view of a known lender");
        pool::allowance_view(
            &self.pool,
            &self.vault,
            l,
            records_from(&self.records, l.epoch),
            self.now,
        )
    }

    pub fn claim(&mut self, user: usize) -> Result<u64, MathError> {
        let i = self.slot(user);
        let l = &mut self.lenders[i].1;
        pool::claim(
            &self.pool,
            &mut self.vault,
            l,
            records_from_mut(&mut self.records, l.epoch),
            self.now,
        )
    }

    pub fn cover(&mut self, user: usize, lamports: u64) -> Result<(), MathError> {
        let i = self.slot(user);
        let l = &mut self.lenders[i].1;
        pool::cover(
            &self.pool,
            &mut self.vault,
            l,
            records_from_mut(&mut self.records, l.epoch),
            self.now,
            lamports,
        )?;
        self.inflow += u128::from(lamports);
        Ok(())
    }

    pub fn lock_of(&mut self, user: usize) -> Result<u128, MathError> {
        let i = self.slot(user);
        pool::lock_of(&self.vault, &self.lenders[i].1, self.now)
    }

    pub fn cal_advance(&mut self) -> Result<(), MathError> {
        pool::cal_advance(&mut self.vault, self.now)
    }

    pub fn release(&mut self) -> Result<u64, MathError> {
        pool::release(&mut self.pool, self.now)
    }

    pub fn borrow(&mut self, q: u64, release_at: i64) -> Result<(), MathError> {
        pool::borrow(&mut self.pool, self.now, q, release_at)
    }

    /// `x`: time passes by `dt`, then the lender rent credit; refused as a whole.
    pub fn accrue(&mut self, dt: i64, lamports: u64) -> Result<(), MathError> {
        let now = self.now.checked_add(dt).ok_or(MathError::Overflow)?;
        pool::accrue(&mut self.pool, &mut self.vault, now, lamports)?;
        self.now = now;
        Ok(())
    }

    pub fn repay(
        &mut self,
        q: u64,
        rent_paid: u64,
        rent_protocol_bps: u32,
        credited: u64,
        lump: u64,
    ) -> Result<(), MathError> {
        let res = Resolution { q, credited, lump };
        let lender_rent = pool::repay(
            &mut self.pool,
            &mut self.vault,
            self.now,
            self.lump_vest_s,
            &res,
            rent_paid,
            rent_protocol_bps,
        )?;
        self.inflow += u128::from(lender_rent) + u128::from(lump);
        Ok(())
    }

    pub fn settle(
        &mut self,
        q: u64,
        sol_in: u64,
        credited: u64,
        lump: u64,
        rent: u64,
    ) -> Result<(), MathError> {
        let res = Resolution { q, credited, lump };
        let rec = pool::settle(
            &mut self.pool,
            &mut self.vault,
            self.now,
            self.lump_vest_s,
            &res,
            sol_in,
            rent,
        )?
        .rec;
        self.inflow += u128::from(sol_in);
        if let Some(r) = rec {
            assert_eq!(
                r.epoch as usize,
                self.records.len(),
                "records are consecutive"
            );
            self.records.push(r);
        }
        Ok(())
    }

    /// Trader sweep; while a wipe record is live it is passed for the top-up.
    pub fn sweep(&mut self, lamports: u64) -> Result<(), MathError> {
        self.sweep_from(lamports, SweepSource::Trader)
    }

    pub fn sweep_from(&mut self, lamports: u64, source: SweepSource) -> Result<(), MathError> {
        let wipe = if self.pool.is_wipe_live() {
            self.records.last_mut()
        } else {
            None
        };
        pool::sweep(
            &mut self.pool,
            &mut self.vault,
            wipe,
            self.now,
            self.lump_vest_s,
            lamports,
            source,
        )?;
        self.inflow += u128::from(lamports);
        Ok(())
    }

    /// Applies one vector operation (`gen_pool.py` vocabulary); returns its result token.
    pub fn apply(&mut self, op: &str, args: &[&str]) -> String {
        let n = |i: usize| -> u128 {
            args[i]
                .parse()
                .unwrap_or_else(|_| panic!("bad arg in {op} {args:?}"))
        };
        let u = || args[0].parse::<usize>().expect("user");
        let u64a = |i: usize| -> u64 { u64::try_from(n(i)).expect("u64 arg") };
        let i64a = |i: usize| -> i64 { args[i].parse().expect("i64 arg") };
        let none = |r: Result<(), MathError>| show(&r.map(|()| None::<u8>));
        match op {
            "d" => show(&self.deposit(u(), u64a(1)).map(Some)),
            "w" => show(&self.withdraw(u(), n(1)).map(Some)),
            "a" => show(&self.allowance(u()).map(Some)),
            "A" => show(&self.allowance_view(u()).map(Some)),
            "c" => show(&self.claim(u()).map(Some)),
            "v" => none(self.cover(u(), u64a(1))),
            "t" => none(self.touch(u()).map(|_| ())),
            "h" => none(self.touch_pot(u()).map(|_| ())),
            "y" => none(self.sync(u(), usize::try_from(n(1)).expect("n"), n(2) == 1)),
            "l" => show(&self.lock_of(u()).map(Some)),
            "k" => none(self.cal_advance()),
            "R" => none(self.release().map(|_| ())),
            "b" => none(self.borrow(u64a(0), i64a(1))),
            "x" => none(self.accrue(i64a(0), u64a(1))),
            "r" => none(self.repay(
                u64a(0),
                u64a(1),
                u32::try_from(n(2)).expect("bps"),
                u64a(3),
                u64a(4),
            )),
            "s" => none(self.settle(u64a(0), u64a(1), u64a(2), u64a(3), u64a(4))),
            "p" => none(self.sweep(u64a(0))),
            _ => panic!("unknown op {op}"),
        }
    }

    /// `pool_model.Pool.check()` on the live state: cal_advance; I-P4 on the untouched
    /// state (each lender's allowance with only it touched); touch every lender in
    /// creation order; I-P1–I-P3, I-P7; then touch + allowance for each (I-P4) and no
    /// lock effective at or after its `lock_until`.
    pub fn check_touch_all(&mut self) -> Result<(), String> {
        for (u, l) in &self.lenders {
            fwd_state(&self.pool, l).map_err(|e| format!("u{u}: {e}"))?;
        }
        self.cal_advance()
            .map_err(|e| format!("cal_advance: {e}"))?;
        let t = u128::from(self.pool.tokens_free);
        let n = self.lenders.len() as u128;
        let mut seen = 0u128;
        for (u, _) in &self.lenders {
            seen += u128::from(
                self.allowance_view(*u)
                    .map_err(|e| format!("allowance_view u{u}: {e}"))?,
            );
        }
        if seen > t + n {
            return Err(format!(
                "I-P4 (untouched state): Σ allowances {seen} > T + n = {}",
                t + n
            ));
        }
        for i in 0..self.lenders.len() {
            let user = self.lenders[i].0;
            self.touch(user)
                .map_err(|e| format!("touch u{user}: {e}"))?;
        }
        self.assert_invariants()?;
        let mut sum = 0u128;
        for i in 0..self.lenders.len() {
            let user = self.lenders[i].0;
            sum += u128::from(
                self.allowance(user)
                    .map_err(|e| format!("allowance u{user}: {e}"))?,
            );
        }
        if sum > t + n {
            return Err(format!("I-P4: Σ allowances {sum} > T + n = {}", t + n));
        }
        for (u, l) in &self.lenders {
            let lk = pool::lock_of(&self.vault, l, self.now).map_err(|e| e.to_string())?;
            if lk > 0 && self.now >= l.lock_until {
                return Err(format!("I-P4: u{u}'s lock outlives lock_until"));
            }
        }
        Ok(())
    }

    /// The invariants on a copy (lenders stay lazy), as `gen_pool.verify`.
    pub fn invariants(&self) -> Result<(), String> {
        self.clone().check_touch_all()
    }

    /// I-P1..I-P3, I-P7 on already-touched lenders (the body of `pool_model.Pool.check`).
    fn assert_invariants(&self) -> Result<(), String> {
        let p = &self.pool;
        let v = &self.vault;
        let t = u128::from(p.tokens_free);
        let b = u128::from(p.borrowed_q);
        let cur: Vec<&Lender> = self
            .lenders
            .iter()
            .map(|(_, l)| l)
            .filter(|l| l.epoch == p.share_epoch)
            .collect();
        let mut claims = 0u128;
        let mut shares = 0u128;
        for l in &cur {
            claims += mul_div(
                l.shares,
                t + b + VIRTUAL_ASSETS,
                p.total_shares + VIRTUAL_SHARES,
                Rounding::Floor,
            )
            .map_err(|e| e.to_string())?;
            shares += l.shares;
        }
        if claims > t + b {
            return Err(format!("I-P1: token claims {claims} > T + B {}", t + b));
        }
        if shares != p.total_shares {
            // KI-65: conversions round cumulatively through each record, so once every
            // lender is touched every share of S has a holder (no rebase orphans).
            return Err(format!("I-P1: Σ shares {shares} ≠ S {}", p.total_shares));
        }
        // I-P2, claim side (strict, V1-10 / V2-2, R3A-2).
        let unclaimed: i128 = self
            .lenders
            .iter()
            .map(|(_, l)| i128::from(l.real) - i128::from(l.claimed))
            .sum();
        let backed = unclaimed + i128::from(v.unmatched) + i128::from(p.vest_bal);
        if backed > i128::from(v.lender_sol) {
            return Err(format!(
                "I-P2: Σ(real − claimed) {unclaimed} + unmatched {} + vest {} > lender_sol {}",
                v.unmatched, p.vest_bal, v.lender_sol
            ));
        }
        if let Some((u, _)) = self.lenders.iter().find(|(_, l)| l.claimed > l.real) {
            return Err(format!("I-P2: u{u} claimed above its realised allocation"));
        }
        if let Some((u, _)) = self.lenders.iter().find(|(_, l)| {
            i128::from(l.real) - i128::from(l.claimed) > i128::from(l.pending.max(0))
        }) {
            return Err(format!("I-P2: u{u}'s own allocation above its entitlement"));
        }
        let claimed: u128 = self
            .lenders
            .iter()
            .map(|(_, l)| u128::from(l.claimed))
            .sum();
        if claimed + u128::from(v.protocol_fees) > self.inflow {
            return Err(format!(
                "I-P2: Σ claimed {claimed} + protocol {} > inflow {}",
                v.protocol_fees, self.inflow
            ));
        }
        // I-P2, entitlement side (signed aggregate).
        let pos: i128 = self
            .lenders
            .iter()
            .map(|(_, l)| i128::from(l.pending.max(0)))
            .sum();
        let neg: i128 = self
            .lenders
            .iter()
            .map(|(_, l)| i128::from((-l.pending).max(0)))
            .sum();
        if pos - neg > i128::from(v.lender_sol) - i128::from(p.vest_bal) + i128::from(p.receivable)
        {
            return Err(format!(
                "I-P2: Σ pending {} > lender_sol {} − vest {} + receivable {}",
                pos - neg,
                v.lender_sol,
                p.vest_bal,
                p.receivable
            ));
        }
        // I-P7 (R3B-9): no phantom receivable once nothing is borrowed.
        if b == 0 && p.receivable != 0 {
            return Err(format!("I-P7: receivable {} with B = 0", p.receivable));
        }
        // I-P3.
        if !(p.total_shares + VIRTUAL_SHARES <= R_MAX * (t + b + VIRTUAL_ASSETS)
            || p.total_shares == 0
            || t + b == 0)
        {
            return Err(format!(
                "I-P3: share ratio S {} T {t} B {b}",
                p.total_shares
            ));
        }
        Ok(())
    }

    /// Canonical state string of the vectors (see `gen_pool.py`).
    pub fn canonical(&self) -> String {
        let p = &self.pool;
        let v = &self.vault;
        let mut s = format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            self.now,
            p.tokens_free,
            p.borrowed_q,
            p.total_shares,
            p.acc,
            p.carry,
            p.acc_r,
            p.carry_r,
            p.dust,
            p.receivable,
            p.max_release,
            p.vest_bal,
            p.vest_last,
            p.vest_end,
            p.share_epoch,
            p.wipe_live,
            u8::from(p.wipe_swept & WIPE_TRADER_SWEPT != 0),
            v.pi,
            v.pi_epoch,
            v.lock_norm,
            v.b_min,
            v.lender_sol,
            v.unmatched,
            v.protocol_fees,
            self.inflow
        );
        let buckets: Vec<String> = v.buckets().map(|(d, b)| format!("{d}:{b}")).collect();
        if !buckets.is_empty() {
            write!(s, ";C{}", buckets.join(",")).expect("write");
        }
        if let Some(r) = self.records.last() {
            write!(
                s,
                ";E{},{},{},{},{},{},{},{},{}",
                r.epoch,
                r.acc_end,
                r.num,
                r.den_log1000,
                r.acc_r_end,
                r.s_old,
                r.carry,
                r.carry_r,
                r.conv_rem
            )
            .expect("write");
        }
        for (u, l) in &self.lenders {
            write!(
                s,
                ";{u}:{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                l.shares,
                l.snap,
                l.pending,
                l.epoch,
                l.lock,
                l.lock_pi,
                l.lock_epoch,
                l.lock_term,
                l.lock_until,
                l.snap_r,
                l.real,
                l.claimed,
                l.fwd,
                l.fwd_epoch,
                l.fwd_shares,
                l.fwd_lag
            )
            .expect("write");
        }
        s
    }
}

/// Field names of the canonical state, for diagnostics.
const POOL_FIELDS: [&str; 25] = [
    "now",
    "T",
    "B",
    "S",
    "acc",
    "carry",
    "acc_r",
    "carry_r",
    "dust",
    "receivable",
    "max_release",
    "vest_bal",
    "vest_last",
    "vest_end",
    "epoch",
    "wipe_live",
    "wipe_swept",
    "pi",
    "pi_epoch",
    "lock_norm",
    "b_min",
    "sol_vault",
    "unmatched",
    "protocol",
    "inflow",
];
const RECORD_FIELDS: [&str; 9] = [
    "epoch",
    "acc_end",
    "num",
    "den_log1000",
    "acc_r_end",
    "s_old",
    "carry",
    "carry_r",
    "conv_rem",
];
const LENDER_FIELDS: [&str; 16] = [
    "shares",
    "snap",
    "pending",
    "epoch",
    "lock",
    "lock_pi",
    "lock_epoch",
    "lock_term",
    "lock_until",
    "snap_r",
    "real",
    "claimed",
    "fwd",
    "fwd_epoch",
    "fwd_shares",
    "fwd_lag",
];

/// Names the fields in which two canonical states differ.
/// KI-66 full: the forward-chain fields of a stored lender (ARCH §2.3.6 `LenderPosition`):
/// none, or a chain the pot-only touch started at a rebase record (`FWD_HOLD`: `epoch <
/// fwd_epoch`, no lag) or that the owner's conversion entered (`FWD_OPEN`: `epoch ≤ fwd_epoch`,
/// lag 0 or 1), never ahead of the pool and never on a current lender.
pub fn fwd_state(p: &Pool, l: &Lender) -> Result<(), String> {
    let ok = match l.fwd {
        pool::FWD_NONE => l.fwd_epoch == 0 && l.fwd_lag == 0 && l.fwd_shares == 0,
        pool::FWD_HOLD | pool::FWD_OPEN => {
            l.epoch < p.share_epoch
                && l.fwd_epoch <= p.share_epoch
                && if l.fwd == pool::FWD_HOLD {
                    l.epoch < l.fwd_epoch && l.fwd_lag == 0
                } else {
                    l.epoch <= l.fwd_epoch && l.fwd_lag <= 1
                }
        }
        _ => false,
    };
    if ok && l._pad == [0; 2] {
        Ok(())
    } else {
        Err(format!(
            "forward chain fwd {} fwd_epoch {} fwd_lag {} fwd_shares {} at epoch {} (share epoch {})",
            l.fwd, l.fwd_epoch, l.fwd_lag, l.fwd_shares, l.epoch, p.share_epoch
        ))
    }
}

pub fn diff_states(expected: &str, got: &str) -> String {
    let (e, g): (Vec<&str>, Vec<&str>) = (expected.split(';').collect(), got.split(';').collect());
    let mut out = String::new();
    if e.len() != g.len() {
        let _ = writeln!(out, "  sections: expected {}, got {}", e.len(), g.len());
    }
    for (i, (es, gs)) in e.iter().zip(&g).enumerate() {
        if es == gs {
            continue;
        }
        let (label, names, ev, gv): (String, &[&str], Vec<&str>, Vec<&str>) = if i == 0 {
            (
                "pool".into(),
                &POOL_FIELDS,
                es.split(',').collect(),
                gs.split(',').collect(),
            )
        } else if let Some(r) = es.strip_prefix('E') {
            (
                "record".into(),
                &RECORD_FIELDS,
                r.split(',').collect(),
                gs.trim_start_matches('E').split(',').collect(),
            )
        } else if es.starts_with('C') || gs.starts_with('C') {
            let _ = writeln!(out, "  calendar: expected {es}, got {gs}");
            continue;
        } else {
            let (eu, er) = es.split_once(':').unwrap_or(("?", es));
            let (_, gr) = gs.split_once(':').unwrap_or(("?", gs));
            (
                format!("lender u{eu}"),
                &LENDER_FIELDS,
                er.split(',').collect(),
                gr.split(',').collect(),
            )
        };
        for (j, (a, b)) in ev.iter().zip(&gv).enumerate() {
            if a != b {
                let _ = writeln!(
                    out,
                    "  {label}.{}: expected {a}, got {b}",
                    names.get(j).unwrap_or(&"?")
                );
            }
        }
    }
    out
}

/// CRC-32 (zlib / IEEE 802.3), chainable like `zlib.crc32(data, prev)`.
pub fn crc32(prev: u32, data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (n, e) in t.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *e = c;
        }
        t
    });
    let mut c = !prev;
    for &b in data {
        c = table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// An open position of the rent emulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub id: u64,
    pub q: u64,
    pub index_open: u128,
    /// The position's `rent_protocol_bps` snapshot (R3A-5).
    pub p: u32,
    pub deadline: i64,
}

/// Rent emulation of `pool_model` (harness state, not pool state): the market rent index,
/// the aggregate `A = Σ q·(10⁴ − p)`, the carried credit remainder and the open positions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rent {
    pub index: u128,
    pub a: u128,
    /// `pool_model.Pool.credit_rem` (AIR-03): the remainder of the last credit, in
    /// `[0, WAD·10⁴)`.
    pub rem: u128,
    /// Governance value for new positions.
    pub protocol_bps: u32,
    pub positions: Vec<Pos>,
    pub next_id: u64,
}

impl Default for Rent {
    fn default() -> Self {
        Self {
            index: 0,
            a: 0,
            rem: 0,
            protocol_bps: RENT_PROTOCOL_BPS,
            positions: Vec::new(),
            next_id: 0,
        }
    }
}

impl Rent {
    /// `pool_model.Pool.advance`'s credit: `⌊(A·dI + rem)/(WAD·10⁴)⌋`, its remainder carried
    /// into the next accrual (AIR-03).
    pub fn accrue(&mut self, d_index: u128) -> u64 {
        self.index += d_index;
        let n = U256::from_product(self.a, d_index)
            .checked_add(U256::from_u128(self.rem))
            .expect("credit");
        let (c, rem) = n.div_rem_u128(WAD * BPS).expect("credit");
        self.rem = rem;
        c.to_u64().expect("credit fits u64")
    }

    /// `pool_model.Pool._credited_for`: `⌈q·(I − I_open)·(10⁴ − p)/(WAD·10⁴)⌉`.
    pub fn credited_for(&self, pos: &Pos) -> u64 {
        let c = mul_div(
            u128::from(pos.q) * (BPS - u128::from(pos.p)),
            self.index - pos.index_open,
            WAD * BPS,
            Rounding::Ceil,
        )
        .expect("credited");
        u64::try_from(c).expect("credited fits u64")
    }

    /// Rent owed by a position: `⌈q·(I − I_open)/WAD⌉`.
    pub fn owed(&self, pos: &Pos) -> u64 {
        u64::try_from(self.owed_wide(pos)).expect("owed fits u64")
    }

    /// [`Self::owed`] clamped to `u64` (a settlement's `R_due`; `gen_pool.py` clamps the same).
    pub fn owed_sat(&self, pos: &Pos) -> u64 {
        u64::try_from(self.owed_wide(pos)).unwrap_or(u64::MAX)
    }

    fn owed_wide(&self, pos: &Pos) -> u128 {
        mul_div(
            u128::from(pos.q),
            self.index - pos.index_open,
            WAD,
            Rounding::Ceil,
        )
        .expect("owed")
    }

    pub fn open(&mut self, q: u64, deadline: i64) -> u64 {
        self.next_id += 1;
        let p = self.protocol_bps;
        self.a += u128::from(q) * (BPS - u128::from(p));
        self.positions.push(Pos {
            id: self.next_id,
            q,
            index_open: self.index,
            p,
            deadline,
        });
        self.next_id
    }

    pub fn get(&self, id: u64) -> Pos {
        *self
            .positions
            .iter()
            .find(|p| p.id == id)
            .expect("position")
    }

    pub fn take(&mut self, id: u64) -> Pos {
        let i = self
            .positions
            .iter()
            .position(|p| p.id == id)
            .expect("position");
        let pos = self.positions.remove(i);
        self.a -= u128::from(pos.q) * (BPS - u128::from(pos.p));
        pos
    }
}

/// `pool_model.Pool` rebuilt on the Rust API: a [`Market`], the rent emulation and the
/// utilisation cap, with the model's method names (drivers after its demos).
#[derive(Clone, Debug)]
pub struct Model {
    pub m: Market,
    pub rent: Rent,
    pub util_cap_bps: u64,
}

impl Model {
    pub fn new(util_cap_bps: u64) -> Self {
        Self::with_params(util_cap_bps, LUMP_VEST_S, LOCK_GRACE_S)
    }

    pub fn with_params(util_cap_bps: u64, lump_vest_s: u32, lock_grace_s: u32) -> Self {
        Self {
            m: Market::with_params(lump_vest_s, lock_grace_s),
            rent: Rent::default(),
            util_cap_bps,
        }
    }

    pub fn deposit(&mut self, u: usize, amount: u64) -> Result<u128, MathError> {
        self.m.deposit(u, amount)
    }

    pub fn withdraw(&mut self, u: usize, shares: u128) -> Result<u64, MathError> {
        self.m.withdraw(u, shares)
    }

    pub fn free_allowance(&mut self, u: usize) -> Result<u64, MathError> {
        self.m.allowance(u)
    }

    pub fn claim(&mut self, u: usize) -> Result<u64, MathError> {
        self.m.claim(u)
    }

    pub fn touch(&mut self, u: usize) -> Lender {
        self.m.touch(u).expect("touch");
        *self.m.lender(u).expect("lender")
    }

    pub fn shares(&self, u: usize) -> u128 {
        self.m.lender(u).map_or(0, |l| l.shares)
    }

    /// Shares that burn `allow` tokens: `⌊allow·(S + VS)/(T + ΣB_Q + VA)⌋`.
    pub fn shares_at(&self, allow: u64) -> u128 {
        let p = &self.m.pool;
        let claims = u128::from(p.tokens_free) + u128::from(p.borrowed_q) + VIRTUAL_ASSETS;
        mul_div(
            u128::from(allow),
            p.total_shares + VIRTUAL_SHARES,
            claims,
            Rounding::Floor,
        )
        .expect("fits")
    }

    /// `pool_model.Pool.borrow(pid, q, duration, tail)`: releases the vest, then refuses
    /// above the utilisation cap (`None`) or opens.
    pub fn borrow(&mut self, q: u64, duration: i64, tail: i64) -> Option<u64> {
        let (t, b) = (
            u128::from(self.m.pool.tokens_free),
            u128::from(self.m.pool.borrowed_q),
        );
        if q == 0
            || q > self.m.pool.tokens_free
            || (b + u128::from(q)) * BPS > u128::from(self.util_cap_bps) * (t + b)
        {
            self.m.release().expect("release");
            return None;
        }
        let deadline = self.m.now + duration;
        self.m.borrow(q, deadline + tail).expect("borrow");
        Some(self.rent.open(q, deadline))
    }

    pub fn borrow30(&mut self, q: u64) -> u64 {
        self.borrow(q, 30 * DAY, S3_TAIL_S).expect("borrow")
    }

    /// `pool_model.Pool.advance(dt, d_index)`.
    pub fn advance(&mut self, dt: i64, d_index: u128) {
        let c = self.rent.accrue(d_index);
        self.m.accrue(dt, c).expect("accrue");
    }

    pub fn repay(&mut self, id: u64, rent_paid: u64, lump: u64) -> Result<(), MathError> {
        let pos = self.rent.get(id);
        let credited = self.rent.credited_for(&pos);
        self.m.repay(pos.q, rent_paid, pos.p, credited, lump)?;
        self.rent.take(id);
        Ok(())
    }

    /// `pool_model.Pool.settle(pid, sol_in, lump)`: the rent `R_due` is the model's default,
    /// the position's accrued rent ([`Self::owed`]).
    pub fn settle(&mut self, id: u64, sol_in: u64, lump: u64) -> Result<(), MathError> {
        self.settle_above(id, sol_in, lump, 0)
    }

    /// `pool_model.Pool.settle(pid, sol_in, lump, rent)` with `R_due` = the accrued rent +
    /// `excess` (an early settlement's `R_min` above it; finding M10-8C-1).
    pub fn settle_above(
        &mut self,
        id: u64,
        sol_in: u64,
        lump: u64,
        excess: u64,
    ) -> Result<(), MathError> {
        let pos = self.rent.get(id);
        let credited = self.rent.credited_for(&pos);
        let rent = self.rent.owed_sat(&pos).saturating_add(excess);
        self.m.settle(pos.q, sol_in, credited, lump, rent)?;
        self.rent.take(id);
        Ok(())
    }

    pub fn owed(&self, id: u64) -> u64 {
        self.rent.owed(&self.rent.get(id))
    }

    pub fn sweep(&mut self, lamports: u64) {
        self.m.sweep(lamports).expect("sweep");
    }

    /// Token value of a converted lender (`pool_model.token_value`).
    pub fn token_value(&mut self, u: usize) -> u128 {
        let l = self.touch(u);
        pool::token_value(&self.m.pool, &l).expect("value")
    }

    /// `pool_model.Pool.wind_down_liveness` on a copy: every position repaid in full, the
    /// vest released, everyone touched, two rounds of claims. Returns (paid, stranded,
    /// unpaid).
    pub fn wind_down(&self) -> (u64, u64, u128) {
        let mut w = self.clone();
        for pos in w.rent.positions.clone() {
            let owed = w.rent.owed(&pos);
            w.repay(pos.id, owed, 0).expect("wind-down repay");
        }
        w.advance(i64::from(w.m.lump_vest_s) + 1, 0);
        let users: Vec<usize> = w.m.lenders.iter().map(|(u, _)| *u).collect();
        for &u in &users {
            w.touch(u);
        }
        let mut paid = 0u64;
        for _ in 0..2 {
            for &u in &users {
                paid += w.m.claim(u).expect("claim");
            }
        }
        let unpaid =
            w.m.lenders
                .iter()
                .map(|(_, l)| u128::try_from(l.pending.max(0)).expect("non-negative"))
                .sum();
        (paid, w.m.vault.lender_sol, unpaid)
    }
}

#[test]
fn crc32_matches_zlib() {
    // zlib.crc32(b"123456789") == 0xCBF43926; chaining equals one pass.
    assert_eq!(crc32(0, b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(crc32(0, b"1234"), b"56789"), 0xCBF4_3926);
}
