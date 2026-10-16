//! Shared machinery of the Fartcoin AMM v4 fork E2E (M3 exit criterion "fork E2E on Fartcoin
//! (AMM v4) … through open → liquidation → S2/S3"; ARCH §8.2, §8.4, §9; ECON §12): the release
//! short.win on the **dumped** AMM v4 program and Fartcoin's main pool `Bzc9NZfM…`
//! (`shortwin_litesvm_tests::{fork, raydium}`), driven by real instructions only — short.win's
//! and the venue's own swaps (`swap_base_in_v2` / `swap_base_out_v2`) and admin instructions
//! (`SetParams`, the admin's signature forged) — with the invariant hooks after every
//! instruction (`Env::send`).
//!
//! `tests/raydium_v4_e2e.rs` and `tests/raydium_v4_attacks.rs` include this file with `#[path]`;
//! compiled on its own it is an empty test binary (it has no tests of its own).
//!
//! **CPMM (M10).** The keeper-exit oracle ([`view`], [`predict`], [`exit_checked`]) is
//! venue-generic: it prices with the hooks' venue view (`Pricing::Cpmm` with each direction's fees
//! and creator-fee side on a CPMM pool), reads the route flag of the market's own venue bit, and
//! falls back to the program's fee view of a pool that no longer parses (AMM v4: the pool's fee,
//! else the cap; CPMM: the cap as a trade fee) — so `tests/raydium_cpmm_exits_it.rs` and the
//! fuzzer run it on CPMM markets as well. The scenario builders (`Fart`, LP removal, `SetParams`)
//! stay AMM v4's.
//!
//! **The differential oracle.** Every short.win instruction the E2E sends goes through a
//! `*_checked` function that recomputes what the program must do **from `shortwin-math` and the
//! venue-state readers alone** (the hooks' `venue_view` for the pool; never the program's
//! adapter or engine glue), then sends the instruction and asserts the program did exactly that:
//!
//! | Function | Recomputed and asserted |
//! |---|---|
//! | [`list_checked`] | `MarketCreated`; the adopted pool cache and SOL side, `pool_d = locked_depth`, `pool_seen_at = graduated_at = created_at = now` (O13: AMM v4 age from listing), Tier C, the observation state `Observation::new(now, P, L)` on the reader's reserves, `graduated_elsewhere` (Fartcoin's real curve) |
//! | [`deposit_checked`] | shares `= pool::shares_for_tokens`, `T`, `S`, the lending vault's tokens |
//! | [`crank_checked`] | the whole prologue: `rent::accrue` + `rent::lender_credit` + `pool::accrue`, `Observation::roll` + `record` (only while executable), both route chains (`settle::chain_step` of `s3_no_route` / `pool_fail`), `pool_d`, the `PriceCranked` event |
//! | [`open_checked`] | `S = quote_sell(q)` on the pre-trade state, `N_open` on the post-trade state, `B̂_ema` on the newest ring entry, the D4 margin, the open fee, `r_min`, the position's figures and locked terms, `pool::borrow`, every lamport and token (borrower, position, `market_vault`, lending vault, both venue vaults) |
//! | [`close_checked`] | `B̂_live`, `R_due`, `waterfall::close`, `pool::repay`, every lamport and token |
//! | [`exit_checked`] | `liquidate` / `expire`: every decision of ARCH §3.6 / §3.7 in order (the refusal code), the references and bounds (`bounds::*`), `waterfall::{liquidate, expire}`, `pool::repay`, every event field, lamport and token (optionally inside an attacker's bundle) |
//! | [`settle_checked`] | `sol_settle`: the settlement box, the legs and `settle::evaluate` (arming clock, S1 / S2 / S3, the box-consistent amount), `waterfall::settle`, `pool::settle`, every event field and lamport |
//!
//! The oracle reads the state an instruction decides on as the market after a **same-second
//! `crank_price` on a clone of the VM** (the instruction's own prologue — accrual, roll,
//! record, chains — is exactly a crank's), as `tests/liquidate_support.rs` does on the mock.
//!
//! **LP removal.** The fixture set holds the pool, its vaults and LP mint but not the pool's
//! `target_orders` / `open_orders` accounts that AMM v4's `withdraw` (tag 4) needs, so the dumped
//! ELF cannot run an LP withdrawal here; [`remove_liquidity`] writes its effect on the accounts
//! short.win reads instead (a scenario builder, ARCH §8.2 "LP burns and unlocked deposits"): the
//! LP burned from the holder and from the mint's supply, the pool's `lp_amount` reduced, each
//! vault paying the holder its pro-rata share `⌊lp·reserve/lp_amount⌋` of the reader's reserves.
//! The venue's PnL counters are left alone (its `calc_take_pnl` update reads `target_orders`);
//! they are 0 on this pool and the pricing reserves exclude them anyway.

#![allow(
    dead_code,
    unused_imports,
    clippy::result_large_err,
    clippy::too_many_arguments
)]

pub use anchor_lang::prelude::Pubkey;
pub use anchor_lang::solana_program::instruction::Instruction;
pub use litesvm::types::TransactionMetadata;
pub use shortwin::errors::ShortwinError;
pub use shortwin::events::{
    Close, Deposit, Expire, Liquidate, MarketCreated, Open, PriceCranked, SettleArmed,
    SettleDisarmed, SolSettle, TraderSwept,
};
pub use shortwin::state::{InitConfigArgs, Market, ShortPosition, VenueKind, WalletExposure};
pub use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, expire_ix, liquidate_ix, open_short_ix,
    sol_settle_ix,
};
pub use shortwin_litesvm_tests::errors::{assert_err, ok};
pub use shortwin_litesvm_tests::fork::{self, AnyPool, Trader, V4Fixture, V4Pool};
pub use shortwin_litesvm_tests::invariants::venue_view::{self, VenueView};
pub use shortwin_litesvm_tests::raydium::{self, RayFixture, RayMarket, RaySpec};
pub use shortwin_litesvm_tests::tokens::TokenProgram;
pub use shortwin_litesvm_tests::{Env, TxResult, events, ix, pda, read};
pub use shortwin_math::num::WAD;
pub use solana_keypair::Keypair;
pub use solana_signer::Signer;

use shortwin::constants::{TOKEN_PROGRAM_ID, VENUE_BIT_RAYDIUM_V4};
use shortwin::state::{
    MARKET_FLAG_GRADUATED_ELSEWHERE, MARKET_FLAG_ROUTE_BROKEN, MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN,
    raydium_cache,
};
use shortwin::venues::ExtrasSel;
use shortwin_math::MathError;
use shortwin_math::bounds;
use shortwin_math::fees::{self, Direction, RayV4Fee};
use shortwin_math::num::{Rounding, mul_div, sqrt_u128};
use shortwin_math::observe::{self, Observation};
use shortwin_math::persist;
use shortwin_math::pool::{self, Pool, Resolution, SweepSource, Vault};
use shortwin_math::pool_select;
use shortwin_math::quote::{self, BuyQuote, Pricing, reprice};
use shortwin_math::rent;
use shortwin_math::risk::{self, Budget};
use shortwin_math::settle::{
    self, ArmChange, ArmState, BoxQuotes, DisarmReason, Evaluation, S3Chain, SettleInputs,
    SettleTerms,
};
use shortwin_math::tier::{self, OraclePrice, Tier, TierInputs, TierVenue};
use shortwin_math::waterfall::{self, ExitInputs, Payout};
use shortwin_venue_state::{raydium_cpmm, raydium_v4};

/// short.win's error codes.
pub type E = ShortwinError;
/// `WAD`.
pub const W: u128 = WAD;
/// Fartcoin's main AMM v4 pool `Bzc9NZfM…` (WSOL is the coin; 25/10,000; λ 99.78 %).
pub const FART: RayFixture = RayFixture::V4(V4Fixture::Fartcoin);
/// A lamport-denominated SOL.
pub const SOL: u64 = 1_000_000_000;

// ---- environments ----------------------------------------------------------------------------

/// The release short.win initialized with `f` applied to the harness's default arguments
/// (ECON §15 defaults), both Raydium programs deployed from their dumped ProgramData, the clock
/// at the fixtures' snapshot.
pub fn fork_env_with(f: impl FnOnce(&mut InitConfigArgs)) -> Env {
    let mut env = Env::new();
    let mut args = env.default_init_args();
    f(&mut args);
    ok(env.initialize_config(args));
    env.load_raydium_programs();
    env.set_fork_clock();
    env
}

/// [`fork_env_with`] at the defaults.
pub fn fork_env() -> Env {
    fork_env_with(|_| {})
}

/// The config's per-market sizing limits lifted within their compiled bounds
/// (`market_max_notional` 10¹³, `wallet_cap_bps` 10⁴): what makes positions at the SPEC size
/// caps possible on a 38,000 SOL pool (the beta notional cap of 25 SOL binds first otherwise).
pub fn lift_notional(a: &mut InitConfigArgs) {
    a.params.market_max_notional = 10_000_000_000_000;
    a.params.wallet_cap_bps = 10_000;
}

// ---- small readers ---------------------------------------------------------------------------

/// Lamports of `key`.
pub fn lamports(env: &Env, key: &Pubkey) -> u64 {
    read::lamports(&env.svm, key)
}

/// Token amount of `key` (0 if not a token account).
pub fn tokens(env: &Env, key: &Pubkey) -> u64 {
    read::token_amount(&env.svm, key).unwrap_or(0)
}

/// The AMM v4 pool of `rm`.
pub fn v4(rm: &RayMarket) -> &V4Pool {
    rm.v4()
}

/// The pool's pricing reserves `(X, Y)` (token, SOL) from the reader (`None` if it cannot
/// price).
pub fn reserves(env: &Env, rm: &RayMarket) -> Option<(u64, u64)> {
    rm.reserves(&env.svm)
}

/// The pool's spot `P = ⌊Y·WAD/X⌋`.
pub fn spot(env: &Env, rm: &RayMarket) -> u128 {
    let (x, y) = reserves(env, rm).expect("reserves");
    risk::price_wad(u128::from(x), u128::from(y)).expect("price")
}

/// `q` tokens worth about `sol` lamports at the spot.
pub fn q_for_sol(env: &Env, rm: &RayMarket, sol: u64) -> u64 {
    let (x, y) = reserves(env, rm).expect("reserves");
    (u128::from(sol) * u128::from(x) / u128::from(y)) as u64
}

/// The fee the pool holds (`swap_fee_numerator/denominator`), whether or not it can execute;
/// the cap `cap_bps/10⁴` if the pool no longer parses (A63: the fee view of a layout change).
pub fn pool_fee(env: &Env, rm: &RayMarket, cap_bps: u64) -> RayV4Fee {
    env.svm
        .get_account(&v4(rm).pool)
        .and_then(|a| raydium_v4::read(&a.owner.to_bytes(), &a.data).ok())
        .and_then(|i| fees::ray_v4(i.swap_fee_numerator, i.swap_fee_denominator).ok())
        .unwrap_or(RayV4Fee {
            num: cap_bps,
            den: 10_000,
        })
}

/// The pool's locked depth `D` on its live state (`pool_select::locked_depth` on the reader's
/// reserves, the pool's `lp_amount` and the LP mint's supply).
pub fn locked_depth(env: &Env, rm: &RayMarket) -> u128 {
    raydium::locked_depth(&env.svm, &rm.pool)
}

/// The config's hard cap and the market's / venue's route-broken flags → `s3_no_route` and
/// `pool_fail` of `venue` (ARCH §5.3; the **global** cap, R3A-4 c). The venue flag is the bit of
/// the market's venue (AMM v4 or CPMM).
fn route_predicates(env: &Env, m: &Market, venue: &VenueView) -> (bool, bool) {
    let config = env.config();
    let bit = m
        .venue_kind()
        .map_or(VENUE_BIT_RAYDIUM_V4, |k| k.venue_bit());
    let flag = m.has_flag(MARKET_FLAG_ROUTE_BROKEN) || config.venue_route_broken & bit != 0;
    let cap = u64::from(config.params.venue_fee_hard_cap_bps);
    (
        settle::s3_no_route(venue.executable, flag, venue.fee_ppm, cap),
        settle::pool_fail(venue.executable, venue.fee_ppm, cap),
    )
}

// ---- market participants and price moves ----------------------------------------------------

/// A participant trading the pool directly (whale, attacker, arbitrageur): a WSOL account
/// holding `sol` and a token account holding `tokens` (planted: the mint has no authority).
pub fn actor(env: &mut Env, rm: &RayMarket, sol: u64, tokens: u64) -> Trader {
    let (mint, program) = rm.pool.token();
    fork::trader(env, &mint, program, sol, tokens)
}

