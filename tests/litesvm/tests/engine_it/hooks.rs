//! The M2 invariant hooks catch what a program mutant would leave (as M1's `lender_it/hooks.rs`
//! does for the lender checks): each check is fed a state or an instruction that violates it.

use shortwin::events::{Close, Open};
use shortwin::state::{Market, ShortPosition};
use shortwin_litesvm_tests::fixtures;
use shortwin_litesvm_tests::invariants::{self, IxEvents, check_all};

use crate::common::*;

fn open_market() -> (Env, MockMarket, Borrower, u64) {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    (env, mm, b, nonce)
}

fn write_market(env: &mut Env, key: &anchor_lang::prelude::Pubkey, m: &Market) {
    let extra = lamports(env, key) - env.svm.minimum_balance_for_rent_exemption(Market::SPACE);
    fixtures::set_zc(&mut env.svm, key, m, extra);
}

#[test]
fn i6_catches_aggregates_that_do_not_match_the_positions() {
    let (mut env, mm, _b, _n) = open_market();
    env.check_invariants = false;
    let good = env.market(&mm.market());
    let mut m = good;
    m.open_positions += 1;
    write_market(&mut env, &mm.market(), &m);
    assert_eq!(
        check_all(&env.svm, &mm.market()).unwrap_err().invariant,
        "I6"
    );
    let mut m = good;
    m.total_notional_open += 1;
    write_market(&mut env, &mm.market(), &m);
    assert_eq!(
        check_all(&env.svm, &mm.market()).unwrap_err().invariant,
        "I6"
    );
    let mut m = good;
    m.rent_aggr[0].set(m.rent_aggr[0].get() + 1);
    write_market(&mut env, &mm.market(), &m);
    assert_eq!(
        check_all(&env.svm, &mm.market()).unwrap_err().invariant,
        "I6"
    );
    let mut m = good;
    m.max_release -= 1;
    write_market(&mut env, &mm.market(), &m);
    assert_eq!(
        check_all(&env.svm, &mm.market()).unwrap_err().invariant,
        "I6"
    );
    write_market(&mut env, &mm.market(), &good);
    assert!(check_all(&env.svm, &mm.market()).is_ok());
}

#[test]
fn i13_catches_a_stale_ring_reprojection() {
    let (mut env, mm, _b, _n) = open_market();
    env.check_invariants = false;
    let mut m = env.market(&mm.market());
    let head = usize::from(m.ring_head);
    m.ring[head].x.set(m.ring[head].x.get() + 1);
    write_market(&mut env, &mm.market(), &m);
    assert_eq!(
        check_all(&env.svm, &mm.market()).unwrap_err().invariant,
        "I13"
    );
}

