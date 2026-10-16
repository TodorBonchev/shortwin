//! The v0.4.1 rows of the ECON §9.3–§9.6 result tables (and the round-3 / round-4 cases of
//! `attacks/pool_r3.py` and ARCH T-POT), reproduced through the Rust API: drivers written
//! after `pool_model.py`'s demos on `pool_harness::Model`, asserting the published
//! numbers. The exact traces of the same demos are also replayed from
//! `tests/vectors/pool/pool_scenarios_*.json`.

mod common;
#[path = "pool_harness.rs"]
mod pool_harness;

use pool_harness::{DAY, LUMP_VEST_S, Model, S3_TAIL_S};
use shortwin_math::MathError;
use shortwin_math::num::WAD;
use shortwin_math::pool::{self, SweepSource};

const A: usize = 0;
const B: usize = 1;
const VEST: i64 = LUMP_VEST_S as i64;

/// Withdraws the most the lender may: `min(shares, ⌊allowance·(S + VS)/(T + ΣB_Q + VA)⌋)`
/// (the demos' form), 0 if that is 0.
fn withdraw_max(s: &mut Model, u: usize) -> u64 {
    let a = s.free_allowance(u).expect("allowance");
    let sh = s.shares_at(a).min(s.touch(u).shares);
    if sh == 0 {
        0
    } else {
        s.withdraw(u, sh).expect("withdraw")
    }
}

/// `pool_model.exit_all(p, u, max_burns)`: `u` withdraws every share in the largest burns it may —
/// its allowance or, while that is 0, the largest burn that pays 0 tokens; stops at a refusal.
fn exit_all(s: &mut Model, u: usize, max_burns: usize) -> u64 {
    let mut got = 0;
    for _ in 0..max_burns {
        let l = s.touch(u);
        let p = s.m.pool;
        if l.shares == 0 || l.epoch != p.share_epoch {
            break;
        }
        let allow = s.free_allowance(u).expect("allowance");
        let c = u128::from(p.tokens_free) + u128::from(p.borrowed_q) + pool::VIRTUAL_ASSETS;
        let zero = (p.total_shares + pool::VIRTUAL_SHARES).div_ceil(c) - 1;
        match s.withdraw(u, l.shares.min(s.shares_at(allow).max(zero).max(1))) {
            Ok(t) => got += t,
            Err(_) => break,
        }
    }
    got
}

const SOL: u64 = 1_000_000_000;

#[test]
fn first_depositor_attack() {
    // ECON §9.2: "victim deposits 1e9, receives 1,000,000,000,000 shares, redeems 1,000,000,000".
    let mut s = Model::new(9_000);
    s.deposit(A, 1).expect("attacker");
    // The 10¹² donation goes to the vault account and never reaches the internal counters.
    let minted = s.deposit(B, 1_000_000_000).expect("victim");
    assert_eq!(minted, 1_000_000_000_000);
    assert_eq!(s.withdraw(B, minted), Ok(1_000_000_000));
}

#[test]
fn jit_rent_capture() {
    // ECON §9.4 JIT table, (a) continuous: rent 36,986,302; attacker 0; honest 31,438,356
    // (85.0 %; 31,438,350 before AIR-03 carried the credit's remainder: the 30 daily credits
    // floored 6 lamports away, now only the true-up's ceil leaves 1); attacker tokens back
    // 9,000,000,000.
    let mut s = Model::new(9_000);
    s.deposit(A, 1_000_000_000).expect("honest");
    let pos = s.borrow30(900_000_000);
    let d_index = WAD * 5_000 * 30 * 86_400 / (10_000 * 31_536_000);
    for _ in 0..30 {
        s.advance(DAY, d_index / 30);
    }
    s.deposit(B, 9_000_000_000).expect("attacker");
    let rent = s.owed(pos);
    assert_eq!(rent, 36_986_302);
    s.repay(pos, rent, 0).expect("repay");
    let shares = s.shares(B);
    assert_eq!(s.withdraw(B, shares), Ok(9_000_000_000));
    assert_eq!(s.claim(B), Ok(0));
    s.advance(VEST + 1, 0);
    assert_eq!(s.claim(A), Ok(31_438_356));
}

#[test]
fn whale_around_the_last_expiry_takes_h_over_6_of_the_lump() {
    // ECON §9.4 / R3B-2, (a) rows: a whale at 9x deposits just before the last position
    // expires (rent 2.4165 SOL, expiry fee 1.2159 SOL) and leaves after h hours.
    let (rent, fee) = (2_416_500_000u64, 1_215_900_000u64);
    for (h, whale, honest) in [
        (0, 0, 3_269_925_000),
        (1, 182_385_000, 3_087_540_000),
        (3, 547_155_000, 2_722_770_000),
        (6, 1_094_310_000, 2_175_615_000),
        (24, 1_094_310_000, 2_175_615_000),
    ] {
        let mut s = Model::new(9_000);
        s.deposit(A, 1_000_000_000_000).expect("H");
        let pos = s.borrow30(500_000_000_000);
        s.advance(30 * DAY, u128::from(rent) * WAD / 500_000_000_000);
        s.deposit(B, 9_000_000_000_000).expect("W");
        s.repay(pos, rent, fee).expect("repay");
        s.advance(h * 3_600, 0);
        let shares = s.shares(B);
        s.withdraw(B, shares).expect("withdraw");
        let mut w = s.claim(B).expect("claim");
        s.advance(VEST + 1, 0);
        w += s.claim(B).expect("claim");
        assert_eq!((w, s.claim(A).expect("claim")), (whale, honest), "h = {h}");
    }
}

