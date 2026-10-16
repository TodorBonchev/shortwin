//! Smoke: list, open and close on every eligible dumped pool.

use crate::common::*;

#[test]
fn open_close_every_eligible_pool() {
    for fixture in ELIGIBLE {
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let s_exp = rm.quote_sell(&env.svm, q).expect("sale quote").proceeds;
        let (open, nonce, meta) = open_ok(&mut env, &rm, &mut b, q);
        println!(
            "{fixture:?}: open q {q} s {} ({} CU)",
            open.s, meta.compute_units_consumed
        );
        assert_eq!(open.s, s_exp, "{fixture:?}");
        env.warp(1);
        env.crank(&rm);
        env.warp(1);
        let b_exp = rm.quote_buy(&env.svm, q).expect("buy quote").cost;
        let (close, meta) = close_ok(&mut env, &rm, &b, nonce);
        println!(
            "{fixture:?}: close cost {} ({} CU)",
            close.buyback_cost, meta.compute_units_consumed
        );
        assert_eq!(close.buyback_cost, b_exp, "{fixture:?}");
    }
}
