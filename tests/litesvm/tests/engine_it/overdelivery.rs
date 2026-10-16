//! A venue that delivers more tokens than a buy asked for (A62; V2-10; the mock's
//! `over_delivery` knob): the close is accepted, the surplus stays in the pool vault as a
//! donation (`T` is unchanged by it: I1 already tolerates donations) and is reported, so a
//! harmless venue change cannot freeze exits.

use shortwin_litesvm_tests::mock::{Knobs, update_knobs};

use crate::common::*;

#[test]
fn over_delivery_on_a_buy_is_a_donation() {
    for program in TokenProgram::BOTH {
        let (mut env, mm) = setup_with(spec_on(program));
        let mut b = env.borrower();
        let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
        env.tick(&mm, 2);
        update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.over_delivery = 12_345);
        let t0 = env.market(&mm.market()).tokens_free;
        let vault0 = tokens(&env, &mm.keys.pool_vault);
        let b_hat = quote_buy(&env, &mm, Q);
        let (close, _) = close_ok(&mut env, &mm, &b, nonce);
        assert_eq!(close.token_surplus, 12_345);
        assert_eq!(close.buyback_cost, b_hat, "the charge is the quote for q");
        assert_eq!(
            env.market(&mm.market()).tokens_free,
            t0 + Q,
            "T takes back exactly q"
        );
        assert_eq!(tokens(&env, &mm.keys.pool_vault), vault0 + Q + 12_345);
    }
}
