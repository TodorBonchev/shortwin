//! Duty 4 (ARCH §7.1, §3.8; ECON §5.6): a gap the vault cannot buy back (S1 legs). The keeper
//! observes every second, calls `sol_settle` every second once the legs hold — it arms, keeps
//! the clock running — and the settlement executes after `settle_persist_n` EMA updates. No
//! liquidation is attempted (no normal buyback).

use shortwin::events::{SettleArmed, SolSettle};
use shortwin_keeper::events;

use crate::common::*;

#[test]
fn a_gap_beyond_the_vault_is_armed_and_settled_in_s1() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let (b, nonce) = open(&h, mm, Q);
    let pos = position_key(mm, &b.key(), nonce);
    let persist_n = h.with_env(|env| env.position(mm, &b.key(), nonce).settle_persist_n);
    h.seconds(3);
    // A whale lifts the price ×4 in one trade and it stays: B̂_live exceeds V − K_s.
    h.with_env(|env| {
        let whale = env.actor(mm, 0, 5_000_000_000_000);
        let target = env.spot(mm) * 4;
        env.push_price(mm, &whale, target);
    });
    let mut armed_at = None;
    let mut calls = Vec::new();
    let mut settled = None;
    for s in 0..200 {
        let r = h.second().clone();
        let now = h.now();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        assert!(r.observed(&key), "second {s}: not observed");
        assert!(landed_for(&r, ExitKind::Liquidate, &pos).next().is_none());
        for out in landed_for(&r, ExitKind::SolSettle, &pos) {
            calls.push(now);
            if !events::of::<SettleArmed>(out).is_empty() && armed_at.is_none() {
                armed_at = Some(now);
            }
            if let Some(e) = events::of::<SolSettle>(out).first() {
                assert_eq!(e.condition, 1, "S1");
                settled = Some(now);
            }
        }
        if settled.is_some() {
            break;
        }
    }
    let armed_at = armed_at.expect("never armed");
    let settled = settled.expect("never settled");
    println!(
        "armed at {armed_at}, S1 at {settled} ({} sol_settle calls)",
        calls.len()
    );
    // One sol_settle per second from arming to settlement (the clock never lapses)…
    let after: Vec<i64> = calls.iter().copied().filter(|t| *t >= armed_at).collect();
    assert!(after.windows(2).all(|w| w[1] - w[0] == 1), "{after:?}");
    // …and the settlement after settle_persist_n EMA updates (one per second here).
    assert!(
        settled - armed_at >= i64::from(persist_n),
        "{armed_at} → {settled}"
    );
    assert!(!position_open(&h, mm, &b.key(), nonce));
}
