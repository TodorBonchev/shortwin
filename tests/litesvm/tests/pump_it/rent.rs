//! T-RENT, curve part (ARCH §2.4 step 3 "Exact funding", §8.2; C1-4, C1-9; R-PUMP U6, Update
//! 2026-09-29; pump-final §3.5, §6.1.4): the venue-side rent pump takes from `user` — the trader
//! PDA — stays within `exec_rent_tol` and is borne as designed, on the real pump ELF:
//!
//! - **no creator-vault top-up**: short.win trades v3, which keeps the creator fee in the curve's
//!   bucket (@125) and names no creator vault, so an empty vault costs neither a sale (`S` = the
//!   proceeds) nor a buy (`B` = pump's charge) anything, and stays empty (under v2 pump topped it up
//!   to `Rent.minimum_balance(0)` = 650,240 out of the trader, and a small sale was refused
//!   `ProceedsBelowMin`);
//! - the pump UVA was created at listing (`create_market`), so no leg pays its 1,346,200;
//! - **growth** (pump's 2026-10-08 redeploy; 141 bytes on the 2026-10-02 one): a curve under 166
//!   bytes grows to 166 on short.win's first trade, its rent reserve (`lamports − rQ −` the fee
//!   buckets, `fork::curve_growth_top_up`) topped up to `Rent.minimum_balance(166)` from the
//!   trader — out of `S` on a sale (the 151-byte `near` curve's reserve is exactly
//!   `minimum_balance(151)`, so it pays 76,200);
//! - **short.win's net check**: a sale whose `S_exp` tolerance (`exec_tol_bps`) cannot cover that
//!   growth is refused `ProceedsBelowMin` after pump's own `min_sol_output` check on the gross
//!   passed (ARCH §2.4 step 3: short.win requires the **net**), and nothing is written.
//!
//! The empty creator vault is planted on pump's side (the vault PDA's lamports removed), as
//! `pump_math_it::an_empty_creator_vault_is_topped_up_by_the_user` does for a v2 user.
//! Every instruction goes through `Env::send`: the invariant hooks run after each one.

use solana_account::Account;

use crate::common::*;

/// Mainnet's `Rent.minimum_balance(0)` (the `sysvar_rent` fixture): what v2 topped an empty
/// creator vault up to.
const TOP_UP: u64 = 650_240;

fn empty_creator_vault(env: &mut Env, pm: &PumpMarket) {
    env.svm
        .set_account(pm.creator_vault, Account::default())
        .unwrap();
    assert_eq!(lamports(env, &pm.creator_vault), 0);
}

