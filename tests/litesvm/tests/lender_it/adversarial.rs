//! The lender adversarial cases of ARCH §8.2 / M1 on the program (C0-6, C0-9, C0-13, R3A-2,
//! R3A-10, R3B-1, R3B-2, V2-2, V2-12), with `pool_model.py` v0.4.1's numbers where the model
//! has a demo (named in each test). The invariant hooks run after every transaction and
//! every injection; the stranding table (R3A-2) is replayed in `traces.rs`.

use shortwin::errors::ShortwinError;
use shortwin_litesvm_tests::errors::ok;
use shortwin_litesvm_tests::invariants::lender::wind_down;
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_litesvm_tests::{events, ix};
use shortwin_math::num::WAD;
use shortwin_math::pool::{R_MAX, VIRTUAL_ASSETS, VIRTUAL_SHARES};
use solana_signer::Signer;

use crate::common::{BPS, D, Lab, cdiv, code};

const U: u64 = 1_000_000_000;

/// C0-6 / V2-12, `pool_model.wipe_demo()` v0.4 row: repeated full SOL settlements. After a
/// full wipe the next deposit mints at the initial rate again (1e14 → 1e17 shares, v0.1 would
/// mint 1e31), the new lender earns its rent and lump (1,000,042,500,000 lamports), and after a
/// second wipe a deposit still mints 1e17: share counts stay bounded (2 epochs rolled).
#[test]
fn c0_6_repeated_full_settlements_keep_shares_bounded() {
    let mut lab = Lab::with_cap(TokenProgram::Token2022, 10_000);
    lab.deposit("old", 100_000_000_000_000).unwrap();
    lab.borrow30("1", 100_000_000_000_000).unwrap();
    lab.settle("1", 50 * U, 0);
    lab.sweep_sol(0);
    assert_eq!(
        lab.deposit("new", 100_000_000_000_000),
        Ok(100_000_000_000_000_000)
    );
    lab.borrow30("2", 50_000_000_000_000).unwrap();
    lab.accrue(WAD / 1_000_000);
    lab.repay(
        "2",
        u64::try_from(cdiv(50_000_000_000_000 * (WAD / 1_000_000), WAD)).unwrap(),
        1_000_000_000_000,
    );
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    assert_eq!(lab.touch("new").unwrap().pending, 1_000_042_500_000);
    let t = lab.market().tokens_free;
    lab.borrow30("3", t).unwrap();
    lab.settle("3", U, 0);
    let m = lab.market();
    let p = m.pool();
    let m2 = 100_000_000_000_000u128 * (p.total_shares + VIRTUAL_SHARES)
        / (u128::from(p.tokens_free) + u128::from(p.borrowed_q) + VIRTUAL_ASSETS);
    assert_eq!((m2, m.share_epoch), (100_000_000_000_000_000, 2));
    // Both wiped lenders still claim their SOL through the records.
    lab.sweep_sol(0);
    let paid_new = lab.claim("new").unwrap();
    let paid_old = lab.claim("old").unwrap();
    assert!(
        paid_new >= 1_000_042_500_000 && paid_old == 50 * U,
        "{paid_new} {paid_old}"
    );
}

/// C0-6: a settlement that leaves one raw token behind a huge share count rebases the epoch
/// (`S + VS ≤ R_MAX·(T + ΣB_Q + VA)` again) instead of letting shares overflow; a lender
/// behind several rebases converts through each record.
#[test]
fn c0_6_share_blow_up_rebases() {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("A", 10_000_000_000_000).unwrap();
    for i in 0..4 {
        if i > 0 {
            lab.deposit("B", 10_000_000_000_000).unwrap();
        }
        let t = lab.market().tokens_free;
        lab.borrow30(&format!("p{i}"), t - 1).unwrap();
        lab.settle(&format!("p{i}"), 7 * U, 0);
        let p = lab.market().pool();
        let claims = u128::from(p.tokens_free) + u128::from(p.borrowed_q) + VIRTUAL_ASSETS;
        assert!(
            p.total_shares + VIRTUAL_SHARES <= R_MAX * claims,
            "rebased: {p:?}"
        );
    }
    assert_eq!(lab.market().share_epoch, 4);
    // A is 4 epochs behind; its claim needs every record and pays its SOL.
    let paid = lab.claim("A").unwrap();
    assert!(paid >= 7 * U - 10, "A was paid {paid}");
    // A new depositor after the rebases mints a sane number of shares.
    let minted = lab.deposit("C", 1_000_000).unwrap();
    assert!(minted <= R_MAX * 1_000_000);
}

