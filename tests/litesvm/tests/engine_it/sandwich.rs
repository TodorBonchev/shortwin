//! Sandwiches around a borrower's open and close (SPEC §12 R4 "Sandwiching opens/closes: tight
//! slippage, bundles"; ECON §4 open guards, §8.1; ARCH §3.4 steps 4, 8, 10, §3.5 step 5): the
//! attacker's front-run, the victim's instruction and the back-run in **one transaction**
//! (atomic: nothing can land in between).
//!
//! What bounds the attack:
//!
//! - **the borrower's own bounds**: `min_proceeds` on an open, `max_buyback_cost` on a close —
//!   a sandwich beyond them reverts the whole bundle (the attacker gets nothing);
//! - **the open deviation guard**: a front-run that moves the spot more than
//!   `open_ema_dev_bps` (10 %) from `P_ema` fails the open (`PriceDeviatesFromEma`);
//! - **the one-second lag**: the front-run never reaches the EMA the open reads (the
//!   instruction's own observation is recorded after the roll), so the D4 margin's EMA leg —
//!   the lenders' protection at open — is the honest one;
//! - **the close waterfall**: rent is senior to the borrower's remainder (`max_in ≤ V − R`), so
//!   lenders are paid in full whatever the attacker does to the buyback price.

use shortwin::errors::ShortwinError as E;
use shortwin::events::{Close, Open};
use shortwin::venues::ExtrasSel;

use crate::common::*;

/// The victim's open with bounds relative to the honest quote.
fn open_ix_for(
    mm: &MockMarket,
    b: &Borrower,
    min_proceeds: u64,
) -> anchor_lang::solana_program::instruction::Instruction {
    open_short_ix(
        mm,
        &b.key(),
        b.next_nonce,
        Q,
        min_proceeds,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    )
}

#[test]
fn sandwich_around_an_open() {
    let (mut env, mm) = setup();
    env.tick(&mm, 3);
    env.warp(1);
    let attacker = env.actor(&mm, 100_000_000_000_000, 100_000_000_000);
    let b = env.borrower();
    let s_honest = quote_sell(&env, &mm, Q);
    let saved = env.svm.clone();

    // Honest open.
    let meta = ok(env.send(&[open_ix_for(&mm, &b, 0)], &[&b.kp]));
    let honest: Open = events::one(&meta);
    assert_eq!(honest.s, s_honest);

    // A front-run of 0.5 % of the pool's tokens with no bound of the borrower's: the open goes
    // through at a worse S, but the EMA the margin reads is the honest one, so lenders' CR at
    // open holds on both legs.
    env.svm = saved.clone();
    let (res, _sol, tokens_gained) = env.sandwich(
        &mm,
        &attacker,
        open_ix_for(&mm, &b, 0),
        &b.kp,
        Dir::BaseIn,
        5_000_000_000_000,
    );
    let meta = ok(res);
    let attacked: Open = events::one(&meta);
    assert!(attacked.s < honest.s);
    assert_eq!(
        attacked.ema_wad, honest.ema_wad,
        "the front-run is not in the EMA the open reads"
    );
    assert_eq!(attacked.b_ema, honest.b_ema);
    let cr_open = u128::from(env.config().tiers[2].cr_open_bps);
    let need = u128::from(attacked.b_spot.max(attacked.b_ema)) * cr_open;
    assert!(
        u128::from(attacked.v) * 10_000 >= need,
        "D4 holds after the sandwich"
    );
    assert!(
        attacked.m > honest.m,
        "the borrower's margin pays for the worse sale"
    );
    assert!(
        tokens_gained > 0,
        "an unbounded borrower is sandwiched profitably"
    );

    // The same front-run against a borrower bounding its proceeds at the honest quote less
    // 0.1 %: the whole bundle reverts. `min_proceeds` is the venue's own slippage argument
    // (ARCH §2.4 rule 3), so the mock refuses the sale itself (its `SlippageMinOut`).
    env.svm = saved.clone();
    let bound = s_honest - s_honest / 1_000;
    let (res, sol, tokens_gained) = env.sandwich(
        &mm,
        &attacker,
        open_ix_for(&mm, &b, bound),
        &b.kp,
        Dir::BaseIn,
        5_000_000_000_000,
    );
    assert_eq!(custom_code(&res), Some(MOCK_SLIPPAGE_MIN_OUT));
    assert_eq!(
        (sol, tokens_gained),
        (0, 0),
        "a reverted bundle moves nothing"
    );

    // A front-run that pushes the spot more than 10 % below the EMA fails the deviation guard
    // whatever the borrower's bounds.
    env.svm = saved;
    // Selling 6 % of the pool's tokens moves the spot by 1 − 1/1.06² ≈ −11 %.
    let (res, _, _) = env.sandwich(
        &mm,
        &attacker,
        open_ix_for(&mm, &b, 0),
        &b.kp,
        Dir::BaseIn,
        60_000_000_000_000,
    );
    assert_eq!(
        custom_code(&res),
        Some(code(E::PriceDeviatesFromEma)),
        "{:?}",
        res.as_ref()
            .err()
            .map(|f| (f.err.clone(), f.meta.pretty_logs()))
    );
}

