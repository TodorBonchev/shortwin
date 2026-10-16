//! The lender hooks themselves (ARCH §8.3 I1, I8): each check added after the M1 review fires
//! on the state or the event a mutant of the program would produce, so a passing suite means
//! the check ran (M1-FID-1, M1-FID-4, M1-FID-9, M1-SEC-3). The mutants the fidelity review
//! built (`mut_deposit_ghost`, `mut_withdraw_nolock`, …) are reproduced here by writing their
//! effect into the accounts, then calling the hook directly.

use shortwin::events::{Deposit, Withdraw};
use shortwin_litesvm_tests::invariants::lender::{self as inv, PoolState};
use shortwin_litesvm_tests::invariants::{self, IxEvents, check_all};
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_litesvm_tests::{events, fixtures, ix, pda, read};
use shortwin_math::pool::VIRTUAL_SHARES;
use solana_signer::Signer;

use crate::common::{Lab, write_zc};

/// Rewrites `owner`'s `LenderPosition` in place with `edit`.
fn edit_lender(lab: &mut Lab, name: &str, edit: impl FnOnce(&mut shortwin::state::LenderPosition)) {
    let owner = lab.key(name);
    let key = pda::lender(&lab.k.market, &owner).0;
    let mut l = lab.env.lender(&lab.k.market, &owner).unwrap();
    edit(&mut l);
    let lamports = read::lamports(&lab.env.svm, &key);
    fixtures::set_borsh(
        &mut lab.env.svm,
        &key,
        &l,
        shortwin::state::LenderPosition::SPACE,
        0,
    );
    let mut acc = lab.env.svm.get_account(&key).unwrap();
    acc.lamports = lamports;
    lab.env.svm.set_account(key, acc).unwrap();
}

/// M1-FID-1 (`mut_deposit_ghost` on the first deposit): the tokens arrive, no shares are
/// minted. Nobody claims them, and the per-instruction check refuses a market that holds new
/// tokens with `S = 0`.
#[test]
fn fid_1_tokens_without_shares_are_caught() {
    let mut lab = Lab::new(TokenProgram::Spl);
    let pre = PoolState::load(&lab.env.svm, &lab.k.market).unwrap();
    let mut m = lab.market();
    m.tokens_free = 1_000;
    write_zc(&mut lab.env, &lab.k.market, &m, 0);
    let post = PoolState::load(&lab.env.svm, &lab.k.market).unwrap();
    let v = inv::empty_pool_holds_no_new_tokens(&pre, &post).unwrap_err();
    assert_eq!(v.invariant, "I1");
    // The last lender's exit leaves at most the virtual shares' part.
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000).unwrap();
    let pre = PoolState::load(&lab.env.svm, &lab.k.market).unwrap();
    let s = lab.position("A").unwrap().shares;
    lab.withdraw("A", s).unwrap();
    let post = PoolState::load(&lab.env.svm, &lab.k.market).unwrap();
    assert!(inv::empty_pool_holds_no_new_tokens(&pre, &post).is_ok());
    assert!(post.pool.tokens_free <= 1);
}

/// M1-FID-1 (`mut_deposit_ghost` on a later deposit): the depositor's shares are not minted,
/// so its tokens accrue to the lenders already in. Totals still satisfy I1's equality; the
/// per-instruction conservation check sees `S` and the depositor's shares not move by the
/// event's `shares`.
#[test]
fn fid_1_a_deposit_that_mints_nothing_is_caught() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000).unwrap();
    let market = lab.k.market;
    let pre = invariants::snapshot(&lab.env.svm, &[market]);
    let lk = ix::LenderKeys {
        owner: lab.key("B"),
        mint: lab.k.mint,
        token_program: lab.k.token_program,
        owner_token_account: lab.user("B").ata,
    };
    let kp = lab.user("B").kp.insecure_clone();
    lab.env.check_invariants = false;
    let meta = lab
        .env
        .send(&[ix::deposit(&lk, 1_000_000, 0)], &[&kp])
        .unwrap();
    lab.env.check_invariants = true;
    let minted = events::one::<Deposit>(&meta).shares;
    // The honest instruction passes.
    assert!(invariants::check_ix(&lab.env.svm, &pre, false, &IxEvents::of(&meta, 0)).is_ok());
    // The ghost: S and B's shares lose what the event says was minted.
    let mut m = lab.market();
    m.total_shares.set(m.total_shares.get() - minted);
    write_zc(&mut lab.env, &market, &m, 0);
    edit_lender(&mut lab, "B", |l| l.shares -= minted);
    let v = invariants::check_ix(&lab.env.svm, &pre, false, &IxEvents::of(&meta, 0)).unwrap_err();
    assert_eq!(v.invariant, "I1", "{v}");
    // At the state level the totals still add up (I1 is about totals): this is why the
    // per-instruction check exists.
    assert!(check_all(&lab.env.svm, &market).is_ok());
}

