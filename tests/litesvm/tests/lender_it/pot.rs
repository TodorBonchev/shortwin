//! T-POT (ARCH §8.2): the lender pot, vesting and lock liveness on the program, with
//! `pool_model.py` v0.4.1's numbers (each test names the model function it reproduces; the
//! numbers are the model's output, to the lamport and the raw unit). The wind-down stranding
//! table and the lock-heavy mix are replayed from the model's traces in `traces.rs`.

use shortwin::errors::ShortwinError;
use shortwin::state::VenueKind;
use shortwin_litesvm_tests::invariants::lender::wind_down;
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_math::pool::{SweepSource, VIRTUAL_SHARES};

use crate::common::{D, Lab, code, params_change, propose_and_apply};

const U: u64 = 1_000_000_000;

/// `pool_model.jit_hold_table()`, option (a) rows (R3B-2): a whale deposits 9× the pool just
/// before the last open position repays 2.4165 SOL of rent and a 1.2159 SOL expiry fee (ECON
/// §12.2 Tier A) and leaves after `h` hours. At `h = 0` it captures nothing.
#[test]
fn jit_lump_capture_is_zero_at_h0_and_linear_in_h() {
    let rent: u64 = 2_416_500_000;
    let fee: u64 = 1_215_900_000;
    let rows: [(i64, u64, u64); 5] = [
        (0, 0, 3_269_925_000),
        (1, 182_385_000, 3_087_540_000),
        (3, 547_155_000, 2_722_770_000),
        (6, 1_094_310_000, 2_175_615_000),
        (24, 1_094_310_000, 2_175_615_000),
    ];
    for (i, (h, whale, honest)) in rows.into_iter().enumerate() {
        let program = TokenProgram::BOTH[i % 2];
        let mut lab = Lab::new(program);
        lab.deposit("H", 1_000_000_000_000).unwrap();
        lab.borrow30("pos", 500_000_000_000).unwrap();
        lab.advance(
            30 * D,
            u128::from(rent) * shortwin_math::num::WAD / 500_000_000_000,
        );
        lab.deposit("W", 9_000_000_000_000).unwrap();
        lab.repay("pos", rent, fee);
        lab.advance(h * 3_600, 0);
        let shares = lab.position("W").unwrap().shares;
        let (_, mut w) = lab.withdraw("W", shares).unwrap();
        w += lab.claim("W").unwrap();
        lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
        w += lab.claim("W").unwrap();
        let hs = lab.claim("H").unwrap();
        assert_eq!((w, hs), (whale, honest), "h = {h}");
    }
}

/// `pool_model.stale_lock_demo()`, v0.4.1 row (R4A-2, R4B-3): E takes its free part at 50 %
/// utilisation; positions roll so Π stays; on day 33 nobody has touched E; F, G, then E
/// withdraw the most they may. The calendar drops E's lock at its `lock_until` (day 32), so
/// F and G get 40 % each and E still gets its 20 %.
#[test]
fn an_idle_lender_past_lock_until_keeps_its_pro_rata_part() {
    for program in TokenProgram::BOTH {
        let mut lab = Lab::new(program);
        for w in ["E", "F", "G"] {
            lab.deposit(w, 1_000_000_000_000).unwrap();
        }
        lab.borrow30("pA", 750_000_000_000).unwrap();
        lab.borrow30("pB", 750_000_000_000).unwrap();
        let a = lab.free_allowance("E");
        let s = lab.shares_for("E", a);
        lab.withdraw("E", s).unwrap();
        let until = lab.position("E").unwrap().lock_until;
        assert_eq!(
            until - lab.base,
            32 * D,
            "lock_until on the day-32 boundary"
        );
        lab.advance(20 * D, 0);
        lab.borrow30("pC", 750_000_000_000).unwrap();
        lab.repay("pA", 0, 0);
        lab.advance(9 * D, 0);
        lab.borrow30("pD", 750_000_000_000).unwrap();
        lab.repay("pB", 0, 0);
        lab.advance(4 * D, 0);
        assert_eq!(lab.market().tokens_free, 1_000_000_000_000);
        let mut got = Vec::new();
        for w in ["F", "G", "E"] {
            let a = lab.free_allowance(w);
            let sh = lab.shares_for(w, a);
            got.push(if sh > 0 {
                lab.withdraw(w, sh).unwrap().0
            } else {
                0
            });
        }
        assert_eq!(got, [400_000_000_000, 400_000_000_000, 200_000_000_000]);
    }
}

