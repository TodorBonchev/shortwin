//! `open_short` and `close_short` happy paths on SPL Token and Token-2022 bases, every number
//! checked against `shortwin-math` on the state the program saw (ARCH §3.4, §3.5; ECON §4,
//! §8.1).

use shortwin::state::{MarketVault, POSITION_OPEN, ShortPosition, WalletExposure};
use shortwin_math::rent::{self, PosRent};
use shortwin_math::risk;
use shortwin_math::tier::Tier;
use shortwin_math::waterfall;

use solana_signer::Signer;

use crate::common::*;

fn happy_path(program: TokenProgram) {
    let (mut env, mm) = setup_with(spec_on(program));
    env.crank(&mm);
    let config = env.config();
    let market0 = env.market(&mm.market());
    let vault0: MarketVault = env.market_vault(&mm.market());
    let mut b = env.borrower();
    let borrower0 = lamports(&env, &b.key());
    let pool_vault0 = tokens(&env, &mm.keys.pool_vault);

    // ---- open ----
    let s_exp = quote_sell(&env, &mm, Q);
    let (open, nonce, meta) = open_ok(&mut env, &mm, &mut b, Q);
    let now = env.now();
    let m1 = env.market(&mm.market());
    let tier = Tier::C; // no oracle yet, age < 24 h (ARCH §4.7: fail safe to C)
    let tp = config.tiers[tier.index()];
    assert_eq!(open.tier, tier as u8);
    assert_eq!(open.q, Q);
    assert_eq!(
        open.s, s_exp,
        "S = the sale quote on the live state (exec tolerance unused)"
    );
    let n_open = quote_buy(&env, &mm, Q);
    assert_eq!(
        open.n_open, n_open,
        "N_open = B̂_spot(q) on the post-open reserves"
    );
    let (x, y) = m1.newest_ring_xy();
    let b_ema = quote_buy_at(&env, &mm, Q, x, y);
    assert_eq!(open.b_ema, b_ema.unwrap_or(0));
    let m_expect = risk::margin_d4(open.s, n_open, b_ema, tp.cr_open_bps).unwrap();
    assert_eq!(open.m, m_expect, "D4 margin");
    assert_eq!(open.v, open.s + open.m);
    assert_eq!(
        open.fee_open,
        risk::open_fee(n_open, config.params.open_fee_bps).unwrap()
    );
    // D4 holds at open: V ≥ CR_open · max(B̂_spot, B̂_ema).
    let need = u128::from(n_open.max(b_ema.unwrap_or(0))) * u128::from(tp.cr_open_bps);
    assert!(u128::from(open.v) * 10_000 >= need);

    let pos: ShortPosition = env.position(&mm, &b.key(), nonce);
    assert_eq!(
        (pos.borrower, pos.market, pos.nonce),
        (b.key(), mm.market(), nonce)
    );
    assert_eq!(
        (pos.status, pos.tier, pos.venue_at_open),
        (POSITION_OPEN, tier as u8, 6)
    );
    assert_eq!((pos.q, pos.s, pos.m, pos.v), (Q, open.s, open.m, open.v));
    assert_eq!((pos.n_open, pos.fee_open), (n_open, open.fee_open));
    assert_eq!(
        pos.entry_price,
        u128::from(open.s) * shortwin_math::num::WAD / u128::from(Q)
    );
    assert_eq!(pos.j_open, m1.basis().j);
    assert_eq!(pos.t_open, now);
    assert_eq!(pos.deadline, now + i64::from(tp.max_duration_s));
    let u_pre = rent::utilization_bps(market0.tokens_free, market0.borrowed_q).unwrap();
    let r_min = rent::min_rent(
        Q,
        m1.ema_price.get(),
        &tp.to_math().rate,
        u_pre,
        m1.util_kink_bps,
        tp.min_rent_s,
    )
    .unwrap();
    assert_eq!(pos.r_min, r_min);
    // Locked terms (A26): the tier's and the config's values; ert 0 on the mock.
    assert_eq!(pos.cr_liq_bps, tp.cr_liq_bps);
    assert_eq!(u32::from(pos.penalty_bps), tp.penalty_bps);
    assert_eq!(u32::from(pos.max_liq_slip_bps), tp.max_liq_slip_bps);
    assert_eq!(u32::from(pos.expiry_fee_bps), config.params.expiry_fee_bps);
    assert_eq!(pos.rate_slope2_bps, tp.rate_slope2_bps);
    assert_eq!(
        u32::from(pos.rent_protocol_bps),
        config.params.rent_protocol_bps
    );
    assert_eq!(pos.settle_keeper_cap, config.params.settle_keeper_cap);
    assert_eq!(pos.delta_liq_bps, config.params.delta_liq_bps);
    assert_eq!(
        u32::from(pos.settle_persist_n),
        config.params.settle_persist_n
    );
    assert_eq!(pos.ert, 0);
    assert_eq!(
        u32::from(pos.venue_fee_hard_cap_bps),
        config.params.venue_fee_hard_cap_bps
    );
    assert_eq!(pos.s3_wait_s, config.params.s3_wait_s);
    assert_eq!(pos.s_armed, 0);
    // The position holds rent + V (I2).
    let pos_rent = env
        .svm
        .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
    assert_eq!(
        lamports(&env, &pda::position(&mm.market(), &b.key(), nonce).0),
        pos_rent + pos.v
    );

    // Market (ARCH §3.4 step 13).
    assert_eq!(m1.borrowed_q, Q);
    assert_eq!(m1.tokens_free, market0.tokens_free - Q);
    assert_eq!(m1.borrowed_q_by_tier[tier.index()], Q);
    assert_eq!((m1.open_positions, m1.total_notional_open), (1, n_open));
    assert_eq!(
        m1.max_release,
        pos.deadline
            + i64::from(config.params.max_migration_pause_s)
            + i64::from(config.params.grad_grace_s)
            + i64::from(config.params.s3_wait_s)
    );
    assert_eq!(tokens(&env, &mm.keys.pool_vault), pool_vault0 - Q);
    let vault1 = env.market_vault(&mm.market());
    assert_eq!(vault1.protocol_fees, vault0.protocol_fees + open.fee_open);
    let exposure: WalletExposure =
        read::borsh(&env.svm, &pda::exposure(&mm.market(), &b.key()).0).unwrap();
    assert_eq!(
        (exposure.market, exposure.wallet, exposure.q_open),
        (mm.market(), b.key(), Q)
    );
    // The borrower paid M, the fee and the two accounts' rent (the WSOL rent came back).
    let exp_rent = env
        .svm
        .minimum_balance_for_rent_exemption(WalletExposure::SPACE);
    assert_eq!(
        borrower0 - lamports(&env, &b.key()),
        open.m + open.fee_open + pos_rent + exp_rent,
        "borrower outlay; logs:\n{}",
        meta.pretty_logs()
    );
    // The trader and the ephemeral WSOL account are empty again (I7).
    assert_eq!(lamports(&env, &mm.keys.trader), 0);
    assert!(read::account(&env.svm, &mm.trader_wsol).is_none_or(|a| a.lamports == 0));

    // ---- close, 2 hours later ----
    env.tick(&mm, 2);
    env.warp(7_200);
    let before = lamports(&env, &b.key());
    let b_hat = quote_buy(&env, &mm, Q);
    let lender_sol0 = env.market_vault(&mm.market()).lender_sol;
    let fees0 = env.market_vault(&mm.market()).protocol_fees;
    let (close, _) = close_ok(&mut env, &mm, &b, nonce);
    let m2 = env.market(&mm.market());
    let pr = PosRent {
        q: Q,
        j_open: pos.j_open,
        terms: pos.rent_terms(),
        rent_protocol_bps: u32::from(pos.rent_protocol_bps),
        r_min: pos.r_min,
    };
    let r_due = rent::due(&pr, &m2.basis()).unwrap();
    assert_eq!(close.rent, r_due);
    assert_eq!(
        (close.buyback_cost, close.b_live),
        (b_hat, b_hat),
        "the venue charged the quote"
    );
    assert_eq!(close.token_surplus, 0);
    let payout = waterfall::close(pos.v, b_hat, r_due, u32::from(pos.rent_protocol_bps)).unwrap();
    assert_eq!(close.rent_protocol, payout.rent_protocol);
    assert_eq!(close.borrower_payout, payout.borrower);
    // The borrower got the remainder and both accounts' rent (the position and the exposure).
    assert_eq!(
        lamports(&env, &b.key()) - before,
        payout.borrower + pos_rent + exp_rent
    );
    assert!(read::account(&env.svm, &pda::position(&mm.market(), &b.key(), nonce).0).is_none());
    assert!(read::account(&env.svm, &pda::exposure(&mm.market(), &b.key()).0).is_none());
    // Market back to no position; lenders got the rent less the protocol share (A_j and
    // receivable back to 0: I8).
    assert_eq!(
        (m2.borrowed_q, m2.open_positions, m2.total_notional_open),
        (0, 0, 0)
    );
    assert_eq!(m2.tokens_free, market0.tokens_free);
    assert_eq!(m2.receivable, 0);
    assert!(m2.rent_aggr.iter().all(|a| a.get() == 0));
    let vault2 = env.market_vault(&mm.market());
    assert_eq!(vault2.protocol_fees, fees0 + payout.rent_protocol);
    assert_eq!(vault2.lender_sol, lender_sol0 + payout.rent_lenders);
    assert_eq!(tokens(&env, &mm.keys.pool_vault), pool_vault0);
}

