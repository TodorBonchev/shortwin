use shortwin::events::{Close, Open};

use crate::common::*;

#[test]
fn open_then_close_on_the_mock() {
    let (mut env, mm) = setup();
    env.crank(&mm);
    let mut b = env.borrower();
    let (res, nonce) = env.open(&mm, &mut b, 3_000_000_000_000);
    let meta = ok(res);
    let open: Open = events::one(&meta);
    println!(
        "open: q {} s {} m {} n_open {} fee {} cu {}",
        open.q, open.s, open.m, open.n_open, open.fee_open, meta.compute_units_consumed
    );
    env.warp(10);
    let meta = ok(env.close(&mm, &b, nonce));
    let close: Close = events::one(&meta);
    println!(
        "close: b {} rent {} payout {} cu {}",
        close.buyback_cost, close.rent, close.borrower_payout, meta.compute_units_consumed
    );
}
