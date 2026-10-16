//! `pool_model` traces replayed through the program (`crates/shortwin-math/tests/vectors/pool`,
//! written by `gen_pool.py` from `docs/design/sim/pool_model.py` v0.4.2).
//!
//! Each trace line is one or more `pool_model` operations. Lender operations run as program
//! instructions through the [`Lab`] (so every one is checked against the math crate on the
//! stored pre-state and followed by the invariant hooks); the borrower side (`b`, `x`, `r`,
//! `s`, `p`, `R`, `k`) is injected with the same `pool` functions. A host model — the math
//! crate driven exactly as `pool_model` drives its pool, which `shortwin-math`'s own
//! `pool_vectors` test proves equal to the traces — runs alongside:
//!
//! - every expected result in the trace (`=VALUE`, `!CODE`) must be the host model's, on the
//!   traces without `check_each_line` (the demos, the stranding seeds, the extended
//!   profiles), whose model touches nobody between operations; the others (the lock-heavy
//!   mix, the ECON §9.6 fuzz) touch every lender after every line, a different rounding path,
//!   and serve as operation scripts;
//! - after every line the program's stored `Market` pool group must equal the model's
//!   exactly, and so must the lock state and calendar of `MarketVault`, every `ShareEpoch`
//!   record, and every lender's token side (shares, lock, lock term, `lock_until`, epochs,
//!   touched on copies).
//!
//! The program's `withdraw` pays the SOL part at once (`pool_model`'s `withdraw` does not);
//! that moves SOL from the vault, and from the pot (`unmatched`), to the lender earlier than
//! the model does, never tokens or shares. The SOL sides then drift apart, either way, and a
//! withdrawal's `pending ≥ 0` check (C0-13) reads them:
//! - an on-chain lender's `pending` below 0 while the model's is not (the program paid it
//!   earlier): the lender covers the difference first (`cover_deficit`);
//! - the model's `pending` below 0 while the on-chain lender's is not (an earlier claim paid
//!   it less on chain: another lender's withdrawal had already drawn the pot or `lender_sol`
//!   that the claim drew in the model): the model refuses `NegativePending` where the program
//!   would carry the withdrawal out. It is sent with an unreachable `min_sol` (`u64::MAX`): the
//!   program runs it on its own state, checked against the math crate like any refusal, and
//!   reverts at the SOL bound (`SolBelowMin`) ([`Stats::sol_gaps`]).
//!
//! Either way the token side stays the model's. A refused `d` / `w` keeps the model's
//! committed release and touch ([`Lab::commit_pre`]).
//!
//! `cargo test` replays a quick subset; `SHORTWIN_TRACES=full` (best with `--release`, about
//! four minutes) replays every trace except the u64-extreme amounts and the u128 lock-term
//! ceiling (their amounts exceed what a mint can supply) and the one scripted trace whose
//! model check fails by design.

use serde_json::Value;
use shortwin::errors::ShortwinError;
use shortwin_litesvm_tests::invariants::lender::{self as inv, wind_down};
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_math::MathError;
use shortwin_math::pool::{
    self, Lender, Pool, Resolution, ShareEpoch as MathShareEpoch, SweepSource, Vault,
};

use crate::common::{Lab, MAX_RECORDS_PER_TX, code, write_zc};

/// Short error codes of the vectors (`pool_harness::code`).
fn vcode(e: MathError) -> &'static str {
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

fn show<T: ToString>(r: &Result<Option<T>, MathError>) -> String {
    match r {
        Ok(Some(v)) => format!("={}", v.to_string()),
        Ok(None) => String::new(),
        Err(e) => format!("!{}", vcode(*e)),
    }
}

/// The host model: the math crate driven as `pool_model` drives its pool (the math crate's
/// `pool_harness::Market`, without the touch-all of `check()`), in unix time.
struct Model {
    pool: Pool,
    vault: Box<Vault>,
    now: i64,
    lump_vest_s: u32,
    lock_grace_s: u32,
    records: Vec<MathShareEpoch>,
    lenders: Vec<(usize, Lender)>,
}

fn recs_from(records: &[MathShareEpoch], epoch: u32) -> &[MathShareEpoch] {
    &records[(epoch as usize).min(records.len())..]
}

fn recs_from_mut(records: &mut [MathShareEpoch], epoch: u32) -> &mut [MathShareEpoch] {
    let n = records.len();
    &mut records[(epoch as usize).min(n)..]
}

impl Model {
    fn new(now: i64, lump_vest_s: u32, lock_grace_s: u32) -> Self {
        Self {
            pool: Pool::new(),
            vault: Box::new(Vault::new()),
            now,
            lump_vest_s,
            lock_grace_s,
            records: Vec::new(),
            lenders: Vec::new(),
        }
    }

