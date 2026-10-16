//! KI-66 security review: adversarial tests of the pot-only touch (a third party's
//! `sync_lender` of a lender behind a rebase record). Every lender instruction runs through the
//! lender lab (`lender_it/common.rs`): checked against the math crate on its pre-state, with the
//! hooks after every transaction.
//!
//! - No double pay: the same market wound down with and without third-party pot-only touches
//!   (repeated, with several `max_epochs`, before and after other lenders' claims, with rent
//!   accruing to the held lender's latent shares after the record) pays every lender the same
//!   total and leaves the same `lender_sol`.
//! - Idempotence across `max_epochs`, read-only records, a slice that skips the lender's epoch,
//!   records out of order or of another epoch.
//! - Locks: a held lender keeps its (expired) lock until its owner converts.
//! - A deficit lender (negative `pending`) behind a rebase record: its pot-only touch moves its
//!   unclaimed realised SOL to the pot exactly as its owner's touch would.
//! - Two rebase records: each epoch is accrued once.
//! - KI-66 full (the forward chain): a chain through two rebases and a full wipe (parked, topped
//!   up, passed); the owner stopping inside it with a lag of 0 and of 1; sliced walks; a deficit
//!   debited after the record; a term-only hold; a jump inside a stale chain; the owner parking
//!   inside its chain on a live wipe record. KI-67: a dead lock term does not keep a position
//!   open.
//!
//! Requires the SBF build: `anchor build --tools-version v1.57 --arch v3`.

#![allow(clippy::result_large_err)]

#[allow(dead_code)]
#[path = "lender_it/common.rs"]
mod common;

use anchor_lang::solana_program::instruction::AccountMeta;
use common::{Lab, code};
use shortwin::errors::ShortwinError;
use shortwin_litesvm_tests::errors::custom_code;
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_litesvm_tests::{ix, pda, read};
use shortwin_math::pool::{self, Standing, VIRTUAL_ASSETS, VIRTUAL_SHARES};

const U: u64 = 1_000_000_000;

/// `ki65_it::rebase`: everything but one raw token settled (`sol − lump` of it), a rebase, then
/// a `lump` swept into the rebased pool in the same second (since AIR-07 a lump that arrives with
/// such a settlement is flushed to the old shares at once).
fn rebase(lab: &mut Lab, pid: &str, sol: u64, lump: u64) {
    let e = lab.market().share_epoch;
    let t = lab.market().tokens_free;
    lab.borrow30(pid, t - 1).unwrap();
    lab.settle(pid, sol - lump, 0);
    assert_eq!(lab.market().share_epoch, e + 1, "{pid}: the epoch rolled");
    let r = *lab.state().records.last().unwrap();
    assert_eq!(r.num, 1, "{pid}: a rebase, not a wipe");
    if lump > 0 {
        lab.sweep_sol(lump);
    }
}

fn exit_all(lab: &mut Lab, name: &str) {
    loop {
        let Some(l) = lab.touch(name) else { return };
        let p = lab.market().pool();
        if l.shares == 0 || l.epoch != p.share_epoch {
            return;
        }
        let allow = u128::from(lab.free_allowance(name));
        let c = u128::from(p.tokens_free) + u128::from(p.borrowed_q) + VIRTUAL_ASSETS;
        let svs = p.total_shares + VIRTUAL_SHARES;
        let zero = svs.div_ceil(c) - 1;
        let fit = allow * svs / c;
        let s = l.shares.min(fit.max(zero).max(1));
        lab.withdraw(name, s).unwrap();
    }
}

/// Every lender exits whole and claims, then two more claim passes (the pot reaches the
/// deficit lenders on a later pass). Returns each lender's lamports gained since `base`.
fn wind_down(lab: &mut Lab, names: &[&str], base: &[u64]) -> Vec<u64> {
    for n in names {
        exit_all(lab, n);
        lab.claim(n).unwrap();
    }
    for _ in 0..2 {
        for n in names {
            lab.claim(n).unwrap();
        }
    }
    names
        .iter()
        .zip(base)
        .map(|(n, b)| i128::from(wealth(lab, n)) - i128::from(*b))
        .map(|g| u64::try_from(g.max(0)).expect("gain"))
        .collect()
}

/// `lender_it`'s pot scenario (A leaves early, C joins late and holds the surplus), rolled by a
/// rebase before anyone touched C; A claimed once (short).
fn pot_market() -> Lab {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    lab.borrow30("p", 900_000_000_000).unwrap();
    lab.advance(10 * common::D, 10u128.pow(16));
    let a = lab.free_allowance("A");
    let s = lab.shares_for("A", a);
    lab.withdraw("A", s).unwrap();
    lab.deposit("C", 2_000_000_000_000).unwrap();
    let owed = lab.owed("p");
    lab.repay("p", owed, 0);
    lab.claim("A").unwrap();
    rebase(&mut lab, "q", U, 0);
    assert_eq!(lab.position("C").unwrap().epoch, 0);
    lab
}

const ABC: [&str; 3] = ["A", "B", "C"];