#[test]
fn idle_lenders_expired_lock_stops_counting_at_lock_until() {
    // ECON §9.3 / R4A-2, R4B-3 (reviewers' stale_lock.py), v0.4.1 row: E's lock_until is
    // day 32; on day 33, untouched, F and G get 0.4 of T each and E 0.2.
    let (e, f, g) = (0, 1, 2);
    let mut s = Model::new(9_000);
    for u in [e, f, g] {
        s.deposit(u, 1_000_000_000_000).expect("deposit");
    }
    let pa = s.borrow30(750_000_000_000);
    let pb = s.borrow30(750_000_000_000);
    withdraw_max(&mut s, e);
    let until = s.touch(e).lock_until;
    assert_eq!(until, 32 * DAY);
    s.advance(20 * DAY, 0);
    s.borrow30(750_000_000_000);
    s.repay(pa, 0, 0).expect("roll");
    s.advance(9 * DAY, 0);
    s.borrow30(750_000_000_000);
    s.repay(pb, 0, 0).expect("roll");
    s.advance(4 * DAY, 0);
    assert_eq!(s.m.pool.tokens_free, 1_000_000_000_000);
    let got: Vec<u64> = [f, g, e].iter().map(|&u| withdraw_max(&mut s, u)).collect();
    assert_eq!(got, [400_000_000_000, 400_000_000_000, 200_000_000_000]);
}

#[test]
fn locks_cover_the_positions_s3_tail() {
    // ECON §9.3 / R4A-8, R4B-8: a position opened at t = 0 (deadline day 30) with the S3
    // timers at their compiled maxima (3 d) and lock_grace_s at its minimum (1 h): the
    // lock lasts until day 34 (v0.4: 30.04, while the position resolves on day 33).
    let mut s = Model::with_params(9_000, LUMP_VEST_S, 3_600);
    s.deposit(A, 1_000_000_000_000).expect("A");
    s.deposit(B, 1_000_000_000_000).expect("B");
    s.borrow(1_800_000_000_000, 30 * DAY, 3 * DAY)
        .expect("open");
    withdraw_max(&mut s, A);
    assert_eq!(s.touch(A).lock_until, 34 * DAY);
}

#[test]
fn deposit_waits_for_the_post_wipe_sweep() {
    // ECON §9.5 / R4A-7 (reviewer's wipe_dust.py), v0.4.1 row: the wiping keeper's
    // [sol_settle -> deposit(2) -> claim_venue_rewards] is refused (WipeSweepPending); after
    // the 1 SOL of pre-wipe cashback the dust depositor gets 0 and the wiped lenders 1 SOL.
    let mut s = Model::new(10_000);
    s.deposit(A, 100_000_000_000_000).expect("old");
    let p = s.borrow30(100_000_000_000_000);
    s.settle(p, 50_000_000_000, 0).expect("wipe");
    assert_eq!(s.deposit(B, 2), Err(MathError::WipeSweepPending));
    s.sweep(1_000_000_000);
    s.deposit(B, 2).expect("after the sweep");
    s.advance(VEST + 1, 0);
    assert_eq!(s.claim(B), Ok(0));
    assert_eq!(s.touch(A).pending - 50_000_000_000, 1_000_000_000);
    // On a pump venue that can execute, the venue-reward claim is required too (ARCH §3.3).
    let mut s = Model::new(10_000);
    s.deposit(A, 100_000_000_000_000).expect("old");
    let p = s.borrow30(100_000_000_000_000);
    s.settle(p, 50_000_000_000, 0).expect("wipe");
    s.sweep(0);
    assert_eq!(
        s.m.deposit_with(B, 2, true),
        Err(MathError::WipeSweepPending)
    );
    s.m.sweep_from(0, SweepSource::VenueRewards)
        .expect("claim_venue_rewards");
    assert_eq!(s.m.deposit_with(B, 2, true), Ok(2_000));
}