/// AMM v4 `swap_base_out_v2` by `who`: buy exactly `tokens` with its WSOL (no price bound).
pub fn buy_ix(rm: &RayMarket, who: &Trader, tokens: u64) -> Instruction {
    v4(rm).swap_base_out_v2_ix(&who.owner.pubkey(), &who.wsol, &who.token, u64::MAX, tokens)
}

/// AMM v4 `swap_base_in_v2` by `who`: sell exactly `tokens` for WSOL (no bound).
pub fn sell_ix(rm: &RayMarket, who: &Trader, tokens: u64) -> Instruction {
    v4(rm).swap_base_in_v2_ix(&who.owner.pubkey(), &who.token, &who.wsol, tokens, 0)
}

/// Tokens to buy (> 0) or sell (< 0) to bring the pool's spot to `target` along its constant
/// product (the attack scripts' `move_size`): `X − isqrt(X·Y·WAD/target)`.
pub fn move_size(env: &Env, rm: &RayMarket, target: u128) -> i128 {
    let (x, y) = reserves(env, rm).expect("reserves");
    let k = u128::from(x) * u128::from(y);
    let tx = sqrt_u128(
        mul_div(k, W, target.max(1), Rounding::Floor).expect("k/P"),
        Rounding::Floor,
    );
    i128::from(x) - i128::try_from(tx).expect("X")
}

/// The one swap by `who` that moves the pool's spot to about `target` (a buy exact-out or a
/// sale exact-in of [`move_size`] tokens), or `None` if it is already there or the sale would
/// pay nothing.
pub fn move_ix(env: &Env, rm: &RayMarket, who: &Trader, target: u128) -> Option<Instruction> {
    match move_size(env, rm, target) {
        0 => None,
        d if d > 0 => Some(buy_ix(rm, who, u64::try_from(d).expect("d"))),
        d => {
            let t = u64::try_from(-d).expect("d");
            let out = rm.quote_sell(&env.svm, t)?;
            (out.proceeds > 0).then(|| sell_ix(rm, who, t))
        }
    }
}

/// Other traders move the pool to about `target` with one real swap by `who` (the sims'
/// `exo_move`).
pub fn move_to(env: &mut Env, rm: &RayMarket, who: &Trader, target: u128) {
    if let Some(ix) = move_ix(env, rm, who, target) {
        let kp = who.owner.insecure_clone();
        ok(env.send(&[ix], &[&kp]));
    }
}

/// `⌊p·m⌋` for a price and a float factor.
pub fn scale(p: u128, m: f64) -> u128 {
    ((p as f64) * m) as u128
}

/// `who`'s WSOL.
pub fn wsol_of(env: &Env, who: &Trader) -> i128 {
    i128::from(tokens(env, &who.wsol))
}

// ---- LP (ARCH §2.6: locked depth; the module docs' byte-level withdraw) ----------------------

/// The pool's internal LP counter (`lp_amount` @720).
pub fn lp_counter(env: &Env, rm: &RayMarket) -> u64 {
    v4(rm).info(&env.svm).lp_amount
}

/// The LP mint's supply.
pub fn lp_supply(env: &Env, rm: &RayMarket) -> u64 {
    raydium::mint_supply(&env.svm, &v4(rm).lp_mint)
}

/// `λ_bps` of the pool now (`pool_select::lambda_bps`).
pub fn lambda_bps(env: &Env, rm: &RayMarket) -> u32 {
    pool_select::lambda_bps(lp_counter(env, rm), lp_supply(env, rm))
}

/// Redistributes the pool's LP before listing so that its locked share is exactly
/// `lambda_bps` and `holder` holds every circulating LP token (a scenario builder: the dumped
/// pool's LP is 99.78 % burned). Returns the holder's LP token account and balance.
pub fn hold_unlocked_lp(
    env: &mut Env,
    pool: &V4Pool,
    holder: &Pubkey,
    lambda_bps: u64,
) -> (Pubkey, u64) {
    let counter = pool.info(&env.svm).lp_amount;
    let supply = raydium::supply_for_lambda(counter, lambda_bps);
    raydium::set_mint_supply(&mut env.svm, &pool.lp_mint, supply);
    let acc = fork::fund_token(env, holder, &pool.lp_mint, TokenProgram::Spl, supply);
    (acc, supply)
}

/// An LP withdrawal of `lp` tokens by the holder of `lp_account`, written byte-level (module
/// docs): each side pays `⌊lp·reserve/lp_amount⌋` of the reader's reserves (`vault −
/// need_take_pnl`) into the holder's accounts, the LP is burned from `lp_account` and the
/// mint's supply, the pool's `lp_amount` falls by `lp`.
/// Runs the invariant hooks on the result (a state write outside `Env::send`). Returns the
/// `(coin, pc)` amounts paid.
pub fn remove_liquidity(
    env: &mut Env,
    rm: &RayMarket,
    holder: &Trader,
    lp_account: &Pubkey,
    lp: u64,
) -> (u64, u64) {
    let p = *v4(rm);
    let info = p.info(&env.svm);
    let (cv, pv) = p.vault_amounts(&env.svm);
    let (rc, rp) = info.reserves(cv, pv).expect("reserves");
    let n = u128::from(info.lp_amount);
    let coin = (u128::from(lp) * u128::from(rc) / n) as u64;
    let pc = (u128::from(lp) * u128::from(rp) / n) as u64;
    fork::set_token_amount(&mut env.svm, &p.coin_vault, cv - coin);
    fork::set_token_amount(&mut env.svm, &p.pc_vault, pv - pc);
    let (to_coin, to_pc) = if p.sol_is_coin {
        (holder.wsol, holder.token)
    } else {
        (holder.token, holder.wsol)
    };
    let c0 = tokens(env, &to_coin);
    fork::set_token_amount(&mut env.svm, &to_coin, c0 + coin);
    let p0 = tokens(env, &to_pc);
    fork::set_token_amount(&mut env.svm, &to_pc, p0 + pc);
    let held = tokens(env, lp_account);
    fork::set_token_amount(&mut env.svm, lp_account, held - lp);
    let supply = raydium::mint_supply(&env.svm, &p.lp_mint);
    raydium::set_mint_supply(&mut env.svm, &p.lp_mint, supply - lp);
    fork::patch(&mut env.svm, &p.pool, |d| {
        let off = raydium_v4::OFF_LP_AMOUNT;
        d[off..off + 8].copy_from_slice(&(info.lp_amount - lp).to_le_bytes());
    });
    env.assert_invariants();
    (coin, pc)
}

// ---- listing and lending ---------------------------------------------------------------------

/// `create_market(RaydiumV4)` for `pool`'s token by a new creator, checked (module docs), with
/// Fartcoin's real graduated curve planted at its bonding-curve address (D1 detection runs on
/// it: complete, zeroed reserves, no canonical pool → the listing proceeds and records
/// `graduated_elsewhere`).
pub fn list_checked(env: &mut Env, pool: &AnyPool) -> MarketCreated {
    let AnyPool::V4(p) = pool else {
        panic!("an AMM v4 pool")
    };
    let mint = p.token_mint();
    raydium::plant_curve(&mut env.svm, &mint, raydium::fartcoin_curve_bytes());
    let now = env.now();
    let (x, y) = pool
        .io_reserves(&env.svm, Direction::Sell)
        .expect("reserves");
    let (xu, yu) = (u128::from(x), u128::from(y));
    let price = risk::price_wad(xu, yu).unwrap();
    let liq = quote::liquidity(xu, yu).unwrap();
    let info = p.info(&env.svm);
    let d = pool_select::locked_depth(
        info.lp_amount,
        raydium::mint_supply(&env.svm, &p.lp_mint),
        x,
        y,
    )
    .unwrap();
    let creator = env.funded_keypair();
    let fee = env.config().params.market_creation_fee;
    let ix = raydium::create_ray_market_ix(&env.svm, &creator.pubkey(), pool, FART.hint());
    let meta = ok(env.send(&[ix], &[&creator]));
    let ev: MarketCreated = events::one(&meta);
    let market = pda::market(&mint).0;
    assert_eq!(ev.market, market);
    assert_eq!(ev.mint, mint);
    assert_eq!(ev.token_program, TOKEN_PROGRAM_ID);
    assert_eq!(ev.venue, VenueKind::RaydiumV4 as u8);
    assert_eq!(ev.ray_pool, p.pool, "the adopted pool");
    assert_eq!(ev.bonding_curve, pda::bonding_curve(&mint));
    assert_eq!(ev.canonical_pool, pda::canonical_pool(&mint));
    assert_eq!(ev.creation_fee, fee);
    assert_eq!(ev.tier, Tier::C as u8, "a new listing is Tier C (O13)");
    let m = env.market(&market);
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumV4));
    assert_eq!(m.has_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN), p.sol_is_coin);
    assert!(
        m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE),
        "Fartcoin graduated elsewhere"
    );
    let (sol_vault, tok_vault) = if p.sol_is_coin {
        (p.coin_vault, p.pc_vault)
    } else {
        (p.pc_vault, p.coin_vault)
    };
    assert_eq!(m.pool_cache[raydium_cache::RAY_POOL], p.pool);
    assert_eq!(m.pool_cache[raydium_cache::RAY_VAULT_SOL], sol_vault);
    assert_eq!(m.pool_cache[raydium_cache::RAY_VAULT_TOK], tok_vault);
    assert_eq!(m.pool_cache[raydium_cache::RAY_LP_MINT], p.lp_mint);
    assert_eq!(m.pool_d.get(), d, "pool_d = the listing's locked depth");
    assert_eq!(
        (m.pool_seen_at, m.graduated_at, m.created_at),
        (now, now, now),
        "O13: an AMM v4 pool's age starts at listing"
    );
    assert_eq!(m.tier, Tier::C as u8);
    let cap = pool_select::depth_at_lambda(d, u64::from(env.config().params.pool_lock_min_bps));
    assert_eq!(
        *m.observation(),
        Observation::new_raydium(now, price, liq, cap, d).unwrap(),
        "the observation state starts at the pool's spot and min(√(X·Y), D/λ_min), L_slow at most \
         D (AIR-13), marked for the post-change warm-up (M4R)"
    );
    ev
}

/// The lender keys of `owner` with token account `account` on `rm`.
pub fn lender_keys(rm: &RayMarket, owner: &Pubkey, account: &Pubkey) -> ix::LenderKeys {
    ix::LenderKeys {
        owner: *owner,
        mint: rm.keys.mint,
        token_program: rm.keys.token_program,
        owner_token_account: *account,
    }
}

/// `deposit(amount)` by `owner` from `account`, checked: shares `= pool::shares_for_tokens` on
/// the pool before, `T` and `S` after, the lending vault's tokens.
pub fn deposit_checked(
    env: &mut Env,
    rm: &RayMarket,
    owner: &Keypair,
    account: &Pubkey,
    amount: u64,
) -> Deposit {
    let p0 = env.market(&rm.market()).pool();
    let shares = pool::shares_for_tokens(&p0, amount).unwrap();
    let v0 = tokens(env, &rm.keys.pool_vault);
    let a0 = tokens(env, account);
    let ix = ix::deposit(&lender_keys(rm, &owner.pubkey(), account), amount, 0);
    let meta = ok(env.send(&[ix], &[owner]));
    let ev: Deposit = events::one(&meta);
    assert_eq!(
        (ev.amount, ev.shares),
        (amount, shares),
        "shares = amount·(S + VS)/(T + ΣB_Q + VA)"
    );
    assert_eq!(ev.t_after, p0.tokens_free + amount);
    assert_eq!(ev.s_after, p0.total_shares + shares);
    assert_eq!(tokens(env, &rm.keys.pool_vault) - v0, amount);
    assert_eq!(a0 - tokens(env, account), amount);
    ev
}

// ---- the observation prologue: crank_price ---------------------------------------------------

/// What an instruction's prologue leaves at the clock's second (ARCH §3.0 steps 2, 4–8),
/// recomputed from `shortwin-math` on the market `m0` and the venue as the readers see it.
#[derive(Clone, Debug)]
pub struct Prologue {
    pub now: i64,
    pub pool: Pool,
    pub basis: rent::Basis,
    pub obs: Observation,
    pub chains: (i64, i64, i64, i64),
    pub pool_d: u128,
    pub venue: VenueView,
    pub s3_no_route: bool,
    pub spot: Option<u128>,
}

