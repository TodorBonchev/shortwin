//! KI-66 full security review: the forward chain against a pool that moves between the syncs.
//! `pool_props::windowed_syncs_equal_owner_alone` fixes every record and the current
//! accumulators before the first sync; here credits (of either sign) land in the current epoch
//! between the syncs, epochs roll (rebases by `1/1,000^k`, full wipes), a live wipe record is
//! topped up and finalised, and third parties' syncs pass random record slices (starting before,
//! at or after where the walk starts, 0–8 records). Each third-party sync must write no record,
//! move no share, lock, lock term, `lock_until`, `lock_epoch` or `claimed` (outside the V2-12
//! jump and the catch-up through full wipes, which order nothing), never lower the pot, keep the
//! stored chain consistent, and repeat as a no-op; once every owner has converted, each lender
//! is bit-equal to the run without the third parties (`real` aside), the records too, and
//! `Σ real + unmatched` is equal. Third parties touch only lenders behind the pool (or chained):
//! a current lender's touch floors per touch, so touching it at two times may differ by a unit
//! from touching it once (pre-KI-66, not the chain's).

use proptest::prelude::*;
use shortwin_math::MathError;
use shortwin_math::pool::{self, Lender, NO_LOCK_EPOCH, Pool, ShareEpoch, Standing, Vault};

const DAY: i64 = 86_400;

#[derive(Clone, Debug)]
enum Ev {
    /// A credit to the current epoch (or a top-up of the live wipe record).
    Credit(i128, u128),
    /// A roll: `k` = 0 a full wipe, else a `1/1,000^k` rebase.
    Roll(u8),
    /// A deposit finalises the live wipe record.
    Finalise,
    /// A third party's sync of lender `i`: slice start offset −2..=2 from where the walk
    /// starts, `n` records.
    Third(usize, i8, u8),
    /// The owner's own sync of lender `i` with `n` records from its epoch.
    Owner(usize, u8),
}

fn ev() -> impl Strategy<Value = Ev> {
    prop_oneof![
        4 => (-10i128.pow(20)..10i128.pow(20), 0u128..10u128.pow(20)).prop_map(|(a, r)| Ev::Credit(a, r)),
        2 => prop_oneof![1 => Just(0u8), 4 => 1u8..=3].prop_map(Ev::Roll),
        1 => Just(Ev::Finalise),
        6 => (0usize..8, -2i8..=2, 0u8..=8).prop_map(|(i, o, n)| Ev::Third(i, o, n)),
        3 => (0usize..8, 1u8..=4).prop_map(|(i, n)| Ev::Owner(i, n)),
    ]
}

struct World {
    p: Pool,
    v: Vault,
    recs: Vec<ShareEpoch>,
    ls: Vec<Lender>,
}

impl World {
    fn slice(&self, from: usize, n: usize) -> (usize, usize) {
        let len = self.recs.len();
        (from.min(len), from.saturating_add(n).min(len))
    }

