//! Every M2 instruction has landed (ARCH §3.5–§3.8; no `NotImplemented` is left): on a healthy
//! position that is not expired, `liquidate` and `expire` refuse with their own gates and
//! change nothing, `sol_settle` is an `Ok` no-op and `add_margin` tops up. The account lists
//! are checked before the body. The full behaviour lives in `tests/add_margin_it.rs`,
//! `tests/liquidate_it.rs`, `tests/expire_it.rs` and `tests/settle_it.rs`.

use shortwin::errors::ShortwinError as E;
use shortwin_litesvm_tests::engine::{add_margin_ix, expire_ix, liquidate_ix, sol_settle_ix};
use solana_signer::Signer;

use crate::common::*;

#[test]
fn the_m2_exits_gate_a_healthy_position() {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 1);
    let keeper = env.funded_keypair();
    let before = env.position(&mm, &b.key(), nonce);
    assert_err(
        &env.send(
            &[liquidate_ix(&mm, &keeper.pubkey(), &b.key(), nonce)],
            &[&keeper],
        ),
        E::NotLiquidatable,
    );
    assert_err(
        &env.send(
            &[expire_ix(&mm, &keeper.pubkey(), &b.key(), nonce)],
            &[&keeper],
        ),
        E::NotExpired,
    );
    // `sol_settle` on a healthy position: an `Ok` no-op.
    let settle = sol_settle_ix(&env, &mm, &keeper.pubkey(), &b.key(), nonce);
    ok(env.send(&[settle], &[&keeper]));
    assert_eq!(env.position(&mm, &b.key(), nonce), before);
    // `add_margin` raises `v` and `m` only.
    ok(env.send(&[add_margin_ix(&mm, &b.key(), nonce, 1_000_000)], &[&b.kp]));
    let after = env.position(&mm, &b.key(), nonce);
    assert_eq!(
        (after.v, after.m),
        (before.v + 1_000_000, before.m + 1_000_000)
    );
    // The account lists are checked before the body: a wrong borrower fails its constraint.
    let other = env.borrower();
    let res = env.send(
        &[liquidate_ix(&mm, &keeper.pubkey(), &other.key(), nonce)],
        &[&keeper],
    );
    assert!(res.is_err());
    assert_ne!(custom_code(&res), Some(code(E::NotLiquidatable)));
}