/// V2-12, `pool_model.wipe_tail_demo()`: SOL credited while `S = 0` after a full wipe (the
/// unreleased vest at the wipe and a 1 SOL post-wipe sweep) goes to the wiped lenders (4 SOL);
/// a dust depositor that arrives first and triggers another sweep gets it only over the
/// vesting window (2,777,777 lamports in total, the late lender 997,222,222); a lender with 0
/// shares 6 epochs behind is caught up in one touch.
#[test]
fn v2_12_post_wipe_credits_go_to_the_wiped_lenders() {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("old", 100_000_000_000_000).unwrap();
    lab.borrow30("1", 100_000_000_000_000).unwrap();
    lab.repay("1", 0, 3 * U);
    lab.borrow30("2", 100_000_000_000_000).unwrap();
    lab.settle("2", 50 * U, 0);
    lab.sweep_sol(U);
    let old = lab.touch("old").unwrap();
    assert_eq!(
        old.pending - 50 * i64::try_from(U).unwrap(),
        4 * i64::try_from(U).unwrap()
    );
    lab.deposit("dust", 1).unwrap();
    lab.sweep_sol(U);
    let now_dust = lab.claim("dust").unwrap();
    assert_eq!(now_dust, 0);
    lab.advance(60, 0);
    lab.deposit("late", 100_000_000_000_000).unwrap();
    lab.advance(i64::from(lab.lump_vest_s), 0);
    let dust_total = now_dust + lab.claim("dust").unwrap();
    let late = lab.claim("late").unwrap();
    assert_eq!((dust_total, late), (2_777_777, 997_222_222));
    let before = lab.position("old").unwrap().epoch;
    for i in 0..5 {
        lab.deposit(&format!("x{i}"), 100_000_000_000_000).unwrap();
        let t = lab.market().tokens_free;
        lab.borrow30(&format!("w{i}"), t).unwrap();
        lab.settle(&format!("w{i}"), U, 0);
        lab.sweep_sol(0);
        lab.deposit(&format!("y{i}"), 1_000).unwrap();
    }
    let e = lab.market().share_epoch;
    assert_eq!(e - before, 6);
    // One touch (one sync_lender with the record of its epoch) catches it up.
    lab.sync("old", 1).unwrap();
    assert_eq!(lab.position("old").unwrap().epoch, e);
}

/// C0-9, `pool_model.run_demo()` pro-rata row: two equal lenders at 90 % utilisation; A
/// withdraws as much as it may, as often as it likes (50 tries); then the whole borrowed
/// inventory settles for half its token value. A took 100 of the 200 free (first come, first
/// served would give it all 200) and ends with 573,684,210,526 against B's 526,315,789,473.
#[test]
fn c0_9_a_run_takes_only_the_pro_rata_part() {
    let mut lab = Lab::new(TokenProgram::Token2022);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    lab.borrow30("1", 1_800_000_000_000).unwrap();
    let mut took = 0u64;
    for _ in 0..50 {
        let a = lab.free_allowance("A");
        let held = lab.position("A").unwrap().shares;
        let s = lab.shares_for("A", a).max(1).min(held);
        match lab.withdraw("A", s) {
            Ok((t, _)) => took += t,
            Err(c) => {
                assert!(
                    c == code(ShortwinError::AboveProRataAllowance)
                        || c == code(ShortwinError::InsufficientShares),
                    "{c}"
                );
                break;
            }
        }
    }
    assert_eq!(took, 100_000_000_000);
    lab.settle("1", 900_000_000_000, 0);
    let mut vals = Vec::new();
    for w in ["A", "B"] {
        let l = lab.touch(w).unwrap();
        let v = lab.token_value(w) + u128::try_from(l.pending).unwrap();
        vals.push(if w == "A" { v + u128::from(took) } else { v });
    }
    assert_eq!(vals, [573_684_210_526, 526_315_789_473]);
}