#[test]
fn exits_and_the_unreleased_vest() {
    // ECON §9.4 / R4B-4: an honest lender that leaves 10 min after 3 SOL of lumps gets them
    // all when it is the last lender, 0.083 SOL when a 1,000-unit dust holder stays (which
    // gets 2.917 SOL); a whale at 199x around the lump takes 0 (no flush, v0.4.1).
    for (dust, honest, holder) in [
        (0u64, 3_000_000_000u64, 0u64),
        (1_000, 83_333_332, 2_916_666_667),
    ] {
        let mut s = Model::new(9_000);
        s.deposit(A, 1_000_000_000_000).expect("H");
        if dust > 0 {
            s.deposit(B, dust).expect("D");
        }
        let pos = s.borrow30(500_000_000_000);
        s.advance(3_600, 0);
        s.repay(pos, 0, 3_000_000_000).expect("lumps");
        s.advance(600, 0);
        let shares = s.shares(A);
        s.withdraw(A, shares).expect("exit");
        let mut h = s.claim(A).expect("claim");
        s.advance(VEST + 1, 0);
        h += s.claim(A).expect("claim");
        let d = if dust > 0 {
            s.claim(B).expect("claim")
        } else {
            0
        };
        assert_eq!((h, d), (honest, holder), "dust {dust}");
    }
    let mut s = Model::new(9_000);
    s.deposit(A, 1_000_000_000_000).expect("H");
    let pos = s.borrow30(500_000_000_000);
    s.advance(30 * DAY, 0);
    s.deposit(B, 199_000_000_000_000).expect("W");
    s.repay(pos, 0, 3_000_000_000).expect("lumps");
    let shares = s.shares(B);
    s.withdraw(B, shares).expect("whale exit");
    let mut w = s.claim(B).expect("claim");
    s.advance(VEST + 1, 0);
    w += s.claim(B).expect("claim");
    assert_eq!((w, s.claim(A).expect("claim")), (0, 3_000_000_000));
}

#[test]
fn dust_sweeps_cannot_restart_the_vest() {
    // ECON §9.4 / R4B-5, AIR-02 rows: a 10 SOL lump, then a 1-lamport sweep every k s; share
    // released after 6 h: 100.0 % for every k, one sweep a second included (the vest end rounded
    // up moved 1 s per dust lump: 100.0 / 99.8 / 98.4 % at k = 3,600 / 600 / 60, and 63.2 % at
    // one a second, which held the end 6 h away); after 24 h: 100.0 %.
    for (per, at6) in [
        (3_600, "100.0%"),
        (600, "100.0%"),
        (60, "100.0%"),
        (1, "100.0%"),
    ] {
        let mut s = Model::new(9_000);
        s.deposit(A, 1_000_000_000_000).expect("H");
        s.sweep(10_000_000_000);
        let (mut t, mut r6, mut r24) = (0, 0.0, 0.0);
        while t < 24 * 3_600 {
            s.advance(per, 0);
            t += per;
            s.sweep(1);
            let rel = 1.0 - s.m.pool.vest_bal as f64 / 1e10;
            if t == 6 * 3_600 {
                r6 = rel;
            }
            if t == 24 * 3_600 {
                r24 = rel;
            }
        }
        assert_eq!(format!("{:.1}%", 100.0 * r6), at6, "k = {per}");
        assert_eq!(format!("{:.1}%", 100.0 * r24), "100.0%", "k = {per}");
    }
}

#[test]
fn whale_around_an_s2_s3_captures_no_premium() {
    // ECON §9.4 / R4B-6: a whale at 9x bundles [deposit -> sol_settle -> withdraw -> claim]
    // around the last position (Q = 500 SOL at P_ema, take = B_FV·1.02). v0.4 paid
    // B_FV + R immediately (gain 15.75 / 31.50 / 90.00 SOL); v0.4.1 pays min(x, Q·P_ema + R)
    // and vests the rest (gain −1 lamport: rounding).
    let q = 500_000_000_000u64;
    for (prem, v04, v041) in [
        (350u64, 15_749_999_999i128, -1i128),
        (700, 31_499_999_999, -1),
        (2_000, 89_999_999_999, -1),
    ] {
        let b_fv = q * (10_000 + prem) / 10_000;
        let x = b_fv + (200 * b_fv).div_ceil(10_000);
        for (immediate, gain) in [(b_fv, v04), (q, v041)] {
            let mut s = Model::new(9_000);
            s.deposit(A, 1_000_000_000_000).expect("H");
            let pos = s.borrow30(q);
            s.deposit(B, 9_000_000_000_000).expect("W");
            s.settle(pos, x, x - immediate).expect("settle");
            let tok = withdraw_max(&mut s, B);
            let sol = s.claim(B).expect("claim");
            let left = s.token_value(B);
            let g = i128::from(tok) + i128::from(sol) + i128::try_from(left).expect("fits")
                - 9_000_000_000_000;
            assert_eq!(g, gain, "premium {prem} bps, immediate {immediate}");
        }
    }
}

#[test]
fn sol_without_lenders_goes_to_the_protocol() {
    // ECON §9.5 / R3A-10, v0.4 row: all lenders withdrew (no wipe), then a 1 SOL sweep,
    // then a dust deposit: the dust depositor claims 0, the former lender 0, protocol 1 SOL.
    let mut s = Model::new(9_000);
    s.deposit(A, 1_000_000_000_000).expect("A");
    let shares = s.shares(A);
    s.withdraw(A, shares).expect("exit");
    s.sweep(1_000_000_000);
    s.deposit(B, 1_000_000).expect("dust");
    s.advance(VEST + 1, 0);
    assert_eq!(s.claim(B), Ok(0));
    assert_eq!(s.claim(A), Ok(0));
    assert_eq!(s.m.vault.protocol_fees, 1_000_000_000);
}