/// `name`'s lamports plus its position account's (the rent `withdraw` refunds when it closes
/// the position, which depends on `pending` being 0 at the exit, not on what is paid).
fn wealth(lab: &mut Lab, name: &str) -> u64 {
    let owner = lab.key(name);
    let pos = pda::lender(&lab.k.market, &owner).0;
    lab.lamports_of(name) + read::lamports(&lab.env.svm, &pos)
}

fn lamports_of(lab: &mut Lab, names: &[&str]) -> Vec<u64> {
    names.iter().map(|n| wealth(lab, n)).collect()
}

/// No double pay: the market wound down by the owners alone, and the same market with a third
/// party's pot-only touches of C (three in a row with `max_epochs` 1, 2 and 0, then again after
/// A's claim) pay A, B and C the same totals and leave the same `lender_sol` and pot.
#[test]
fn pot_only_touches_pay_nobody_twice() {
    // Owners alone.
    let mut x = pot_market();
    let base = lamports_of(&mut x, &ABC);
    let gx = wind_down(&mut x, &ABC, &base);
    let vx = x.vault();
    // A third party touches C, repeatedly.
    let mut y = pot_market();
    let base = lamports_of(&mut y, &ABC);
    for n in [1, 2, 0, 1] {
        assert_eq!(y.sync("C", n), Ok(Standing::Behind), "max_epochs {n}");
    }
    let pot = y.vault().unmatched;
    assert!(pot > 0);
    y.claim("A").unwrap();
    assert_eq!(y.position("A").unwrap().pending, 0, "A paid without C");
    for n in [1, 1] {
        assert_eq!(y.sync("C", n), Ok(Standing::Behind));
    }
    let gy = wind_down(&mut y, &ABC, &base);
    let vy = y.vault();
    eprintln!(
        "owners alone {gx:?} (lender_sol {}, pot {}); with pot-only touches {gy:?} \
         (lender_sol {}, pot {}); pot after the touches {pot}",
        vx.lender_sol, vx.unmatched, vy.lender_sol, vy.unmatched
    );
    assert_eq!(gx, gy, "every lender is paid the same");
    assert_eq!((vx.lender_sol, vx.unmatched), (vy.lender_sol, vy.unmatched));
}

/// No double pay with rent accruing to C's latent (unconverted) shares after the record: a
/// second position borrows after the rebase, rent accrues for 5 days with C held (touched by a
/// third party before and during the accrual), A claims in between, the position repays. The
/// totals equal the owners-alone wind-down's to the lamport (only who waits for the pot may
/// differ in between).
#[test]
fn pot_only_touches_with_post_record_accrual_pay_nobody_twice() {
    let run = |third_party: bool| -> (Vec<u64>, u64, u64) {
        let mut lab = pot_market();
        let base = lamports_of(&mut lab, &ABC);
        if third_party {
            assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
        }
        lab.deposit("D", 1_000_000_000_000).unwrap();
        let t = lab.market().tokens_free;
        lab.borrow30("r", t / 2).unwrap();
        for _ in 0..5 {
            lab.advance(common::D, 10u128.pow(15));
            if third_party {
                assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
                lab.claim("A").unwrap();
            }
        }
        let owed = lab.owed("r");
        lab.repay("r", owed, 0);
        if third_party {
            assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
        }
        let g = wind_down(&mut lab, &ABC, &base);
        let v = lab.vault();
        (g, v.lender_sol, v.unmatched)
    };
    let x = run(false);
    let y = run(true);
    eprintln!("owners alone {x:?}; with pot-only touches {y:?}");
    assert_eq!(x.0.iter().sum::<u64>(), y.0.iter().sum::<u64>(), "Σ paid");
    assert_eq!(x, y, "every lender is paid the same, the same left");
}

/// Idempotence and the record slice: read-only records are accepted (the pot-only touch writes
/// none); a second touch with any `max_epochs` changes nothing; a slice that starts after the
/// lender's epoch passes no record of it (a no-op); records out of order are refused.
#[test]
fn pot_only_touch_record_slices() {
    let mut lab = pot_market();
    // A second rebase, so C is two records behind.
    lab.deposit("D", 1_000_000_000_000).unwrap();
    rebase(&mut lab, "q2", U, 0);
    let m = lab.k.market;
    let c = lab.key("C");
    let payer = lab.env.payer.insecure_clone();
    let ro: Vec<AccountMeta> = (0..2)
        .map(|e| AccountMeta::new_readonly(pda::share_epoch(&m, e).0, false))
        .collect();
    // Expected: the math crate's pot-only touch on the pre-state.
    let st = lab.state();
    let mut want = st.lender_of(&c).unwrap().lender();
    let mut v = *st.vault;
    let recs = st.records.clone();
    assert_eq!(
        pool::touch_pot(&st.pool, &mut v, &mut want, &recs, st.now),
        Ok(Standing::Behind)
    );
    let res = lab.env.send(
        &[ix::with_remaining(ix::sync_lender(&c, &m, 2), &ro)],
        &[&payer],
    );
    assert!(res.is_ok(), "read-only records: {:?}", res.err());
    assert_eq!(lab.position("C").unwrap().lender(), want);
    assert_eq!(lab.vault().unmatched, v.unmatched);
    let (records, held, pot) = (lab.state().records, lab.position("C").unwrap(), v.unmatched);
    for n in [0, 1, 2, 8] {
        assert_eq!(lab.sync("C", n), Ok(Standing::Behind), "max_epochs {n}");
        assert_eq!(lab.position("C").unwrap(), held, "max_epochs {n}");
        assert_eq!(lab.vault().unmatched, pot);
    }
    // Only the record after C's epoch: no record of C's epoch, nothing moves.
    let res = lab.env.send(
        &[ix::with_remaining(
            ix::sync_lender(&c, &m, 1),
            &[AccountMeta::new_readonly(pda::share_epoch(&m, 1).0, false)],
        )],
        &[&payer],
    );
    assert!(res.is_ok(), "{:?}", res.err());
    assert_eq!(lab.position("C").unwrap(), held);
    // Out of order.
    let res = lab.env.send(
        &[ix::with_remaining(
            ix::sync_lender(&c, &m, 2),
            &[ro[1].clone(), ro[0].clone()],
        )],
        &[&payer],
    );
    assert_eq!(
        custom_code(&res),
        Some(code(ShortwinError::EpochRecordMismatch))
    );
    assert_eq!(lab.state().records, records, "no record written");
    assert_eq!(lab.position("C").unwrap(), held);
}

