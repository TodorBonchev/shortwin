//! Duty 3, expiry (ARCH §7.1, §3.7, §5.1): a healthy position reaches its deadline; the
//! keeper observes it every second in the last minute and expires it at the first second the
//! expiry bound is met.

use shortwin::events::Expire;
use shortwin_keeper::events;
use shortwin_litesvm_tests::engine::expire_ix;

use solana_signer::Signer;

use crate::common::*;

#[test]
fn a_position_is_expired_at_its_deadline() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let (b, nonce) = open(&h, mm, Q);
    let pos = position_key(mm, &b.key(), nonce);
    let deadline = h.with_env(|env| env.position(mm, &b.key(), nonce).deadline);
    let oracle = Oracle::new(&h);
    h.seconds(3);
    // The keeper is idle until 90 s before the deadline (a gap: degraded, then recovering).
    let wait = deadline - 90 - h.now();
    h.idle(wait);
    let mut first_ok = None;
    let mut expired = None;
    for s in 0..150 {
        h.with_env(|env| env.warp(1));
        let now = h.now();
        if first_ok.is_none()
            && oracle
                .accepts(&h, expire_ix(mm, &oracle.kp.pubkey(), &b.key(), nonce))
                .is_ok()
        {
            first_ok = Some(now);
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if let Some(out) = landed_for(&r, ExitKind::Expire, &pos).next() {
            let e: Expire = events::of(out).into_iter().next().expect("Expire event");
            assert_eq!(e.keeper, h.keeper.payer());
            expired = Some(now);
            break;
        }
        // The last minute before the deadline is covered every second (duty 1).
        if deadline - now <= 60 {
            assert!(
                r.observed(&key),
                "{} s before the deadline: not observed",
                deadline - now
            );
        }
        assert!(landed_for(&r, ExitKind::Liquidate, &pos).next().is_none());
    }
    let first_ok = first_ok.expect("the program never accepted the expiry");
    assert!(first_ok >= deadline);
    assert_eq!(expired, Some(first_ok));
    assert!(!position_open(&h, mm, &b.key(), nonce));
    assert_eq!(h.keeper.metrics.markets[&key].expiries, 1);
}