/// AIR-08 (I-08): the same market in the other order. With the deposit first, the dust shares
/// were the market's only shares, `sweep_trader` vested the swept SOL (venue cashback claimed
/// into the trader, donations) to them instead of booking it to the protocol (KI-10, R3A-10),
/// and the last-lender flush (R3B-2) paid all of it back in the same transaction:
/// `[deposit(dust), sweep_trader, withdraw]` took 1 SOL. A deposit into a pool without shares
/// now waits for the sweeps since the last shares left, as after a full wipe (R4A-7); the sweep
/// before it books the SOL to the protocol. A fresh market needs none (nothing to sweep).
#[test]
fn a_deposit_into_an_emptied_pool_waits_for_the_sweep() {
    let mut s = Model::new(9_000);
    s.deposit(A, 1_000_000_000_000).expect("A");
    let shares = s.shares(A);
    s.withdraw(A, shares).expect("the last lender leaves");
    assert_eq!(
        s.deposit(B, 1_000),
        Err(MathError::WipeSweepPending),
        "the deposit before the sweep"
    );
    s.sweep(1_000_000_000);
    assert_eq!(s.m.vault.protocol_fees, 1_000_000_000, "KI-10");
    s.deposit(B, 1_000).expect("after the sweep");
    let shares = s.shares(B);
    s.withdraw(B, shares).expect("the dust leaves at once");
    assert_eq!(s.claim(B), Ok(0));
    // On a pump venue (rewards_required) the venue-reward bit is needed too; the last exit
    // clears both bits again.
    let shares = s.shares(B);
    assert_eq!(shares, 0);
    assert_eq!(
        s.m.deposit_with(B, 1_000, true),
        Err(MathError::WipeSweepPending)
    );
    s.m.sweep_from(0, SweepSource::Trader)
        .expect("sweep_trader");
    assert_eq!(
        s.m.deposit_with(B, 1_000, true),
        Err(MathError::WipeSweepPending)
    );
    s.m.sweep_from(0, SweepSource::VenueRewards)
        .expect("the venue-reward sweep");
    assert_eq!(s.m.deposit_with(B, 1_000, true), Ok(1_000_000));
}

#[test]
fn governance_change_of_the_protocol_share() {
    // ECON §9.4 / R3B-9, R3A-5, v0.4 row: rent_protocol_bps 15 % -> 30 % halfway through
    // the only position's life: receivable 0 after the close, lender pending 850,000,000 =
    // lender_sol (the position keeps its snapshot).
    let mut s = Model::new(9_000);
    s.deposit(A, 1_000_000_000_000).expect("L");
    let pos = s.borrow30(500_000_000_000);
    s.advance(0, 1_000_000_000_000_000);
    s.rent.protocol_bps = 3_000;
    s.advance(0, 1_000_000_000_000_000);
    let rent = s.owed(pos);
    s.repay(pos, rent, 0).expect("repay");
    assert_eq!(s.m.pool.receivable, 0);
    assert_eq!(s.touch(A).pending, 850_000_000);
    assert_eq!(s.m.vault.lender_sol, 850_000_000);
}

#[test]
fn lender_run_before_s1() {
    // ECON §9.3 run test: two equal lenders, 90 % utilisation, A runs, then the borrowed
    // inventory is SOL-settled at 50 % of its value.
    let mut s = Model::new(9_000);
    s.deposit(A, 1_000_000_000_000).expect("A");
    s.deposit(B, 1_000_000_000_000).expect("B");
    let pos = s.borrow30(1_800_000_000_000);
    let mut took = 0u64;
    for _ in 0..50 {
        let a = s.free_allowance(A).expect("allowance");
        let sh = s.shares_at(a).max(1).min(s.shares(A));
        match s.withdraw(A, sh) {
            Ok(t) => took += t,
            Err(_) => break,
        }
    }
    s.settle(pos, 900_000_000_000, 0).expect("S1");
    assert_eq!(took, 100_000_000_000);
    let value = |s: &mut Model, u: usize, took: u64| {
        let l = s.touch(u);
        i128::from(took) + i128::try_from(s.token_value(u)).expect("fits") + i128::from(l.pending)
    };
    assert_eq!(value(&mut s, A, took), 573_684_210_526);
    assert_eq!(value(&mut s, B, 0), 526_315_789_473);
}

#[test]
fn claim_loop_pays_once() {
    // ECON §9.4 V2-2 (v0.4 rows): A's fair realised share 76,500,000,000 whatever the calls.
    for n in [1, 2, 5, 20, 100] {
        let mut s = Model::new(9_000);
        s.deposit(A, 1_000_000_000_000).expect("A");
        s.deposit(B, 1_000_000_000_000).expect("B");
        let p1 = s.borrow30(900_000_000_000);
        s.borrow30(900_000_000_000);
        s.advance(0, 200 * WAD / 1_000);
        let owed = s.owed(p1);
        s.repay(p1, owed, 0).expect("repay");
        let pending = i128::from(s.touch(A).pending);
        let (sol, recv) = (
            i128::from(s.m.vault.lender_sol),
            i128::from(s.m.pool.receivable),
        );
        assert_eq!(pending * sol / (sol + recv), 76_500_000_000);
        let got: u64 = (0..n).map(|_| s.claim(A).expect("claim")).sum();
        assert_eq!(got, 76_500_000_000, "n = {n}");
        assert_eq!(s.m.vault.lender_sol, 76_500_000_000, "n = {n}");
    }
}