/// A held lender keeps its lock, expired or not, until its owner converts it (the calendar
/// already dropped the term at `lock_until`); after the owner's conversion it binds nothing.
#[test]
fn a_held_lender_keeps_its_expired_lock_until_its_owner_converts() {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.borrow30("p", 900_000_000_000).unwrap();
    lab.deposit("C", 2_000_000_000_000).unwrap();
    let a = lab.free_allowance("C");
    let s = lab.shares_for("C", a / 10);
    lab.withdraw("C", s).unwrap();
    let c0 = lab.position("C").unwrap();
    assert!(
        c0.lock > 0,
        "C withdrew while inventory was borrowed: a lock"
    );
    lab.advance(10 * common::D, 10u128.pow(16));
    let owed = lab.owed("p");
    lab.repay("p", owed, 0);
    rebase(&mut lab, "q", U, 0);
    let held = lab.position("C").unwrap();
    lab.advance(40 * common::D, 0);
    assert!(lab.now() > held.lock_until, "the lock expired");
    assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
    let after = lab.position("C").unwrap();
    assert_eq!(
        (
            after.lock,
            after.lock_term,
            after.lock_until,
            after.lock_epoch
        ),
        (held.lock, held.lock_term, held.lock_until, held.lock_epoch),
        "the pot-only touch keeps the lock"
    );
    assert_eq!(lab.sync_signed("C", 1), Ok(Standing::Current));
    let st = lab.state();
    let c = st.lender_of(&lab.key("C")).unwrap().lender();
    let den = 1_000u128.pow(u32::from(st.records[0].den_log1000));
    assert_eq!(c.lock, held.lock / den, "the owner's conversion scales it");
    assert_eq!(
        pool::lock_of(&st.vault, &c, st.now),
        Ok(0),
        "no longer binding (expired, or of an earlier Π epoch)"
    );
    exit_all(&mut lab, "C");
    exit_all(&mut lab, "A");
}

/// A deficit lender (negative `pending`) behind a rebase record: the pot-only touch moves its
/// unclaimed realised SOL to the pot (`max(pending, 0) = 0`), as its owner's own touch would;
/// with `cover_deficit` and the wind-down after, every lender is paid what the owners-alone
/// market pays.
#[test]
fn a_deficit_lender_behind_a_rebase_record() {
    let run = |third_party: bool| -> (Vec<u64>, u64, u64, i64) {
        let mut lab = Lab::with_cap(TokenProgram::Token2022, 10_000);
        lab.deposit("B", 1_000_000_000_000).unwrap();
        // 90 % lent: since AIR-07's residual fix a drained pool takes no deposit, and A joins
        // late, after the rent it then shares the shortfall of.
        lab.borrow30("1", 900_000_000_000).unwrap();
        lab.advance(20 * common::D, 10u128.pow(17));
        lab.deposit("A", 6_000_000_000_000).unwrap();
        lab.repay("1", 0, 0);
        rebase(&mut lab, "2", 3 * U, 3 * U);
        let names = ["A", "B"];
        let pot0 = lab.vault().unmatched;
        if third_party {
            assert_eq!(lab.sync("A", 1), Ok(Standing::Behind));
            let a = lab.position("A").unwrap();
            assert!(a.pending < 0, "A's pending {}", a.pending);
            assert!(lab.vault().unmatched >= pot0);
        }
        let st = lab.state();
        let a = st.lender_of(&lab.key("A")).unwrap().lender();
        let mut l = a;
        let mut v = *st.vault;
        let mut recs = st.records.clone();
        pool::touch(&st.pool, &mut v, &mut l, &mut recs, st.now).unwrap();
        let neg = l.pending;
        assert!(neg < 0);
        lab.cover("A", neg.unsigned_abs()).unwrap();
        let base = lamports_of(&mut lab, &names);
        let g = wind_down(&mut lab, &names, &base);
        let v = lab.vault();
        (g, v.lender_sol, v.unmatched, neg)
    };
    let x = run(false);
    let y = run(true);
    eprintln!("deficit lender, owners alone {x:?}; with a pot-only touch {y:?}");
    assert_eq!(x, y);
}

