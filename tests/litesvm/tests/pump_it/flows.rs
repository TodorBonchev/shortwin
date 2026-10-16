//! `open_short` → `close_short` on the real pump program (ARCH §2.4.1 PumpCurve row, §3.4, §3.5;
//! T-OPEN-V3, pump-final §6.1.4): every lamport and token accounted for on Token-2022 curves of 125,
//! 115 and 151 bytes, a legacy SPL-Token curve of 115 bytes without a creator (creator fee 0), and
//! the 166-byte curves of pump's 2026-10-08 layout (`fresh166`, `near166`, `buckets166`, the last
//! carrying fee buckets).
//!
//! Each round trip asserts: `S` = the `sell_v3` proceeds, `quote_sell(q)` on the pre-trade state
//! exactly (no creator-vault top-up: v3 keeps the creator fee in the curve's bucket and names no
//! vault; only a short curve's growth: `S` is net of it, `PumpCurve::growth_top_up`); the lending
//! vault loses exactly `q`, the curve's base ATA gains it, and the curve's lamports fall by the
//! gross `g` less the fees it keeps in its buckets (and plus that growth); the borrower pays the
//! position's and the exposure record's rent, `M` and the open fee, and gets its `ert` back (it
//! funds the trader for the leg); `trader` and `trader_base_ata` hold nothing (I7); `N_open` = `B̂`
//! on the post-open state. At close: the venue charges exactly pump's `curve_charge_exact_out(q)`
//! (≤ short.win's quote `B̂_live(q)`, by at most `1 + #fees`), the lending vault receives exactly
//! `q` through the trader's base ATA, the curve gains the pre-fee `n` and the fees it keeps, the
//! waterfall pays `B + R` and the borrower the rest; the creator vault never moves.

use shortwin::state::WalletExposure;

use crate::common::*;

/// Balances around one instruction.
#[derive(Clone, Copy, Debug)]
struct Snap {
    pool_vault: u64,
    curve_tokens: u64,
    curve: u64,
    /// The curve's fee buckets (`creator_fee` @125 + `protocol_fees` @133; v3 keeps its fees
    /// there, outside `rQ`).
    buckets: u64,
    borrower: u64,
    market_vault: u64,
    creator_vault: u64,
}

fn snap(env: &Env, pm: &PumpMarket, borrower: &Pubkey) -> Snap {
    let data = env.svm.get_account(&pm.curve.curve).unwrap().data;
    let (creator_fee, protocol_fees) = fork::curve_fee_buckets(&data);
    Snap {
        pool_vault: tokens(env, &pm.keys.pool_vault),
        curve_tokens: tokens(env, &pm.curve.curve_base_ata),
        curve: lamports(env, &pm.curve.curve),
        buckets: creator_fee + protocol_fees,
        borrower: lamports(env, borrower),
        market_vault: lamports(env, &pm.keys.market_vault),
        creator_vault: lamports(env, &pm.creator_vault),
    }
}

/// One exact open → close round trip of `q` on `pm` (module docs). Returns `(S, B)`.
pub fn round_trip(env: &mut Env, pm: &PumpMarket, q: u64) -> (u64, u64) {
    let mut b = env.borrower();
    let s0 = snap(env, pm, &b.key());
    let sale = pm.curve.quote_sell(&env.svm, q).expect("sale quote");
    let vq0 = pm.curve.state(&env.svm).virtual_quote_reserves;
    let gross = vq0 - u64::try_from(sale.sol_after).unwrap();
    let first_exposure =
        read::account(&env.svm, &pda::exposure(&pm.market(), &b.key()).0).is_none();
    let growth = pm.curve.growth_top_up(&env.svm);
    let (open, nonce, _) = open_ok(env, pm, &mut b, q);
    let s1 = snap(env, pm, &b.key());
    assert_eq!(
        open.s,
        sale.proceeds - growth,
        "S = the sale quote on the pre-trade state (less the curve's growth)"
    );
    assert_eq!(
        s0.pool_vault - s1.pool_vault,
        q,
        "the lending vault spent exactly q"
    );
    assert_eq!(s1.curve_tokens - s0.curve_tokens, q, "the curve received q");
    assert_eq!(
        s0.curve + growth + (s1.buckets - s0.buckets) - s1.curve,
        gross,
        "the curve paid the gross g, kept its fees in the buckets (and took the growth)"
    );
    assert_eq!(
        s1.creator_vault, s0.creator_vault,
        "no creator-vault top-up"
    );
    assert_trader_clean(env, pm);
    let n_open = pm
        .curve
        .quote_buy(&env.svm, q)
        .expect("post-open buy quote")
        .cost;
    assert_eq!(open.n_open, n_open, "N_open = B̂ on the post-open state");
    assert_eq!(open.venue, VenueKind::PumpCurve as u8);
    assert_eq!(
        open.tier,
        shortwin_math::tier::Tier::D as u8,
        "the curve is Tier D"
    );
    let pos_key = pda::position(&pm.market(), &b.key(), nonce).0;
    let pos: ShortPosition = read::borsh(&env.svm, &pos_key).expect("position");
    let cfg = env.config().params;
    assert_eq!(
        pos.ert, cfg.exec_rent_tol,
        "a curve position locks ert = exec_rent_tol"
    );
    let pos_rent = env
        .svm
        .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
    assert_eq!(
        lamports(env, &pos_key),
        pos_rent + pos.v,
        "position = rent + V"
    );
    let exp_rent = if first_exposure {
        env.svm
            .minimum_balance_for_rent_exemption(WalletExposure::SPACE)
    } else {
        0
    };
    // The borrower funded ert and got it back: it pays only the accounts, M and the fee.
    assert_eq!(
        s0.borrower - s1.borrower,
        pos_rent + exp_rent + open.m + open.fee_open,
        "the borrower paid the accounts, M and the open fee"
    );
    assert_eq!(
        s1.market_vault - s0.market_vault,
        open.fee_open,
        "the open fee went to the market vault"
    );

    // ---- close ----
    let quote = pm.curve.quote_buy(&env.svm, q).expect("buy quote");
    let charge = pm.curve.charge_buy(&env.svm, q).expect("pump's charge");
    let (close, _) = close_ok(env, pm, &b, nonce);
    let s2 = snap(env, pm, &b.key());
    assert_eq!(close.b_live, quote.cost, "the leg is bounded by B̂_live(q)");
    assert_eq!(close.buyback_cost, charge.cost, "B = pump's charge");
    assert!(quote.cost >= charge.cost && quote.cost - charge.cost <= 3);
    assert_eq!(
        s2.pool_vault - s1.pool_vault,
        q,
        "the lending vault got q back"
    );
    assert_eq!(s1.curve_tokens - s2.curve_tokens, q, "the curve sold q");
    assert_eq!(
        s2.curve - s1.curve - (s2.buckets - s1.buckets),
        charge.net,
        "the curve gained n (and the fees it keeps)"
    );
    assert_eq!(
        s2.creator_vault, s1.creator_vault,
        "no creator-vault top-up"
    );
    assert_eq!(close.token_surplus, 0);
    assert_trader_clean(env, pm);
    assert!(
        read::account(&env.svm, &pos_key).is_none(),
        "position closed"
    );
    (open.s, close.buyback_cost)
}