/// [`Prologue`] of the next instruction on `rm` at the current clock.
pub fn prologue(env: &Env, rm: &RayMarket) -> Prologue {
    let m0 = env.market(&rm.market());
    let now = env.now().max(m0.last_accrual_ts);
    let venue = venue_view::view(&env.svm, &m0).expect("a Raydium view");
    let mut pool = m0.pool();
    let mut vault = *env.market_vault(&rm.market()).vault();
    let dt = u64::try_from(now - m0.last_accrual_ts).unwrap_or(0);
    let util = rent::utilization_bps(pool.tokens_free, pool.borrowed_q).unwrap();
    let mut basis = m0.basis();
    let delta = rent::accrue(&mut basis, m0.ema_price.get(), util, m0.util_kink_bps, dt).unwrap();
    // AIR-03: the credit with the market's carried remainder.
    let (credit, _) =
        rent::lender_credit_carried(&m0.aggr(), &delta, m0.lender_credit_rem.get()).unwrap();
    pool::accrue(&mut pool, &mut vault, now, credit).unwrap();
    let mut obs = *m0.observation();
    obs.roll(now, &m0.ema_params()).unwrap();
    let mut spot = None;
    let mut pool_d = m0.pool_d.get();
    if venue.executable {
        let (x, y) = venue.reserves.expect("reserves");
        let (xu, yu) = (u128::from(x), u128::from(y));
        let p = risk::price_wad(xu, yu).unwrap();
        obs.record(now, p, quote::liquidity(xu, yu).unwrap())
            .unwrap();
        spot = Some(p);
        pool_d = pool_select::locked_depth(lp_counter(env, rm), lp_supply(env, rm), x, y).unwrap();
    }
    let (s3, pf) = route_predicates(env, &m0, &venue);
    let (nrs, nrl) = settle::chain_step(m0.no_route_since, m0.last_no_route_obs, now, s3);
    let (pfs, pfl) = settle::chain_step(m0.pool_fail_since, m0.last_pool_fail_obs, now, pf);
    Prologue {
        now,
        pool,
        basis,
        obs,
        chains: (nrs, nrl, pfs, pfl),
        pool_d,
        venue,
        s3_no_route: s3,
        spot,
    }
}

/// Asserts the market's prologue fields equal `pr`'s.
pub fn assert_prologue(env: &Env, rm: &RayMarket, pr: &Prologue, what: &str) {
    let m = env.market(&rm.market());
    assert_eq!(m.last_accrual_ts, pr.now, "{what}: last_accrual_ts");
    assert_eq!(m.basis(), pr.basis, "{what}: rent::accrue");
    assert_eq!(
        *m.observation(),
        pr.obs,
        "{what}: Observation::roll + record"
    );
    assert_eq!(
        (
            m.no_route_since,
            m.last_no_route_obs,
            m.pool_fail_since,
            m.last_pool_fail_obs
        ),
        pr.chains,
        "{what}: the route chains"
    );
    assert_eq!(m.pool_d.get(), pr.pool_d, "{what}: pool_d");
}

/// The tier ARCH §4.7 assigns on the post-prologue market `m` and the venue `venue` at `now`
/// (`tier::assign`: `R_now`, `R_slow` from `reprice(L_slow, P_ema)`, the locked-depth bound
/// `pool_d`/`λ_min`, the age from `graduated_at`, the cached oracle); `None` when the venue
/// cannot execute (the program then keeps the cached tier).
pub fn expected_tier(env: &Env, m: &Market, venue: &VenueView, now: i64) -> Option<Tier> {
    let (_, y) = venue.reserves?;
    let config = env.config();
    let p = config.params;
    let p_ema = m.ema_price.get();
    let o = env.oracle_cache();
    Some(tier::assign(&TierInputs {
        venue: TierVenue::Raydium {
            locked_depth: m.pool_d.get(),
            lambda_min_bps: p.pool_lock_min_bps,
        },
        r_now: y,
        y_slow: reprice(m.liq_slow.get(), p_ema).map_or(0, |(_, ys)| ys),
        vqr: 0,
        p_ema,
        now,
        graduated_at: m.graduated_at,
        oracle: OraclePrice {
            price: o.price,
            conf: o.conf,
            expo: o.expo,
            publish_time: o.publish_time,
        },
        oracle_stale_s: p.oracle_stale_s,
        theta_a_usd_e6: p.theta_a_usd_e6,
        theta_b_usd_e6: p.theta_b_usd_e6,
    }))
}

/// `crank_price` on `rm`, checked against [`prologue`]: the accrual, the lender pool, the roll
/// and the record, both route chains, `pool_d`, and the `PriceCranked` event.
pub fn crank_checked(env: &mut Env, rm: &RayMarket) -> PriceCranked {
    let pr = prologue(env, rm);
    let meta = ok(env.send(&[crank_price_ix(rm)], &[]));
    assert_prologue(env, rm, &pr, "crank_price");
    let m = env.market(&rm.market());
    assert_eq!(m.pool(), pr.pool, "crank_price: pool::accrue");
    let tier = expected_tier(env, &m, &pr.venue, pr.now).map_or(m.tier, |t| t as u8);
    assert_eq!(m.tier, tier, "crank_price: tier::assign");
    let ev: PriceCranked = events::one(&meta);
    assert_eq!(ev.tier, tier);
    assert_eq!(ev.spot_wad, pr.spot.unwrap_or(0));
    assert_eq!(ev.ema_price, pr.obs.ema.price);
    assert_eq!(ev.ema_liq, pr.obs.ema.liq);
    assert_eq!(ev.liq_slow, pr.obs.ema.liq_slow);
    assert_eq!(ev.ema_count, pr.obs.ema.count);
    assert_eq!((ev.lb_lo, ev.lb_hi), (pr.obs.last.lo, pr.obs.last.hi));
    assert_eq!(ev.rent_basis, pr.basis.j);
    assert_eq!(ev.vest_bal, pr.pool.vest_bal);
    ev
}

/// One keeper second: the clock advances by one, the pool moves to `target` (if any) by a real
/// swap of `who`, and the keeper cranks (checked).
pub fn honest_second(env: &mut Env, rm: &RayMarket, who: &Trader, target: Option<u128>) {
    env.warp(1);
    if let Some(t) = target {
        move_to(env, rm, who, t);
    }
    crank_checked(env, rm);
}

/// `n` keeper seconds with the pool left alone.
pub fn tick_checked(env: &mut Env, rm: &RayMarket, n: u32) {
    for _ in 0..n {
        env.warp(1);
        crank_checked(env, rm);
    }
}

// ---- the market view an instruction decides on -----------------------------------------------

/// The market after a same-second `crank_price` and the venue as the readers see it.
#[derive(Clone)]
pub struct MarketView {
    pub now: i64,
    pub market: Market,
    pub pool: Pool,
    pub vault: Vault,
    pub venue: VenueView,
    /// Execution pricing: the pool's own fee (also when it cannot execute; the fee view).
    pub live: Pricing,
    /// Lamports the trader normalisation sweeps (`trader` + the `trader_wsol` address).
    pub donated: u64,
    pub s3_no_route: bool,
    /// PumpSwap (AIR-10): the one-way fee (ppm) at the stored `P_ema`'s market cap as the exit
    /// reads it before its prologue rolls the EMA, which caps the risk view
    /// (`VenueState::exit_risk_cap_bps`); `None` on the venues without one (Raydium, the mock).
    pub ema_fee_ppm: Option<u32>,
}

/// A position's view: the market's and the position's accounts.
#[derive(Clone)]
pub struct View {
    pub m: MarketView,
    pub pos: ShortPosition,
    pub pos_lamports: u64,
    pub exposure_q_open: u64,
    pub exposure_lamports: u64,
}

impl View {
    pub fn now(&self) -> i64 {
        self.m.now
    }
    pub fn market(&self) -> &Market {
        &self.m.market
    }
    pub fn executable(&self) -> bool {
        self.m.venue.executable
    }
    /// `(X, Y)` as `u128` (0 when the venue cannot execute).
    pub fn xy(&self) -> (u128, u128) {
        self.m
            .venue
            .reserves
            .map_or((0, 0), |(x, y)| (u128::from(x), u128::from(y)))
    }
    /// Risk pricing at the position's locked hard cap — on PumpSwap at most the fee at `P_ema`
    /// (AIR-10, `VenueState::exit_risk_cap_bps`).
    pub fn risky(&self) -> Pricing {
        let hard = u64::from(self.pos.venue_fee_hard_cap_bps);
        let cap = self
            .m
            .ema_fee_ppm
            .map_or(hard, |f| hard.min(u64::from(f).div_ceil(100)));
        self.m.live.capped(cap)
    }
}

/// The [`MarketView`] of the state as it is (valid right after a crank in this second).
pub fn read_market_view(env: &Env, rm: &RayMarket) -> MarketView {
    let market = env.market(&rm.market());
    let venue = venue_view::view(&env.svm, &market).expect("a Raydium view");
    let cap = u64::from(env.config().params.venue_fee_hard_cap_bps);
    let (s3, _) = route_predicates(env, &market, &venue);
    MarketView {
        now: market.last_accrual_ts,
        pool: market.pool(),
        vault: *env.market_vault(&rm.market()).vault(),
        live: venue.pricing.unwrap_or_else(|| fee_view(env, rm, cap)),
        donated: lamports(env, &market.trader) + lamports(env, &market.trader_wsol),
        venue,
        s3_no_route: s3,
        ema_fee_ppm: None,
        market,
    }
}

/// The fee view of a pool that cannot execute (`venues::raydium_v4::fee_view` /
/// `venues::raydium_cpmm::fee_view`) **before their final `.capped(cap_bps)`**: the pool's own
/// fee while it parses — AMM v4's [`pool_fee`]; CPMM's both directions from the cached pool and
/// the config it names (`fees::cpmm`, the creator-fee side included) — else `cap_bps` itself
/// (on CPMM as a trade fee). It is [`MarketView`]'s uncapped `live` pricing, as an executable
/// pool's is: the program caps both at the **position's** locked hard cap
/// (`venues::risk_quoter`), which [`View::risky`] applies; capping here at the config's cap
/// would take `min(config cap, position cap)` where the two differ. One divergence remains:
/// [`read_market_view`] passes the config's cap, so on a pool that no longer parses the
/// cap-as-fee is the config's where the program's is the position's. An exit refuses `NoRoute`
/// before it quotes on such a pool, so no exit decision reads either.
pub fn fee_view(env: &Env, rm: &RayMarket, cap_bps: u64) -> Pricing {
    if matches!(rm.pool, AnyPool::V4(_)) {
        return Pricing::RayV4(pool_fee(env, rm, cap_bps));
    }
    let m = env.market(&rm.market());
    let live = (|| {
        let acc = env
            .svm
            .get_account(&m.pool_cache[raydium_cache::RAY_POOL])?;
        let pool = raydium_cpmm::read_pool(&acc.owner.to_bytes(), &acc.data).ok()?;
        let cfg_key = m.pool_cache[raydium_cache::RAY_AMM_CONFIG];
        if pool.amm_config != cfg_key.to_bytes() {
            return None;
        }
        let acc = env.svm.get_account(&cfg_key)?;
        let cfg = raydium_cpmm::read_config(&acc.owner.to_bytes(), &acc.data).ok()?;
        let resolve = |dir| {
            fees::cpmm(
                &fees::CpmmConfig {
                    trade_fee_rate: cfg.trade_fee_rate,
                    protocol_fee_rate: cfg.protocol_fee_rate,
                    fund_fee_rate: cfg.fund_fee_rate,
                    creator_fee_rate: cfg.creator_fee_rate,
                },
                &fees::CpmmPoolFees {
                    enable_creator_fee: pool.enable_creator_fee,
                    creator_fee_on: pool.creator_fee_on,
                    sol_is_token0: m.has_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN),
                },
                dir,
            )
            .ok()
        };
        Some(Pricing::Cpmm {
            buy: resolve(Direction::Buy)?,
            sell: resolve(Direction::Sell)?,
        })
    })();
    live.unwrap_or_else(|| {
        let cap = fees::CpmmFees {
            trade: cap_bps.saturating_mul(100),
            creator: 0,
            creator_on_input: true,
            protocol: 0,
            fund: 0,
        };
        Pricing::Cpmm {
            buy: cap,
            sell: cap,
        }
    })
}

/// The [`View`] of `borrower`'s position `nonce` as it is.
pub fn read_view(env: &Env, rm: &RayMarket, borrower: &Pubkey, nonce: u64) -> View {
    let m = read_market_view(env, rm);
    let pos_key = pda::position(&rm.market(), borrower, nonce).0;
    let exposure_key = pda::exposure(&rm.market(), borrower).0;
    let exposure: Option<WalletExposure> = read::borsh(&env.svm, &exposure_key);
    View {
        m,
        pos: env.position(rm, borrower, nonce),
        pos_lamports: lamports(env, &pos_key),
        exposure_q_open: exposure.map_or(0, |e| e.q_open),
        exposure_lamports: lamports(env, &exposure_key),
    }
}