    fn apply(&mut self, e: &Ev, third: bool, stats: &mut [u32; 4]) -> Result<(), TestCaseError> {
        let fail = |e: MathError| TestCaseError::fail(format!("{e:?}"));
        match *e {
            Ev::Credit(a, r) => {
                if self.p.is_wipe_live() {
                    let rec = self.recs.last_mut().expect("the wipe record");
                    rec.acc_end = rec.acc_end.saturating_add(a);
                    rec.acc_r_end = rec.acc_r_end.saturating_add(r);
                } else {
                    self.p.acc = self.p.acc.saturating_add(a);
                    self.p.acc_r = self.p.acc_r.saturating_add(r);
                }
            }
            Ev::Roll(k) => {
                if self.p.is_wipe_live() || self.recs.len() >= 9 {
                    return Ok(());
                }
                let mut rec = ShareEpoch {
                    acc_end: self.p.acc,
                    acc_r_end: self.p.acc_r,
                    s_old: u128::MAX / 2,
                    carry: 0,
                    carry_r: 0,
                    conv_rem: 0,
                    epoch: self.p.share_epoch,
                    num: u8::from(k != 0),
                    den_log1000: k,
                    _pad: [0; 10],
                };
                rec.conv_rem = 0;
                self.recs.push(rec);
                self.p.share_epoch += 1;
                (self.p.acc, self.p.acc_r) = (0, 0);
                self.p.wipe_live = u8::from(k == 0);
            }
            Ev::Finalise => self.p.wipe_live = 0,
            Ev::Third(i, off, n) => {
                if !third {
                    return Ok(());
                }
                let i = i % self.ls.len();
                let b = self.ls[i];
                let parked = self.p.is_wipe_live() && b.epoch + 1 == self.p.share_epoch;
                if b.fwd == pool::FWD_NONE && (b.epoch == self.p.share_epoch || parked) {
                    // A current or parked lender's own touch floors per touch (pre-KI-66,
                    // "touches at two times"; `ki67_parked_touch_floors_per_touch`): not the
                    // forward chain's business.
                    return Ok(());
                }
                let start = if b.fwd == pool::FWD_NONE {
                    b.epoch
                } else {
                    b.fwd_epoch
                };
                let from = (i64::from(start) + i64::from(off)).max(0) as usize;
                let (lo, hi) = self.slice(from, usize::from(n));
                let before_recs = self.recs.clone();
                let pot = self.v.unmatched;
                let st =
                    pool::touch_pot(&self.p, &mut self.v, &mut self.ls[i], &self.recs[lo..hi], 0)
                        .map_err(fail)?;
                let a = self.ls[i];
                prop_assert_eq!(&self.recs, &before_recs, "a third party wrote a record");
                prop_assert!(self.v.unmatched >= pot, "the pot shrank");
                if a.epoch == b.epoch {
                    prop_assert_eq!(
                        (
                            a.shares,
                            a.lock,
                            a.lock_term,
                            a.lock_until,
                            a.lock_epoch,
                            a.claimed
                        ),
                        (
                            b.shares,
                            b.lock,
                            b.lock_term,
                            b.lock_until,
                            b.lock_epoch,
                            b.claimed
                        ),
                        "a third party moved a conversion field"
                    );
                } else {
                    // Only the V2-12 jump or the catch-up through full wipes moves an epoch.
                    prop_assert_eq!(a.fwd, pool::FWD_NONE);
                    // (`engine::lender::reads_rebase`: the record of its starting epoch.)
                    let empty = (b.shares, b.lock, b.lock_term) == (0, 0, 0);
                    prop_assert!(
                        empty || self.recs[b.epoch as usize].num == 0,
                        "a third party converted through a rebase record"
                    );
                }
                fwd_state(&self.p, &a)?;
                let (v1, l1) = (self.v, a);
                let st2 =
                    pool::touch_pot(&self.p, &mut self.v, &mut self.ls[i], &self.recs[lo..hi], 0)
                        .map_err(fail)?;
                prop_assert_eq!((st2, self.v, self.ls[i]), (st, v1, l1), "not idempotent");
                stats[0] += 1;
                if a.fwd == pool::FWD_HOLD {
                    stats[1] += 1;
                }
                if a.fwd == pool::FWD_OPEN {
                    stats[2] += 1;
                }
            }
            Ev::Owner(i, n) => {
                let i = i % self.ls.len();
                let l = &mut self.ls[i];
                let (lo, hi) = {
                    let len = self.recs.len();
                    let from = l.epoch as usize;
                    (from.min(len), from.saturating_add(usize::from(n)).min(len))
                };
                let was = l.fwd;
                if pool::convert(&self.p, l, &mut self.recs[lo..hi]).map_err(fail)?
                    != Standing::Behind
                {
                    pool::touch(&self.p, &mut self.v, l, &mut self.recs[lo..hi], 0)
                        .map_err(fail)?;
                }
                if was != pool::FWD_NONE && l.fwd == pool::FWD_OPEN {
                    stats[3] += 1;
                }
                fwd_state(&self.p, l)?;
            }
        }
        Ok(())
    }
}

/// The stored chain's invariant (`pool_harness::fwd_state`).
fn fwd_state(p: &Pool, l: &Lender) -> Result<(), TestCaseError> {
    let ok = match l.fwd {
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
    };
    prop_assert!(ok, "chain state {:?}", l);
    Ok(())
}

type Holding = (u128, u128, (i64, u64));

fn holdings() -> impl Strategy<Value = Vec<Holding>> {
    prop::collection::vec(
        (
            prop_oneof![
                0u128..2_000,
                0u128..10_000_000,
                0u128..10u128.pow(12),
                10u128.pow(12)..10u128.pow(14)
            ],
            prop_oneof![3 => Just(0u128), 1 => 0u128..10u128.pow(9)],
            (-10i64.pow(12)..10i64.pow(12), 0u64..10u64.pow(12)),
        ),
        1..8,
    )
}