/// The mock's `SlippageMinOut` (INTERFACE.md errors): the venue enforcing the leg's `min_out`.
const MOCK_SLIPPAGE_MIN_OUT: u32 = 6_018;

/// A small front-run inside the borrower's 0.5 % tolerance: the open goes through and the
/// borrower loses at most its tolerance.
#[test]
fn a_front_run_inside_the_tolerance_costs_at_most_the_tolerance() {
    let (mut env, mm) = setup();
    env.tick(&mm, 2);
    env.warp(1);
    let attacker = env.actor(&mm, 50_000_000_000_000, 0);
    let b = env.borrower();
    let s_honest = quote_sell(&env, &mm, Q);
    let bound = s_honest - s_honest / 200;
    let (res, _, _) = env.sandwich(
        &mm,
        &attacker,
        open_ix_for(&mm, &b, bound),
        &b.kp,
        Dir::BaseIn,
        1_000_000_000_000,
    );
    let meta = ok(res);
    let open: Open = events::one(&meta);
    assert!(open.s >= bound && open.s < s_honest);
}

#[test]
fn sandwich_around_a_close() {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (open, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 3);
    env.warp(3_600);
    let attacker = env.actor(&mm, 0, 200_000_000_000);
    let b_honest = quote_buy(&env, &mm, Q);
    let saved = env.svm.clone();
    let close_ix = |max: u64| close_short_ix(&mm, &b.key(), nonce, max, ExtrasSel::NONE, &[]);

    // Honest close.
    let meta = ok(env.send(&[close_ix(u64::MAX)], &[&b.kp]));
    let honest: Close = events::one(&meta);
    assert_eq!(honest.buyback_cost, b_honest);
    let vault_honest = env.market_vault(&mm.market());

    // A front-run buy of 20 SOL against an unbounded close: the borrower pays more, the
    // attacker profits, and lenders are paid exactly the same rent (rent is senior).
    env.svm = saved.clone();
    let (res, sol_gained, _) = env.sandwich(
        &mm,
        &attacker,
        close_ix(u64::MAX),
        &b.kp,
        Dir::QuoteIn,
        20_000_000_000,
    );
    let meta = ok(res);
    let attacked: Close = events::one(&meta);
    assert!(attacked.buyback_cost > honest.buyback_cost);
    assert_eq!(attacked.rent, honest.rent);
    assert_eq!(attacked.rent_protocol, honest.rent_protocol);
    assert_eq!(
        attacked.borrower_payout,
        honest.borrower_payout - (attacked.buyback_cost - honest.buyback_cost),
        "the borrower alone bears the sandwich"
    );
    assert!(sol_gained > 0);
    let vault_attacked = env.market_vault(&mm.market());
    assert_eq!(vault_attacked.lender_sol, vault_honest.lender_sol);
    assert_eq!(vault_attacked.protocol_fees, vault_honest.protocol_fees);

    // The borrower bounds its buyback at the honest quote + 0.2 %: the bundle reverts.
    env.svm = saved;
    let (res, sol_gained, base_gained) = env.sandwich(
        &mm,
        &attacker,
        close_ix(b_honest + b_honest / 500),
        &b.kp,
        Dir::QuoteIn,
        20_000_000_000,
    );
    assert_eq!(custom_code(&res), Some(code(E::BuybackAboveMax)));
    assert_eq!((sol_gained, base_gained), (0, 0));
    assert!(open.v > 0);
    // The close still works afterwards (nothing was committed).
    ok(env.send(&[close_ix(u64::MAX)], &[&b.kp]));
}