#[test]
fn runner_advantage_by_utilisation() {
    // ECON §9.3 residual table, v0.4 lock rule, without and with 30 rounds of churn.
    for (util, pct) in [
        (90u64, "+4.3%"),
        (80, "+7.4%"),
        (60, "+10.7%"),
        (50, "+11.1%"),
        (30, "+9.5%"),
    ] {
        for churn in [0, 30] {
            let mut s = Model::new(9_000);
            s.deposit(A, 1_000_000_000_000).expect("A");
            s.deposit(B, 1_000_000_000_000).expect("B");
            let big = util * 2 * 1_000_000_000_000 / 100;
            let pos = s.borrow30(big);
            let drain = |s: &mut Model| {
                let mut took = 0;
                for _ in 0..50 {
                    let a = s.free_allowance(A).expect("allowance");
                    if a == 0 {
                        break;
                    }
                    let sh = s.shares_at(a).max(1).min(s.shares(A));
                    match s.withdraw(A, sh) {
                        Ok(t) => took += t,
                        Err(_) => break,
                    }
                }
                took
            };
            let mut took = drain(&mut s);
            for _ in 0..churn {
                let p = s.m.pool;
                let (t, b) = (i128::from(p.tokens_free), i128::from(p.borrowed_q));
                let c = t.min(9 * (t + b) / 10 - b);
                if c <= 0 {
                    break;
                }
                let id = s
                    .borrow(u64::try_from(c).expect("c"), 3_600, S3_TAIL_S)
                    .expect("churn");
                s.repay(id, 0, 0).expect("close");
                took += drain(&mut s);
            }
            s.settle(pos, big / 2, 0).expect("settle");
            let mut value = |u: usize, took: u64| {
                let l = s.touch(u);
                i128::from(took)
                    + i128::try_from(s.token_value(u)).expect("fits")
                    + i128::from(l.pending)
            };
            let (va, vb) = (value(A, took), value(B, 0));
            let fair = (va + vb) as f64 / 2.0;
            let adv = format!("{:+.1}%", 100.0 * (va as f64 - fair) / fair);
            assert_eq!(adv, pct, "util {util} %, churn {churn}");
            s.m.invariants().expect("invariants");
        }
    }
}

#[test]
fn repeated_full_wipes() {
    // ECON §9.5 / C0-6, v0.4 row: 1e14 mints 1e17 shares; the new lender's pending after a
    // 1,000 SOL lump (vested) + rent is 1,000,042,500,000; after a 2nd wipe the next 1e14
    // would mint 1e17 again; two epochs.
    let mut s = Model::new(10_000);
    s.deposit(A, 100_000_000_000_000).expect("old");
    let p1 = s.borrow30(100_000_000_000_000);
    s.settle(p1, 50_000_000_000, 0).expect("wipe");
    s.sweep(0);
    assert_eq!(
        s.deposit(B, 100_000_000_000_000),
        Ok(100_000_000_000_000_000)
    );
    let p2 = s.borrow30(50_000_000_000_000);
    s.advance(0, WAD / 1_000_000);
    let rent = s.owed(p2);
    s.repay(p2, rent, 1_000_000_000_000).expect("repay");
    s.advance(VEST + 1, 0);
    assert_eq!(s.touch(B).pending, 1_000_042_500_000);
    let t = s.m.pool.tokens_free;
    let p3 = s.borrow30(t);
    s.settle(p3, 1_000_000_000, 0).expect("wipe");
    assert_eq!(
        pool::shares_for_tokens(&s.m.pool, 100_000_000_000_000),
        Ok(100_000_000_000_000_000)
    );
    assert_eq!(s.m.pool.share_epoch, 2);
}