/// One case: the same events with and without the third parties, then every owner converts.
fn case(holdings: &[Holding], events: &[Ev], stats: &mut [u32; 4]) -> Result<(), TestCaseError> {
    let mut p = Pool::new();
    p.total_shares = u128::MAX / 4;
    let ls: Vec<Lender> = holdings
        .iter()
        .map(|&(shares, lock, (pending, real))| Lender {
            shares,
            lock: lock.min(shares),
            lock_term: lock.min(shares),
            lock_epoch: if lock == 0 { NO_LOCK_EPOCH } else { 0 },
            lock_until: if lock == 0 { 0 } else { 30 * DAY },
            pending,
            real,
            claimed: real / 2,
            ..Lender::new(&p)
        })
        .collect();
    let mut worlds = Vec::new();
    for third in [false, true] {
        let mut w = World {
            p,
            v: Vault::new(),
            recs: Vec::new(),
            ls: ls.clone(),
        };
        for e in events {
            w.apply(e, third, stats)?;
        }
        for l in &mut w.ls {
            let from = (l.epoch as usize).min(w.recs.len());
            pool::touch(&w.p, &mut w.v, l, &mut w.recs[from..], 0)
                .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
            prop_assert_eq!(l.fwd, pool::FWD_NONE);
        }
        worlds.push(w);
    }
    let (a, b) = (&worlds[0], &worlds[1]);
    // An empty lender (no shares, lock or term) may end parked on a live wipe record in one
    // run and jumped past it in the other (a V2-12 jump moved it to an epoch that a later
    // full wipe then made "the one before the live record"): nothing accrues on it either way.
    let norm = |l: &Lender| {
        if (l.shares, l.lock, l.lock_term) == (0, 0, 0) {
            Lender {
                real: 0,
                epoch: 0,
                snap: 0,
                snap_r: 0,
                ..*l
            }
        } else {
            Lender { real: 0, ..*l }
        }
    };
    for (x, y) in a.ls.iter().zip(&b.ls) {
        prop_assert_eq!(norm(x), norm(y));
    }
    prop_assert_eq!(&a.recs, &b.recs);
    let total = |w: &World| {
        w.ls.iter().map(|l| u128::from(l.real)).sum::<u128>() + u128::from(w.v.unmatched)
    };
    prop_assert_eq!(total(a), total(b));
    Ok(())
}

/// `PROPTEST_CASES` cases (default 4,000), then the coverage: third-party syncs, ones that left
/// a held chain, ones that left an open chain (the owner inside it), and owner syncs that stopped
/// inside a chain.
#[test]
fn the_forward_chain_on_a_moving_pool_pays_nobody_twice() {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(4_000);
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig {
        cases,
        ..ProptestConfig::default()
    });
    let stats = std::cell::RefCell::new([0u32; 4]);
    let strat = (holdings(), prop::collection::vec(ev(), 1..80));
    let res = runner.run(&strat, |(h, e)| case(&h, &e, &mut stats.borrow_mut()));
    if let Err(e) = res {
        panic!("{e}");
    }
    let s = *stats.borrow();
    eprintln!(
        "{cases} cases: third-party syncs {}, held after {}, open after {}, owners stopping inside {}",
        s[0], s[1], s[2], s[3]
    );
    assert!(s[1] > 1_000 && s[2] > 100 && s[3] > 100, "coverage {s:?}");
}

/// Pinned (pre-KI-66, V2-12): a lender parked on a live wipe record accrues per touch,
/// `⌊s·(end − snap)⌋`, not by cumulative floor as the forward chain does, so a third party's
/// touch between two top-ups loses it a unit: 1 share, top-ups of 0.036… and 36.96… lamports
/// per share (37 in all) pay 37 to one touch and 0 + 36 to two.
#[test]
fn ki67_parked_touch_floors_per_touch() {
    let mut p = Pool::new();
    p.share_epoch = 1;
    p.wipe_live = 1;
    let mut rec = ShareEpoch {
        acc_end: 0,
        acc_r_end: 36_460_607_208_990_761,
        s_old: 1,
        carry: 0,
        carry_r: 0,
        conv_rem: 0,
        epoch: 0,
        num: 0,
        den_log1000: 0,
        _pad: [0; 10],
    };
    let l0 = Lender {
        shares: 1,
        epoch: 0,
        ..Lender::new(&Pool::new())
    };
    let (mut v, mut twice) = (Vault::new(), l0);
    assert_eq!(
        pool::touch_pot(&p, &mut v, &mut twice, &[rec], 0),
        Ok(Standing::Parked)
    );
    rec.acc_r_end += 36_963_539_392_791_009_239;
    assert_eq!(
        pool::touch_pot(&p, &mut v, &mut twice, &[rec], 0),
        Ok(Standing::Parked)
    );
    let (mut w, mut once) = (Vault::new(), l0);
    assert_eq!(
        pool::touch_pot(&p, &mut w, &mut once, &[rec], 0),
        Ok(Standing::Parked)
    );
    assert_eq!(
        (
            u128::from(once.real) + u128::from(w.unmatched),
            u128::from(twice.real) + u128::from(v.unmatched)
        ),
        (37, 36)
    );
}