/// Two rebase records: the owner converts one record, a third party's touch holds C at the
/// second (accruing C's epoch-1 earnings once), the owner converts the second; the result is the
/// owner's two-record conversion's.
#[test]
fn two_rebase_records_each_epoch_accrued_once() {
    let run = |third_party: bool| -> (Vec<u64>, u64, u64) {
        let mut lab = pot_market();
        lab.deposit("D", 1_000_000_000_000).unwrap();
        let t = lab.market().tokens_free;
        lab.borrow30("r", t / 2).unwrap();
        lab.advance(3 * common::D, 10u128.pow(15));
        let owed = lab.owed("r");
        lab.repay("r", owed, 0);
        lab.deposit("E", 1_000_000_000_000).unwrap();
        rebase(&mut lab, "q2", U, 0);
        let base = lamports_of(&mut lab, &ABC);
        if third_party {
            assert_eq!(lab.sync("C", 2), Ok(Standing::Behind));
            assert_eq!(lab.sync_signed("C", 1), Ok(Standing::Behind));
            assert_eq!(lab.position("C").unwrap().epoch, 1);
            assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
            assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
            lab.claim("A").unwrap();
            lab.claim("B").unwrap();
        }
        assert_eq!(lab.sync_signed("C", 2), Ok(Standing::Current));
        let g = wind_down(&mut lab, &ABC, &base);
        let v = lab.vault();
        (g, v.lender_sol, v.unmatched)
    };
    let x = run(false);
    let y = run(true);
    eprintln!("two rebase records, owners alone {x:?}; with pot-only touches {y:?}");
    assert_eq!(x, y);
}

// ---- KI-66 full: the forward chain (owner decision of 2026-10-08, eighth round) ------------

/// `name` deposits `amount` tokens, then burns (for 0 tokens) what leaves its shares at `rem`
/// modulo 1,000 (`ki65_it::dust`).
fn dust(lab: &mut Lab, name: &str, amount: u64, rem: u128) -> u128 {
    lab.deposit(name, amount).unwrap();
    let s = lab.position(name).unwrap().shares;
    let extra = (s % 1_000 + 1_000 - rem) % 1_000;
    if extra > 0 {
        assert_eq!(
            lab.withdraw(name, extra).map(|(t, _)| t),
            Ok(0),
            "a 0-token burn"
        );
    }
    lab.position(name).unwrap().shares
}

/// A settlement that leaves about `S / (2·R_MAX)` raw tokens: a rebase by `1/1,000` (k = 1).
fn rebase_k1(lab: &mut Lab, pid: &str, sol: u64) {
    let e = lab.market().share_epoch;
    let p = lab.market().pool();
    let keep = u64::try_from((p.total_shares / (2 * pool::R_MAX)).max(1)).unwrap();
    lab.borrow30(pid, p.tokens_free - keep).unwrap();
    lab.settle(pid, sol, 0);
    assert_eq!(lab.market().share_epoch, e + 1, "{pid}: the epoch rolled");
    let r = *lab.state().records.last().unwrap();
    assert_eq!((r.num, r.den_log1000), (1, 1), "{pid}: a 1/1,000 rebase");
}

/// Rent in the current epoch with an early leaver: half the free tokens borrowed, `days` of
/// rent at `d_index` credited, `leaver` withdraws its allowance, the rent is paid — realised
/// on the shares still there (latent ones included) beyond their entitlement.
fn rent_with_leaver(lab: &mut Lab, pid: &str, leaver: &str, days: i64, d_index: u128) {
    let t = lab.market().tokens_free;
    lab.borrow30(pid, t / 2).unwrap();
    lab.advance(days * common::D, d_index);
    let a = lab.free_allowance(leaver);
    let s = lab.shares_for(leaver, a);
    if s > 0 {
        lab.withdraw(leaver, s).unwrap();
    }
    let owed = lab.owed(pid);
    lab.repay(pid, owed, 0);
}