/// `pool_model.stuck_lock_table()` v0.4 rows (R3B-1 and the Π decision): A and B deposit 500
/// each, 900 is borrowed, A withdraws its free part once. (i) The market grows and stays busy
/// (positions replaced every day): A's allowance is 0 until its `lock_until`, then its whole
/// pro-rata part. (ii) Interest fades (100 of 900 stays out): Π returns 94 % within a day.
#[test]
fn stuck_locks_clear_at_lock_until_and_fade_with_pi() {
    let checkpoints = [24i64, 7 * 24, 30 * 24, 32 * 24];
    let growing = [
        (0, 160_169_491_525),
        (0, 160_169_491_525),
        (0, 160_169_491_525),
        (160_169_491_525, 160_169_491_525),
    ];
    let fades = [
        (377_777_777_778, 402_631_578_947),
        (377_777_777_778, 402_631_578_947),
        (377_777_777_778, 402_631_578_947),
        (402_631_578_947, 402_631_578_947),
    ];
    for (scen, want) in [("growing", growing), ("fades", fades)] {
        let mut lab = Lab::new(TokenProgram::Spl);
        lab.deposit("A", 500 * U).unwrap();
        lab.deposit("B", 500 * U).unwrap();
        if scen == "growing" {
            lab.borrow30("p0", 900 * U).unwrap();
        } else {
            for (pid, q) in [("p0a", 300), ("p0b", 300), ("p0c", 200), ("p0d", 100)] {
                lab.borrow(pid, q * U, 30 * D, 4_800).unwrap();
            }
        }
        let shares = lab.touch("A").unwrap().shares;
        let s = shares * u128::from(lab.free_allowance("A")) / u128::from(500 * U) - 1;
        assert_eq!(s, 49_999_999_999_999);
        assert_eq!(lab.withdraw("A", s).unwrap().0, 49_999_999_999);
        let l = lab.position("A").unwrap();
        assert_eq!(
            (l.lock, l.lock_until - lab.base),
            (449_999_999_999_991, 2_764_800)
        );
        if scen == "growing" {
            lab.deposit("C", 2_000 * U).unwrap();
            lab.borrow30("p1", 1_000 * U).unwrap();
        }
        let mut k = 0;
        let mut cells = Vec::new();
        for step in 1..=checkpoints[3] {
            lab.advance(3_600, 0);
            if scen == "growing" && step % 24 == 0 {
                let old = if k == 0 {
                    "p0".to_owned()
                } else {
                    format!("r{}", k - 1)
                };
                lab.repay(&old, 0, 0);
                lab.borrow30(&format!("r{k}"), 900 * U).unwrap();
                k += 1;
            }
            if scen == "fades" {
                for (hh, pid) in [(6, "p0a"), (12, "p0b"), (24, "p0c")] {
                    if step == hh {
                        lab.repay(pid, 0, 0);
                    }
                }
            }
            if checkpoints.contains(&step) {
                let l = lab.touch("A").unwrap();
                let m = lab.market();
                let fair = u64::try_from(
                    l.shares * u128::from(m.tokens_free) / (m.total_shares.get() + VIRTUAL_SHARES),
                )
                .unwrap();
                cells.push((lab.free_allowance("A"), fair));
            }
        }
        assert_eq!(cells, want, "{scen}");
    }
}