/// C0-9: a withdrawal right before a settlement (withdraw-then-settle) cannot take more than
/// the pro-rata part either; above it the program refuses.
#[test]
fn c0_9_withdraw_then_settle() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    lab.borrow30("1", 1_800_000_000_000).unwrap();
    let a = lab.free_allowance("A");
    assert_eq!(a, 100_000_000_000);
    let held = lab.position("A").unwrap().shares;
    // Everything A holds would be 1e12 tokens: refused.
    assert_eq!(
        lab.withdraw("A", held),
        Err(code(ShortwinError::InsufficientFreeInventory))
    );
    let over = lab.shares_for("A", a) + 1_000_000;
    assert_eq!(
        lab.withdraw("A", over),
        Err(code(ShortwinError::AboveProRataAllowance))
    );
    let s = lab.shares_for("A", a);
    assert_eq!(lab.withdraw("A", s).unwrap().0, a);
    lab.settle("1", 900_000_000_000, 0);
    // Nothing is borrowed any more (every lock cleared with the Π epoch): B may take its whole
    // remaining token claim.
    let b = lab.free_allowance("B");
    assert_eq!(u128::from(b), lab.token_value("B"));
    let s = lab.position("B").unwrap().shares;
    assert_eq!(lab.withdraw("B", s).unwrap().0, b);
}

/// C0-13, `pool_model.escape_demo()` v0.4 row with the program's withdrawal (which pays the SOL
/// part at once): an early claim of realised SOL, then a rent shortfall on the other position.
/// A cannot escape with more than its entitlement: it claimed 500 SOL, its pending after the
/// shortfall is 1,688,356,163, it exits and is paid exactly that; B is paid 501,688,356,164
/// and nothing is owed to anyone (2 lamports of rounding stay, 1 in the pot).
#[test]
fn c0_13_an_early_claim_cannot_escape_a_shortfall() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    lab.borrow30("1", 1_000_000_000_000).unwrap();
    let d = WAD * 5_000 * 29 * 86_400 / (BPS * 31_536_000);
    assert_eq!(d, 39_726_027_397_260_273);
    lab.accrue(d);
    lab.borrow30("2", 500_000_000_000).unwrap();
    lab.settle("2", 1_000_000_000_000, 0);
    assert_eq!(lab.claim("A"), Ok(500_000_000_000));
    let owed = lab.owed("1");
    assert_eq!(owed, 39_726_027_398);
    lab.repay("1", owed / 10, 0);
    let pend_a = lab.touch("A").unwrap().pending;
    assert_eq!(pend_a, 1_688_356_163);
    let shares = lab.position("A").unwrap().shares;
    assert_eq!(
        lab.withdraw("A", shares),
        Ok((750_000_000_000, 1_688_356_163))
    );
    assert!(
        lab.position("A").is_none(),
        "A left with nothing owed either way"
    );
    assert_eq!(lab.claim("B"), Ok(501_688_356_164));
    assert_eq!(lab.touch("B").unwrap().pending, 0);
    let (m, v) = (lab.market(), lab.vault());
    assert_eq!((v.lender_sol, v.unmatched, m.receivable), (2, 1, 0));
}

/// C0-13: a shortfall is debited pro-rata to current shares, so a lender who bought in after
/// the rent was credited can end with `pending < 0`; it cannot withdraw (`NegativePending`)
/// until it covers the deficit with `cover_deficit`, and then leaves with its tokens.
#[test]
fn c0_13_negative_pending_must_be_covered_before_exit() {
    let mut lab = Lab::new(TokenProgram::Token2022);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    lab.borrow30("1", 1_000_000_000_000).unwrap();
    lab.advance(20 * D, 10u128.pow(17));
    // A claims realised SOL (a settled position) and triples its stake after the credit.
    lab.borrow30("2", 100_000_000_000).unwrap();
    lab.settle("2", 200 * U, 0);
    lab.claim("A").unwrap();
    lab.deposit("A", 6_000_000_000_000).unwrap();
    // The borrower of position 1 pays no rent: the whole credited rent is debited.
    lab.repay("1", 0, 0);
    let pend = lab.touch("A").unwrap().pending;
    assert!(pend < 0, "A's pending {pend}");
    let shares = lab.position("A").unwrap().shares;
    let neg = code(ShortwinError::NegativePending);
    assert_eq!(lab.withdraw("A", shares), Err(neg));
    assert_eq!(lab.withdraw("A", 1), Err(neg));
    // A partial cover is not enough.
    lab.cover("A", u64::try_from(-pend).unwrap() - 1).unwrap();
    assert_eq!(lab.withdraw("A", 1), Err(neg));
    lab.cover("A", 1).unwrap();
    assert_eq!(lab.position("A").unwrap().pending, 0);
    // Now A leaves with its pro-rata part (nothing is borrowed: its whole claim up to T).
    let a = lab.free_allowance("A");
    let s = lab.shares_for("A", a);
    let (tok, sol) = lab.withdraw("A", s).unwrap();
    assert_eq!(sol, 0);
    assert!(tok <= a && a - tok <= 1, "{tok} of {a} (share rounding)");
    assert!(tok > 6_000_000_000_000);
    // B was never affected beyond its own share of the shortfall; everyone's SOL is backed.
    let b = lab.claim("B").unwrap();
    assert!(b > 0);
}