#[test]
fn instruction_level_position_checks() {
    let (mut env, mm, b, nonce) = open_market();
    env.check_invariants = false;
    let pos_key = pda::position(&mm.market(), &b.key(), nonce).0;
    let pre = invariants::snapshot(&env.svm, &[mm.market()]);
    // I3: position SOL leaves without an exit event.
    let mut acc = env.svm.get_account(&pos_key).unwrap();
    acc.lamports -= 1;
    env.svm.set_account(pos_key, acc.clone()).unwrap();
    let v = invariants::check_ix(&env.svm, &pre, false, &IxEvents::default()).unwrap_err();
    assert_eq!(v.invariant, "I3");
    acc.lamports += 1;
    env.svm.set_account(pos_key, acc).unwrap();
    let pos: ShortPosition = env.position(&mm, &b.key(), nonce);
    let close = |buyback: u64, b_live: u64| Close {
        market: mm.market(),
        slot: 0,
        ts: 0,
        position: pos_key,
        borrower: b.key(),
        q: pos.q,
        buyback_cost: buyback,
        rent: 10,
        rent_protocol: 1,
        borrower_payout: pos.v - buyback - 10,
        b_live,
        token_surplus: 0,
    };
    // I9: a close whose legs do not sum to V.
    let mut bad = close(100, 100);
    bad.borrower_payout += 1;
    let ev = IxEvents {
        closes: vec![bad],
        ..IxEvents::default()
    };
    assert_eq!(
        invariants::check_ix(&env.svm, &pre, false, &ev)
            .unwrap_err()
            .invariant,
        "I9"
    );
    // I13: a venue charge above its quote.
    let ev = IxEvents {
        closes: vec![close(101, 100)],
        ..IxEvents::default()
    };
    assert_eq!(
        invariants::check_ix(&env.svm, &pre, false, &ev)
            .unwrap_err()
            .invariant,
        "I13"
    );
    // I10: an exit naming the oracle cache.
    let ev = IxEvents {
        closes: vec![close(100, 100)],
        accounts: vec![pda::oracle_cache()],
        ..IxEvents::default()
    };
    assert_eq!(
        invariants::check_ix(&env.svm, &pre, false, &ev)
            .unwrap_err()
            .invariant,
        "I10"
    );
    // I7: a leg that left lamports on the trader.
    env.fund(&mm.keys.trader, 1_000_000);
    let open = Open {
        market: mm.market(),
        slot: 0,
        ts: 0,
        position: pos_key,
        borrower: b.key(),
        nonce,
        q: pos.q,
        s: pos.s,
        m: pos.m,
        v: pos.v,
        n_open: pos.n_open,
        fee_open: pos.fee_open,
        tier: pos.tier,
        entry_price: pos.entry_price,
        spot_wad: 0,
        ema_wad: 0,
        b_spot: 0,
        b_ema: 0,
        deadline: pos.deadline,
        venue: 6,
        venue_rent: 0,
    };
    let ev = IxEvents {
        opens: vec![open],
        ..IxEvents::default()
    };
    assert_eq!(
        invariants::check_ix(&env.svm, &pre, false, &ev)
            .unwrap_err()
            .invariant,
        "I7"
    );
}

