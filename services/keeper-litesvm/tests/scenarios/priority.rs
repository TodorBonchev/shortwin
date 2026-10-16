//! Duty 14 (ARCH §7.1, §6.1; C0-17): compute-unit limit `⌈simulated × 1.1⌉`, priority price
//! from the recent per-account fees, capped at the O17 ceiling; the fee the cluster charges
//! follows.

use shortwin_services_common::tx::{ComputeBudget, cu_limit, priority_fee_lamports};

use crate::common::*;

#[test]
fn the_priority_price_is_the_recent_p75_capped_at_the_ceiling() {
    let mut cfg = KeeperConfig::default();
    cfg.budget.priority_ceiling = 10_000;
    let (mut h, mms) = setup(1, cfg);
    let _ = open(&h, &mms[0], Q);
    h.chain().set_priority_fees(vec![0, 2_000, 4_000, 8_000]);
    let r = h.second().clone();
    let b = &r.batches[0];
    assert_eq!(b.price, 4_000); // p75 of the four
    // The limit is 1.1× the simulation (the landed run uses the same units).
    assert_eq!(b.unit_limit, cu_limit(b.units));
    let prio = priority_fee_lamports(&ComputeBudget {
        unit_limit: b.unit_limit,
        unit_price: b.price,
    });
    assert_eq!(b.fee, 5_000 + prio, "{b:?}");
    // Above the ceiling: capped.
    h.chain().set_priority_fees(vec![1_000_000; 4]);
    h.seconds(30);
    let b = h
        .reports
        .iter()
        .rev()
        .find_map(|r| r.batches.first())
        .unwrap();
    assert_eq!(b.price, 10_000);
}