/// `pool_model.lock_tail_demo()`, v0.4.1 row (R4A-8, R4B-8): a position opened at t = 0 with a
/// 30-day deadline can be stuck in `PumpMigrating` and reach S3 only after its tail
/// (`max_migration_pause_s + grad_grace_s + s3_wait_s`, each at its compiled maximum of 1 day);
/// with `lock_grace_s` at its compiled minimum (1 h, set through governance) a lock taken then
/// still expires after the position's latest release: day 34 ≥ day 33.
#[test]
fn a_lock_outlives_a_position_stuck_in_migration() {
    let mut env = shortwin_litesvm_tests::Env::initialized();
    let change = params_change(&env, |p| p.lock_grace_s = 3_600);
    propose_and_apply(&mut env, change);
    assert_eq!(env.config().params.lock_grace_s, 3_600);
    let mut lab = Lab::on(env, TokenProgram::Token2022, 9_000);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    lab.borrow("pos", 1_800_000_000_000, 30 * D, 3 * D).unwrap();
    let a = lab.free_allowance("A");
    let s = lab.shares_for("A", a);
    lab.withdraw("A", s).unwrap();
    let l = lab.position("A").unwrap();
    assert_eq!(l.lock_until - lab.base, 34 * D);
    assert!(l.lock_until >= lab.base + 30 * D + 3 * D);
    assert!(l.lock > 0);
}

/// `pool_model.restart_grief_table()`, AIR-02 row k = 60 (R4B-5): a 10 SOL lump, then a
/// 1-lamport sweep every minute for a day. With the value-weighted vest end rounded down, a dust
/// lump leaves the end where it is: the whole lump is released at 6 h, only the latest minute's
/// lamport is still vesting (rounded up, each sweep moved the end 1 s: 98.4 %, 155,270,942
/// lamports left; v0.4 restarted the window: 63.3 %), and the lender is paid all of it.
#[test]
fn one_lamport_sweeps_every_minute_cannot_stall_the_vest() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("H", 1_000_000_000_000).unwrap();
    lab.sweep_sol(10 * U);
    let mut t = 0;
    let mut at = std::collections::BTreeMap::new();
    while t < 24 * 3_600 {
        lab.advance(60, 0);
        t += 60;
        lab.sweep_sol(1);
        if t % 3_600 == 0 {
            at.insert(t / 3_600, lab.market().vest_bal);
        }
    }
    assert_eq!(
        (at[&6], at[&24]),
        (1, 241),
        "AIR-02: the whole lump released at 6 h"
    );
    // The last lender out is paid everything, the unreleased rest included (R3B-2).
    let shares = lab.position("H").unwrap().shares;
    assert_eq!(
        lab.withdraw("H", shares),
        Ok((1_000_000_000_000, 10_000_001_440))
    );
    assert_eq!(lab.vault().lender_sol, 0);
}

/// `pool_model.wipe_gate_demo()`, v0.4.1 row (R4A-7): the keeper that executes the wiping
/// settlement cannot land a dust deposit before the post-wipe sweep; the 1 SOL of pre-wipe
/// cashback goes to the wiped lenders. On a pump venue the deposit also waits for the venue
/// reward claim.
#[test]
fn a_deposit_into_a_live_wipe_record_waits_for_the_sweeps() {
    let pending = code(ShortwinError::WipeSweepPending);
    for venue in [VenueKind::None, VenueKind::PumpCurve] {
        let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
        lab.set_venue(venue);
        lab.deposit("old", 100_000_000_000_000).unwrap();
        lab.borrow30("1", 100_000_000_000_000).unwrap();
        lab.settle("1", 50 * U, 0);
        assert_eq!(lab.deposit("ATT", 2), Err(pending));
        if venue == VenueKind::PumpCurve {
            lab.sweep_from(U, SweepSource::VenueRewards);
            assert_eq!(lab.deposit("ATT", 2), Err(pending));
            lab.sweep_sol(0);
        } else {
            lab.sweep_sol(U);
        }
        lab.deposit("ATT", 2).unwrap();
        lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
        assert_eq!(lab.claim("ATT"), Ok(0));
        let old = lab.touch("old").unwrap();
        assert_eq!(old.pending, 51 * i64::try_from(U).unwrap());
    }
}