/// V2-2, `pool_model.claim_loop_demo()` v0.4 rows: `claim_sol ×N` in one transaction pays A its
/// fair realised share (76.5 SOL) once, whatever N.
#[test]
fn v2_2_repeated_claims_in_one_transaction_pay_once() {
    for n in [1usize, 2, 5, 20] {
        let mut lab = Lab::new(TokenProgram::Spl);
        lab.deposit("A", 1_000_000_000_000).unwrap();
        lab.deposit("B", 1_000_000_000_000).unwrap();
        lab.borrow30("pos1", 900_000_000_000).unwrap();
        lab.borrow30("pos2", 900_000_000_000).unwrap();
        lab.accrue(200 * WAD / 1_000);
        let owed = lab.owed("pos1");
        lab.repay("pos1", owed, 0);
        let owner = lab.users["A"].kp.insecure_clone();
        let ixs: Vec<_> = (0..n)
            .map(|_| ix::claim_sol(&owner.pubkey(), &lab.k.market))
            .collect();
        let meta = ok(lab.env.send(&ixs, &[&owner]));
        let paid: Vec<u64> = events::of::<shortwin::events::ClaimSol>(&meta)
            .iter()
            .map(|e| e.sol_paid)
            .collect();
        assert_eq!(paid.len(), n);
        assert_eq!(paid[0], 76_500_000_000, "n = {n}");
        assert!(paid[1..].iter().all(|p| *p == 0), "n = {n}: {paid:?}");
        assert_eq!(lab.vault().lender_sol, 76_500_000_000);
    }
}

/// R3A-10, `pool_model.zero_share_demo()` v0.4 row: every lender withdraws (no wipe), then a
/// trader sweep arrives and a dust deposit follows. The SOL has no lender: it goes to the
/// protocol (1 SOL), not to the dust depositor (0) — and the fee sweep takes it out.
#[test]
fn r3a_10_sol_without_lenders_goes_to_the_protocol() {
    let mut lab = Lab::new(TokenProgram::Token2022);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    let shares = lab.position("A").unwrap().shares;
    lab.withdraw("A", shares).unwrap();
    lab.sweep_sol(U);
    lab.deposit("dust", 1_000_000).unwrap();
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    assert_eq!(lab.claim("dust"), Ok(0));
    assert_eq!(lab.claim("A"), Ok(0));
    let v = lab.vault();
    assert_eq!((v.protocol_fees, v.lender_sol), (U, 0));
    let recipient = lab.env.fee_recipient.pubkey();
    let before = shortwin_litesvm_tests::read::lamports(&lab.env.svm, &recipient);
    ok(lab
        .env
        .send(&[ix::sweep_protocol_fees(&lab.k.market, &recipient)], &[]));
    assert_eq!(
        shortwin_litesvm_tests::read::lamports(&lab.env.svm, &recipient),
        before + U
    );
}