#[test]
fn post_wipe_credits_and_the_epoch_jump() {
    // ECON §9.5 / V2-12, R3B-2: the old epoch's lenders +4 SOL (3 SOL unreleased vest +
    // 1 SOL post-wipe sweep); a dust depositor that arrives first and triggers a 1 SOL sweep
    // gets 0 at once and 2,777,777 in total after a lender joins a minute later (which gets
    // 997,222,222); a lender with 0 shares 6 epochs behind catches up in one touch.
    let (old, dust, late) = (0, 1, 2);
    let mut s = Model::new(10_000);
    s.deposit(old, 100_000_000_000_000).expect("old");
    let p1 = s.borrow30(100_000_000_000_000);
    s.repay(p1, 0, 3_000_000_000).expect("lump");
    let p2 = s.borrow30(100_000_000_000_000);
    s.settle(p2, 50_000_000_000, 0).expect("wipe");
    s.sweep(1_000_000_000);
    assert_eq!(s.touch(old).pending - 50_000_000_000, 4_000_000_000);
    s.deposit(dust, 1).expect("dust");
    s.sweep(1_000_000_000);
    let now_dust = s.claim(dust).expect("claim");
    s.advance(60, 0);
    s.deposit(late, 100_000_000_000_000).expect("late");
    s.advance(VEST, 0);
    let dust_total = now_dust + s.claim(dust).expect("claim");
    assert_eq!(
        (now_dust, dust_total, s.claim(late).expect("claim")),
        (0, 2_777_777, 997_222_222)
    );
    for i in 0..5 {
        s.deposit(10 + i, 100_000_000_000_000).expect("x");
        let t = s.m.pool.tokens_free;
        let id = s.borrow30(t);
        s.settle(id, 1_000_000_000, 0).expect("wipe");
        s.sweep(0);
        s.deposit(20 + i, 1_000).expect("y");
    }
    let before = s.m.lender(old).expect("old").epoch;
    assert_eq!(s.m.pool.share_epoch - before, 6);
    // One touch with one record: the wipe record converts the parked shares to 0, then the
    // lender jumps over the five later epochs without reading their records.
    let (p, mut v, mut l) = (s.m.pool, s.m.vault, *s.m.lender(old).expect("old"));
    assert_eq!(
        pool::touch(&p, &mut v, &mut l, &mut [], s.m.now),
        Err(MathError::LenderBehind)
    );
    assert_eq!(
        pool::touch(&p, &mut v, &mut l, &mut s.m.records[..1], s.m.now),
        Ok(pool::Standing::Current)
    );
    assert_eq!((l.epoch, l.shares), (s.m.pool.share_epoch, 0));
}

/// AIR-07 (I-07), the attack: every token is lent and an attacker deposits 1 raw unit before the
/// last settlement, so the settlement leaves 1 raw token instead of 0: the epoch rebases instead
/// of wiping, the flush of the unreleased vest (ECON §9.5: "the wiped lenders keep their
/// unreleased lumps") is skipped, and a depositor of 10⁶ raw units right after held nearly every
/// share and took 4,999,993,333 lamports of the 5 SOL lump. A settlement that leaves at most
/// 1/1,000 of the token claims it found flushes the vest to the shares that held them; since
/// AIR-07's residual fix the dust deposit itself is refused (every token lent: the pool is
/// drained), so the settlement wipes and the vest goes with the wipe record (49,999,999,998
/// through the flush of a rebase before that fix; the rebase case stays covered by the organic
/// test below, where a run leaves the dust).
#[test]
fn a_dust_deposit_before_the_last_settlement_takes_no_vest() {
    let mut s = Model::new(10_000);
    s.deposit(A, 1_000_000_000_000).expect("A");
    s.deposit(B, 1_000_000_000_000).expect("B");
    let t = s.m.pool.tokens_free;
    let pos = s.borrow30(t);
    assert_eq!(
        s.deposit(2, 1),
        Err(MathError::PoolDrained),
        "the dust deposit"
    );
    s.settle(pos, 50 * SOL, 5 * SOL)
        .expect("the last settlement");
    assert_eq!(s.m.records[0].num, 0, "a full wipe");
    assert_eq!(
        s.m.pool.vest_bal, 0,
        "the vest is flushed into the wipe record"
    );
    s.sweep(0); // the post-wipe sweep the newcomer's deposit waits for
    s.deposit(3, 1_000_000).expect("the newcomer");
    s.advance(VEST + 1, 0);
    assert_eq!(s.claim(3), Ok(0), "the newcomer");
    let old = s.claim(A).expect("A") + s.claim(B).expect("B");
    assert_eq!(old, 50 * SOL, "the old lenders (before: 45,000,006,664)");
}

/// AIR-07, the organic case (no attacker): two lenders withdraw everything they can while 90 %
/// is borrowed; the allowance's floor leaves 1 raw token free, so the last settlement rebased and
/// a newcomer's 10⁶ raw units took 3,999,992,004 lamports of its 4 SOL lump (the impact
/// reviewer's 300 random pools all ended at T = 1). Now the run's dust counts as a wipe.
#[test]
fn the_dust_a_run_leaves_counts_as_a_wipe_for_the_vest() {
    let mut s = Model::new(9_000);
    s.deposit(A, 1_000_000_000_000).expect("A");
    s.deposit(B, 1_000_000_000_000).expect("B");
    let pos = s.borrow30(s.m.pool.tokens_free * 9 / 10);
    exit_all(&mut s, A, 8);
    exit_all(&mut s, B, 8);
    assert_eq!(s.m.pool.tokens_free, 1, "the allowance's floor");
    s.advance(60, 0);
    s.settle(pos, 50 * SOL, 4 * SOL)
        .expect("the last settlement");
    assert_eq!(s.m.records[0].num, 1, "a rebase, not a wipe");
    assert_eq!(
        s.m.pool.vest_bal, 0,
        "the vest is flushed to the old shares"
    );
    s.deposit(2, 1_000_000).expect("the newcomer");
    s.advance(VEST + 1, 0);
    assert_eq!(s.claim(2), Ok(0), "the newcomer");
    let old = s.claim(A).expect("A") + s.claim(B).expect("B");
    assert_eq!(
        old, 49_999_999_999,
        "the old lenders (before: 46,000,007,993)"
    );
}