/// The [`MarketView`] after `pre` (signed by `signers`) and a same-second crank, on a clone
/// of the VM (restored).
pub fn market_view_after(
    env: &mut Env,
    rm: &RayMarket,
    pre: &[Instruction],
    signers: &[&Keypair],
) -> MarketView {
    let saved = env.svm.clone();
    if !pre.is_empty() {
        ok(env.send(pre, signers));
    }
    ok(env.send(&[crank_price_ix(rm)], &[]));
    let v = read_market_view(env, rm);
    env.svm = saved;
    v
}

/// The [`View`] after `pre` and a same-second crank, on a clone (restored).
pub fn view_after(
    env: &mut Env,
    rm: &RayMarket,
    borrower: &Pubkey,
    nonce: u64,
    pre: &[Instruction],
    signers: &[&Keypair],
) -> View {
    let saved = env.svm.clone();
    if !pre.is_empty() {
        ok(env.send(pre, signers));
    }
    ok(env.send(&[crank_price_ix(rm)], &[]));
    let v = read_view(env, rm, borrower, nonce);
    env.svm = saved;
    v
}

/// The [`View`] now (a same-second crank on a clone).
pub fn view(env: &mut Env, rm: &RayMarket, borrower: &Pubkey, nonce: u64) -> View {
    view_after(env, rm, borrower, nonce, &[], &[])
}

/// Balances an instruction moves.
#[derive(Clone, Copy, Debug)]
pub struct Balances {
    pub keeper: u64,
    pub borrower: u64,
    pub market_vault: u64,
    pub pool_vault: u64,
    pub venue_tok: u64,
    pub venue_sol: u64,
}

pub fn balances(env: &Env, rm: &RayMarket, keeper: &Pubkey, borrower: &Pubkey) -> Balances {
    let (tok, sol) = rm.vaults();
    Balances {
        keeper: lamports(env, keeper),
        borrower: lamports(env, borrower),
        market_vault: lamports(env, &rm.keys.market_vault),
        pool_vault: tokens(env, &rm.keys.pool_vault),
        venue_tok: tokens(env, &tok),
        venue_sol: tokens(env, &sol),
    }
}

/// The trader normalisation's sweep of `donated` lamports into the lender pool (ARCH §2.4 rule
/// 2: a lump that vests).
fn sweep_donation(p: &mut Pool, v: &mut Vault, now: i64, lump_vest_s: u32, donated: u64) {
    if donated > 0 {
        pool::sweep(p, v, None, now, lump_vest_s, donated, SweepSource::Trader).unwrap();
    }
}

// ---- open_short ------------------------------------------------------------------------------

/// What an open of `q` does on a [`MarketView`] (ARCH §3.4 steps 5–13), from `shortwin-math`.
#[derive(Clone, Debug)]
pub struct OpenPlan {
    pub tier: u8,
    pub s: u64,
    pub n_open: u64,
    pub b_ema: Option<u64>,
    pub m: u64,
    pub fee_open: u64,
    pub v: u64,
    pub entry_price: u128,
    pub r_min: u64,
    pub deadline: i64,
    pub spot: u128,
    pub p_ema: u128,
    pub pool: Pool,
    pub vault: Vault,
}

/// [`OpenPlan`] of `q` on `mv` under the config of `env`.
pub fn plan_open(env: &Env, mv: &MarketView, q: u64) -> OpenPlan {
    let config = env.config();
    let params = config.params;
    let (x, y) = mv.venue.reserves.expect("executable");
    let (xu, yu) = (u128::from(x), u128::from(y));
    let sell = mv.live.sell(q, xu, yu, y).expect("a sale quote");
    let n_open = mv
        .live
        .buy(q, sell.token_after, sell.sol_after)
        .expect("N_open")
        .cost;
    let (ex, ey) = mv.market.newest_ring_xy();
    let b_ema = if ex == 0 {
        None
    } else {
        mv.live.buy(q, ex, ey).map(|b| b.cost)
    };
    let tier = expected_tier(env, &mv.market, &mv.venue, mv.now).expect("executable") as u8;
    let tp = config.tiers[usize::from(tier)];
    let m = risk::margin_d4(sell.proceeds, n_open, b_ema, tp.cr_open_bps).unwrap();
    let fee_open = risk::open_fee(n_open, params.open_fee_bps).unwrap();
    let p_ema = mv.market.ema_price.get();
    let util = rent::utilization_bps(mv.pool.tokens_free, mv.pool.borrowed_q).unwrap();
    let r_min = rent::min_rent(
        q,
        p_ema,
        &tp.to_math().rate,
        util,
        mv.market.util_kink_bps,
        tp.min_rent_s,
    )
    .unwrap();
    let deadline = mv.now + i64::from(tp.max_duration_s);
    let release_at = deadline
        + i64::from(params.max_migration_pause_s)
        + i64::from(params.grad_grace_s)
        + i64::from(params.s3_wait_s);
    let (mut p, mut vlt) = (mv.pool, mv.vault);
    sweep_donation(&mut p, &mut vlt, mv.now, params.lump_vest_s, mv.donated);
    pool::borrow(&mut p, mv.now, q, release_at).unwrap();
    vlt.protocol_fees += fee_open;
    OpenPlan {
        tier,
        s: sell.proceeds,
        n_open,
        b_ema,
        m,
        fee_open,
        v: sell.proceeds + m,
        entry_price: mul_div(u128::from(sell.proceeds), W, u128::from(q), Rounding::Floor).unwrap(),
        r_min,
        deadline,
        spot: risk::price_wad(xu, yu).unwrap(),
        p_ema,
        pool: p,
        vault: vlt,
    }
}

/// `open_short(q)` by `b` with no bounds of its own, checked against [`plan_open`] (module
/// docs). Returns the event and the nonce.
pub fn open_checked(env: &mut Env, rm: &RayMarket, b: &mut Borrower, q: u64) -> (Open, u64) {
    let mv = market_view_after(env, rm, &[], &[]);
    let plan = plan_open(env, &mv, q);
    let nonce = b.next_nonce;
    let market = rm.market();
    let pos_key = pda::position(&market, &b.key(), nonce).0;
    let exp_key = pda::exposure(&market, &b.key()).0;
    let new_exposure = read::account(&env.svm, &exp_key).is_none();
    let m0 = env.market(&market);
    let pre = balances(env, rm, &b.key(), &b.key());
    let ix = open_short_ix(
        rm,
        &b.key(),
        nonce,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let kp = b.kp.insecure_clone();
    let meta = ok(env.send(&[ix], &[&kp]));
    b.next_nonce += 1;
    let ev: Open = events::one(&meta);
    assert_eq!(ev.q, q);
    assert_eq!(ev.s, plan.s, "S = quote_sell(q) on the pre-trade state");
    assert_eq!(
        ev.n_open, plan.n_open,
        "N_open = B̂(q) on the post-trade state"
    );
    assert_eq!(ev.b_spot, plan.n_open);
    assert_eq!(
        ev.b_ema,
        plan.b_ema.unwrap_or(0),
        "B̂_ema on the newest ring entry"
    );
    assert_eq!(ev.m, plan.m, "the D4 margin");
    assert_eq!(ev.v, plan.v);
    assert_eq!(ev.fee_open, plan.fee_open);
    assert_eq!(ev.tier, plan.tier);
    assert_eq!(ev.entry_price, plan.entry_price);
    assert_eq!(ev.spot_wad, plan.spot);
    assert_eq!(ev.ema_wad, plan.p_ema);
    assert_eq!(ev.deadline, plan.deadline);
    assert_eq!(ev.venue, VenueKind::RaydiumV4 as u8);
    assert_eq!(ev.venue_rent, 0);
    let pos = env.position(rm, &b.key(), nonce);
    let config = env.config();
    let tp = config.tiers[usize::from(plan.tier)];
    assert_eq!(
        (pos.q, pos.s, pos.m, pos.v, pos.n_open, pos.fee_open),
        (q, plan.s, plan.m, plan.v, plan.n_open, plan.fee_open)
    );
    assert_eq!(
        (pos.r_min, pos.t_open, pos.deadline),
        (plan.r_min, mv.now, plan.deadline)
    );
    assert_eq!(pos.j_open, mv.market.basis().j);
    assert_eq!(
        (
            pos.cr_liq_bps,
            u32::from(pos.penalty_bps),
            u32::from(pos.max_liq_slip_bps)
        ),
        (tp.cr_liq_bps, tp.penalty_bps, tp.max_liq_slip_bps),
        "the tier's terms, locked"
    );
    assert_eq!(pos.ert, 0, "ert = 0 on Raydium");
    assert_eq!(
        u32::from(pos.venue_fee_hard_cap_bps),
        config.params.venue_fee_hard_cap_bps
    );
    assert_eq!(pos.venue_at_open, VenueKind::RaydiumV4 as u8);
    assert_eq!(pos.s_armed, 0);
    // Lamports and tokens.
    let pos_rent = env
        .svm
        .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
    let exp_rent = if new_exposure {
        env.svm
            .minimum_balance_for_rent_exemption(WalletExposure::SPACE)
    } else {
        0
    };
    let post = balances(env, rm, &b.key(), &b.key());
    assert_eq!(lamports(env, &pos_key), pos_rent + plan.v);
    assert_eq!(
        pre.borrower - post.borrower,
        pos_rent + exp_rent + plan.m + plan.fee_open,
        "the borrower pays the accounts' rent, M and the open fee"
    );
    assert_eq!(
        post.market_vault - pre.market_vault,
        plan.fee_open + mv.donated
    );
    assert_eq!(
        pre.pool_vault - post.pool_vault,
        q,
        "the lending vault spent q"
    );
    assert_eq!(post.venue_tok - pre.venue_tok, q, "the venue received q");
    assert_eq!(pre.venue_sol - post.venue_sol, plan.s, "the venue paid S");
    // The lender pool and the market aggregates.
    let m1 = env.market(&market);
    assert_eq!(m1.pool(), plan.pool, "pool::borrow");
    assert_eq!(*env.market_vault(&market).vault(), plan.vault);
    assert_eq!(m1.open_positions, m0.open_positions + 1);
    assert_eq!(m1.total_notional_open, m0.total_notional_open + plan.n_open);
    assert_eq!(
        m1.borrowed_q_by_tier[usize::from(plan.tier)],
        m0.borrowed_q_by_tier[usize::from(plan.tier)] + q
    );
    (ev, nonce)
}

// ---- close_short -----------------------------------------------------------------------------

/// `close_short(max_buyback_cost)` of `b`'s position `nonce`, checked (ARCH §3.5): the refusal
/// the oracle predicts, or `B = B̂_live(q)`, `waterfall::close`, `pool::repay`, every lamport and
/// token. Returns the event, or the refusal.
pub fn close_checked(
    env: &mut Env,
    rm: &RayMarket,
    b: &Borrower,
    nonce: u64,
    max_buyback_cost: u64,
) -> Result<Close, E> {
    let v = view(env, rm, &b.key(), nonce);
    let pos = v.pos.clone();
    let now = v.now();
    let r_due = rent::due(&pos.pos_rent(), &v.market().basis()).unwrap();
    let decision = (|| {
        if !v.executable() {
            return Err(E::NoRoute);
        }
        let (x, y) = v.xy();
        let b_hat = v.m.live.buy(pos.q, x, y).ok_or(E::InsufficientVault)?.cost;
        if u128::from(b_hat) + u128::from(r_due) + u128::from(pos.ert) > u128::from(pos.v) {
            return Err(E::InsufficientVault);
        }
        let max_in = pos
            .v
            .checked_sub(r_due + pos.ert)
            .ok_or(E::InsufficientVault)?
            .min(max_buyback_cost);
        if b_hat > max_in {
            return Err(E::BuybackAboveMax);
        }
        Ok(b_hat)
    })();
    let ix = close_short_ix(rm, &b.key(), nonce, max_buyback_cost, ExtrasSel::NONE, &[]);
    let kp = b.kp.insecure_clone();
    let b_hat = match decision {
        Err(e) => {
            assert_err(&env.send(&[ix], &[&kp]), e);
            return Err(e);
        }
        Ok(b_hat) => b_hat,
    };
    let payout = waterfall::close(pos.v, b_hat, r_due, u32::from(pos.rent_protocol_bps)).unwrap();
    let lump_vest_s = env.config().params.lump_vest_s;
    let (mut p, mut vlt) = (v.m.pool, v.m.vault);
    sweep_donation(&mut p, &mut vlt, now, lump_vest_s, v.m.donated);
    let credited = rent::credited_for(&pos.pos_rent(), &v.market().basis()).unwrap();
    pool::repay(
        &mut p,
        &mut vlt,
        now,
        lump_vest_s,
        &Resolution {
            q: pos.q,
            credited,
            lump: 0,
        },
        payout.rent,
        u32::from(pos.rent_protocol_bps),
    )
    .unwrap();
    vlt.protocol_fees += payout.protocol;
    let pre = balances(env, rm, &b.key(), &b.key());
    let meta = ok(env.send(&[ix], &[&kp]));
    let ev: Close = events::one(&meta);
    assert_eq!(ev.q, pos.q);
    assert_eq!(
        (ev.buyback_cost, ev.b_live),
        (b_hat, b_hat),
        "B = B̂_live(q)"
    );
    assert_eq!(ev.rent, payout.rent);
    assert_eq!(ev.rent_protocol, payout.rent_protocol);
    assert_eq!(ev.borrower_payout, payout.borrower);
    assert_eq!(ev.token_surplus, 0);
    let post = balances(env, rm, &b.key(), &b.key());
    let exposure_back = if v.exposure_q_open == pos.q {
        v.exposure_lamports
    } else {
        0
    };
    assert_eq!(
        post.borrower - pre.borrower,
        v.pos_lamports - pos.v + payout.borrower + exposure_back,
        "the borrower: the remainder, the position's rent and a closed exposure"
    );
    assert_eq!(
        post.market_vault - pre.market_vault,
        payout.lenders + payout.protocol + v.m.donated
    );
    assert_eq!(
        post.pool_vault - pre.pool_vault,
        pos.q,
        "q back in the lending vault"
    );
    assert_eq!(pre.venue_tok - post.venue_tok, pos.q);
    assert_eq!(
        post.venue_sol - pre.venue_sol,
        b_hat,
        "the venue took exactly B"
    );
    let m1 = env.market(&rm.market());
    assert_eq!(m1.pool(), p, "pool::repay");
    assert_eq!(*env.market_vault(&rm.market()).vault(), vlt);
    assert!(read::account(&env.svm, &pda::position(&rm.market(), &b.key(), nonce).0).is_none());
    Ok(ev)
}

// ---- liquidate / expire ----------------------------------------------------------------------

/// A reference and its bound (ARCH §3.6 step 6, §3.7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefPred {
    pub p_ref: Option<u128>,
    pub l_b: u128,
    pub b_ref: Option<u64>,
    pub max_in: Option<u64>,
    pub pi_ref: u64,
}