/// I9, I11 and I12 read off the keeper exits' events (liquidation, expiry, settlement) against
/// the position's `V`, locked terms and arming clock: synthetic events that break exactly one
/// row each (the real exits run through the same hooks in `liquidate_it`, `expire_it`,
/// `settle_it` and the attack suites).
#[test]
fn exit_event_checks_on_synthetic_exits() {
    use shortwin::events::{Liquidate, SolSettle};
    let (mut env, mm, b, nonce) = open_market();
    env.check_invariants = false;
    let pos_key = pda::position(&mm.market(), &b.key(), nonce).0;
    // A live settlement clock that has run `settle_persist_n` EMA updates (I12's persistence
    // part), so that the synthetic S1 below is a valid one.
    let now = env.now();
    {
        let mut m = env.market(&mm.market());
        m.ema_count += 100;
        write_market(&mut env, &mm.market(), &m);
        let mut p: ShortPosition = env.position(&mm, &b.key(), nonce);
        p.s_armed = 1;
        p.s_count = m.ema_count - u64::from(p.settle_persist_n);
        p.s_last = now;
        let extra = lamports(&env, &pos_key)
            - env
                .svm
                .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
        fixtures::set_borsh(&mut env.svm, &pos_key, &p, ShortPosition::SPACE, extra);
    }
    let pre = invariants::snapshot(&env.svm, &[mm.market()]);
    let pos: ShortPosition = env.position(&mm, &b.key(), nonce);
    // The synthetic exits closed the position (I3: an exit leaves no position behind).
    env.svm
        .set_account(pos_key, solana_account::Account::default())
        .unwrap();
    let k_s = (pos.settle_keeper_fixed + pos.v * u64::from(pos.settle_keeper_bps) / 10_000)
        .min(pos.settle_keeper_cap)
        .min(pos.v);
    let liq = |buyback: u64, b_ref: u64, pk: u64, topup: u64| {
        let penalty = pk * 2;
        Liquidate {
            market: mm.market(),
            slot: 0,
            ts: 0,
            position: pos_key,
            keeper: b.key(),
            cr_spot_bps: 0,
            cr_ema_bps: 0,
            b_live: b_ref,
            b_ref,
            p_ref: 0,
            delta_bps: pos.delta_liq_bps,
            buyback_cost: buyback,
            rent: 0,
            penalty,
            penalty_lender: penalty - pk,
            penalty_protocol: 0,
            penalty_keeper: pk,
            keeper_topup: topup,
            borrower_payout: pos.v - buyback - penalty - topup,
            expired: false,
            token_surplus: 0,
            b_live_risk: b_ref,
            max_in: b_ref * (10_000 + u64::from(pos.delta_liq_bps)) / 10_000,
        }
    };
    let b_ref = pos.n_open;
    let ok_liq = liq(b_ref, b_ref, k_s, 0);
    let ev = IxEvents {
        liquidations: vec![ok_liq],
        ..IxEvents::default()
    };
    assert!(invariants::check_ix(&env.svm, &pre, false, &ev).is_ok());
    // Spend above the fixed bound.
    let ev = IxEvents {
        liquidations: vec![liq(b_ref + b_ref / 20, b_ref, k_s, 0)],
        ..IxEvents::default()
    };
    assert_eq!(
        invariants::check_ix(&env.svm, &pre, false, &ev)
            .unwrap_err()
            .invariant,
        "I11"
    );
    // Keeper below K_s while the borrower keeps equity.
    let ev = IxEvents {
        liquidations: vec![liq(b_ref, b_ref, 1, 0)],
        ..IxEvents::default()
    };
    assert_eq!(
        invariants::check_ix(&env.svm, &pre, false, &ev)
            .unwrap_err()
            .invariant,
        "I12"
    );
    // A settlement: S1 pays V − K_s to lenders; an S2 outside its box.
    let settle = |condition: u8, lenders: u64, x_lo: u64, x_hi: u64| SolSettle {
        market: mm.market(),
        slot: 0,
        ts: now,
        position: pos_key,
        keeper: b.key(),
        condition,
        b_live: 0,
        b_rs: 0,
        b_ema: 0,
        p_lo: 0,
        p_hi: 0,
        l_lo: 0,
        l_hi: 0,
        x_lo,
        x_hi,
        pprime_lo: 0,
        pprime_hi: 0,
        fv: 0,
        k_s,
        lender_amount: lenders,
        lump: 0,
        borrower_amount: pos.v - k_s - lenders,
        ema_age_s: 0,
        vest_flushed: 0,
    };
    let ev = IxEvents {
        settlements: vec![settle(1, pos.v - k_s, 0, 0)],
        ..IxEvents::default()
    };
    assert!(invariants::check_ix(&env.svm, &pre, false, &ev).is_ok());
    let ev = IxEvents {
        settlements: vec![settle(1, pos.v - k_s - 1, 0, 0)],
        ..IxEvents::default()
    };
    assert_eq!(
        invariants::check_ix(&env.svm, &pre, false, &ev)
            .unwrap_err()
            .invariant,
        "I12"
    );
    let x = pos.v / 2;
    let ev = IxEvents {
        settlements: vec![settle(2, x, x + 10, x + 20)],
        ..IxEvents::default()
    };
    assert_eq!(
        invariants::check_ix(&env.svm, &pre, false, &ev)
            .unwrap_err()
            .invariant,
        "I12"
    );
    // The settle-o6 track's I12 parts: S1 on a lapsed clock (last evaluation more than
    // settle_arm_gap_s before), S3 without a no-route chain, an S2 of a position not expired
    // priced with the expiry fee.
    let mut lapsed = settle(1, pos.v - k_s, 0, 0);
    lapsed.ts = now + i64::from(pos.settle_arm_gap_s) + 1;
    let mut s2_fee = settle(2, x, x, x);
    s2_fee.pprime_lo = pos.expiry_fee_bps;
    s2_fee.pprime_hi = pos.expiry_fee_bps;
    let mut s2_ok = settle(2, x, x, x);
    s2_ok.pprime_lo = pos.penalty_bps;
    s2_ok.pprime_hi = pos.penalty_bps;
    for (e, want) in [
        (lapsed, Some("I12")),
        (settle(3, x, x, x), Some("I12")),
        (s2_fee, Some("I12")),
        (s2_ok, None),
    ] {
        let ev = IxEvents {
            settlements: vec![e],
            ..IxEvents::default()
        };
        let got = invariants::check_ix(&env.svm, &pre, false, &ev).err();
        assert_eq!(got.map(|v| v.invariant), want);
    }
}