#[test]
fn open_and_close_spl_base() {
    happy_path(TokenProgram::Spl);
}

#[test]
fn open_and_close_token_2022_base() {
    happy_path(TokenProgram::Token2022);
}

/// Several borrowers and positions: aggregates add up (I6), exposure per wallet, a wallet's
/// exposure record closes only when its last position does, and a fresh open after that
/// re-creates it.
#[test]
fn several_positions_and_wallets() {
    let (mut env, mm) = setup();
    let mut a = env.borrower();
    let mut b = env.borrower();
    let (oa1, na1, _) = open_ok(&mut env, &mm, &mut a, Q / 3);
    env.tick(&mm, 1);
    let (_ob1, nb1, _) = open_ok(&mut env, &mm, &mut b, Q / 2);
    env.tick(&mm, 1);
    let (_oa2, na2, _) = open_ok(&mut env, &mm, &mut a, Q / 4);
    let m = env.market(&mm.market());
    assert_eq!(m.open_positions, 3);
    assert_eq!(m.borrowed_q, Q / 3 + Q / 2 + Q / 4);
    let exp_a: WalletExposure =
        read::borsh(&env.svm, &pda::exposure(&mm.market(), &a.key()).0).unwrap();
    assert_eq!(exp_a.q_open, Q / 3 + Q / 4);
    env.tick(&mm, 3);
    close_ok(&mut env, &mm, &a, na1);
    let exp_a: WalletExposure =
        read::borsh(&env.svm, &pda::exposure(&mm.market(), &a.key()).0).unwrap();
    assert_eq!(
        exp_a.q_open,
        Q / 4,
        "exposure stays while a position is open"
    );
    close_ok(&mut env, &mm, &a, na2);
    assert!(read::account(&env.svm, &pda::exposure(&mm.market(), &a.key()).0).is_none());
    // Re-open with a new nonce: the exposure record comes back.
    let (_oa3, na3, _) = open_ok(&mut env, &mm, &mut a, Q / 5);
    assert_eq!(na3, 2);
    env.tick(&mm, 1);
    close_ok(&mut env, &mm, &b, nb1);
    close_ok(&mut env, &mm, &a, na3);
    let m = env.market(&mm.market());
    assert_eq!((m.open_positions, m.borrowed_q), (0, 0));
    assert!(oa1.s > 0);
}