/// `pool_model.premium_jit_table()`, v0.4.1 rows (R4B-6): a whale bundles [deposit 9× →
/// sol_settle(S3) of the last position → withdraw → claim]. Only `min(x, ⌊Q·P_ema/WAD⌋ +
/// R_due)` of the take is immediate; the premium over market value vests, so the whale gains
/// nothing (−1 raw unit of rounding) at every premium.
#[test]
fn a_whale_around_the_last_settlement_captures_nothing() {
    let q: u64 = 500_000_000_000;
    for prem in [350u64, 700, 2_000] {
        let b_fv = q * (10_000 + prem) / 10_000;
        let x = b_fv + (200 * b_fv).div_ceil(10_000);
        let mut lab = Lab::new(TokenProgram::Token2022);
        lab.deposit("H", 1_000_000_000_000).unwrap();
        lab.borrow30("pos", q).unwrap();
        lab.deposit("W", 9_000_000_000_000).unwrap();
        lab.settle("pos", x, x - q);
        let a = lab.free_allowance("W");
        let s = lab.shares_for("W", a);
        let (tok, sol) = lab.withdraw("W", s).unwrap();
        let sol = sol + lab.claim("W").unwrap();
        let left = lab.token_value("W");
        let gain =
            i128::from(tok) + i128::from(sol) + i128::try_from(left).unwrap() - 9_000_000_000_000;
        assert_eq!(gain, -1, "premium {prem} bps");
    }
}

/// The pot (R3A-2) end to end on the program: A's rent entitlement accrues, A withdraws what
/// it may before the rent is paid (it keeps its entitlement; nothing is realised yet), C joins
/// late and is allocated realised SOL it is not entitled to; C's surplus reaches the pot at
/// C's next touch (`sync_lender`, permissionless; behind a rebase record only C's owner
/// converts it, KI65-REV-SEC1, and anyone else's sync is the pot-only touch, KI-66 — `ki65_it`'s
/// `a_surplus_behind_a_rebase_record_reaches_the_pot_without_its_owner`) and A draws its
/// deficit from it. Nothing is stranded beyond rounding.
#[test]
fn the_pot_connects_early_leavers_and_late_joiners() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    lab.borrow30("p", 900_000_000_000).unwrap();
    lab.advance(10 * D, 10u128.pow(16));
    let a = lab.free_allowance("A");
    let s = lab.shares_for("A", a);
    let (_, paid) = lab.withdraw("A", s).unwrap();
    assert_eq!(paid, 0, "nothing realised yet");
    let a_pending = lab.position("A").unwrap().pending;
    assert!(a_pending > 0);
    lab.deposit("C", 2_000_000_000_000).unwrap();
    let owed = lab.owed("p");
    lab.repay("p", owed, 0);
    let first = lab.claim("A").unwrap();
    assert!(
        first < u64::try_from(a_pending).unwrap(),
        "own allocation short"
    );
    lab.sync("C", 0).unwrap();
    assert!(lab.vault().unmatched > 0, "C's surplus reached the pot");
    let second = lab.claim("A").unwrap();
    assert!(second > 0);
    assert_eq!(
        lab.position("A").unwrap().pending,
        0,
        "A drew its whole deficit"
    );
    for u in ["B", "C"] {
        lab.claim(u).unwrap();
    }
    // What is left is rounding: a few lamports per touch.
    let w = wind_down(&lab.state()).unwrap();
    assert!(w.stranded <= 10, "stranded {w:?}");
    for u in ["A", "B", "C"] {
        let l = lab.touch(u).unwrap();
        assert!(l.pending <= 10, "{u} pending {}", l.pending);
    }
}