/// M1-FID-1 (and KI-65: rebases included, every share has a holder once every lender is
/// touched): shares taken from a lender but left in `S` (orphaned) are caught at the state
/// level, and so are tokens no lender claims beyond the virtual (and orphan) shares' part.
#[test]
fn fid_1_orphan_shares_and_unclaimed_tokens_are_caught() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000).unwrap();
    lab.deposit("B", 1_000_000).unwrap();
    let market = lab.k.market;
    edit_lender(&mut lab, "B", |l| l.shares -= 1);
    let v = check_all(&lab.env.svm, &market).unwrap_err();
    assert_eq!(v.invariant, "I1", "{v}");
    assert!(v.detail.contains("!= S"), "{v}");
    // The unclaimed bound: B's shares vanish from both B and S, their tokens stay in T.
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000).unwrap();
    lab.deposit("B", 1_000_000).unwrap();
    let market = lab.k.market;
    let b = lab.position("B").unwrap().shares;
    edit_lender(&mut lab, "B", |l| l.shares = 0);
    let mut m = lab.market();
    m.total_shares.set(m.total_shares.get() - b);
    write_zc(&mut lab.env, &market, &m, 0);
    // A now holds every share and so claims B's tokens: the totals hold …
    assert!(check_all(&lab.env.svm, &market).is_ok());
    // … but with A's shares orphaned instead, half of T is claimed by nobody.
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000).unwrap();
    lab.deposit("B", 1_000_000).unwrap();
    let st = PoolState::load(&lab.env.svm, &lab.k.market).unwrap();
    let p = st.pool;
    assert!(
        inv::unclaimed_bound(&p, p.total_shares / 2).unwrap() >= u128::from(p.tokens_free) / 2 - 1,
        "orphans count toward the bound"
    );
    assert_eq!(
        inv::unclaimed_bound(&p, p.total_shares).unwrap(),
        ((u128::from(p.tokens_free) + 1) * VIRTUAL_SHARES)
            .div_ceil(p.total_shares + VIRTUAL_SHARES)
    );
}

/// M1-SEC-3: `S = 0 ⇒ ΣB_Q = 0` (the program refuses the last shares' exit while tokens are
/// borrowed; a state with borrowed tokens and no shares is caught).
#[test]
fn sec_3_borrowed_tokens_without_shares_are_caught() {
    let mut lab = Lab::new(TokenProgram::Spl);
    let mut m = lab.market();
    m.borrowed_q = 1;
    m.borrowed_q_by_tier = [0, 0, 1, 0];
    write_zc(&mut lab.env, &lab.k.market, &m, 0);
    let v = check_all(&lab.env.svm, &lab.k.market).unwrap_err();
    assert_eq!(v.invariant, "I8");
    assert!(v.detail.contains("S = 0 while"), "{v}");
}