/// A sale into an empty creator vault: `S` = the quote less the curve's growth only (no top-up),
/// `ert` back to the borrower, the vault still empty, the trader clean.
#[test]
fn a_sale_into_an_empty_creator_vault_pays_no_top_up() {
    let (mut env, pm, _) = setup(PumpCoin::Near);
    assert_eq!(env.svm.minimum_balance_for_rent_exemption(0), TOP_UP);
    let vt = pm.curve.state(&env.svm).virtual_token_reserves;
    let q = vt / 100;
    let sale = pm.curve.quote_sell(&env.svm, q).unwrap();
    let fee = pm.curve.fees(&env.svm).unwrap();
    assert!(fee.creator > 0, "a creator fee: v2 would top the vault up");
    empty_creator_vault(&mut env, &pm);
    // The 151-byte curve's growth to 166 (the 2026-10-08 build) comes out of `S`.
    let growth = pm.curve.growth_top_up(&env.svm);
    assert_eq!(growth, 76_200);
    let mut b = env.borrower();
    let b0 = lamports(&env, &b.key());
    let (open, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(open.s, sale.proceeds - growth, "S = proceeds − the growth");
    assert_eq!(lamports(&env, &pm.creator_vault), 0, "no top-up");
    assert_trader_clean(&env, &pm);
    // The borrower got ert back: it paid the accounts, M and the fee only.
    let pos_key = pda::position(&pm.market(), &b.key(), nonce).0;
    let pos_rent = env
        .svm
        .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
    let exp_rent = env
        .svm
        .minimum_balance_for_rent_exemption(shortwin::state::WalletExposure::SPACE);
    assert_eq!(
        b0 - lamports(&env, &b.key()),
        pos_rent + exp_rent + open.m + open.fee_open,
        "ert refunded exactly"
    );
    assert_eq!(lamports(&env, &pos_key), pos_rent + open.s + open.m);
}

/// The small sale v2 refused (`ProceedsBelowMin`: the 650,240 top-up exceeded `S_exp·exec_tol_bps/
/// 10⁴`) lands under v3: `S` = the proceeds less the growth, the vault still empty.
#[test]
fn a_small_sale_into_an_empty_vault_lands() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let sale = pm.curve.quote_sell(&env.svm, q).unwrap();
    let tol = env.config().params.exec_tol_bps;
    assert!(
        u128::from(TOP_UP) * 10_000 > u128::from(sale.proceeds) * u128::from(tol),
        "v2's top-up would exceed the tolerance"
    );
    empty_creator_vault(&mut env, &pm);
    let growth = pm.curve.growth_top_up(&env.svm);
    let mut b = env.borrower();
    let (open, _, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(open.s, sale.proceeds - growth);
    assert_eq!(lamports(&env, &pm.creator_vault), 0, "no top-up");
    assert_trader_clean(&env, &pm);
}

/// The 115-byte curve's rent reserve planted down to `minimum_balance(115)` (as
/// [`a_sale_pays_a_short_curves_growth_out_of_its_proceeds`]), so a first sale owes pump the
/// 259,080 lamports of its growth to 166 bytes. A sale of the default size, whose `S_exp`
/// tolerance (`exec_tol_bps`) is below that growth: pump's `sell_v3` succeeds — its own
/// `min_sol_output` check on the **gross** proceeds passes —, the net `S` = proceeds − growth
/// misses `min_out`, and the open is refused `ProceedsBelowMin` by short.win (ARCH §2.4 step 3).
/// Nothing is written: the curve keeps its 115 bytes and lamports, the market, the lending vault
/// and the borrower are as before, no position exists, the trader is clean.
#[test]
fn a_small_sale_on_a_short_curve_is_refused() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    assert_eq!(pm.curve.data_len(&env.svm), 115);
    let min115 = env.svm.minimum_balance_for_rent_exemption(115);
    let rq = pm.curve.state(&env.svm).real_quote_reserves;
    let mut acc = env.svm.get_account(&pm.curve.curve).unwrap();
    acc.lamports = rq + min115;
    env.svm.set_account(pm.curve.curve, acc).unwrap();
    let growth = pm.curve.growth_top_up(&env.svm);
    assert_eq!(growth, 259_080);
    let sale = pm.curve.quote_sell(&env.svm, q).unwrap();
    let tol = u64::from(env.config().params.exec_tol_bps);
    assert!(
        sale.proceeds * tol / 10_000 < growth,
        "the growth exceeds the tolerance: S_exp {}",
        sale.proceeds
    );
    let curve0 = env.svm.get_account(&pm.curve.curve);
    let market0 = env.svm.get_account(&pm.market());
    let vault0 = env.svm.get_account(&pm.keys.pool_vault);
    let mut b = env.borrower();
    let b0 = lamports(&env, &b.key());
    let (res, nonce) = env.open(&pm, &mut b, q);
    assert_err(&res, E::ProceedsBelowMin);
    let logs = &res.as_ref().expect_err("refused").meta.logs;
    let pump_ok = format!("Program {} success", shortwin::constants::PUMP_PROGRAM_ID);
    assert!(
        logs.iter().any(|l| l == &pump_ok)
            && logs
                .iter()
                .any(|l| l.contains("Error Code: ProceedsBelowMin")),
        "pump's sale passed, short.win's net check refused:\n{}",
        logs.join("\n")
    );
    assert_eq!(
        env.svm.get_account(&pm.curve.curve),
        curve0,
        "the curve untouched"
    );
    assert_eq!(pm.curve.data_len(&env.svm), 115);
    assert_eq!(env.svm.get_account(&pm.market()), market0);
    assert_eq!(env.svm.get_account(&pm.keys.pool_vault), vault0);
    assert_eq!(lamports(&env, &b.key()), b0, "the borrower paid nothing");
    let pos = pda::position(&pm.market(), &b.key(), nonce).0;
    assert_eq!(lamports(&env, &pos), 0, "no position");
    assert_trader_clean(&env, &pm);
}

