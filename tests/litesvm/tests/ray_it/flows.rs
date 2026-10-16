//! `open_short` → `close_short` on the real AMM v4 and CPMM programs (ARCH §2.4.1 Raydium row,
//! §3.4, §3.5): every lamport and token accounted for, on SPL Token and Token-2022 bases, WSOL
//! on either side, and every fee variant the venues price (AMM v4 fee and `need_take_pnl`;
//! CPMM configs, the creator fee off, on the SOL side, on the token side and on both).
//!
//! Each round trip asserts: `S` = `quote_sell(q)` on the pre-trade state exactly (ε unused);
//! the lending vault loses exactly `q` and keeps no delegate; the venue's token vault gains `q`
//! and its SOL vault loses exactly `S`; `trader` and `trader_wsol` hold nothing; the borrower
//! pays the position's and the exposure record's rent, `M` and the open fee (the WSOL account's
//! rent comes back); `N_open` = `B̂` on the post-open state. At close: the venue charges exactly
//! `B̂_live(q)` on the pre-trade state (`max_in` passed as the quote bound), the lending vault
//! receives exactly `q` straight from the venue, the venue's SOL vault gains exactly `B`, the
//! waterfall pays `B + R` and the borrower the rest.

use shortwin::state::{ShortPosition, WalletExposure};
use shortwin_litesvm_tests::raydium::{lp_mint, set_mint_supply};
use shortwin_math::fees::Direction;

use crate::common::*;

/// Balances around one instruction.
#[derive(Clone, Copy, Debug)]
struct Snap {
    pool_vault: u64,
    venue_tok: u64,
    venue_sol: u64,
    borrower: u64,
    market_vault: u64,
}

fn snap(env: &Env, rm: &RayMarket, borrower: &Pubkey) -> Snap {
    let (tok, sol) = rm.vaults();
    Snap {
        pool_vault: tokens(env, &rm.keys.pool_vault),
        venue_tok: tokens(env, &tok),
        venue_sol: tokens(env, &sol),
        borrower: lamports(env, borrower),
        market_vault: lamports(env, &rm.keys.market_vault),
    }
}

fn assert_trader_clean(env: &Env, rm: &RayMarket) {
    assert_eq!(lamports(env, &rm.keys.trader), 0, "trader holds nothing");
    let wsol = read::account(&env.svm, &rm.trader_wsol);
    assert!(
        wsol.is_none_or(|a| a.lamports == 0 && a.data.is_empty()),
        "trader_wsol closed"
    );
    assert_eq!(
        read::token_delegate(&env.svm, &rm.keys.pool_vault),
        None,
        "pool_vault keeps no delegate"
    );
}

/// One exact open → close round trip of `q` on `rm` (module docs). Returns `(S, B)`.
pub fn round_trip(env: &mut Env, rm: &RayMarket, q: u64) -> (u64, u64) {
    let mut b = env.borrower();
    let s0 = snap(env, rm, &b.key());
    let s_exp = rm.quote_sell(&env.svm, q).expect("sale quote").proceeds;
    let first_exposure =
        read::account(&env.svm, &pda::exposure(&rm.market(), &b.key()).0).is_none();
    let (open, nonce, _) = open_ok(env, rm, &mut b, q);
    let s1 = snap(env, rm, &b.key());
    assert_eq!(open.s, s_exp, "S = the sale quote on the pre-trade state");
    assert_eq!(
        s0.pool_vault - s1.pool_vault,
        q,
        "the lending vault spent exactly q"
    );
    assert_eq!(s1.venue_tok - s0.venue_tok, q, "the venue received q");
    assert_eq!(
        s0.venue_sol - s1.venue_sol,
        open.s,
        "the venue paid exactly S"
    );
    assert_trader_clean(env, rm);
    let n_open = rm.quote_buy(&env.svm, q).expect("post-open buy quote").cost;
    assert_eq!(open.n_open, n_open, "N_open = B̂ on the post-open state");
    let pos_key = pda::position(&rm.market(), &b.key(), nonce).0;
    let pos_rent = env
        .svm
        .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
    assert_eq!(lamports(env, &pos_key), pos_rent + open.v);
    let exp_rent = if first_exposure {
        env.svm
            .minimum_balance_for_rent_exemption(WalletExposure::SPACE)
    } else {
        0
    };
    assert_eq!(
        s0.borrower - s1.borrower,
        pos_rent + exp_rent + open.m + open.fee_open,
        "the borrower paid the accounts' rent, M and the open fee (the WSOL rent came back)"
    );
    assert_eq!(s1.market_vault - s0.market_vault, open.fee_open);

    env.warp(1);
    env.crank(rm);
    env.warp(1);
    let s2 = snap(env, rm, &b.key());
    let pos_lamports = lamports(env, &pos_key);
    let b_exp = rm.quote_buy(&env.svm, q).expect("buy quote").cost;
    let (close, _) = close_ok(env, rm, &b, nonce);
    let s3 = snap(env, rm, &b.key());
    assert_eq!(
        (close.buyback_cost, close.b_live),
        (b_exp, b_exp),
        "B = B̂_live(q)"
    );
    assert_eq!(close.token_surplus, 0);
    assert_eq!(
        s3.pool_vault - s2.pool_vault,
        q,
        "the venue delivered q into the lending vault"
    );
    assert_eq!(s2.venue_tok - s3.venue_tok, q);
    assert_eq!(
        s3.venue_sol - s2.venue_sol,
        close.buyback_cost,
        "the venue took exactly B"
    );
    assert_trader_clean(env, rm);
    assert!(
        read::account(&env.svm, &pos_key).is_none(),
        "the position closed"
    );
    assert_eq!(
        s3.market_vault - s2.market_vault,
        close.rent,
        "the rent went to the vault"
    );
    assert_eq!(
        s3.borrower - s2.borrower,
        pos_lamports - close.buyback_cost - close.rent + exp_rent,
        "the borrower got the rest (and the exposure record's rent)"
    );
    assert_eq!(
        close.borrower_payout + close.buyback_cost + close.rent,
        open.v
    );
    (open.s, close.buyback_cost)
}