/// M1-FID-4: every withdrawal is bounded by the allowance its lender had just before it,
/// whatever else the transaction holds. A lender with a live lock that takes more than its
/// allowance (what `mut_withdraw_nolock` / `mut_withdraw_future` let through) is caught.
#[test]
fn fid_4_a_withdrawal_above_the_allowance_is_caught() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000).unwrap();
    lab.borrow30("p", 1_000_000_000).unwrap();
    let allow = lab.free_allowance("A");
    let st = PoolState::load(&lab.env.svm, &lab.k.market).unwrap();
    let a = lab.position("A").unwrap();
    let value = shortwin_math::pool::tokens_for_shares(&st.pool, a.shares).unwrap();
    assert!(
        u128::from(allow) < value,
        "A's allowance is below its value"
    );
    let over = Withdraw {
        market: lab.k.market,
        slot: 0,
        ts: 0,
        owner: a.owner,
        amount: allow + 1,
        shares: a.shares,
        t_after: 0,
        s_after: 0,
        sol_paid: 0,
        pot_draw: 0,
        lock: 0,
        lock_until: 0,
        epoch: 0,
    };
    let pre = invariants::snapshot(&lab.env.svm, &[lab.k.market]);
    let ev = IxEvents {
        withdraws: vec![over],
        ..Default::default()
    };
    let v = invariants::check_ix(&lab.env.svm, &pre, false, &ev).unwrap_err();
    assert!(v.detail.contains("above its pro-rata allowance"), "{v}");
    // Two honest withdrawals in one transaction, each within its own allowance, pass.
    let half_a = lab.free_allowance("A") / 2;
    let half_b = lab.free_allowance("B") / 2;
    let shares_a = lab.shares_for("A", half_a);
    let shares_b = lab.shares_for("B", half_b);
    let keys = |lab: &mut Lab, name: &str| ix::LenderKeys {
        owner: lab.key(name),
        mint: lab.k.mint,
        token_program: lab.k.token_program,
        owner_token_account: lab.user(name).ata,
    };
    let (ka, kb) = (keys(&mut lab, "A"), keys(&mut lab, "B"));
    let (sa, sb) = (
        lab.user("A").kp.insecure_clone(),
        lab.user("B").kp.insecure_clone(),
    );
    let meta = lab
        .env
        .send(
            &[
                ix::withdraw(&ka, shares_a, 0),
                ix::withdraw(&kb, shares_b, 0),
            ],
            &[&sa, &sb],
        )
        .unwrap();
    assert_eq!(events::of::<Withdraw>(&meta).len(), 2);
    let _ = sa.pubkey();
}

/// M1-FID-9: locks never decrease while `now < lock_until` with `Π` and the epochs unchanged
/// (`mut_withdraw_nolock` drops the withdrawing lender's live lock; any lender's lock dropping
/// is caught, the withdrawer's below its remaining shares).
#[test]
fn fid_9_a_dropped_lock_is_caught() {
    let mut lab = Lab::new(TokenProgram::Spl);
    lab.deposit("A", 1_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000).unwrap();
    lab.borrow30("p", 1_000_000_000).unwrap();
    let allow = lab.free_allowance("A");
    let shares = lab.shares_for("A", allow);
    lab.withdraw("A", shares).unwrap();
    let a = lab.position("A").unwrap();
    assert!(a.lock > 0, "A holds a live lock");
    let pre = PoolState::load(&lab.env.svm, &lab.k.market).unwrap();
    assert!(inv::locks_never_decrease(&pre, &pre, None).is_ok());
    edit_lender(&mut lab, "A", |l| {
        l.lock = 0;
        l.lock_term = 0;
        l.lock_epoch = shortwin_math::pool::NO_LOCK_EPOCH;
    });
    let post = PoolState::load(&lab.env.svm, &lab.k.market).unwrap();
    let v = inv::locks_never_decrease(&pre, &post, None).unwrap_err();
    assert!(v.detail.contains("lock fell"), "{v}");
    // As the withdrawer, A may keep less only down to its remaining shares (here: none left
    // would be fine, a lock of 0 with shares left is not).
    assert!(inv::locks_never_decrease(&pre, &post, Some(&a.owner)).is_err());
    // After lock_until the lock may go.
    let later = PoolState {
        now: a.lock_until,
        ..post.clone()
    };
    assert!(inv::locks_never_decrease(&pre, &later, None).is_ok());
}