/// AIR-08 (I-08), `pool_model.empty_pool_gate_demo()`, on the program: the same market with
/// the deposit first. Every lender left by ordinary withdrawals (`S = 0`, no wipe record) and
/// 1 SOL sits on the trader PDA (a donation, or venue cashback claimed into it). Before the fix
/// `[deposit(1,000), sweep_trader, withdraw]` vested it to the dust shares and the last-lender
/// flush paid it back at once; now the deposit waits for `sweep_trader` (the real instruction
/// here), which books the SOL to the protocol (KI-10, R3A-10).
#[test]
fn air_08_a_deposit_after_the_last_exit_waits_for_the_sweep() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    let shares = lab.position("A").unwrap().shares;
    lab.withdraw("A", shares).unwrap();
    let market = lab.k.market;
    lab.env
        .fund(&shortwin_litesvm_tests::pda::trader(&market).0, U);
    assert_eq!(
        lab.deposit("ATT", 1_000),
        Err(code(ShortwinError::WipeSweepPending)),
        "the deposit first"
    );
    ok(lab.env.send(
        &[shortwin_litesvm_tests::engine::sweep_trader_ix(
            &market, None,
        )],
        &[],
    ));
    assert_eq!(lab.vault().protocol_fees, U, "KI-10: the protocol's");
    lab.deposit("ATT", 1_000).unwrap();
    let shares = lab.position("ATT").unwrap().shares;
    assert_eq!(lab.withdraw("ATT", shares), Ok((1_000, 0)));
    assert_eq!(lab.vault().lender_sol, 0);
}

/// AIR-07 (I-07), `pool_model.rebase_vest_demo()` attack row, on the program: every token is
/// lent, an attacker deposits 1 raw unit before the last settlement, which therefore left
/// 1 raw token (a rebase, not a full wipe). Before the fix its 5 SOL lump stayed vesting and a
/// depositor of 10⁶ raw units right after took 4,999,993,333 lamports of it. A settlement that
/// leaves at most 1/1,000 of the claims it found flushes the vest to the old shares, and since
/// AIR-07's residual fix the dust deposit itself is refused (`PoolDrained`: every token is
/// lent), so the settlement wipes and the wipe record carries the vest.
#[test]
fn air_07_a_dust_deposit_before_the_last_settlement_takes_no_vest() {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    let t = lab.market().tokens_free;
    lab.borrow30("1", t).unwrap();
    assert_eq!(
        lab.deposit("ATT", 1),
        Err(code(ShortwinError::PoolDrained)),
        "the dust deposit"
    );
    lab.settle("1", 50 * U, 5 * U);
    assert_eq!(lab.state().records[0].num, 0, "a full wipe");
    assert_eq!(lab.market().vest_bal, 0, "flushed into the wipe record");
    lab.sweep_sol(0); // the post-wipe sweep the newcomer's deposit waits for
    lab.deposit("NEW", 1_000_000).unwrap();
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    assert_eq!(lab.claim("NEW"), Ok(0));
    let old = lab.claim("A").unwrap() + lab.claim("B").unwrap();
    assert_eq!(old, 50 * U, "the old lenders (before: 45,000,006,664)");
}

/// AIR-07's residual, `pool_model.rebase_vest_demo()` residual row, on the program: a deposit
/// above 1/999 of the claims before the last settlement kept its own tokens as claims, so the
/// settlement left more than 1/1,000 of them, the vest kept vesting, and a 3·10¹⁰ deposit after
/// it took 4,613,534,243 lamports of the 5 SOL lump by holding through the vest. No deposit is
/// taken while the pool is drained (tokens lent out, at most 1/1,000 of the claims unborrowed).
#[test]
fn air_07_residual_a_drained_pool_takes_no_deposit() {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    let t = lab.market().tokens_free;
    lab.borrow30("1", t).unwrap();
    assert_eq!(
        lab.deposit("ATT", 3_000_000_000),
        Err(code(ShortwinError::PoolDrained)),
        "the pre-deposit"
    );
    lab.settle("1", 50 * U, 5 * U);
    assert_eq!(lab.state().records[0].num, 0, "a full wipe");
    lab.sweep_sol(0);
    lab.deposit("NEW", 30_000_000_000).unwrap();
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    assert_eq!(lab.claim("NEW"), Ok(0));
    let old = lab.claim("A").unwrap() + lab.claim("B").unwrap();
    assert_eq!(old, 50 * U, "the old lenders (before: 45,386,465,756)");
}