/// A chain through three records — two rebases and a full wipe — walked by third parties
/// while the wipe record is live (the chain parks on it), after a top-up of that record, and
/// after a deposit finalises it (the chain passes it with 0 shares and jumps to the current
/// epoch). The rent after each record reaches the pot without C's owner; the wind-down pays
/// every lender what the owners-alone market pays, to the lamport.
#[test]
fn a_chain_through_three_records_with_a_full_wipe_parks_and_pays_nobody_twice() {
    let names = ["A", "B", "C", "D", "E"];
    let run = |third_party: bool| -> (Vec<u64>, u64, u64) {
        let mut lab = pot_market();
        let base = lamports_of(&mut lab, &names);
        lab.deposit("D", 1_000_000_000_000).unwrap();
        rent_with_leaver(&mut lab, "r1", "D", 3, 10u128.pow(15));
        rebase_k1(&mut lab, "q2", U);
        lab.deposit("E", 1_000_000_000_000).unwrap();
        rent_with_leaver(&mut lab, "r2", "E", 3, 10u128.pow(15));
        if third_party {
            assert_eq!(lab.sync("C", 8), Ok(Standing::Behind));
            let c = lab.position("C").unwrap();
            assert_eq!((c.fwd, c.fwd_epoch, c.epoch), (pool::FWD_HOLD, 2, 0));
            lab.claim("A").unwrap();
        }
        // A full wipe: everything borrowed and settled.
        let t = lab.market().tokens_free;
        lab.borrow30("w", t).unwrap();
        lab.settle("w", 2 * U, 0);
        let w = *lab.state().records.last().unwrap();
        assert!(w.is_full_wipe() && lab.market().pool().is_wipe_live());
        if third_party {
            assert_eq!(lab.sync("C", 8), Ok(Standing::Behind));
            let c = lab.position("C").unwrap();
            assert_eq!(c.fwd_epoch, w.epoch, "parked on the live wipe record");
            assert!(c.fwd_shares > 0);
        }
        lab.sweep_sol(U / 3);
        if third_party {
            let before = lab.position("C").unwrap();
            assert_eq!(lab.sync("C", 8), Ok(Standing::Behind));
            let c = lab.position("C").unwrap();
            assert!(
                c.pending >= before.pending,
                "the top-up accrued to the parked chain"
            );
            assert_eq!(c.fwd_epoch, w.epoch);
        }
        lab.deposit("A", 1_000_000_000).unwrap();
        assert!(
            !lab.market().pool().is_wipe_live(),
            "the deposit finalised the wipe record"
        );
        if third_party {
            assert_eq!(lab.sync("C", 8), Ok(Standing::Behind));
            let c = lab.position("C").unwrap();
            assert_eq!(
                (c.fwd, c.fwd_epoch, c.fwd_shares, c.epoch),
                (pool::FWD_HOLD, lab.market().share_epoch, 0, 0),
                "the chain passed the wipe with 0 shares and jumped"
            );
            assert_eq!(lab.sync("C", 8), Ok(Standing::Behind));
            assert_eq!(lab.position("C").unwrap(), c, "idempotent");
            lab.claim("B").unwrap();
        }
        let g = wind_down(&mut lab, &names, &base);
        let v = lab.vault();
        (g, v.lender_sol, v.unmatched)
    };
    let x = run(false);
    let y = run(true);
    eprintln!("three records and a wipe, owners alone {x:?}; with the forward chain {y:?}");
    assert_eq!(x, y);
}

/// The owner stops inside the chain (`FWD_OPEN`) with a lag of 0 and of 1: residues tuned so
/// that the first conversion through a `1/1,000` record leaves `conv_rem` 0 (lag 0) or 999 (C's
/// conversion carries a unit: lag 1); a third party continues the chain from `fwd_epoch`, the
/// owner finishes; nobody is paid twice (the wind-down equals the run without third parties,
/// where the owner converts at the same points).
#[test]
fn an_owner_stopping_inside_the_chain_with_lag_0_and_1_pays_nobody_twice() {
    for lag in [0u8, 1] {
        let names = ["B", "C", "H"];
        let run = |third_party: bool| -> (Vec<u64>, u64, u64) {
            let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
            let base = lamports_of(&mut lab, &names);
            dust(&mut lab, "H", 1_000_000_000, if lag == 1 { 999 } else { 0 });
            assert_eq!(dust(&mut lab, "C", 1_000_000_000_000, 600) % 1_000, 600);
            dust(&mut lab, "B", 1_000_000_000_000, 0);
            rebase_k1(&mut lab, "p1", U);
            lab.claim("H").unwrap();
            assert_eq!(
                lab.state().records[0].conv_rem,
                if lag == 1 { 999 } else { 0 }
            );
            rent_with_leaver(&mut lab, "r1", "B", 2, 10u128.pow(19));
            rebase_k1(&mut lab, "p2", U);
            rent_with_leaver(&mut lab, "r2", "H", 2, 10u128.pow(19));
            if third_party {
                assert_eq!(lab.sync("C", 2), Ok(Standing::Behind));
                assert_eq!(lab.position("C").unwrap().fwd_epoch, 2);
            }
            assert_eq!(lab.sync_signed("C", 1), Ok(Standing::Behind));
            if third_party {
                let c = lab.position("C").unwrap();
                assert_eq!(
                    (c.fwd, c.epoch, c.fwd_epoch, c.fwd_lag),
                    (pool::FWD_OPEN, 1, 2, lag),
                    "the owner stopped inside the chain"
                );
            }
            let t = lab.market().tokens_free;
            lab.borrow30("r3", t / 2).unwrap();
            lab.advance(2 * common::D, 10u128.pow(19));
            let owed = lab.owed("r3");
            lab.repay("r3", owed, 0);
            if third_party {
                assert_eq!(lab.sync("C", 8), Ok(Standing::Behind));
                assert_eq!(lab.position("C").unwrap().fwd, pool::FWD_OPEN);
                lab.claim("B").unwrap();
                lab.claim("H").unwrap();
            }
            assert_eq!(lab.sync_signed("C", 1), Ok(Standing::Current));
            assert_eq!(lab.position("C").unwrap().fwd, pool::FWD_NONE);
            let g = wind_down(&mut lab, &names, &base);
            let v = lab.vault();
            (g, v.lender_sol, v.unmatched)
        };
        let x = run(false);
        let y = run(true);
        eprintln!("lag {lag}: owners alone {x:?}; with the forward chain {y:?}");
        assert_eq!(x, y, "lag {lag}");
    }
}