#[test]
fn amm_v4_spl() {
    let (mut env, rm, q) = setup(FART);
    round_trip(&mut env, &rm, q);
    round_trip(&mut env, &rm, q / 3);
}

#[test]
fn cpmm_spl() {
    let (mut env, rm, q) = setup(USELESS);
    round_trip(&mut env, &rm, q);
    round_trip(&mut env, &rm, q / 4);
}

#[test]
fn cpmm_token_2022() {
    let (mut env, rm, q) = setup(T22);
    assert_eq!(rm.program(), TokenProgram::Token2022);
    round_trip(&mut env, &rm, q);
}

/// Creator fee on WSOL as token 0 (`FQd8z3…`, `creator_fee_on = 1`): the sale pays it on its
/// SOL output, the buyback on its SOL input.
#[test]
fn cpmm_creator_fee_on_the_sol_side() {
    let (mut env, rm, q) = setup(CREATOR_FEE);
    let p = rm.cpmm();
    let st = p.state(&env.svm);
    assert!(st.enable_creator_fee && st.creator_fee_on == 1 && p.sol_is_token0);
    assert!(!p.fees(&env.svm, Direction::Sell).unwrap().creator_on_input);
    assert!(p.fees(&env.svm, Direction::Buy).unwrap().creator_on_input);
    round_trip(&mut env, &rm, q);
}

/// Creator fee on both tokens (`FrZnJAFt…`): always on the input.
#[test]
fn cpmm_creator_fee_on_both() {
    let (mut env, rm, q) = setup(CREATOR_BOTH);
    let p = rm.cpmm();
    assert!(p.fees(&env.svm, Direction::Sell).unwrap().creator_on_input);
    assert!(p.fees(&env.svm, Direction::Buy).unwrap().creator_on_input);
    round_trip(&mut env, &rm, q);
}

/// WSOL as token 1 (`3Cv9uPpa…`, config 10, creator fee on token 1 = WSOL) once its LP is
/// burned (it is not eligible as dumped): the CPMM adapter names its vaults by side.
#[test]
fn cpmm_wsol_as_token_1() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(WSOL_T1);
    set_mint_supply(&mut env.svm, &lp_mint(&pool), 0);
    ok(env.list_ray(&pool, WSOL_T1.hint()));
    let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let q = ((u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y))) as u64;
    let rm = env.ray_market_listed(RaySpec::new(WSOL_T1, q * 40), pool);
    env.crank(&rm);
    assert!(!rm.cpmm().sol_is_token0);
    round_trip(&mut env, &rm, q);
}

/// U11 (R-RAY §3.4: exact out with the creator fee on the **output**, source only): the
/// creator fee moved to the token side of two pools (a scenario builder: no mainnet pool charges
/// it there). The buyback grosses `q` up (`q' = pre(q, c)`), yet the lending vault receives
/// exactly `q` and the venue charges exactly the quote; the sale pays the fee on its token
/// input.
#[test]
fn cpmm_creator_fee_on_the_token_side() {
    for (fixture, on, burn) in [(CREATOR_FEE, 2u8, false), (WSOL_T1, 1u8, true)] {
        let mut env = Env::ray_fork();
        let pool = env.load_ray_pool(fixture);
        let p = match &pool {
            AnyPool::Cpmm(p) => *p,
            AnyPool::V4(_) => unreachable!(),
        };
        fork::set_cpmm_creator_fee(&mut env.svm, &p, true, on);
        if burn {
            set_mint_supply(&mut env.svm, &p.lp_mint, 0);
        }
        assert!(!p.fees(&env.svm, Direction::Buy).unwrap().creator_on_input);
        assert!(p.fees(&env.svm, Direction::Sell).unwrap().creator_on_input);
        ok(env.list_ray(&pool, fixture.hint()));
        let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        let q = ((u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y))) as u64;
        let rm = env.ray_market_listed(RaySpec::new(fixture, q * 40), pool);
        env.crank(&rm);
        round_trip(&mut env, &rm, q);
    }
}

