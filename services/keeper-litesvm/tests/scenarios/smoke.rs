//! The keeper runs end to end on LiteSVM.

use crate::common::*;

#[test]
fn keeper_cranks_a_market_with_an_open_position() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let _ = open(&h, &mms[0], Q);
    let r = h.second();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.cranked, vec![mms[0].market()], "{r:?}");
    let sent = h.chain().sent();
    assert_eq!(sent.len(), 1);
    println!("{sent:?}");
}
