//! O17's "markets covered" and the dry-run mode.

use shortwin_keeper::config::MarketSelection;

use crate::common::*;

#[test]
fn only_the_selected_markets_are_covered() {
    let mut env = Env::test_venue();
    let a = env.mock_market(MarketSpec::default());
    let b = env.mock_market(MarketSpec::default());
    let cfg = KeeperConfig {
        markets: MarketSelection::Only(vec![b.market().to_string()]),
        ..KeeperConfig::default()
    };
    let mut h = Harness::new(env, cfg);
    let _ = open(&h, &a, Q);
    let _ = open(&h, &b, Q);
    let r = h.second();
    assert_eq!(r.cranked, vec![b.market()]);
    assert_eq!(h.keeper.markets(), &[b.market()]);
}

#[test]
fn a_dry_run_simulates_and_sends_nothing() {
    let cfg = KeeperConfig {
        dry_run: true,
        ..KeeperConfig::default()
    };
    let (mut h, mms) = setup(1, cfg);
    let _ = open(&h, &mms[0], Q);
    h.seconds(3);
    assert!(h.chain().sent().is_empty());
    assert!(h.keeper.metrics.simulations >= 1);
}