/// A buyback into an empty creator vault: `B` = pump's charge exactly (no top-up), within the
/// quote; the lending vault gets exactly `q`; the vault stays empty.
#[test]
fn a_buyback_into_an_empty_creator_vault_pays_no_top_up() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    empty_creator_vault(&mut env, &pm);
    let charge = pm.curve.charge_buy(&env.svm, q).unwrap();
    let quote = pm.curve.quote_buy(&env.svm, q).unwrap();
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.buyback_cost, charge.cost, "B = pump's charge");
    assert!(close.b_live >= charge.cost && close.b_live == quote.cost);
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, q);
    assert_eq!(lamports(&env, &pm.creator_vault), 0, "no top-up");
    assert_trader_clean(&env, &pm);
}

/// The trader's pump UVA exists from the listing: a first open and close on a fresh market
/// leave it as it was (no leg pays UVA rent), and it accumulates for the trader.
#[test]
fn the_uva_created_at_listing_is_never_paid_by_a_leg() {
    let (mut env, pm, q) = setup(PumpCoin::Fresh);
    let uva = fork::pump_user_volume_accumulator(&pm.trader());
    let l0 = lamports(&env, &uva);
    assert!(l0 > 0, "created at listing");
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(lamports(&env, &uva), l0, "no rent moved");
}

/// A sale on a 115-byte curve whose rent reserve is planted down to `minimum_balance(115)` (the
/// dump's is over-funded at the old rent): pump's 2026-10-08 redeploy grows it to 166 bytes (141
/// on the 2026-10-02 build) and tops the reserve up to `minimum_balance(166)` — 259,080 lamports
/// (132,080 to 141) — from the trader before the proceeds, so `S` = the quote less the growth;
/// the curve is 166 bytes after it, the borrower got `ert` back, the trader is clean.
#[test]
fn a_sale_pays_a_short_curves_growth_out_of_its_proceeds() {
    let (mut env, pm, _) = setup(PumpCoin::T22_115);
    assert_eq!(pm.curve.data_len(&env.svm), 115);
    let grows_to = fork::PumpDeploy::CURRENT.curve_len();
    let min115 = env.svm.minimum_balance_for_rent_exemption(115);
    let min_new = env.svm.minimum_balance_for_rent_exemption(grows_to);
    let growth = min_new - min115;
    assert_eq!(growth, 259_080);
    let rq = pm.curve.state(&env.svm).real_quote_reserves;
    let mut acc = env.svm.get_account(&pm.curve.curve).unwrap();
    acc.lamports = rq + min115;
    env.svm.set_account(pm.curve.curve, acc).unwrap();
    let vt = pm.curve.state(&env.svm).virtual_token_reserves;
    // Large enough that the growth fits the 10 bps tolerance on S_exp.
    let q = vt / 100;
    let sale = pm.curve.quote_sell(&env.svm, q).unwrap();
    let tol = u64::from(env.config().params.exec_tol_bps);
    assert!(
        sale.proceeds * tol / 10_000 >= growth,
        "S_exp {}",
        sale.proceeds
    );
    let mut b = env.borrower();
    let (open, _, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(open.s, sale.proceeds - growth, "S = proceeds − the growth");
    let acc = env.svm.get_account(&pm.curve.curve).unwrap();
    assert_eq!(acc.data.len(), grows_to);
    // v3 keeps the sale's protocol and creator fees on the curve, in the buckets outside `rQ`.
    let (creator_fee, protocol_fees) = fork::curve_fee_buckets(&acc.data);
    assert_eq!(
        acc.lamports - pm.curve.state(&env.svm).real_quote_reserves - creator_fee - protocol_fees,
        min_new
    );
    assert_trader_clean(&env, &pm);
}