/// AIR-07 against R4B-6 (the flush is at the settlement, so a just-in-time deposit would share in
/// it): the flush needs the settlement to leave at most 1/1,000 of the claims and the deposit's
/// own tokens stay as claims, so a bundle `[deposit x, sol_settle, withdraw, claim]` took at most
/// `x/claims` ≤ 0.1 % of the vest (4,995,003 lamports of a 5 SOL lump for x = 10⁹ around the
/// settlement of 10¹²; a larger deposit kept the lump vesting and took 0 of it at once). A
/// flushing settlement starts only from a drained pool, where AIR-07's residual fix refuses every
/// deposit: the JIT deposit is refused for each `x` and the lender keeps the whole lump (a
/// deposit allowed with more than 1/1,000 unborrowed can never be followed by a flush from the
/// same state). The R4B-6 rows hold (`premium_jit_table`).
#[test]
fn a_jit_deposit_around_a_wiping_settlement_takes_at_most_a_thousandth() {
    for x in [1_000_000u64, 1_000_000_000, 10_000_000_000] {
        let mut s = Model::new(10_000);
        s.deposit(A, 1_000_000_000_000).expect("H");
        let t = s.m.pool.tokens_free;
        let pos = s.borrow30(t);
        assert_eq!(
            s.deposit(B, x),
            Err(MathError::PoolDrained),
            "the JIT deposit, x = {x}"
        );
        // S2/S3 at P_ema = 1 lamport per raw unit: Q·P_ema immediate, a 5 SOL lump vests.
        s.settle(pos, t + 5 * SOL, 5 * SOL).expect("settle");
        s.advance(VEST + 1, 0);
        assert_eq!(s.claim(A), Ok(t + 5 * SOL), "the lender, x = {x}");
    }
}

/// AIR-07's residual: a deposit above 1/999 of the claims made just before the settlement keeps
/// its own tokens as claims, so the settlement leaves more than 1/1,000 of them and the vest is
/// not flushed; a deposit after it, at the collapsed share price, then held most of the shares
/// and took 4,613,534,243 lamports of the old lenders' 5 SOL lump by holding through the vest
/// (`rebase_vest_demo`'s residual row: 3·10⁹ before, 3·10¹⁰ after, on 2·10¹² lent). No deposit
/// is taken while the pool is drained (`ΣB_Q > 0`, `1,000·T ≤ T + ΣB_Q`), the only states a
/// flushing settlement starts from: the pre-deposit is refused, the settlement wipes, and the
/// later deposit (after the post-wipe sweep) takes none of the vest.
#[test]
fn a_deposit_above_a_thousandth_cannot_keep_the_vest_from_the_old_lenders() {
    let mut s = Model::new(10_000);
    s.deposit(A, 1_000_000_000_000).expect("A");
    s.deposit(B, 1_000_000_000_000).expect("B");
    let t = s.m.pool.tokens_free;
    let pos = s.borrow30(t);
    let pre = s.deposit(2, 3_000_000_000);
    s.settle(pos, 50 * SOL, 5 * SOL)
        .expect("the last settlement");
    if s.m.pool.total_shares == 0 {
        s.sweep(0); // the post-wipe sweep the later deposit waits for
    }
    s.deposit(3, 30_000_000_000).expect("the later deposit");
    s.advance(VEST + 1, 0);
    let mut out = s.claim(3).expect("the later depositor");
    if pre.is_ok() {
        out += s.claim(2).expect("the pre-depositor");
    }
    assert!(
        out <= 5 * SOL / 1_000,
        "the outsiders took {out} lamports of the 5 SOL lump"
    );
    assert_eq!(pre, Err(MathError::PoolDrained), "the pre-deposit");
    let old = s.claim(A).expect("A") + s.claim(B).expect("B");
    assert_eq!(old, 50 * SOL, "the old lenders (before: 45,386,465,756)");
}

#[test]
fn early_claim_then_shortfall() {
    // ECON §9.4 / C0-13, v0.4 row: A claimed 500,000,000,000; A pending after the 90 %
    // shortfall 1,688,356,163; A could exit; B claims 501,688,356,164; B pending 0;
    // sol_vault 1,688,356,165; receivable 0.
    let mut s = Model::new(9_000);
    s.deposit(A, 1_000_000_000_000).expect("A");
    s.deposit(B, 1_000_000_000_000).expect("B");
    let p1 = s.borrow30(1_000_000_000_000);
    s.advance(0, WAD * 5_000 * 29 * 86_400 / (10_000 * 31_536_000));
    let p2 = s.borrow30(500_000_000_000);
    s.settle(p2, 1_000_000_000_000, 0).expect("settle");
    assert_eq!(s.claim(A), Ok(500_000_000_000));
    let owed = s.owed(p1);
    s.repay(p1, owed / 10, 0).expect("shortfall");
    assert_eq!(s.touch(A).pending, 1_688_356_163);
    let shares = s.shares(A);
    assert!(s.withdraw(A, shares).is_ok(), "A can exit");
    assert_eq!(s.claim(B), Ok(501_688_356_164));
    assert_eq!(s.touch(B).pending, 0);
    assert_eq!(s.m.vault.lender_sol, 1_688_356_165);
    assert_eq!(s.m.pool.receivable, 0);
}