/// R3B-1: steady borrowing after a withdrawal. The lock A takes never decreases while the
/// market stays busy (open/close churn below the running minimum cannot release it, V2-3),
/// and it clears at its `lock_until` without A being touched in between.
#[test]
fn r3b_1_locks_hold_under_churn_and_clear_at_lock_until() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    lab.borrow30("base", 1_000_000_000_000).unwrap();
    let a = lab.free_allowance("A");
    let s = lab.shares_for("A", a);
    lab.withdraw("A", s).unwrap();
    let l = lab.position("A").unwrap();
    assert!(l.lock > 0);
    let until = l.lock_until;
    let lock_now = |lab: &mut Lab| {
        let st = lab.state();
        let pos = st.lender_of(&lab.users["A"].kp.pubkey()).unwrap().lender();
        shortwin_math::pool::lock_of(&st.vault, &pos, st.now).unwrap()
    };
    let l0 = lock_now(&mut lab);
    // Churn: positions open and close every day; the base position stays open.
    let mut k = 0;
    while lab.now() + D < until {
        lab.borrow30(&format!("c{k}"), 50_000_000_000).unwrap();
        lab.advance(D / 2, 0);
        lab.repay(&format!("c{k}"), 0, 0);
        lab.advance(D / 2, 0);
        assert!(
            lock_now(&mut lab) >= l0,
            "churn released the lock on day {k}"
        );
        k += 1;
    }
    assert!(
        lab.free_allowance("A") < a,
        "still locked before lock_until"
    );
    // Past lock_until the lock is gone for A and for the aggregate, untouched.
    let wait = until - lab.now();
    lab.advance(wait, 0);
    assert_eq!(lock_now(&mut lab), 0);
    assert!(lab.free_allowance("A") > 0);
}

/// R3B-2, `pool_model.vest_exit_demo()` v0.4.1 rows: (i) an honest lender leaving 10 min after
/// 3 SOL of lumps is paid everything if it is the last lender, else forfeits the unreleased
/// part to a dust holder (83,333,332 / 2,916,666,667, disclosed R4B-4); (ii) a whale depositing
/// 199× just before a 3 SOL lump and leaving at once gets 0.
#[test]
fn r3b_2_lumps_vest_and_a_jit_whale_gets_nothing() {
    for (dust, h_want, d_want) in [(0u64, 3 * U, 0u64), (1_000, 83_333_332, 2_916_666_667)] {
        let mut lab = Lab::new(TokenProgram::Spl);
        lab.deposit("H", 1_000_000_000_000).unwrap();
        if dust > 0 {
            lab.deposit("D", dust).unwrap();
        }
        lab.borrow30("pos", 500_000_000_000).unwrap();
        lab.advance(3_600, 0);
        lab.repay("pos", 0, 3 * U);
        lab.advance(600, 0);
        let shares = lab.position("H").unwrap().shares;
        let (_, mut h) = lab.withdraw("H", shares).unwrap();
        h += lab.claim("H").unwrap();
        lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
        h += lab.claim("H").unwrap();
        let d = if dust > 0 { lab.claim("D").unwrap() } else { 0 };
        assert_eq!((h, d), (h_want, d_want), "dust {dust}");
    }
    let mut lab = Lab::new(TokenProgram::Token2022);
    lab.deposit("H", 1_000_000_000_000).unwrap();
    lab.borrow30("pos", 500_000_000_000).unwrap();
    lab.advance(30 * D, 0);
    lab.deposit("W", 199_000_000_000_000).unwrap();
    lab.repay("pos", 0, 3 * U);
    let shares = lab.position("W").unwrap().shares;
    let (_, mut w) = lab.withdraw("W", shares).unwrap();
    w += lab.claim("W").unwrap();
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    w += lab.claim("W").unwrap();
    let h = lab.claim("H").unwrap();
    assert_eq!((w, h), (0, 3 * U));
}