/// Every decision input of ARCH §3.6 / §3.7 on a [`View`], from `shortwin-math` alone.
#[derive(Clone)]
pub struct Predicted {
    pub view: View,
    pub r_due: u64,
    pub v_eff: u64,
    pub k_s: u64,
    pub fresh: bool,
    pub b_live: Option<BuyQuote>,
    pub b_live_risk: Option<u64>,
    pub sigma_live: u64,
    pub p_rs: Option<u128>,
    pub b_rs: Option<u64>,
    pub spot_leg: bool,
    pub ema_leg: bool,
    pub expired: bool,
    pub buyback_allowed: bool,
    pub cr_spot_bps: u64,
    pub cr_ema_bps: u64,
    pub liq: RefPred,
    pub exp: RefPred,
}

/// [`Predicted`] of `v` (ARCH §4.5; ECON §5.5, §8.2).
pub fn predict(v: &View) -> Predicted {
    let pos = &v.pos;
    let q = pos.q;
    let now = v.now();
    let obs = v.market().observation();
    let r_due = rent::due(&pos.pos_rent(), &v.market().basis()).unwrap();
    let v_eff = risk::v_eff(pos.v, r_due);
    let k_s = risk::k_s(pos.v, &pos.settle_keeper_terms()).unwrap();
    let live = v.m.live;
    let risky = v.risky();
    let exec = v.executable();
    let (x, y) = v.xy();
    let b_live = if exec { live.buy(q, x, y) } else { None };
    let b_live_risk = if exec {
        risky.buy(q, x, y).map(|b| b.cost)
    } else {
        None
    };
    let sigma_live = b_live.map_or(u64::MAX, |b| risk::sigma_xy(Some(b.net), q, x, y).unwrap());
    let fresh = obs.last.is_fresh(now, pos.ref_max_age_s);
    let (p_live, l_live) = if exec {
        (
            Some(risk::price_wad(x, y).unwrap()),
            quote::liquidity(x, y).unwrap(),
        )
    } else {
        (None, obs.ema.liq)
    };
    let p_rs = p_live.and_then(|p| observe::robust_spot(p, &obs.last, now, pos.ref_max_age_s));
    let l_spot = spot_liquidity_of(l_live, &obs);
    let b_rs = p_rs.and_then(|p| risky.buy_repriced(q, l_spot, p).map(|b| b.cost));
    let l_bps = pos.cr_liq_bps;
    let spot_leg = risk::spot_leg(exec, fresh, v_eff, b_rs, l_bps).unwrap();
    let quote_at = |x: u128, y: u128| {
        if x == 0 {
            None
        } else {
            risky.buy(q, x, y).map(|b| b.cost)
        }
    };
    let ema_leg = persist::liq_ema_leg(
        &obs.ring,
        |x, y| risk::cr_below(v_eff, quote_at(x, y), l_bps),
        pos.liq_persist_n,
    )
    .unwrap();
    let budget = Budget {
        v: pos.v,
        k_s,
        ert: pos.ert,
    };
    let buyback_allowed = risk::buyback_allowed(
        &budget,
        b_live.map(|b| b.cost),
        sigma_live,
        u32::from(pos.max_liq_slip_bps),
    )
    .unwrap();
    let newest = obs.ring.newest().copied().unwrap_or_default();
    let cr_ema_bps = risk::cr_bps(v_eff, quote_at(newest.x, newest.y)).unwrap_or(u64::MAX);
    // M4F-1: a Raydium market's reference credits at most its locked depth D through its EMA terms
    // (AIR-14: `⌊D·10⁴/λ⌋` at λ = 100 %; `λ_min` before).
    let cap = bounds::ref_depth_cap(v.market().pool_d.get(), 10_000);
    let l_b = bounds::ref_liquidity(l_live, &obs.ema, &obs.ring, Some(cap)).unwrap();
    let prm = pos.exit_params();
    let liq_p = bounds::liq_reference(&obs.ema, &obs.ring, &obs.last, now, &prm).unwrap();
    let liq_b = liq_p.and_then(|p| risky.buy_repriced(q, l_b, p).map(|b| b.cost));
    let liq = RefPred {
        p_ref: liq_p,
        l_b,
        b_ref: liq_b,
        max_in: liq_b.and_then(|b| bounds::max_in_liq(b, pos.delta_liq_bps, pos.v, k_s, pos.ert)),
        pi_ref: 0,
    };
    let exp_p = bounds::expiry_reference(&obs.ema, &obs.ring, &obs.last, now, &prm).unwrap();
    let exp_b = exp_p.and_then(|p| risky.buy_repriced(q, l_b, p).map(|b| b.cost));
    let pi_ref = exp_b.map_or(0, |b| bounds::pi_ref(b, pos.expiry_fee_bps).unwrap());
    let exp = RefPred {
        p_ref: exp_p,
        l_b,
        b_ref: exp_b,
        max_in: exp_b.and_then(|b| {
            bounds::max_in_exp(
                b,
                pos.delta_exp_bps,
                pos.exec_tol_bps,
                pos.v,
                k_s,
                r_due,
                pi_ref,
                pos.ert,
            )
        }),
        pi_ref,
    };
    Predicted {
        view: v.clone(),
        r_due,
        v_eff,
        k_s,
        fresh,
        b_live,
        b_live_risk,
        sigma_live,
        p_rs,
        b_rs,
        spot_leg,
        ema_leg,
        expired: now >= pos.deadline,
        buyback_allowed,
        cr_spot_bps: risk::cr_bps(v_eff, b_rs).unwrap_or(u64::MAX),
        cr_ema_bps,
        liq,
        exp,
    }
}

/// Which keeper exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Liquidate,
    Expire,
}

/// A keeper exit the oracle expects to execute.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub b_live: u64,
    pub b_live_risk: u64,
    pub r: RefPred,
    pub b_ref: u64,
    pub max_in: u64,
}

impl Predicted {
    /// Liquidatable if an attacker pumps the live spot to at least the last bucket low inside
    /// its own final transaction (the attack scripts' `common.would_liq`): the EMA leg holds,
    /// the bucket is fresh, and the robust-spot leg at `lb_lo` (on the live liquidity) is below
    /// `L`.
    pub fn would_liq(&self) -> bool {
        if !self.ema_leg || !self.fresh || !self.view.executable() {
            return false;
        }
        let pos = &self.view.pos;
        let obs = self.view.market().observation();
        let (x, y) = self.view.xy();
        let l = quote::liquidity(x, y).unwrap();
        let q = self
            .view
            .risky()
            .buy_repriced(pos.q, l, obs.last.lo)
            .map(|b| b.cost);
        risk::cr_below(self.v_eff, q, pos.cr_liq_bps).unwrap()
    }

    /// CR-liquidatable (spot ∧ EMA legs).
    pub fn cr_liquidatable(&self) -> bool {
        self.spot_leg && self.ema_leg
    }

    /// `liquidate`'s decision (ARCH §3.6 steps 1–6, in order).
    pub fn liquidation(&self) -> Result<Plan, E> {
        if !self.view.executable() {
            return Err(E::NoRoute);
        }
        if self.view.market().ema_warm != 0 {
            return Err(E::EmaWarmingUp);
        }
        if !self.fresh {
            return Err(E::ReferenceStale);
        }
        if !(self.cr_liquidatable() || self.expired) {
            return Err(E::NotLiquidatable);
        }
        if !self.buyback_allowed {
            return Err(E::UseSolSettle);
        }
        if self.expired && !self.cr_liquidatable() {
            return Err(E::UseExpire);
        }
        self.plan(self.liq)
    }

    /// `expire`'s decision (ARCH §3.7, in the program's order).
    pub fn expiry(&self) -> Result<Plan, E> {
        if !self.view.executable() {
            return Err(E::NoRoute);
        }
        if self.view.market().ema_warm != 0 {
            return Err(E::EmaWarmingUp);
        }
        if !self.expired {
            return Err(E::NotExpired);
        }
        if !self.fresh {
            return Err(E::ReferenceStale);
        }
        if self.cr_liquidatable() {
            return Err(E::UseLiquidate);
        }
        if !self.buyback_allowed {
            return Err(E::UseSolSettle);
        }
        self.plan(self.exp)
    }

    /// The decision of `which`.
    pub fn decision(&self, which: Which) -> Result<Plan, E> {
        match which {
            Which::Liquidate => self.liquidation(),
            Which::Expire => self.expiry(),
        }
    }

    fn plan(&self, r: RefPred) -> Result<Plan, E> {
        r.p_ref.ok_or(E::ReferenceStale)?;
        let b_ref = r.b_ref.ok_or(E::ExitBoundNotMet)?;
        let max_in = r.max_in.ok_or(E::ExitBoundNotMet)?;
        let b_live = self.b_live.ok_or(E::UseSolSettle)?.cost;
        if b_live > max_in {
            return Err(E::ExitBoundNotMet);
        }
        Ok(Plan {
            b_live,
            b_live_risk: self.b_live_risk.unwrap_or(b_live),
            r,
            b_ref,
            max_in,
        })
    }

    /// The waterfall of `which` with measured spend `spent`.
    pub fn payout(&self, which: Which, plan: &Plan, spent: u64) -> Payout {
        let pos = &self.view.pos;
        let x = ExitInputs {
            v: pos.v,
            b: spent,
            r_due: self.r_due,
            b_live: plan.b_live_risk,
            b_ref: plan.b_ref,
            rent_protocol_bps: u32::from(pos.rent_protocol_bps),
        };
        match which {
            Which::Liquidate => waterfall::liquidate(
                &x,
                u32::from(pos.penalty_bps),
                &pos.penalty_split(),
                self.k_s,
            )
            .unwrap(),
            Which::Expire => waterfall::expire(
                &x,
                u32::from(pos.expiry_fee_bps),
                &pos.expiry_keeper_terms(),
            )
            .unwrap(),
        }
    }

    /// The lender pool after the exit: the normalisation's sweep, then `pool::repay`.
    pub fn pool_after(&self, payout: &Payout, lump_vest_s: u32) -> (Pool, Vault) {
        let (mut p, mut v) = (self.view.m.pool, self.view.m.vault);
        sweep_donation(
            &mut p,
            &mut v,
            self.view.now(),
            lump_vest_s,
            self.view.m.donated,
        );
        let pos = &self.view.pos;
        let credited = rent::credited_for(&pos.pos_rent(), &self.view.market().basis()).unwrap();
        pool::repay(
            &mut p,
            &mut v,
            self.view.now(),
            lump_vest_s,
            &Resolution {
                q: pos.q,
                credited,
                lump: payout.lump,
            },
            payout.rent,
            u32::from(pos.rent_protocol_bps),
        )
        .unwrap();
        v.protocol_fees += payout.protocol;
        (p, v)
    }
}