    fn slot(&mut self, u: usize) -> usize {
        if let Some(i) = self.lenders.iter().position(|(x, _)| *x == u) {
            return i;
        }
        self.lenders.push((u, Lender::new(&self.pool)));
        self.lenders.len() - 1
    }

    fn lender(&self, u: usize) -> Option<&Lender> {
        self.lenders.iter().find(|(x, _)| *x == u).map(|(_, l)| l)
    }

    fn touch(&mut self, u: usize) -> Result<(), MathError> {
        let i = self.slot(u);
        let l = &mut self.lenders[i].1;
        pool::touch(
            &self.pool,
            &mut self.vault,
            l,
            recs_from_mut(&mut self.records, l.epoch),
            self.now,
        )
        .map(|_| ())
    }

    /// KI-66: the pot-only touch (a third party's `sync_lender`).
    fn touch_pot(&mut self, u: usize) -> Result<(), MathError> {
        let i = self.slot(u);
        let l = &mut self.lenders[i].1;
        pool::touch_pot(
            &self.pool,
            &mut self.vault,
            l,
            recs_from(&self.records, l.epoch),
            self.now,
        )
        .map(|_| ())
    }

    /// KI-66 full: `sync_lender(n)` with the `n` records from where it starts
    /// (`pool_harness::Market::sync`): signed, `convert` then `touch` unless still behind;
    /// unsigned, `touch_pot` from the lender's epoch or its chain's `fwd_epoch`.
    fn sync(&mut self, u: usize, n: usize, signed: bool) -> Result<(), MathError> {
        let i = self.slot(u);
        let (mut v, mut l) = (self.vault.clone(), self.lenders[i].1);
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
            if st != pool::Standing::Behind {
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

    fn pre(&mut self, u: usize) -> Result<(), MathError> {
        let i = self.slot(u);
        let (mut p, mut v, mut l) = (self.pool, self.vault.clone(), self.lenders[i].1);
        pool::release(&mut p, self.now)?;
        let e = l.epoch;
        pool::touch(
            &p,
            &mut v,
            &mut l,
            recs_from_mut(&mut self.records, e),
            self.now,
        )?;
        self.pool = p;
        self.vault = v;
        self.lenders[i].1 = l;
        Ok(())
    }

    fn deposit(&mut self, u: usize, amount: u64) -> Result<u128, MathError> {
        self.pre(u)?;
        let i = self.slot(u);
        let l = &mut self.lenders[i].1;
        pool::deposit(
            &mut self.pool,
            &mut self.vault,
            l,
            recs_from_mut(&mut self.records, l.epoch),
            self.now,
            amount,
            false,
        )
    }

    fn withdraw(&mut self, u: usize, shares: u128) -> Result<u64, MathError> {
        self.pre(u)?;
        let i = self.slot(u);
        let l = &mut self.lenders[i].1;
        pool::withdraw(
            &mut self.pool,
            &mut self.vault,
            l,
            recs_from_mut(&mut self.records, l.epoch),
            self.now,
            self.lock_grace_s,
            shares,
        )
    }

    fn allowance(&mut self, u: usize) -> Result<u64, MathError> {
        self.touch(u)?;
        pool::allowance(
            &self.pool,
            &self.vault,
            self.lender(u).expect("touched"),
            self.now,
        )
    }

    fn allowance_view(&mut self, u: usize) -> Result<u64, MathError> {
        let i = self.slot(u);
        let l = self.lenders[i].1;
        pool::allowance_view(
            &self.pool,
            &self.vault,
            &l,
            recs_from(&self.records, l.epoch),
            self.now,
        )
    }

    fn claim(&mut self, u: usize) -> Result<u64, MathError> {
        let i = self.slot(u);
        let l = &mut self.lenders[i].1;
        pool::claim(
            &self.pool,
            &mut self.vault,
            l,
            recs_from_mut(&mut self.records, l.epoch),
            self.now,
        )
    }

    fn cover(&mut self, u: usize, lamports: u64) -> Result<(), MathError> {
        let i = self.slot(u);
        let l = &mut self.lenders[i].1;
        pool::cover(
            &self.pool,
            &mut self.vault,
            l,
            recs_from_mut(&mut self.records, l.epoch),
            self.now,
            lamports,
        )
    }

    fn lock_of(&mut self, u: usize) -> Result<u128, MathError> {
        let i = self.slot(u);
        pool::lock_of(&self.vault, &self.lenders[i].1, self.now)
    }

    fn accrue(&mut self, dt: i64, lamports: u64) -> Result<(), MathError> {
        let now = self.now.checked_add(dt).ok_or(MathError::Overflow)?;
        pool::accrue(&mut self.pool, &mut self.vault, now, lamports)?;
        self.now = now;
        Ok(())
    }

    fn settle(
        &mut self,
        q: u64,
        sol_in: u64,
        credited: u64,
        lump: u64,
        rent: u64,
    ) -> Result<(), MathError> {
        let res = Resolution { q, credited, lump };
        if let Some(r) = pool::settle(
            &mut self.pool,
            &mut self.vault,
            self.now,
            self.lump_vest_s,
            &res,
            sol_in,
            rent,
        )?
        .rec
        {
            self.records.push(r);
        }
        Ok(())
    }

    fn sweep(&mut self, lamports: u64) -> Result<(), MathError> {
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
            SweepSource::Trader,
        )
    }
}

/// What a replay saw.
#[derive(Default, Debug)]
struct Stats {
    lines: usize,
    ops: usize,
    txs_refused: usize,
    covers_added: usize,
    /// Withdrawals the model refused with `NegativePending` while the on-chain lender's
    /// `pending` was not below 0 (sent with `min_sol = u64::MAX`, see the module docs).
    sol_gaps: usize,
    final_lender_sol: u64,
    final_unallocated: u64,
    /// The host model's own unallocated realised SOL at the same point (rounding; since KI-65
    /// no SOL is credited to rebase-orphan shares, ECON §9.5): the program's differs only by
    /// the rounding of its earlier SOL payments.
    model_unallocated: u64,
}

/// The host model as a [`inv::PoolState`] (for the wind-down).
fn model_state(m: &Model, market: anchor_lang::prelude::Pubkey) -> inv::PoolState {
    let lenders = m
        .lenders
        .iter()
        .map(|(u, l)| {
            let mut pos = shortwin::state::LenderPosition {
                owner: anchor_lang::prelude::Pubkey::new_from_array(
                    [u8::try_from(*u).unwrap() + 1; 32],
                ),
                market,
                shares: 0,
                snap: 0,
                pending: 0,
                snap_r: 0,
                real: 0,
                claimed: 0,
                epoch: 0,
                lock: 0,
                lock_pi: 0,
                lock_epoch: 0,
                lock_term: 0,
                lock_until: 0,
                bump: 0,
                fwd_shares: 0,
                fwd_epoch: 0,
                fwd: 0,
                fwd_lag: 0,
                _reserved: [0; 10],
            };
            pos.set_lender(l);
            (pos.owner, pos)
        })
        .collect();
    inv::PoolState {
        market,
        pool: m.pool,
        vault: m.vault.clone(),
        lenders,
        records: m.records.clone(),
        open_positions: 0,
        now: m.now,
    }
}

fn parse_op(op: &str) -> (&str, Vec<&str>, Option<&str>) {
    let mut words: Vec<&str> = op.split(' ').filter(|w| !w.is_empty()).collect();
    let expected = match words.last() {
        Some(w) if w.starts_with('=') || w.starts_with('!') => words.pop(),
        _ => None,
    };
    (words[0], words[1..].to_vec(), expected)
}

fn n<T: std::str::FromStr>(args: &[&str], i: usize) -> T {
    args[i]
        .parse()
        .unwrap_or_else(|_| panic!("bad arg {i} in {args:?}"))
}

/// `name`'s on-chain `pending` as the program's `withdraw` finds it: after its own vest release
/// and touch (on copies).
fn chain_pending(lab: &Lab, name: &str) -> i64 {
    let st = lab.state();
    let mut l = st.lender_of(&lab.users[name].pubkey()).unwrap().lender();
    let (mut p, mut v) = (st.pool, st.vault.clone());
    pool::release(&mut p, st.now).unwrap();
    let e = l.epoch;
    pool::touch(&p, &mut v, &mut l, &mut st.records_from(e).to_vec(), st.now).unwrap();
    l.pending
}

/// The program's error for a model refusal.
fn prog(e: MathError) -> u32 {
    code(ShortwinError::from(e))
}

/// Compares the stored state with the model's after a line (see the module docs).
fn compare(lab: &Lab, m: &Model, users: &[String], ctx: &str) {
    let st = lab.state();
    assert_eq!(st.pool, m.pool, "{ctx}: Market pool state");
    let (mut a, mut b) = (st.vault.clone(), m.vault.clone());
    pool::cal_advance(&mut a, m.now).unwrap();
    pool::cal_advance(&mut b, m.now).unwrap();
    assert_eq!(
        (
            a.pi,
            a.pi_epoch,
            a.lock_norm,
            a.b_min,
            a.cal_day,
            a.lock_cal
        ),
        (
            b.pi,
            b.pi_epoch,
            b.lock_norm,
            b.b_min,
            b.cal_day,
            b.lock_cal
        ),
        "{ctx}: MarketVault lock state"
    );
    assert_eq!(st.records, m.records, "{ctx}: ShareEpoch records");
    for (u, name) in users.iter().enumerate() {
        let Some(owner) = lab.users.get(name).map(crate::common::User::pubkey) else {
            continue;
        };
        let Some(on) = st.lender_of(&owner) else {
            continue;
        };
        let Some(ml) = m.lender(u) else {
            panic!("{ctx}: {name} has a position on chain but not in the model");
        };
        let mut lc = on.lender();
        let mut va = st.vault.clone();
        let e = lc.epoch;
        pool::touch(
            &st.pool,
            &mut va,
            &mut lc,
            &mut st.records_from(e).to_vec(),
            m.now,
        )
        .unwrap();
        let mut lm = *ml;
        let mut vb = m.vault.clone();
        let e = lm.epoch;
        pool::touch(
            &m.pool,
            &mut vb,
            &mut lm,
            &mut recs_from(&m.records, e).to_vec(),
            m.now,
        )
        .unwrap();
        // A lock's Π, epoch, expiry and term mean nothing without a lock (a term without a
        // lock is never read again: a position the program closed and re-created has the
        // defaults, KI-67; the model keeps the stale values).
        let tok = |l: &Lender| {
            if l.lock == 0 {
                (l.shares, 0, 0, 0, 0, 0, l.epoch, l.snap, l.snap_r)
            } else {
                (
                    l.shares,
                    l.lock,
                    l.lock_pi,
                    l.lock_epoch,
                    l.lock_term,
                    l.lock_until,
                    l.epoch,
                    l.snap,
                    l.snap_r,
                )
            }
        };
        assert_eq!(tok(&lc), tok(&lm), "{ctx}: lender {name} token side");
    }
}

/// Points the I-P6 liveness hook at ECON's own figure (M1-FID-8): whenever the host model has
/// nothing borrowed and nothing receivable (the state the hook's wind-down runs on), the cap on
/// the realised SOL no lender is allocated becomes the model's wind-down figure — rounding (no
/// rebase-orphan credit since KI-65, ECON §9.5) — plus one lamport per lender, never
/// below [`inv::DEFAULT_LIVENESS_DUST_CAP`]. Called after each model operation, before the
/// program's, so the hook compares the program with the model at the same point.
fn liveness_cap_from_model(m: &Model, lab: &Lab) {
    if m.pool.borrowed_q != 0 || m.pool.receivable != 0 {
        return;
    }
    let st = model_state(m, lab.k.market);
    let slack = u64::try_from(st.lenders.len()).unwrap() + 1;
    let model = wind_down(&st).map(|w| w.unallocated).unwrap_or(u64::MAX);
    inv::set_liveness_dust_cap(
        model
            .saturating_add(slack)
            .max(inv::DEFAULT_LIVENESS_DUST_CAP),
    );
}

/// Replays `trace` (at most `max_lines` lines) on `program`.
///
/// `pool_model` borrows a pool's orphan tokens while `S = 0` (a borrower-side step the traces
/// inject; M2's `open_short` must refuse it, ARCH §9 M0b note 4), so the replay relaxes the
/// hook's `S = 0 ⇒ ΣB_Q = 0` for its market ([`inv::allow_shareless_borrow`]); every other
/// check runs, and the liveness cap follows the model ([`liveness_cap_from_model`]).
fn replay(trace: &Value, program: TokenProgram, max_lines: usize) -> Stats {
    let name = trace["name"].as_str().unwrap();
    let dec = |k: &str, d: u64| {
        trace
            .get(k)
            .and_then(Value::as_str)
            .map_or(d, |s| s.parse().unwrap())
    };
    let lump_vest_s = u32::try_from(dec("lump_vest_s", 21_600)).unwrap();
    let lock_grace_s = u32::try_from(dec("lock_grace_s", 86_400)).unwrap();
    let cap = u32::try_from(dec("util_cap_bps", 9_000)).unwrap();
    let check_each_line = trace["check_each_line"].as_bool().unwrap_or(false);
    let users: Vec<String> = trace["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u.as_str().unwrap().to_owned())
        .collect();

    // The trace's parameters, written into the config (in bounds; governance would take
    // 48 h, which the traces do not model).
    let mut env = shortwin_litesvm_tests::Env::initialized();
    let mut c = env.config();
    c.params.lump_vest_s = lump_vest_s;
    c.params.lock_grace_s = lock_grace_s;
    write_zc(&mut env, &shortwin_litesvm_tests::pda::config(), &c, 0);
    let mut lab = Lab::on(env, program, cap);
    inv::allow_shareless_borrow(true);
    inv::set_liveness_dust_cap(inv::DEFAULT_LIVENESS_DUST_CAP);
    let base = lab.base;
    let mut m = Model::new(base, lump_vest_s, lock_grace_s);
    let mut stats = Stats::default();

    for (i, line) in trace["lines"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .take(max_lines)
    {
        let line = line.as_str().unwrap();
        let body = line.rsplit_once('#').map_or(line, |(b, _)| b.trim_end());
        for op in body.split(';').filter(|s| !s.trim().is_empty()) {
            let (o, args, expected) = parse_op(op.trim());
            let ctx = format!("{name} line {i} `{op}`");
            stats.ops += 1;
            let u = || args[0].parse::<usize>().expect("user");
            let who = |lab: &Lab, u: usize| -> (String, bool) {
                let nm = users[u].clone();
                let exists = lab
                    .users
                    .get(&nm)
                    .is_some_and(|x| lab.env.lender(&lab.k.market, &x.pubkey()).is_some());
                (nm, exists)
            };
            let got: String = match o {
                "d" => {
                    let amount: u64 = n(&args, 1);
                    let r = m.deposit(u(), amount);
                    liveness_cap_from_model(&m, &lab);
                    let (nm, exists) = who(&lab, u());
                    let chain = lab.deposit(&nm, amount);
                    match (&r, &chain) {
                        (Ok(a), Ok(b)) => assert_eq!(a, b, "{ctx}: minted"),
                        (Err(e), Err(c)) => {
                            assert_eq!(*c, prog(*e), "{ctx}: refusal");
                            stats.txs_refused += 1;
                            if exists || lab.position(&nm).is_some() {
                                lab.commit_pre(&nm).unwrap();
                            } else {
                                lab.release();
                            }
                        }
                        _ => panic!("{ctx}: model {r:?}, program {chain:?}"),
                    }
                    show(&r.map(Some))
                }
                "w" => {
                    let shares: u128 = n(&args, 1);
                    let (nm, exists) = who(&lab, u());
                    let r = m.withdraw(u(), shares);
                    liveness_cap_from_model(&m, &lab);
                    if !exists {
                        assert!(
                            r.is_err(),
                            "{ctx}: model withdrew without a position on chain"
                        );
                        lab.release();
                    } else if r == Err(MathError::NegativePending) && chain_pending(&lab, &nm) >= 0
                    {
                        // The model's deficit is its own: an earlier claim paid this lender less
                        // on chain (module docs). The program would burn; with an unreachable
                        // `min_sol` it runs the withdrawal and reverts at the SOL bound, and at
                        // nothing earlier (any other refusal, `NegativePending` included, means
                        // `chain_pending` missed the program's view).
                        let chain = lab.withdraw_bounded(&nm, shares, 0, u64::MAX);
                        assert_eq!(
                            chain,
                            Err(code(ShortwinError::SolBelowMin)),
                            "{ctx}: program {chain:?}"
                        );
                        stats.sol_gaps += 1;
                        lab.commit_pre(&nm).unwrap();
                    } else {
                        if r != Err(MathError::NegativePending) {
                            // The program paid SOL earlier than the model: cover a deficit that
                            // exists only on chain, so the token side stays the model's.
                            let st = lab.state();
                            let l = st.lender_of(&lab.users[&nm].pubkey()).unwrap().lender();
                            let mut x = l;
                            let mut v = st.vault.clone();
                            pool::touch(
                                &st.pool,
                                &mut v,
                                &mut x,
                                &mut st.records_from(l.epoch).to_vec(),
                                st.now,
                            )
                            .unwrap();
                            if x.pending < 0 {
                                lab.cover(&nm, u64::try_from(-x.pending).unwrap()).unwrap();
                                stats.covers_added += 1;
                            }
                        }
                        let chain = lab.withdraw(&nm, shares);
                        match (&r, &chain) {
                            (Ok(a), Ok((b, _))) => assert_eq!(a, b, "{ctx}: tokens"),
                            (Err(e), Err(c)) => {
                                assert_eq!(*c, prog(*e), "{ctx}: refusal");
                                stats.txs_refused += 1;
                                lab.commit_pre(&nm).unwrap();
                            }
                            _ => panic!("{ctx}: model {r:?}, program {chain:?}"),
                        }
                    }
                    show(&r.map(Some))
                }
                "a" => {
                    let r = m.allowance(u());
                    liveness_cap_from_model(&m, &lab);
                    let (nm, exists) = who(&lab, u());
                    if exists {
                        assert_eq!(Ok(lab.free_allowance(&nm)), r, "{ctx}: allowance");
                    }
                    show(&r.map(Some))
                }
                "A" => {
                    let r = m.allowance_view(u());
                    let (nm, exists) = who(&lab, u());
                    if exists {
                        let st = lab.state();
                        let l = st.lender_of(&lab.users[&nm].pubkey()).unwrap().lender();
                        let on = pool::allowance_view(
                            &st.pool,
                            &st.vault,
                            &l,
                            st.records_from(l.epoch),
                            st.now,
                        );
                        assert_eq!(on, r, "{ctx}: allowance_view");
                    }
                    show(&r.map(Some))
                }
                "c" => {
                    let r = m.claim(u());
                    liveness_cap_from_model(&m, &lab);
                    let (nm, exists) = who(&lab, u());
                    if exists {
                        lab.claim(&nm).unwrap();
                    }
                    show(&r.map(Some))
                }
                "v" => {
                    let x: u64 = n(&args, 1);
                    let r = m.cover(u(), x);
                    liveness_cap_from_model(&m, &lab);
                    let (nm, exists) = who(&lab, u());
                    if exists {
                        if x > 0 {
                            lab.cover(&nm, x).unwrap();
                        } else {
                            lab.touch(&nm);
                        }
                    }
                    show(&r.map(|()| None::<u8>))
                }
                "t" => {
                    let r = m.touch(u());
                    liveness_cap_from_model(&m, &lab);
                    let (nm, exists) = who(&lab, u());
                    if exists {
                        lab.touch(&nm);
                    }
                    show(&r.map(|()| None::<u8>))
                }
                "h" => {
                    // KI-66: a third party's sync_lender, with the records it may need.
                    let r = m.touch_pot(u());
                    liveness_cap_from_model(&m, &lab);
                    let (nm, exists) = who(&lab, u());
                    if exists {
                        let steps = u8::try_from(MAX_RECORDS_PER_TX).expect("u8");
                        lab.sync(&nm, steps).unwrap();
                    }
                    show(&r.map(|()| None::<u8>))
                }
                "y" => {
                    // KI-66 full: sync_lender(n), the owner's (1) or a third party's (0).
                    let (k, signed): (usize, u8) = (n(&args, 1), n(&args, 2));
                    let r = m.sync(u(), k, signed == 1);
                    liveness_cap_from_model(&m, &lab);
                    let (nm, exists) = who(&lab, u());
                    if exists {
                        let k = u8::try_from(k).expect("u8");
                        if signed == 1 {
                            lab.sync_signed(&nm, k).unwrap();
                        } else {
                            lab.sync(&nm, k).unwrap();
                        }
                    }
                    show(&r.map(|()| None::<u8>))
                }
                "l" => show(&m.lock_of(u()).map(Some)),
                "k" => {
                    let r = pool::cal_advance(&mut m.vault, m.now);
                    liveness_cap_from_model(&m, &lab);
                    lab.cal_advance_raw().unwrap();
                    show(&r.map(|()| None::<u8>))
                }
                "R" => {
                    let r = pool::release(&mut m.pool, m.now).map(|_| ());
                    liveness_cap_from_model(&m, &lab);
                    lab.release();
                    show(&r.map(|()| None::<u8>))
                }
                "b" => {
                    let q: u64 = n(&args, 0);
                    let rel = base + n::<i64>(&args, 1);
                    let r = pool::borrow(&mut m.pool, m.now, q, rel);
                    liveness_cap_from_model(&m, &lab);
                    assert_eq!(lab.borrow_raw(q, rel), r, "{ctx}");
                    show(&r.map(|()| None::<u8>))
                }
                "x" => {
                    let dt: i64 = n(&args, 0);
                    let lam: u64 = n(&args, 1);
                    let r = m.accrue(dt, lam);
                    liveness_cap_from_model(&m, &lab);
                    assert_eq!(lab.accrue_raw(dt, lam), r, "{ctx}");
                    show(&r.map(|()| None::<u8>))
                }
                "r" => {
                    let (q, rent, p, cred, lump) = (
                        n(&args, 0),
                        n(&args, 1),
                        n(&args, 2),
                        n(&args, 3),
                        n(&args, 4),
                    );
                    let res = Resolution {
                        q,
                        credited: cred,
                        lump,
                    };
                    let r = pool::repay(
                        &mut m.pool,
                        &mut m.vault,
                        m.now,
                        m.lump_vest_s,
                        &res,
                        rent,
                        p,
                    )
                    .map(|_| ());
                    liveness_cap_from_model(&m, &lab);
                    assert_eq!(lab.repay_raw(q, rent, p, cred, lump), r, "{ctx}");
                    show(&r.map(|()| None::<u8>))
                }
                "s" => {
                    let (q, sol, cred, lump, rent) = (
                        n(&args, 0),
                        n(&args, 1),
                        n(&args, 2),
                        n(&args, 3),
                        n(&args, 4),
                    );
                    let r = m.settle(q, sol, cred, lump, rent);
                    liveness_cap_from_model(&m, &lab);
                    assert_eq!(lab.settle_raw(q, sol, cred, lump, rent), r, "{ctx}");
                    show(&r.map(|()| None::<u8>))
                }
                "p" => {
                    let lam: u64 = n(&args, 0);
                    let r = m.sweep(lam);
                    liveness_cap_from_model(&m, &lab);
                    assert_eq!(lab.sweep_raw(lam), r, "{ctx}");
                    show(&r.map(|()| None::<u8>))
                }
                _ => panic!("{ctx}: unknown op"),
            };
            // With `check_each_line` pool_model touches every lender after every line; each
            // touch floors `pending` and `real`, so its SOL side (and a refusal that reads
            // `pending`) follows a different rounding path than a pool nobody touches, and
            // the token side can follow once such a refusal differs. Those traces are an op
            // script here: the program is held to the host model, line by line.
            if let Some(e) = expected
                && !check_each_line
            {
                assert_eq!(got, e, "{ctx}: host model vs pool_model");
            }
        }
        compare(&lab, &m, &users, &format!("{name} after line {i}"));
        stats.lines += 1;
    }
    stats.final_lender_sol = lab.vault().lender_sol;
    let st = lab.state();
    if st.pool.borrowed_q == 0 && st.pool.receivable == 0 {
        stats.final_unallocated = wind_down(&st).map(|w| w.unallocated).unwrap_or(0);
        stats.model_unallocated = wind_down(&model_state(&m, lab.k.market))
            .map(|w| w.unallocated)
            .unwrap_or(0);
        // ECON §9.5's bound is the model's own figure: the program strands exactly as much
        // (M1-FID-8).
        assert_eq!(
            stats.final_unallocated, stats.model_unallocated,
            "{name}: unallocated SOL after the wind-down differs from the model"
        );
    }
    inv::allow_shareless_borrow(false);
    inv::set_liveness_dust_cap(inv::DEFAULT_LIVENESS_DUST_CAP);
    stats
}

fn load(file: &str) -> Vec<Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/shortwin-math/tests/vectors/pool")
        .join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let v: Value = serde_json::from_str(&text).unwrap();
    v["traces"].as_array().unwrap().clone()
}

fn full() -> bool {
    std::env::var("SHORTWIN_TRACES").is_ok_and(|v| v == "full")
}

/// T-POT: "I-P4 holds on the untouched state over the lock-heavy mix" — the reviewers' lock
/// fuzz (6 seeds × 4,000 operations, ECON §9.3), every lender instruction through the program
/// and the untouched-state I-P4 check after every transaction.
#[test]
fn lock_heavy_mix_keeps_i_p4_on_the_untouched_state() {
    let traces = load("pool_lock_fuzz_1.json");
    let lines = if full() { usize::MAX } else { 1_000 };
    for (k, t) in traces.iter().enumerate() {
        let s = replay(t, TokenProgram::BOTH[k % 2], lines);
        println!("{}: {s:?}", t["name"]);
    }
}

/// T-POT / R3A-2: the reviewer's 40-seed stranding table (`pool_model.stranded_table()`,
/// v0.4 rows: 15,044 lamports stranded over 50,850 SOL of inflow, worst seed 435 — 15,058 over
/// 51,186 SOL and 440 before AIR-07's drained-pool gate refused some of the table's deposits,
/// 19,887 and 582 before AIR-03 carried the rent credit's remainder, 20,399 and 596 before
/// M10-8C-1's vested rent top-up). Through the program (which pays the SOL part of withdrawals
/// at once) nothing is stranded beyond rounding either.
#[test]
fn stranding_table_wind_down_strands_only_dust() {
    // Quick: the first seed of two files; full: all 40 seeds.
    let (files, per_file) = if full() { (5, usize::MAX) } else { (2, 1) };
    let mut total = 0u64;
    let mut worst = 0u64;
    let mut seeds = 0;
    for k in 1..=files {
        for t in load(&format!("pool_stranded_{k}.json"))
            .iter()
            .take(per_file)
        {
            let s = replay(t, TokenProgram::Spl, usize::MAX);
            println!("{}: {s:?}", t["name"]);
            total += s.final_lender_sol;
            worst = worst.max(s.final_lender_sol);
            seeds += 1;
            assert!(
                s.final_lender_sol <= 10_000_000,
                "{}: stranded {}",
                t["name"],
                s.final_lender_sol
            );
        }
    }
    println!("stranding: {seeds} seeds, total {total} lamports, worst {worst}");
    if full() {
        // Exactly the model's R3A-2 row (the generator's `source` line of these files).
        assert_eq!((seeds, total, worst), (40, 15_044, 435));
    }
}

/// The `pool_model` demos (ECON §9.3–§9.6, round-3 and round-4 cases), every result checked.
#[test]
fn pool_model_scenarios_replay() {
    for (k, t) in load("pool_scenarios_1.json").iter().enumerate() {
        let s = replay(t, TokenProgram::BOTH[k % 2], usize::MAX);
        assert!(s.lines > 0);
    }
}

/// The extended profiles: lazy lenders many epochs behind (repeated wipes, deep rebases),
/// runs and churn, dust amounts, exits whose rent top-up vests (M10-8C-1; not the u64-extreme
/// profile) with the scripted JIT whale, KI-65's lazy conversion orders and orphan / whale demos,
/// KI-66's pot-only touches, the scripted lock-term and over-lock cases.
#[test]
fn extended_profiles_replay() {
    let lines = if full() { usize::MAX } else { 1_000 };
    for f in [
        "pool_ext_lazy_1.json",
        "pool_ext_runs_1.json",
        "pool_ext_dust_1.json",
    ] {
        for t in load(f) {
            // The replay caps the liveness hook at the model's own figure (rounding).
            let s = replay(&t, TokenProgram::Token2022, lines);
            println!("{}: {s:?}", t["name"]);
        }
    }
    for t in load("pool_ext_topup_1.json") {
        let nm = t["name"].as_str().unwrap();
        if nm != "ext_topup_extreme_100" {
            let s = replay(
                &t,
                TokenProgram::BOTH[usize::from(nm.ends_with("_90"))],
                lines,
            );
            println!("{nm}: {s:?}");
        }
    }
    // KI-65: lazy lenders converting in random orders through deep rebases, dust residues,
    // whole exits during a vest, and the orphan / whale demos' v0.4.2 pools.
    for t in load("pool_ext_orphans_1.json") {
        let nm = t["name"].as_str().unwrap();
        let s = replay(
            &t,
            TokenProgram::BOTH[usize::from(nm.contains("dust"))],
            lines,
        );
        println!("{nm}: {s:?}");
    }
    // KI-66: third parties' syncs of lazy lenders behind rebase records (the pot-only touch).
    for t in load("pool_ext_pot_1.json") {
        let nm = t["name"].as_str().unwrap();
        let s = replay(
            &t,
            TokenProgram::BOTH[usize::from(nm.contains("dust"))],
            lines,
        );
        println!("{nm}: {s:?}");
        if nm == "ext_pot_fwd_dust_100" {
            // Seed 66: withdrawals the model refuses on its own SOL side (module docs), at
            // lines 208, 304, 457 and 464 (all within the quick subset).
            assert_eq!(s.sol_gaps, 4, "{nm}");
        }
    }
    for t in load("pool_ext_extreme_1.json") {
        let nm = t["name"].as_str().unwrap();
        if nm.starts_with("ext_scripted_")
            && nm != "ext_scripted_no_shares"
            && nm != "ext_scripted_lock_terms"
        {
            let s = replay(&t, TokenProgram::Spl, usize::MAX);
            println!("{nm}: {s:?}");
        }
    }
}

/// The ECON §9.6 option-(a) fuzz runs (60,000 operations each, util caps 90 % and 100 %).
#[test]
fn econ_fuzz_runs_replay() {
    let lines = if full() { usize::MAX } else { 1_000 };
    let parts = if full() { 3 } else { 1 };
    for cap in [90, 100] {
        let mut all: Vec<String> = Vec::new();
        let mut first: Option<Value> = None;
        for k in 1..=parts {
            let t = load(&format!("pool_fuzz_continuous_{cap}_{k}.json")).remove(0);
            all.extend(
                t["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| l.as_str().unwrap().to_owned()),
            );
            first.get_or_insert(t);
        }
        let mut t = first.unwrap();
        t["lines"] = Value::Array(all.into_iter().map(Value::String).collect());
        let s = replay(&t, TokenProgram::BOTH[usize::from(cap == 100)], lines);
        println!("fuzz {cap}: {s:?}");
    }
}
