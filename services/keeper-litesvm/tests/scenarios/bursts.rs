//! Duty 2 (ARCH §7.1; ECON §5.8, §12.11): after an observation the keeper did not make that
//! deviates from its own view by more than 2 %, it observes the market every slot for 60 s —
//! an atomic `[pump, crank, dump]` bundle then has no second of its own.

use shortwin_litesvm_tests::drive::swap_in_ix;
use shortwin_litesvm_tests::engine::crank_price_ix;
use shortwin_litesvm_tests::mock::Dir;

use crate::common::*;

#[test]
fn an_atomic_pump_crank_dump_starts_a_60_second_per_slot_burst() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(3);
    // The keeper has read this second's price; now an attacker pushes +20 %, cranks and
    // dumps back, atomically.
    let attacked_at = h.now();
    h.with_env(|env| {
        let attacker = env.actor(mm, 0, 1_000_000_000_000);
        let (_, amount, pump) = env
            .push_price_ix(mm, &attacker, env.spot(mm) * 12 / 10)
            .unwrap();
        let bought = mock::quote_exact_in(&env.svm, mm.mock(), Dir::QuoteIn, amount)
            .unwrap()
            .proceeds;
        let dump = swap_in_ix(mm, &attacker, Dir::BaseIn, bought, 0);
        let kp = attacker.kp.insecure_clone();
        ok(env.bundle(&[pump, crank_price_ix(mm), dump], &[&kp]));
    });
    let r = h.slot().clone();
    assert_eq!(r.bursts, vec![(key, attacked_at)], "{r:?}");
    assert!(r.cranked.contains(&key), "the burst's first slot observes");
    // Every slot until 60 s after the attacked second.
    let mut slots = 0;
    while h.now() < attacked_at + 60 {
        let r = h.slot().clone();
        assert!(
            r.cranked.contains(&key),
            "slot {slots} of the burst not observed"
        );
        slots += 1;
        if slots % 2 == 0 {
            h.with_env(|env| env.warp(1));
        }
    }
    assert!(slots >= 100, "{slots}");
    assert_eq!(h.keeper.metrics.markets[&key].bursts, 1);
    // Afterwards the idle cadence again: most slots observe nothing.
    let mut quiet = 0;
    for _ in 0..20 {
        let r = h.slot().clone();
        quiet += usize::from(!r.cranked.contains(&key));
        h.with_env(|env| env.warp(1));
    }
    assert!(quiet >= 18, "{quiet}");
}

#[test]
fn honest_observations_within_two_percent_start_no_burst() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let _ = open(&h, mm, Q);
    h.second();
    // Someone else cranks in the same second at the same price.
    h.with_env(|env| ok(env.send(&[crank_price_ix(mm)], &[])));
    let r = h.slot().clone();
    assert!(r.bursts.is_empty(), "{r:?}");
}