/// The exit instruction of `which` by `keeper`.
pub fn exit_ix(
    which: Which,
    rm: &RayMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
) -> Instruction {
    match which {
        Which::Liquidate => liquidate_ix(rm, keeper, borrower, nonce),
        Which::Expire => expire_ix(rm, keeper, borrower, nonce),
    }
}

/// The result of a checked keeper exit.
pub enum Exit {
    Liquidated(Box<Liquidate>, Box<Predicted>, TransactionMetadata),
    Expired(Box<Expire>, Box<Predicted>, TransactionMetadata),
    Refused(E, Box<Predicted>),
}

impl Exit {
    pub fn liquidated(&self) -> &Liquidate {
        match self {
            Exit::Liquidated(e, ..) => e,
            _ => panic!("expected a liquidation, got {}", self.describe()),
        }
    }

    pub fn expired(&self) -> &Expire {
        match self {
            Exit::Expired(e, ..) => e,
            _ => panic!("expected an expiry, got {}", self.describe()),
        }
    }

    pub fn refused(&self) -> E {
        match self {
            Exit::Refused(e, _) => *e,
            _ => panic!("expected a refusal, got {}", self.describe()),
        }
    }

    pub fn executed(&self) -> bool {
        !matches!(self, Exit::Refused(..))
    }

    pub fn predicted(&self) -> &Predicted {
        match self {
            Exit::Liquidated(_, p, _) | Exit::Expired(_, p, _) | Exit::Refused(_, p) => p,
        }
    }

    fn describe(&self) -> String {
        match self {
            Exit::Liquidated(..) => "a liquidation".into(),
            Exit::Expired(..) => "an expiry".into(),
            Exit::Refused(e, _) => format!("the refusal {e:?}"),
        }
    }
}

fn borsh_bytes<T: anchor_lang::AnchorSerialize>(v: &T) -> Vec<u8> {
    let mut out = Vec::new();
    v.serialize(&mut out).unwrap();
    out
}

/// A copy of an event (the events are not `Clone`): a Borsh round trip.
pub fn clone_event<T: anchor_lang::AnchorSerialize + anchor_lang::AnchorDeserialize>(e: &T) -> T {
    T::try_from_slice(&borsh_bytes(e)).unwrap()
}

/// Sends `front ++ [exit] ++ back` (one transaction; `signers` sign the bundle) and asserts the
/// program decided and paid exactly what the oracle computes on the state after `front`. The
/// keeper must not be the fee payer and `front` / `back` must not move its lamports (an
/// attacker-keeper trades through its token accounts).
pub fn exit_checked(
    env: &mut Env,
    rm: &RayMarket,
    which: Which,
    keeper: &Keypair,
    borrower: &Pubkey,
    nonce: u64,
    front: &[Instruction],
    back: &[Instruction],
    signers: &[&Keypair],
) -> Exit {
    let mut all: Vec<&Keypair> = signers.to_vec();
    all.push(keeper);
    let v = view_after(env, rm, borrower, nonce, front, &all);
    let pred = Box::new(predict(&v));
    let decision = pred.decision(which);
    let exit = exit_ix(which, rm, &keeper.pubkey(), borrower, nonce);
    let mut ixs: Vec<Instruction> = front.to_vec();
    let exit_index = ixs.len();
    ixs.push(exit);
    ixs.extend_from_slice(back);

    let plan = match decision {
        Err(e) => {
            let res = env.send(&ixs, &all);
            assert_err(&res, e);
            return Exit::Refused(e, pred);
        }
        Ok(plan) => plan,
    };
    // The exit alone (after `front`) on a clone, for its exact deltas; then the bundle.
    let saved = env.svm.clone();
    if !front.is_empty() {
        ok(env.send(front, &all));
    }
    let pre = balances(env, rm, &keeper.pubkey(), borrower);
    let alone = ok(env.send(&ixs[exit_index..=exit_index], &all));
    let post = balances(env, rm, &keeper.pubkey(), borrower);
    let market_after = env.market(&rm.market());
    let vault_after = *env.market_vault(&rm.market()).vault();
    let pos_gone = read::account(&env.svm, &pda::position(&rm.market(), borrower, nonce).0)
        .is_none_or(|a| a.lamports == 0);
    env.svm = saved;
    let meta = match env.send(&ixs, &all) {
        Ok(m) => m,
        Err(f) => panic!(
            "the bundle failed although its exit alone succeeded: {:?}\n{}",
            f.err,
            f.meta.pretty_logs()
        ),
    };
    assert!(pos_gone, "the position is closed");
    let lump_vest_s = env.config().params.lump_vest_s;
    let same_wallet = keeper.pubkey() == *borrower;
    let (spent, payout) = match which {
        Which::Liquidate => {
            let ev: Liquidate = events::one(&alone);
            let payout = pred.payout(which, &plan, ev.buyback_cost);
            check_liquidation(&ev, &pred, &plan, &payout);
            (ev.buyback_cost, payout)
        }
        Which::Expire => {
            let ev: Expire = events::one(&alone);
            let payout = pred.payout(which, &plan, ev.buyback_cost);
            check_expiry(&ev, &pred, &plan, &payout);
            (ev.buyback_cost, payout)
        }
    };
    check_side_events(&pred, &alone);
    check_after(
        &pred,
        &payout,
        &pre,
        &post,
        &market_after,
        &vault_after,
        lump_vest_s,
        spent,
        same_wallet,
    );
    match which {
        Which::Liquidate => {
            let ev: Liquidate = events::one(&alone);
            let bundled: Liquidate = events::of_ix(&meta, exit_index).remove(0);
            assert_eq!(
                borsh_bytes(&bundled),
                borsh_bytes(&ev),
                "the bundle's liquidation is the checked one"
            );
            Exit::Liquidated(Box::new(bundled), pred, meta)
        }
        Which::Expire => {
            let ev: Expire = events::one(&alone);
            let bundled: Expire = events::of_ix(&meta, exit_index).remove(0);
            assert_eq!(
                borsh_bytes(&bundled),
                borsh_bytes(&ev),
                "the bundle's expiry is the checked one"
            );
            Exit::Expired(Box::new(bundled), pred, meta)
        }
    }
}

/// A checked exit with no bundle.
pub fn exit_alone(
    env: &mut Env,
    rm: &RayMarket,
    which: Which,
    keeper: &Keypair,
    borrower: &Pubkey,
    nonce: u64,
) -> Exit {
    exit_checked(env, rm, which, keeper, borrower, nonce, &[], &[], &[])
}

fn check_side_events(pred: &Predicted, meta: &TransactionMetadata) {
    let swept: Vec<TraderSwept> = events::of(meta);
    if pred.view.m.donated > 0 {
        assert_eq!(swept.len(), 1, "one TraderSwept");
        assert_eq!(swept[0].lamports, pred.view.m.donated);
    } else {
        assert!(swept.is_empty(), "no TraderSwept without a donation");
    }
}

fn check_liquidation(ev: &Liquidate, pred: &Predicted, plan: &Plan, payout: &Payout) {
    let pos = &pred.view.pos;
    assert_eq!(
        ev.buyback_cost, plan.b_live,
        "the venue charges exactly B̂_live (AMM v4; CPMM's exact out, U11's creator fee included)"
    );
    assert_eq!(ev.b_live, plan.b_live);
    assert_eq!(ev.b_live_risk, plan.b_live_risk);
    assert_eq!(ev.b_ref, plan.b_ref);
    assert_eq!(Some(ev.p_ref), plan.r.p_ref);
    assert_eq!(ev.max_in, plan.max_in);
    assert_eq!(ev.delta_bps, pos.delta_liq_bps);
    assert_eq!(ev.cr_spot_bps, pred.cr_spot_bps);
    assert_eq!(ev.cr_ema_bps, pred.cr_ema_bps);
    assert_eq!(ev.expired, pred.expired);
    assert_eq!(ev.token_surplus, 0);
    assert_eq!(ev.rent, payout.rent);
    assert_eq!(ev.penalty, payout.penalty);
    assert_eq!(ev.penalty_lender, payout.penalty_lenders);
    assert_eq!(ev.penalty_protocol, payout.penalty_protocol);
    assert_eq!(ev.penalty_keeper, payout.penalty_keeper);
    assert_eq!(ev.keeper_topup, payout.keeper_topup);
    assert_eq!(ev.borrower_payout, payout.borrower);
}

fn check_expiry(ev: &Expire, pred: &Predicted, plan: &Plan, payout: &Payout) {
    let _ = pred;
    assert_eq!(
        ev.buyback_cost, plan.b_live,
        "the venue charges exactly B̂_live (AMM v4; CPMM's exact out, U11's creator fee included)"
    );
    assert_eq!(ev.b_live, plan.b_live);
    assert_eq!(ev.b_live_risk, plan.b_live_risk);
    assert_eq!(ev.b_ref, plan.b_ref);
    assert_eq!(ev.pi_ref, plan.r.pi_ref);
    assert_eq!(ev.max_in, plan.max_in);
    assert_eq!(ev.token_surplus, 0);
    assert_eq!(ev.rent, payout.rent);
    assert_eq!(ev.expiry_fee, payout.expiry_fee);
    assert_eq!(ev.keeper_reward, payout.keeper);
    assert_eq!(ev.borrower_payout, payout.borrower);
}

fn check_after(
    pred: &Predicted,
    payout: &Payout,
    pre: &Balances,
    post: &Balances,
    market_after: &Market,
    vault_after: &Vault,
    lump_vest_s: u32,
    spent: u64,
    same_wallet: bool,
) {
    let v = &pred.view;
    let pos = &v.pos;
    assert_eq!(payout.total().unwrap(), pos.v, "I9");
    assert_eq!(spent, payout.buyback);
    let exposure_back = if v.exposure_q_open == pos.q {
        v.exposure_lamports
    } else {
        0
    };
    let to_borrower = v.pos_lamports - pos.v + payout.borrower + exposure_back;
    if same_wallet {
        assert_eq!(post.borrower - pre.borrower, to_borrower + payout.keeper);
    } else {
        assert_eq!(post.keeper - pre.keeper, payout.keeper, "the keeper's legs");
        assert_eq!(
            post.borrower - pre.borrower,
            to_borrower,
            "the borrower: its remainder, the position's rent (A18) and a closed exposure"
        );
    }
    assert_eq!(
        post.market_vault - pre.market_vault,
        payout.lenders + payout.protocol + v.m.donated,
        "market_vault: lenders, protocol and the swept donation"
    );
    assert_eq!(
        post.pool_vault - pre.pool_vault,
        pos.q,
        "q delivered into the lending vault"
    );
    assert_eq!(
        pre.venue_tok - post.venue_tok,
        pos.q,
        "the venue paid out q"
    );
    assert_eq!(
        post.venue_sol - pre.venue_sol,
        spent,
        "the venue took exactly B"
    );
    let (p, vault) = pred.pool_after(payout, lump_vest_s);
    assert_eq!(market_after.pool(), p, "the lender pool after pool::repay");
    assert_eq!(*vault_after, vault, "the vault after pool::repay");
    assert_eq!(market_after.open_positions, v.market().open_positions - 1);
    assert_eq!(
        market_after.total_notional_open,
        v.market().total_notional_open - pos.n_open
    );
    assert_eq!(market_after.borrowed_q, v.market().borrowed_q - pos.q);
}

/// Tries `which` now and then once a second (one checked crank per second, the pool held), up
/// to `max_s` attempts, each checked. Returns the executed exit (if any) and the refusals.
pub fn attempts(
    env: &mut Env,
    rm: &RayMarket,
    which: Which,
    keeper: &Keypair,
    borrower: &Pubkey,
    nonce: u64,
    max_s: u32,
) -> (Option<Exit>, Vec<E>) {
    let mut refusals = Vec::new();
    for i in 0..max_s {
        if i > 0 {
            tick_checked(env, rm, 1);
        }
        let ex = exit_alone(env, rm, which, keeper, borrower, nonce);
        if ex.executed() {
            return (Some(ex), refusals);
        }
        refusals.push(ex.refused());
    }
    (None, refusals)
}

// ---- sol_settle ------------------------------------------------------------------------------

/// The position's risk quotes on `(P, L)` states for the box predicates (ARCH §4.3, §4.5): every
/// quote at the capped fee, `+∞` (`None` / `u64::MAX`) where undefined.
#[derive(Clone, Copy, Debug)]
pub struct Quotes {
    pub pricing: Pricing,
    pub q: u64,
}

impl Quotes {
    pub fn cost_xy(&self, x: u128, y: u128) -> Option<u64> {
        if x == 0 {
            return None;
        }
        self.pricing.buy(self.q, x, y).map(|b| b.cost)
    }