/// Every live `AmmConfig` whose one-way fee fits `venue_fee_cap_bps` (150 bps), with the
/// creator fee off and on (on WSOL as token 0), on USELESS re-pointed at it before listing.
#[test]
fn cpmm_every_eligible_config() {
    let cap_ppm = 15_000u64;
    let mut ran = 0;
    for index in 0..fork::CPMM_CONFIGS {
        for creator in [false, true] {
            let mut env = Env::ray_fork();
            let pool = env.load_ray_pool(USELESS);
            let p = match &pool {
                AnyPool::Cpmm(p) => *p,
                AnyPool::V4(_) => unreachable!(),
            };
            let cfg = fork::cpmm_config(index);
            fork::set_cpmm_pool_config(&mut env.svm, &p, &cfg);
            fork::set_cpmm_creator_fee(&mut env.svm, &p, creator, 1);
            let c = p.config(&env.svm);
            let phi = c.trade_fee_rate + if creator { c.creator_fee_rate } else { 0 };
            let res = env.list_ray(&pool, USELESS.hint());
            if phi > cap_ppm {
                assert_err(&res, E::PoolIneligible);
                continue;
            }
            ok(res);
            let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
            let q =
                ((u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y))) as u64;
            let rm = env.ray_market_listed(RaySpec::new(USELESS, q * 40), pool);
            env.crank(&rm);
            round_trip(&mut env, &rm, q / 2);
            ran += 1;
        }
    }
    assert!(ran >= 20, "{ran} configs ran");
}

/// AMM v4 with WSOL as the **pc** and a non-zero `need_take_pnl` on both sides (the reversed
/// Fartcoin pool `9GWMrDgH…`, made eligible: vaults deepened, LP burned): reserves exclude the
/// PnL counters on the side the market recorded.
#[test]
fn amm_v4_wsol_as_pc_with_need_take_pnl() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(FART_REV);
    let p = match &pool {
        AnyPool::V4(p) => *p,
        AnyPool::Cpmm(_) => unreachable!(),
    };
    assert!(!p.sol_is_coin);
    let info = p.info(&env.svm);
    assert!(info.need_take_pnl_coin > 0 && info.need_take_pnl_pc > 0);
    // 60 SOL and 38,000 FART of pricing reserves (about Fartcoin's own price).
    fork::set_token_amount(
        &mut env.svm,
        &p.pc_vault,
        60_000_000_000 + info.need_take_pnl_pc,
    );
    fork::set_token_amount(
        &mut env.svm,
        &p.coin_vault,
        38_000_000_000 + info.need_take_pnl_coin,
    );
    set_mint_supply(&mut env.svm, &p.lp_mint, 0);
    ok(env.list_ray(&pool, FART_REV.hint()));
    let rm = env.ray_market_listed(RaySpec::new(FART_REV, 4_000_000_000), pool);
    env.crank(&rm);
    let (x, y) = rm.reserves(&env.svm).unwrap();
    assert_eq!((x, y), (38_000_000_000, 60_000_000_000));
    round_trip(&mut env, &rm, 90_000_000);
    // The same pool with the PnL counters raised after listing: the next trip prices on the
    // smaller reserves.
    fork::set_v4_need_take_pnl(
        &mut env.svm,
        &p,
        info.need_take_pnl_coin + 1_000,
        info.need_take_pnl_pc + 500_000_000,
    );
    round_trip(&mut env, &rm, 90_000_000);
}

/// AMM v4 fee variants after listing (the admin sets them per pool; ≤ the cap, so opens still
/// run): the quotes follow the live `num/den`, and the fee-scaled size cap `3·φ·X₀` with it
/// (a zero fee admits no position at all).
#[test]
fn amm_v4_fee_variants() {
    for (num, den) in [
        (25, 10_000),
        (1, 10_000),
        (3, 1_000),
        (150, 10_000),
        (7, 4_667),
        (0, 10_000),
    ] {
        let (mut env, rm, q) = setup(FART);
        fork::set_v4_swap_fee(&mut env.svm, rm.v4(), num, den);
        env.warp(1);
        env.crank(&rm);
        let phi = (u128::from(num) * 1_000_000).div_ceil(u128::from(den));
        let (x, _) = rm.reserves(&env.svm).unwrap();
        let fee_cap = (3 * phi * u128::from(x) / 1_000_000) as u64;
        if fee_cap == 0 {
            let mut b = env.borrower();
            assert_err(&env.open(&rm, &mut b, q).0, E::CapPositionImpact);
            continue;
        }
        round_trip(&mut env, &rm, (q / 2).min(fee_cap / 10 * 9));
    }
}