#[test]
fn stuck_lock_clears_at_lock_until() {
    // ECON §9.3 / R3B-1 table, v0.4 (Π + lock_until) rows: A's allowance as a share of its
    // pro-rata part of unborrowed inventory at 24 h, 7 d, 30 d and 32 d.
    let u = 1_000_000_000u64;
    for (growing, cells) in [
        (true, ["0%", "0%", "0%", "100%"]),
        (false, ["94%", "94%", "94%", "100%"]),
    ] {
        let mut s = Model::new(9_000);
        s.deposit(A, 500 * u).expect("A");
        s.deposit(B, 500 * u).expect("B");
        let mut ids = std::collections::HashMap::new();
        if growing {
            ids.insert("p0", s.borrow30(900 * u));
        } else {
            for (k, q) in [("p0a", 300), ("p0b", 300), ("p0c", 200), ("p0d", 100)] {
                ids.insert(k, s.borrow30(q * u));
            }
        }
        let sh = s.touch(A).shares * u128::from(s.free_allowance(A).expect("allowance"))
            / u128::from(500 * u)
            - 1;
        s.withdraw(A, sh).expect("withdraw");
        if growing {
            s.deposit(2, 2_000 * u).expect("C");
            s.borrow30(1_000 * u);
        }
        let mut got = Vec::new();
        let mut old = ids.get("p0").copied().unwrap_or(0);
        let fades = [(6, "p0a"), (12, "p0b"), (24, "p0c")];
        for step in 1..=32 * 24 {
            s.advance(3_600, 0);
            if growing && step % 24 == 0 {
                s.repay(old, 0, 0).expect("roll");
                old = s.borrow30(900 * u);
            }
            if !growing && let Some((_, k)) = fades.iter().find(|(h, _)| *h == step) {
                s.repay(ids[k], 0, 0).expect("close");
            }
            if [24, 7 * 24, 30 * 24, 32 * 24].contains(&step) {
                let l = s.touch(A);
                let p = s.m.pool;
                let fair =
                    l.shares * u128::from(p.tokens_free) / (p.total_shares + pool::VIRTUAL_SHARES);
                let a = s.free_allowance(A).expect("allowance");
                let frac = if fair == 0 {
                    1.0
                } else {
                    a as f64 / fair as f64
                };
                got.push(format!("{:.0}%", 100.0 * frac));
            }
        }
        assert_eq!(got, cells, "growing {growing}");
    }
}

/// AIR-12 (I-12): a lock era ends when every lock has expired (`lock_norm` back to 0) while
/// borrowing goes on (a small position stays open, so `ΣB_Q` never reaches 0 and the `Π` epoch
/// never rolls). The next lock restarted `b_min` at `ΣB_Q` but kept `Π`, so every era's decay
/// multiplied into the next: with a 1/1,001 repay per era `Π` fell 10¹⁸ → 10¹⁵ → … → 1 by the
/// sixth era, after which a lock no longer shrank when its borrowers repaid (`⌈lock·1/1⌉`), only
/// at `lock_until` (`pool_model.lock_era_demo`). Now `Π` restarts at `WAD` with `b_min`, and
/// every era's lock decays as the first one did.
#[test]
fn lock_decay_restarts_with_every_lock_era() {
    let mut s = Model::new(9_000);
    s.deposit(A, 1_000_000_000_000).expect("A");
    s.deposit(B, 1_000_000_000_000).expect("B");
    s.borrow(1_000_000_000, 3_600, S3_TAIL_S)
        .expect("the small position, kept open");
    for era in 0..8 {
        let big = s
            .borrow(1_000_000_000_000, 3_600, S3_TAIL_S)
            .expect("an open");
        let a = s.free_allowance(A).expect("allowance");
        let sh = (s.shares_at(a) / 2).max(1);
        s.withdraw(A, sh)
            .expect("A takes half its free part: a lock");
        let set = s.m.lock_of(A).expect("lock");
        assert!(set > 0, "era {era}");
        assert_eq!(s.m.vault.pi, WAD, "era {era}: Π restarts with the lock era");
        s.repay(big, 0, 0)
            .expect("the open closes: ΣB_Q falls by 1,000/1,001");
        let left = s.m.lock_of(A).expect("lock");
        assert!(
            left <= set / 1_000 + 1,
            "era {era}: the lock decays with the repay ({set} -> {left})"
        );
        // Past A's lock_until: the lock expires and its bucket leaves lock_norm.
        let until = s.touch(A).lock_until;
        s.advance(until - s.m.now + 1, 0);
        s.m.cal_advance().expect("calendar");
        assert_eq!(s.m.vault.lock_norm, 0, "era {era}: the era ends");
    }
}