/// The owner parks inside its chain (the KI-66 full review's mutant 6, park half): a third
/// party's walk parked C's chain on a live full-wipe record, and C's owner converts while that
/// record is live, through the rebases into the chain (`FWD_OPEN`, lag 0 and 1) and onto the
/// wipe record, where it parks: the park settles the chain (`fwd` none; the lab checks every
/// stored field against the math crate), a top-up and a second owner sync accrue nothing twice,
/// and once a deposit finalises the wipe the wind-down pays every lender what the run without
/// third parties pays, where the owner converts at the same points.
#[test]
fn an_owner_parking_inside_its_chain_on_a_live_wipe_settles_it() {
    for lag in [0u8, 1] {
        let names = ["B", "C", "H"];
        let run = |third_party: bool| -> (Vec<u64>, u64, u64) {
            let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
            let base = lamports_of(&mut lab, &names);
            dust(&mut lab, "H", 1_000_000_000, if lag == 1 { 999 } else { 0 });
            assert_eq!(dust(&mut lab, "C", 1_000_000_000_000, 600) % 1_000, 600);
            dust(&mut lab, "B", 1_000_000_000_000, 0);
            rebase_k1(&mut lab, "p1", U);
            lab.claim("H").unwrap();
            rent_with_leaver(&mut lab, "r1", "B", 2, 10u128.pow(19));
            rebase_k1(&mut lab, "p2", U);
            rent_with_leaver(&mut lab, "r2", "H", 2, 10u128.pow(19));
            if third_party {
                assert_eq!(lab.sync("C", 8), Ok(Standing::Behind));
                assert_eq!(lab.position("C").unwrap().fwd_epoch, 2);
            }
            // A full wipe: everything borrowed and settled; record 2 stays live.
            let t = lab.market().tokens_free;
            lab.borrow30("w", t).unwrap();
            lab.settle("w", 2 * U, 0);
            let w = *lab.state().records.last().unwrap();
            assert!(w.is_full_wipe() && lab.market().pool().is_wipe_live());
            lab.sweep_sol(U / 3);
            if third_party {
                assert_eq!(lab.sync("C", 8), Ok(Standing::Behind));
                let c = lab.position("C").unwrap();
                assert_eq!(
                    (c.fwd, c.fwd_epoch),
                    (pool::FWD_HOLD, w.epoch),
                    "parked chain"
                );
            }
            assert_eq!(lab.sync_signed("C", 8), Ok(Standing::Parked));
            let c = lab.position("C").unwrap();
            assert_eq!(
                (c.fwd, c.fwd_epoch, c.fwd_lag, c.fwd_shares, c.epoch),
                (pool::FWD_NONE, 0, 0, 0, w.epoch),
                "the owner's park settled the chain"
            );
            lab.sweep_sol(U / 5);
            assert_eq!(lab.sync_signed("C", 8), Ok(Standing::Parked));
            if third_party {
                assert_eq!(lab.sync("C", 8), Ok(Standing::Parked));
                lab.claim("B").unwrap();
            }
            lab.deposit("B", 1_000_000_000).unwrap();
            assert!(!lab.market().pool().is_wipe_live(), "finalised");
            let g = wind_down(&mut lab, &names, &base);
            let v = lab.vault();
            (g, v.lender_sol, v.unmatched)
        };
        let x = run(false);
        let y = run(true);
        eprintln!("lag {lag}, parked owner: owners alone {x:?}; with the forward chain {y:?}");
        assert_eq!(x, y, "lag {lag}");
    }
}