    pub fn sigma_on(&self, x: u128, y: u128) -> u64 {
        if x == 0 || y == 0 {
            return u64::MAX;
        }
        let n = self.pricing.buy(self.q, x, y).map(|b| b.net);
        match risk::sigma_xy(n, self.q, x, y) {
            Err(MathError::DivByZero) => u64::MAX,
            r => r.unwrap(),
        }
    }
}

impl BoxQuotes for Quotes {
    fn cost(&mut self, p: u128, l: u128) -> Result<Option<u64>, MathError> {
        Ok(reprice(l, p).and_then(|(x, y)| self.cost_xy(x, y)))
    }

    fn sigma(&mut self, p: u128, l: u128) -> Result<u64, MathError> {
        Ok(reprice(l, p).map_or(u64::MAX, |(x, y)| self.sigma_on(x, y)))
    }
}

/// The locked settlement terms of a position (ARCH §2.3.5).
pub fn settle_terms(pos: &ShortPosition) -> SettleTerms {
    SettleTerms {
        cr_liq_bps: pos.cr_liq_bps,
        sigma_max_bps: u32::from(pos.max_liq_slip_bps),
        penalty_bps: u32::from(pos.penalty_bps),
        expiry_fee_bps: u32::from(pos.expiry_fee_bps),
        fv_dev_bps: u32::from(pos.fv_dev_bps),
        persist_n: pos.settle_persist_n,
        arm_gap_s: pos.settle_arm_gap_s,
        s3: pos.s3_terms(),
    }
}

/// What `sol_settle` decides on a [`View`] (ARCH §3.8, §4.5): the inputs, the box and
/// `settle::evaluate`'s outcome on the position's arming clock.
#[derive(Clone)]
pub struct SettlePrediction {
    pub view: View,
    pub inp: SettleInputs,
    pub eval: Evaluation,
    pub arm: ArmState,
    pub b_live: Option<u64>,
    pub b_rs: Option<u64>,
    pub b_ema: Option<u64>,
    pub quotes: Quotes,
}

/// [`SettlePrediction`] of `v`, from `shortwin-math` alone.
pub fn predict_settle(v: &View) -> SettlePrediction {
    let pos = &v.pos;
    let now = v.now();
    let market = v.market();
    let obs = market.observation();
    let r_due = rent::due(&pos.pos_rent(), &market.basis()).unwrap();
    let k_s = risk::k_s(pos.v, &pos.settle_keeper_terms()).unwrap();
    let v_eff = risk::v_eff(pos.v, r_due);
    let exec = v.executable();
    let mut quotes = Quotes {
        pricing: v.risky(),
        q: pos.q,
    };
    let (x, y) = v.xy();
    let (p_live, l_live) = if exec {
        (
            Some(risk::price_wad(x, y).unwrap()),
            quote::liquidity(x, y).unwrap(),
        )
    } else {
        (None, obs.ema.liq)
    };
    let p_rs = p_live.and_then(|p| observe::robust_spot(p, &obs.last, now, pos.ref_max_age_s));
    let fresh = obs.last.is_fresh(now, pos.ref_max_age_s);
    let (b_live, sigma_live) = if exec {
        (quotes.cost_xy(x, y), quotes.sigma_on(x, y))
    } else {
        (None, u64::MAX)
    };
    // FITS-2 (a): the spot leg of "CR-liquidatable" at max(L_live, L_ema'); the S1 / S2 legs
    // and the refutation keep L_live (`inp.l_live`).
    let b_rs = p_rs.and_then(|p| quotes.cost(p, spot_liquidity_of(l_live, &obs)).unwrap());
    let spot_leg = risk::spot_leg(exec, fresh, v_eff, b_rs, pos.cr_liq_bps).unwrap();
    let ema_leg = {
        let q = quotes;
        persist::liq_ema_leg(
            &obs.ring,
            risk::ring_below(v_eff, pos.cr_liq_bps, |x, y| q.cost_xy(x, y)),
            pos.liq_persist_n,
        )
        .unwrap()
    };
    // AIR-15: the box's L_mhi is capped at the locked depth D, as the reference's EMA terms are
    // (AIR-14: `⌊D·10⁴/λ⌋` at λ = 100 %).
    let cap = bounds::ref_depth_cap(market.pool_d.get(), 10_000);
    let bx = settle::box_of_capped(&obs.ema, &obs.ring, now, exec, Some(cap)).unwrap();
    let nz = |t: i64| (t != 0).then_some(t);
    let inp = SettleInputs {
        now,
        deadline: pos.deadline,
        ema_count: obs.ema.count,
        p_ema: obs.ema.price,
        l_ema: obs.ema.liq,
        bx,
        budget: Budget {
            v: pos.v,
            k_s,
            ert: pos.ert,
        },
        r_due,
        cr_liquidatable: spot_leg && ema_leg,
        ema_leg,
        executable: exec,
        warmed: shortwin_math::observe::warmed(&obs.ema),
        p_rs,
        l_live,
        b_live,
        sigma_live,
        s3: S3Chain {
            no_route_now: v.m.s3_no_route,
            no_route_since: nz(market.no_route_since),
            graduated_at: nz(market.graduated_at),
            migrating: false,
            migration_seen_at: nz(market.migration_seen_at),
        },
    };
    let mut arm = pos.arm_state();
    let eval = settle::evaluate(&mut arm, &inp, &settle_terms(pos), &mut quotes).unwrap();
    let b_ema = quotes.cost(inp.p_ema, inp.l_ema).unwrap();
    SettlePrediction {
        view: v.clone(),
        inp,
        eval,
        arm,
        b_live,
        b_rs,
        b_ema,
        quotes,
    }
}

/// The outcome of a checked `sol_settle`.
pub struct Settled {
    pub pred: Box<SettlePrediction>,
    pub ev: Option<SolSettle>,
    pub armed: Option<SettleArmed>,
    pub disarmed: Option<SettleDisarmed>,
    pub meta: TransactionMetadata,
}

/// `sol_settle` of `borrower`'s position `nonce` by `keeper`, checked (module docs): the arming
/// clock's change and its events, or the settlement — its kind, the box and its corners, `p'`
/// per corner, FV, `K_s`, the lenders' take and the vesting lump (`waterfall::settle`), the
/// borrower's remainder, `pool::settle`, every lamport, the market aggregates.
pub fn settle_checked(
    env: &mut Env,
    rm: &RayMarket,
    keeper: &Keypair,
    borrower: &Pubkey,
    nonce: u64,
) -> Settled {
    let v = view(env, rm, borrower, nonce);
    let pred = Box::new(predict_settle(&v));
    let pos = v.pos.clone();
    let pre = balances(env, rm, &keeper.pubkey(), borrower);
    let ix = sol_settle_ix(env, rm, &keeper.pubkey(), borrower, nonce);
    let meta = ok(env.send(&[ix], &[keeper]));
    let ev: Option<SolSettle> = events::of::<SolSettle>(&meta).pop();
    let armed: Option<SettleArmed> = events::of::<SettleArmed>(&meta).pop();
    let disarmed: Option<SettleDisarmed> = events::of::<SettleDisarmed>(&meta).pop();
    assert_eq!(
        armed.is_some(),
        pred.eval.arm == ArmChange::Armed,
        "SettleArmed iff the clock armed: {:?}",
        pred.eval
    );
    assert_eq!(
        disarmed.as_ref().map(|d| d.reason),
        match pred.eval.arm {
            ArmChange::Disarmed(DisarmReason::Refuted) =>
                Some(shortwin::events::SETTLE_REASON_REFUTED),
            ArmChange::Disarmed(DisarmReason::NotLiquidatable) => {
                Some(shortwin::events::SETTLE_REASON_NOT_LIQUIDATABLE)
            }
            _ => None,
        },
        "SettleDisarmed iff the clock disarmed on the box"
    );
    let Some(s) = pred.eval.settlement else {
        assert!(ev.is_none(), "no settlement: {:?}", pred.eval);
        let after = env.position(rm, borrower, nonce);
        assert_eq!(after.arm_state(), pred.arm, "the arming clock");
        return Settled {
            pred,
            ev,
            armed,
            disarmed,
            meta,
        };
    };
    let ev_s = ev.as_ref().map(clone_event).expect("a SolSettle event");
    // AIR-09: a venue that cannot execute prices the extinguished tokens at 0 for the immediate
    // part (its `P_ema` is the pre-halt mark): all but `R_due` vests.
    let p_imm = if pred.inp.executable {
        pred.inp.p_ema
    } else {
        0
    };
    let payout =
        waterfall::settle(pos.v, s.kind, s.k_s, s.fv, pos.q, p_imm, pred.inp.r_due).unwrap();
    assert_eq!(ev_s.condition, s.kind as u8, "the settlement kind");
    assert_eq!(ev_s.k_s, s.k_s);
    assert_eq!(ev_s.lender_amount, s.lenders);
    assert_eq!(ev_s.borrower_amount, s.borrower);
    assert_eq!(ev_s.lump, payout.lump);
    assert_eq!(ev_s.fv, s.fv.unwrap_or(0));
    let bx = pred.inp.bx;
    assert_eq!(
        (ev_s.p_lo, ev_s.p_hi, ev_s.l_lo, ev_s.l_hi),
        (bx.p_lo, bx.p_hi, bx.l_lo, bx.l_hi),
        "the settlement box"
    );
    match s.corners {
        Some([lo, hi]) => {
            assert_eq!((ev_s.x_lo, ev_s.x_hi), (lo.x, hi.x), "the corners' amounts");
            assert_eq!(
                (u32::from(ev_s.pprime_lo), u32::from(ev_s.pprime_hi)),
                (lo.pprime.unwrap_or(0), hi.pprime.unwrap_or(0)),
                "p' per corner"
            );
        }
        None => assert_eq!((ev_s.x_lo, ev_s.x_hi), (0, 0)),
    }
    assert_eq!(ev_s.b_live, pred.b_live.unwrap_or(0));
    assert_eq!(ev_s.b_rs, pred.b_rs.unwrap_or(0));
    assert_eq!(ev_s.b_ema, pred.b_ema.unwrap_or(0));
    assert_eq!(ev_s.ema_age_s, v.now() - v.market().observation().ema.ts);
    // Lamports, the lender pool, the market.
    let post = balances(env, rm, &keeper.pubkey(), borrower);
    assert_eq!(post.keeper - pre.keeper, s.k_s, "K_s to the keeper");
    let exposure_back = if v.exposure_q_open == pos.q {
        v.exposure_lamports
    } else {
        0
    };
    assert_eq!(
        post.borrower - pre.borrower,
        v.pos_lamports - pos.v + s.borrower + exposure_back,
        "V − K_s − x and the accounts' rent to the borrower"
    );
    assert_eq!(
        post.market_vault - pre.market_vault,
        s.lenders,
        "x to the lenders"
    );
    assert_eq!(post.pool_vault, pre.pool_vault, "no token moves");
    assert_eq!(
        (post.venue_tok, post.venue_sol),
        (pre.venue_tok, pre.venue_sol),
        "no venue CPI"
    );
    let (mut p, mut vlt) = (v.m.pool, v.m.vault);
    let credited = rent::credited_for(&pos.pos_rent(), &v.market().basis()).unwrap();
    let rec = pool::settle(
        &mut p,
        &mut vlt,
        v.now(),
        env.config().params.lump_vest_s,
        &Resolution {
            q: pos.q,
            credited,
            lump: payout.lump,
        },
        payout.lenders,
        pred.inp.r_due,
    )
    .unwrap()
    .rec;
    assert!(rec.is_none(), "no epoch roll in these scenarios");
    let m1 = env.market(&rm.market());
    assert_eq!(m1.pool(), p, "pool::settle");
    assert_eq!(*env.market_vault(&rm.market()).vault(), vlt);
    assert_eq!(m1.open_positions, v.market().open_positions - 1);
    assert_eq!(m1.borrowed_q, v.market().borrowed_q - pos.q);
    assert!(read::account(&env.svm, &pda::position(&rm.market(), borrower, nonce).0).is_none());
    Settled {
        pred,
        ev,
        armed,
        disarmed,
        meta,
    }
}

// ---- the Fartcoin market ---------------------------------------------------------------------

/// A lender of the market.
pub struct Lender {
    pub kp: Keypair,
    pub account: Pubkey,
}

/// The Fartcoin market on the fork: listed (checked), one lender's deposit (checked), the
/// listing window waited out and one checked crank; a keeper and a whale that trades the pool.
pub struct Fart {
    pub env: Env,
    pub rm: RayMarket,
    pub lender: Lender,
    pub keeper: Keypair,
    pub whale: Trader,
}

impl Fart {
    /// Lists Fartcoin in `env`, lends `lend` tokens, waits out the window.
    pub fn new(env: Env, lend: u64) -> Self {
        Self::with_pool(env, lend, |_, _| {})
    }