/// A Token-2022 curve of 115 bytes (the first v2 trade — the listing's warm-up already — grows it
/// to the current layout, 166 bytes since the 2026-10-08 redeploy, 141 since the 2026-10-02 one,
/// 125 before; its rent reserve already covers that, so no top-up).
#[test]
fn round_trip_t22_115() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    round_trip(&mut env, &pm, q);
    assert_eq!(
        pm.curve.data_len(&env.svm),
        fork::PumpDeploy::CURRENT.curve_len(),
        "the curve grew to the current layout"
    );
}

/// A legacy SPL-Token curve of 115 bytes without a creator: no creator fee, no creator vault.
#[test]
fn round_trip_legacy_spl_without_creator() {
    let (mut env, pm, q) = setup(PumpCoin::Legacy115);
    assert_eq!(pm.curve.token_program, TokenProgram::Spl);
    let fees = pm.curve.fees(&env.svm).unwrap();
    assert_eq!(fees.creator, 0, "a default creator pays no creator fee");
    round_trip(&mut env, &pm, q);
}

/// A fresh `create_v2` curve (125 bytes, Token-2022) after 5 SOL of buys (which grow it to the
/// current layout: 166 bytes since the 2026-10-08 redeploy).
#[test]
fn round_trip_fresh_125() {
    let (mut env, pm, q) = setup(PumpCoin::Fresh);
    assert_eq!(
        pm.curve.data_len(&env.svm),
        fork::PumpDeploy::CURRENT.curve_len()
    );
    round_trip(&mut env, &pm, q);
}

/// T-OPEN-V3 on the 166-byte layout of pump's 2026-10-08 redeploy (no growth): a `create_v2`
/// curve minutes old (after a crowd's buys, so it can pay a sale), the near-complete `near166`, and
/// `buckets166`, which carries large fee buckets the trades add to.
#[test]
fn round_trip_on_166_byte_curves() {
    for coin in [PumpCoin::Fresh166, PumpCoin::Near166, PumpCoin::Buckets166] {
        let (mut env, pm, q) = setup(coin);
        assert_eq!(pm.curve.data_len(&env.svm), 166, "{coin:?}");
        assert_eq!(pm.curve.growth_top_up(&env.svm), 0);
        round_trip(&mut env, &pm, q);
    }
}

/// The near-complete 151-byte curve, twice (the second open reuses the exposure record... after
/// the first closed it; each round trip is exact on its own pre-state). Its rent reserve is
/// `minimum_balance(151)`, so the first open's sale grows it to 166 bytes on the 2026-10-08
/// build and pays the 76,200 of rent out of `S`; the second pays none.
#[test]
fn round_trip_near_151_twice() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    assert_eq!(pm.curve.data_len(&env.svm), 151);
    assert_eq!(pm.curve.growth_top_up(&env.svm), 76_200);
    round_trip(&mut env, &pm, q);
    assert_eq!(
        pm.curve.data_len(&env.svm),
        fork::PumpDeploy::CURRENT.curve_len()
    );
    assert_eq!(pm.curve.growth_top_up(&env.svm), 0);
    env.warp(3);
    round_trip(&mut env, &pm, q / 2);
}