/// R3A-2: withdraw between accrual and realisation, then wind down: every lender's SOL is
/// claimable in the end (the invariant hooks check the liveness wind-down after every
/// transaction once nothing is borrowed; here it is run explicitly at the end too).
#[test]
fn r3a_2_nothing_is_stranded_after_a_wind_down() {
    let mut lab = Lab::new(TokenProgram::Token2022);
    for (i, u) in ["A", "B", "C", "D"].into_iter().enumerate() {
        lab.deposit(u, (i as u64 + 1) * 1_000_000_000_000).unwrap();
    }
    for round in 0..6 {
        let pid = format!("p{round}");
        let t = lab.market().tokens_free;
        lab.borrow30(&pid, t / 2).unwrap();
        lab.advance(D, 10u128.pow(16));
        let who = ["A", "B", "C", "D"][round % 4];
        let a = lab.free_allowance(who);
        let s = lab.shares_for(who, a) / 2;
        if s > 0 {
            lab.withdraw(who, s).unwrap();
        }
        lab.deposit(["D", "C", "B", "A"][round % 4], 500_000_000_000)
            .unwrap();
        let owed = lab.owed(&pid);
        lab.repay(&pid, owed, 10_000_000);
        lab.sweep_sol(1_000_000);
    }
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    let st = lab.state();
    let w = wind_down(&st).unwrap();
    assert!(w.unpaid == 0 || w.stranded <= w.unallocated, "{w:?}");
    assert!(w.unallocated <= 64, "{w:?}");
    // And on the program: sync everyone (surpluses to the pot), then claim in sequence.
    for u in ["A", "B", "C", "D"] {
        lab.touch(u);
    }
    for _ in 0..2 {
        for u in ["A", "B", "C", "D"] {
            lab.claim(u).unwrap();
        }
    }
    assert!(
        lab.vault().lender_sol <= 64,
        "stranded {}",
        lab.vault().lender_sol
    );
}

/// ARCH §8.2 "first-depositor inflation" (M1-FID-14): tokens donated straight to `pool_vault`
/// and SOL sent straight to `MarketVault`, before the first deposit and again between
/// deposits, change nothing the pool accounts: `T` is an internal counter and I1 ignores the
/// surplus, `lender_sol` counts only what the program booked. Every deposit mints
/// `⌊amount·VS/VA⌋` shares as on an untouched market, the SOL donations stay untracked
/// lamports (I2 holds with the surplus), and every lender exits with exactly its deposit.
#[test]
fn first_depositor_inflation_by_donation() {
    use anchor_lang::solana_program::system_instruction;
    use shortwin::constants::TOKEN_PROGRAM_ID;
    use shortwin_litesvm_tests::read;

    let mut lab = Lab::new(TokenProgram::Spl);
    let donor = lab.user("donor").kp.insecure_clone();
    let donor_ata = lab.user("donor").ata;
    let (pool_vault, market_vault) = (lab.k.pool_vault, lab.k.market_vault);
    let donate = |lab: &mut Lab, tokens: u64, lamports: u64| {
        let payer = lab.env.payer.pubkey();
        let ixs = [
            spl_token_interface::instruction::transfer(
                &TOKEN_PROGRAM_ID,
                &donor_ata,
                &pool_vault,
                &donor.pubkey(),
                &[],
                tokens,
            )
            .unwrap(),
            system_instruction::transfer(&payer, &market_vault, lamports),
        ];
        ok(lab.env.send(&ixs, &[&donor]));
    };

    // Before the first deposit: 10¹⁵ raw units and 50 SOL.
    donate(&mut lab, 1_000_000_000_000_000, 50 * U);
    let v = lab.vault();
    assert_eq!((v.lender_sol, v.protocol_fees), (0, 0));
    let first = 1_000;
    assert_eq!(
        lab.deposit("attacker", first),
        Ok(u128::from(first) * VIRTUAL_SHARES / VIRTUAL_ASSETS)
    );
    assert_eq!(lab.market().tokens_free, first);

    // Between deposits: another donation; the victim still mints at the initial rate.
    donate(&mut lab, 1_000_000_000_000_000, 50 * U);
    let amount = 1_000_000_000;
    assert_eq!(
        lab.deposit("victim", amount),
        Ok(u128::from(amount) * VIRTUAL_SHARES / VIRTUAL_ASSETS)
    );
    assert_eq!(lab.vault().lender_sol, 0);

    // Both exit with exactly their deposits and no SOL; the donations stay in the vaults.
    let s = lab.position("victim").unwrap().shares;
    assert_eq!(lab.withdraw("victim", s), Ok((amount, 0)));
    let s = lab.position("attacker").unwrap().shares;
    assert_eq!(lab.withdraw("attacker", s), Ok((first, 0)));
    assert_eq!(lab.pool_vault_amount(), 2_000_000_000_000_000);
    let rent = lab
        .env
        .svm
        .minimum_balance_for_rent_exemption(shortwin::state::MarketVault::SPACE);
    assert_eq!(read::lamports(&lab.env.svm, &market_vault), rent + 100 * U);
}
