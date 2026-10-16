//! Duty 1 (ARCH §7.1; ECON §5.8): one observation per second on a market with a position near
//! a threshold (or `|P_spot/P_ema − 1| > 2 %`), otherwise one every 30 s on markets with open
//! positions, none on markets without.

use crate::common::*;

#[test]
fn a_healthy_position_is_observed_every_30_seconds_and_an_empty_market_never() {
    let (mut h, mms) = setup(2, KeeperConfig::default());
    let (with, without) = (mms[0].market(), mms[1].market());
    let _ = open(&h, &mms[0], Q);
    let mut cranked_at = Vec::new();
    for _ in 0..65 {
        let now = {
            let r = h.second();
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            assert!(
                !r.observed(&without),
                "a market without positions is never observed"
            );
            r.cranked
                .contains(&with)
                .then_some(r.clock.unwrap().unix_timestamp)
        };
        cranked_at.extend(now);
    }
    // The first tick (nothing observed yet), then every 30 s.
    assert_eq!(cranked_at.len(), 3, "{cranked_at:?}");
    assert_eq!(cranked_at[1] - cranked_at[0], 30);
    assert_eq!(cranked_at[2] - cranked_at[1], 30);
    let ev = &h.reports.last().unwrap().evaluations[&with][0];
    assert!(!ev.near, "{ev:?}");
    let m = &h.keeper.metrics.markets[&with];
    assert_eq!((m.cranks, m.covered_seconds), (3, 3));
    assert_eq!(m.misses, 0);
}

#[test]
fn a_position_near_its_threshold_gets_an_observation_every_second() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let _ = open(&h, mm, Q);
    h.second();
    // Push the price ×1.7: the position's live CR falls below 1.3·L (Tier C: L = 150 %,
    // opened at 300 %) but stays above L.
    h.with_env(|env| {
        let actor = env.actor(mm, 0, 2_000_000_000_000);
        let target = env.spot(mm) * 17 / 10;
        env.push_price(mm, &actor, target);
    });
    let mut per_second = 0;
    for i in 0..90 {
        let r = h.second();
        let key = mm.market();
        let ev = &r.evaluations[&key][0];
        assert!(ev.near, "second {i}: {ev:?}");
        assert!(!ev.spot_leg || !ev.ema_leg, "not liquidatable: {ev:?}");
        assert!(r.cranked.contains(&key), "second {i}: no observation");
        if !r.needs[&key].ema_deviates {
            per_second += 1;
        }
    }
    // The EMA has converged (no 2 % deviation left) and the near CR alone keeps the cadence.
    assert!(per_second > 30, "{per_second}");
}