    /// As [`Fart::new`], with `before` run on the loaded pool before the listing (a scenario
    /// builder: e.g. [`hold_unlocked_lp`]).
    pub fn with_pool(mut env: Env, lend: u64, before: impl FnOnce(&mut Env, &V4Pool)) -> Self {
        let pool = env.load_ray_pool(FART);
        let AnyPool::V4(p) = pool else { unreachable!() };
        before(&mut env, &p);
        list_checked(&mut env, &pool);
        let rm = env.ray_market_listed(
            RaySpec {
                fixture: FART,
                lend: 0,
                warmup: 0,
            },
            pool,
        );
        let kp = env.funded_keypair();
        let account = fork::fund_token(
            &mut env,
            &kp.pubkey(),
            &rm.keys.mint,
            TokenProgram::Spl,
            lend,
        );
        deposit_checked(&mut env, &rm, &kp, &account, lend);
        let window = i64::from(env.config().params.pool_list_window_s);
        // The keeper's observations at the end of the listing window warm the price state up
        // (M4V-5) before the first open.
        env.warp(window - raydium::PRICE_WARMUP_CRANKS);
        env.warm_up_price(&rm);
        crank_checked(&mut env, &rm);
        let keeper = env.funded_keypair();
        let whale = actor(&mut env, &rm, 60_000 * SOL, 40_000_000_000_000);
        Self {
            env,
            rm,
            lender: Lender { kp, account },
            keeper,
            whale,
        }
    }

    pub fn market(&self) -> Market {
        self.env.market(&self.rm.market())
    }

    pub fn spot(&self) -> u128 {
        spot(&self.env, &self.rm)
    }

    /// One keeper second with no trade.
    pub fn second(&mut self) {
        tick_checked(&mut self.env, &self.rm, 1);
    }

    /// `n` keeper seconds with no trade.
    pub fn tick(&mut self, n: u32) {
        tick_checked(&mut self.env, &self.rm, n);
    }

    /// One keeper second in which the whale first moves the pool to `target`.
    pub fn second_at(&mut self, target: u128) {
        let whale = self.whale.clone_trader();
        honest_second(&mut self.env, &self.rm, &whale, Some(target));
    }

    /// The whale moves the pool to `target` now (no crank).
    pub fn move_to(&mut self, target: u128) {
        let whale = self.whale.clone_trader();
        move_to(&mut self.env, &self.rm, &whale, target);
    }

    /// The whale moves the pool by `mult` now.
    pub fn move_by(&mut self, mult: f64) {
        let t = scale(self.spot(), mult);
        self.move_to(t);
    }

    /// A checked open of `q` by a new borrower.
    pub fn open(&mut self, q: u64) -> (Borrower, u64, Open) {
        let mut b = self.env.borrower();
        let (ev, nonce) = open_checked(&mut self.env, &self.rm, &mut b, q);
        (b, nonce, ev)
    }

    /// The position `nonce` of `b`.
    pub fn pos(&self, b: &Borrower, nonce: u64) -> ShortPosition {
        self.env.position(&self.rm, &b.key(), nonce)
    }

    /// The oracle's prediction for `b`'s position now (a same-second crank on a clone).
    pub fn predict(&mut self, b: &Pubkey, nonce: u64) -> Predicted {
        predict(&view(&mut self.env, &self.rm, b, nonce))
    }

    /// A checked keeper exit by the keeper.
    pub fn exit(&mut self, which: Which, b: &Pubkey, nonce: u64) -> Exit {
        let k = self.keeper.insecure_clone();
        exit_alone(&mut self.env, &self.rm, which, &k, b, nonce)
    }

    /// A checked `sol_settle` by the keeper.
    pub fn settle(&mut self, b: &Pubkey, nonce: u64) -> Settled {
        let k = self.keeper.insecure_clone();
        settle_checked(&mut self.env, &self.rm, &k, b, nonce)
    }

    /// Real buys ramp the pool geometrically to `mult`× its spot over `ramp_s` seconds (a
    /// checked crank each second), then the keeper cranks with the pool held until the oracle
    /// says `b`'s position `nonce` is CR-liquidatable (spot and EMA legs), at most `hold_s`
    /// seconds. Returns the seconds held.
    pub fn pump_until_liquidatable(
        &mut self,
        b: &Pubkey,
        nonce: u64,
        mult: f64,
        ramp_s: u32,
        hold_s: u32,
    ) -> u32 {
        let p0 = self.spot();
        for k in 1..=ramp_s {
            self.second_at(scale(p0, mult.powf(f64::from(k) / f64::from(ramp_s))));
        }
        for held in 0..=hold_s {
            if self.predict(b, nonce).cr_liquidatable() {
                return held;
            }
            self.second();
        }
        panic!("not liquidatable after a ×{mult} pump held {hold_s} s");
    }

    /// The lender's keys.
    pub fn lender_keys(&self) -> ix::LenderKeys {
        lender_keys(&self.rm, &self.lender.kp.pubkey(), &self.lender.account)
    }
}

/// A copy of a [`Trader`]'s keys (the harness type is not `Clone`).
pub trait CloneTrader {
    fn clone_trader(&self) -> Trader;
}

impl CloneTrader for Trader {
    fn clone_trader(&self) -> Trader {
        Trader {
            owner: self.owner.insecure_clone(),
            wsol: self.wsol,
            token: self.token,
        }
    }
}

/// `K_s = min(fixed + ⌊V·bps/10⁴⌋, cap, V)` with the position's locked terms.
pub fn k_s(p: &ShortPosition) -> u64 {
    risk::k_s(p.v, &p.settle_keeper_terms()).unwrap()
}

/// The spot multiple at which the position's S1 live leg starts to hold (`B̂(q) + ert > V −
/// K_s` on the pool moved along its constant product; the attack scripts' `s1_boundary`).
pub fn s1_boundary(env: &Env, rm: &RayMarket, p: &ShortPosition) -> f64 {
    let room = u128::from(p.v - k_s(p));
    let (x, y) = reserves(env, rm).expect("reserves");
    let (x, y) = (x as f64, y as f64);
    let fee = pool_fee(env, rm, u64::from(p.venue_fee_hard_cap_bps));
    let cost = |m: f64| -> Option<u128> {
        let x1 = (x / m.sqrt()).floor();
        let y1 = (y * m.sqrt()).ceil();
        Pricing::RayV4(fee)
            .buy(p.q, x1 as u128, y1 as u128)
            .map(|b| u128::from(b.cost))
    };
    let (mut lo, mut hi) = (1.0f64, 50.0f64);
    for _ in 0..80 {
        let mid = (lo + hi) / 2.0;
        match cost(mid) {
            Some(c) if c + u128::from(p.ert) <= room => lo = mid,
            _ => hi = mid,
        }
    }
    hi
}

/// The Raydium pool admin's `SetParams` status change on the real program (the admin's
/// signature forged; the hooks run).
pub fn set_status(env: &mut Env, rm: &RayMarket, status: u64) {
    ok(fork::raydium_admin_send(
        env,
        fork::v4_set_params_ix(v4(rm), 0, status),
    ));
}

/// The largest `q` the SPEC and fee-scaled caps admit on the market now (bisection on the real
/// instruction, on a clone).
pub fn max_open_q(f: &mut Fart) -> u64 {
    let (x, _) = reserves(&f.env, &f.rm).unwrap();
    let (mut lo, mut hi) = (0u64, x / 20);
    let saved = f.env.svm.clone();
    while hi - lo > x / 100_000 {
        let mid = lo + (hi - lo) / 2;
        let mut b = f.env.borrower();
        let ok = f.env.open(&f.rm, &mut b, mid).0.is_ok();
        f.env.svm = saved.clone();
        if ok {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

// ---- a thin pool by LP removal (S2) -----------------------------------------------------------

/// The governance corner, every value inside its compiled bounds (ARCH §2.3.3), under which a
/// pool thinned by LP removal can reach S2: `λ_min` 2,000 bps (the bound's floor), Tier C
/// `σ_max` 500 bps (the floor), `fee_size_cap_x100` 500 (the ceiling), and the sizing limits
/// lifted ([`lift_notional`]). At the defaults (`λ_min` 50 %, `σ_max` 20 %, `3·φ`) a position
/// at the size caps cannot get there: withdrawing every unlocked LP token leaves at least half
/// the depth it was sized on (ARCH §2.6 rule 5).
pub fn s2_corner(a: &mut InitConfigArgs) {
    lift_notional(a);
    a.params.pool_lock_min_bps = 2_000;
    a.params.fee_size_cap_x100 = 500;
    a.tiers[Tier::C as usize].max_liq_slip_bps = 500;
}

/// A position on a pool thinned into S2 territory (see [`thin_pool_market`]).
pub struct ThinPool {
    pub f: Fart,
    pub b: Borrower,
    pub nonce: u64,
    pub open: Open,
    pub pos: ShortPosition,
    /// The `(SOL, token)` amounts the LP withdrawal took out.
    pub removed: (u64, u64),
}

/// Fartcoin under [`s2_corner`] with its LP redistributed before listing (λ = 20 %, a holder
/// owns every circulating LP token), one position at the fee-scaled size cap `5·φ·X₀`
/// (1.25 % of the pool), then the holder withdraws every unlocked LP token (80 % of the pool
/// leaves: σ of the buyback rises above `σ_max` while the position is still healthy) and a real
/// pump on the thin pool makes the position liquidatable, short of the S1 boundary.
pub fn thin_pool_market() -> ThinPool {
    let lp_holder = Keypair::new();
    let holder_key = lp_holder.pubkey();
    let mut lp_acc = Pubkey::default();
    let mut f = Fart::with_pool(fork_env_with(s2_corner), 2_000_000_000_000, |env, p| {
        lp_acc = hold_unlocked_lp(env, p, &holder_key, 2_000).0;
    });
    assert_eq!(lambda_bps(&f.env, &f.rm), 2_000);
    // AIR-13: the listing started `L_slow` at the pool's locked depth `D`, 20 % of its √(X·Y)
    // here; five hours of the keeper's 60-s observations bring it within 1 % of the pool
    // (τ_slow 1 h) before the open at the fee-scaled cap, which `X₀ = min(X, L_slow/√P_ema, …)`
    // bounds.
    for _ in 0..300 {
        f.env.warp(60);
        crank_checked(&mut f.env, &f.rm);
    }
    let q = max_open_q(&mut f);
    let (x, _) = reserves(&f.env, &f.rm).unwrap();
    assert!(
        q * 10_000 >= x * 120 && q * 10_000 <= x * 125,
        "q ≈ 1.25 % of X: {q} of {x}"
    );
    let (b, nonce, open) = f.open(q);
    let pos = f.pos(&b, nonce);
    assert_eq!(pos.max_liq_slip_bps, 500);
    f.tick(10);
    let holder = Trader {
        owner: lp_holder.insecure_clone(),
        wsol: fork::fund_wsol(&mut f.env, &holder_key, 0),
        token: fork::fund_token(
            &mut f.env,
            &holder_key,
            &f.rm.keys.mint,
            TokenProgram::Spl,
            0,
        ),
    };
    let held = tokens(&f.env, &lp_acc);
    let (coin, pc) = remove_liquidity(&mut f.env, &f.rm, &holder, &lp_acc, held);
    let removed = if v4(&f.rm).sol_is_coin {
        (coin, pc)
    } else {
        (pc, coin)
    };
    assert!(
        removed.0 > 30_000 * SOL && removed.1 > 0,
        "80 % of the pool left"
    );
    f.tick(90);
    let p = f.predict(&b.key(), nonce);
    assert!(
        p.sigma_live > u64::from(pos.max_liq_slip_bps) && !p.cr_liquidatable(),
        "thin (σ {} bps) but healthy",
        p.sigma_live
    );
    f.pump_until_liquidatable(&b.key(), nonce, 2.2, 5, 60);
    assert!(
        s1_boundary(&f.env, &f.rm, &pos) > 1.1,
        "no gap: S1 is not due"
    );
    ThinPool {
        f,
        b,
        nonce,
        open,
        pos,
        removed,
    }
}

/// The liquidity the liquidation spot leg is judged at (FITS-2 (a); an independent spelling of
/// `shortwin_math::bounds::spot_liquidity`): `max(L_live, L_floor)`, `L_floor = min(L_ema,
/// L_mhi)` — the upper median of the bucket liquidity highs (AIR-17) — while the post-change guard
/// is set, else `L_ema`.
pub fn spot_liquidity_of(l_live: u128, obs: &shortwin_math::observe::Observation) -> u128 {
    let l_floor = if obs.ema.guard != 0 {
        let mut highs: Vec<u128> = obs.ring.iter().map(|e| e.lhi).collect();
        highs.sort_unstable();
        obs.ema.liq.min(highs[highs.len() / 2])
    } else {
        obs.ema.liq
    };
    l_live.max(l_floor)
}