/// Sliced third-party walks: records starting before, at and after the chain's `fwd_epoch`,
/// passed writable. A slice that misses `fwd_epoch` moves nothing; the others continue the
/// chain; the sum of the walks equals one pot-only touch with every record (the math crate on
/// the pre-state): `pending`, the chain, `real + unmatched`. No record's bytes change (the
/// unsigned sync's post-condition, `records == loaded`, holds).
#[test]
fn sliced_third_party_walks_continue_the_chain() {
    let mut lab = pot_market();
    lab.deposit("D", 1_000_000_000_000).unwrap();
    rent_with_leaver(&mut lab, "r1", "D", 3, 10u128.pow(15));
    rebase_k1(&mut lab, "q2", U);
    lab.deposit("E", 1_000_000_000_000).unwrap();
    rent_with_leaver(&mut lab, "r2", "E", 3, 10u128.pow(15));
    rebase_k1(&mut lab, "q3", U);
    lab.deposit("F", 1_000_000_000_000).unwrap();
    rent_with_leaver(&mut lab, "r3", "F", 3, 10u128.pow(15));
    assert_eq!(lab.market().share_epoch, 3);
    let m = lab.k.market;
    let raw = |lab: &Lab| -> Vec<Vec<u8>> {
        (0..3)
            .map(|e| {
                lab.env
                    .svm
                    .get_account(&pda::share_epoch(&m, e).0)
                    .unwrap()
                    .data
            })
            .collect()
    };
    let bytes = raw(&lab);
    let st = lab.state();
    let c0 = st.lender_of(&lab.key("C")).unwrap().lender();
    let (mut want, mut v) = (c0, *st.vault);
    pool::touch_pot(&st.pool, &mut v, &mut want, &st.records, st.now).unwrap();
    // Record 0 only: the hold, the chain stops for want of record 1.
    assert_eq!(lab.sync_from("C", 0, 1), Ok(Standing::Behind));
    let c = lab.position("C").unwrap();
    assert_eq!((c.fwd, c.fwd_epoch, c.epoch), (pool::FWD_HOLD, 1, 0));
    // After fwd_epoch: nothing.
    assert_eq!(lab.sync_from("C", 2, 1), Ok(Standing::Behind));
    assert_eq!(
        lab.position("C").unwrap(),
        c,
        "a slice after fwd_epoch moves nothing"
    );
    // Before fwd_epoch, covering it: one more record.
    assert_eq!(lab.sync_from("C", 0, 2), Ok(Standing::Behind));
    assert_eq!(lab.position("C").unwrap().fwd_epoch, 2);
    // At fwd_epoch: the last record, then the current epoch.
    assert_eq!(lab.sync_from("C", 2, 1), Ok(Standing::Behind));
    let c = lab.position("C").unwrap();
    assert_eq!(
        (c.fwd, c.fwd_epoch, c.fwd_shares),
        (want.fwd, 3, want.fwd_shares)
    );
    assert_eq!(
        (c.pending, c.snap, c.snap_r),
        (want.pending, want.snap, want.snap_r)
    );
    assert_eq!(
        u128::from(c.real) + u128::from(lab.vault().unmatched),
        u128::from(want.real) + u128::from(v.unmatched),
        "real + unmatched"
    );
    assert_eq!(
        (c.shares, c.epoch, c.lock, c.lock_term, c.lock_until),
        (c0.shares, c0.epoch, c0.lock, c0.lock_term, c0.lock_until)
    );
    assert_eq!(raw(&lab), bytes, "no record written");
    // The owner settles the chain through the three records.
    assert_eq!(lab.sync_signed("C", 3), Ok(Standing::Current));
    assert_eq!(lab.position("C").unwrap().fwd, pool::FWD_NONE);
}

/// A deficit lender behind a rebase record whose debit falls after the record: C joins after a
/// position's rent was credited, the position stays open across the roll and repays nothing in
/// the new epoch, so the shortfall is debited there and C's chain accrues a negative `pending`.
/// The third party's walk moves C's realised SOL to the pot (`max(pending, 0) = 0`); with
/// `cover_deficit` and the wind-down every lender is paid what the owners-alone market pays.
#[test]
fn a_deficit_after_the_record_through_the_chain() {
    let run = |third_party: bool| -> (Vec<u64>, u64, u64, i64) {
        let mut lab = Lab::with_cap(TokenProgram::Token2022, 10_000);
        lab.deposit("B", 1_000_000_000_000).unwrap();
        lab.borrow30("1", 1_000_000_000).unwrap();
        lab.advance(20 * common::D, 10u128.pow(19));
        lab.deposit("C", 6_000_000_000_000).unwrap();
        rebase(&mut lab, "2", U, 0);
        lab.touch("B");
        lab.repay("1", 0, 0);
        let names = ["B", "C"];
        let base = lamports_of(&mut lab, &names);
        let pot0 = lab.vault().unmatched;
        if third_party {
            assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
            let c = lab.position("C").unwrap();
            assert_eq!(c.fwd, pool::FWD_HOLD);
            assert!(c.pending < 0, "C's chain was debited: {}", c.pending);
            assert!(lab.vault().unmatched >= pot0);
            assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
            lab.claim("B").unwrap();
        }
        let st = lab.state();
        let mut l = st.lender_of(&lab.key("C")).unwrap().lender();
        let mut v = *st.vault;
        let mut recs = st.records.clone();
        pool::touch(&st.pool, &mut v, &mut l, &mut recs, st.now).unwrap();
        let neg = l.pending;
        assert!(neg < 0);
        lab.cover("C", neg.unsigned_abs()).unwrap();
        let g = wind_down(&mut lab, &names, &base);
        let v = lab.vault();
        (g, v.lender_sol, v.unmatched, neg)
    };
    let x = run(false);
    let y = run(true);
    eprintln!("deficit after the record, owners alone {x:?}; with the chain {y:?}");
    assert_eq!(x, y);
}