/// A nonce cannot be reused while its position exists (`init`), and a closed position's nonce
/// can be opened again (a new account at the same address).
#[test]
fn nonce_reuse() {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q / 3);
    b.next_nonce = nonce;
    let (res, _) = env.open(&mm, &mut b, Q / 3);
    assert!(res.is_err(), "an existing position blocks its nonce");
    env.tick(&mm, 1);
    close_ok(&mut env, &mm, &b, nonce);
    b.next_nonce = nonce;
    let (res, _) = env.open(&mm, &mut b, Q / 3);
    ok(res);
}

/// Closing while the market has other activity: a lender deposit and a withdrawal between an
/// open and its close keep every invariant (I1, I6, I8 run after each instruction).
#[test]
fn lender_activity_between_open_and_close() {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 2);
    let lender = mm.lender.insecure_clone();
    let shares = env.lender(&mm.market(), &lender.pubkey()).unwrap().shares;
    ok(env.send(
        &[shortwin_litesvm_tests::ix::withdraw(
            &mm.lender_keys(),
            shares / 10,
            0,
        )],
        &[&lender],
    ));
    env.warp(3_600);
    close_ok(&mut env, &mm, &b, nonce);
    ok(env.send(
        &[shortwin_litesvm_tests::ix::withdraw(
            &mm.lender_keys(),
            shares / 10,
            0,
        )],
        &[&lender],
    ));
}