/// A term-only hold (M0B-A-1's lender: shares and lock floored to 0, lock term 1) behind a
/// later rebase record: the third party holds it with a chain of 0 shares (`fwd_shares = 0`,
/// at the current epoch: nothing more can accrue), its owner's conversion scales the term and
/// jumps, dropping the chain.
#[test]
fn a_term_only_hold_has_a_chain_of_no_shares() {
    let mut lab = term_only_lender();
    rebase(&mut lab, "4", U, 0);
    let x0 = lab.position("X").unwrap();
    assert_eq!((x0.shares, x0.lock, x0.lock_term), (0, 0, 1));
    assert_eq!(lab.sync("X", 1), Ok(Standing::Behind));
    let x = lab.position("X").unwrap();
    assert_eq!(
        (x.fwd, x.fwd_shares, x.fwd_epoch, x.epoch),
        (pool::FWD_HOLD, 0, lab.market().share_epoch, x0.epoch)
    );
    assert_eq!(lab.sync("X", 1), Ok(Standing::Behind));
    assert_eq!(lab.position("X").unwrap(), x, "idempotent");
    assert_eq!(lab.sync_signed("X", 1), Ok(Standing::Current));
    let x = lab.position("X").unwrap();
    assert_eq!(
        (x.fwd, x.lock_term, x.epoch),
        (pool::FWD_NONE, 0, lab.market().share_epoch)
    );
}

/// `gen_pool.scripted_empty_lock_term` (M0b review M0B-A-1) through the program: `Π` decays
/// below `WAD`, so X's lock term exceeds its lock; a `1/10⁶` rebase floors X's shares and lock
/// to 0 but leaves the term at 1; X's own touch jumps it to the current epoch.
fn term_only_lender() -> Lab {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("Y", 2_000_000).unwrap();
    lab.deposit("X", 1_000).unwrap();
    lab.borrow30("1", 1_500_000).unwrap();
    lab.borrow30("2", 400_000).unwrap();
    let at = |lab: &mut Lab| {
        let a = lab.free_allowance("X");
        lab.shares_for("X", a)
    };
    let s = at(&mut lab);
    lab.withdraw("X", s).unwrap();
    lab.repay("2", 0, 0);
    let s = at(&mut lab);
    lab.withdraw("X", s).unwrap();
    let t = lab.market().tokens_free;
    lab.borrow30("3", t - 1).unwrap();
    lab.settle("3", 0, 0);
    lab.settle("1", 0, 0);
    lab.touch("X");
    let x = lab.position("X").unwrap();
    assert_eq!(
        (x.shares, x.lock, x.lock_term),
        (0, 0, 1),
        "M0B-A-1's lender"
    );
    lab.deposit("Y", 1_000_000).unwrap();
    lab
}

/// KI-67: M0B-A-1's lender (no shares, no lock, a lock term of 1 that is never read again:
/// the calendar holds its own copy until `lock_until`) is closable; its claim closes the position.
#[test]
fn a_dead_lock_term_does_not_keep_a_position_open() {
    let mut lab = term_only_lender();
    let x = lab.position("X").unwrap();
    assert_eq!((x.shares, x.lock, x.lock_term, x.pending), (0, 0, 1, 0));
    let lam = lab.lamports_of("X");
    assert_eq!(lab.claim("X"), Ok(0));
    assert_eq!(lab.position("X"), None, "closed despite the dead term");
    let rent = lab
        .env
        .svm
        .minimum_balance_for_rent_exemption(shortwin::state::LenderPosition::SPACE);
    assert_eq!(lab.lamports_of("X"), lam + rent);
}

/// A chain of no shares that a later roll leaves behind: a dust lender (600 shares, below one
/// share of a `1/1,000` rebase) is held by a third party; its chain floors to 0 shares and jumps
/// to the then-current epoch (`fwd_epoch` 1); a second rebase rolls past it. The owner's claim
/// converts the dust to 0 shares inside the chain (`FWD_OPEN` for a moment) and jumps (V2-12),
/// which drops the stale chain — the touch after it finds none — and, empty and paid, the claim
/// closes the position (KI-67).
#[test]
fn a_jump_inside_a_stale_chain_drops_it() {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    assert_eq!(dust(&mut lab, "D", 1, 600) % 1_000, 600);
    dust(&mut lab, "B", 1_000_000_000_000, 0);
    rebase_k1(&mut lab, "p1", U);
    assert_eq!(lab.sync("D", 1), Ok(Standing::Behind));
    let d = lab.position("D").unwrap();
    assert_eq!(
        (d.fwd, d.fwd_shares, d.fwd_epoch, d.epoch),
        (pool::FWD_HOLD, 0, 1, 0),
        "a chain of no shares, at the then-current epoch"
    );
    lab.deposit("E", 1_000_000_000_000).unwrap();
    rebase_k1(&mut lab, "p2", U);
    assert_eq!(lab.market().share_epoch, 2);
    let d = lab.position("D").unwrap();
    assert!(d.fwd_epoch < lab.market().share_epoch, "the chain is stale");
    lab.claim("D").unwrap();
    assert_eq!(
        lab.position("D"),
        None,
        "converted to 0 shares, jumped, paid and closed"
    );
}
