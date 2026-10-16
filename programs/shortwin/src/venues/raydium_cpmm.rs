//! Raydium CPMM adapter (ARCH §3.12 "RaydiumCpmm account sets", §2.4.1 Raydium row, §2.6,
//! §4.1–§4.3, §5.3; R-RAY §2.4–§2.5, §3.2–§3.3, §4, §5.2–§5.3, §6.2, §7, §9.2–§9.3). Program
//! `CPMMoo8…`, an Anchor program: 8-byte discriminators.
//!
//! **Account sets** (order binding; W = writable in trades; every slot mandatory and
//! key-checked against a compile-time constant or the market's pool cache, C0-2):
//!
//! - ReadSet ([`read_keys`]): R0 `pool` (`PoolState`, W), R1 `amm_config`, R2 `token_0_vault`
//!   (W), R3 `token_1_vault` (W), R4 `lp_mint`. Which token is WSOL is the market's flag
//!   `sol_is_token0_or_coin`.
//! - TradeExtras ([`trade_keys`]): X0 the CPMM program, X1 its authority `GpMZb…`, X2
//!   `observation` (W), X3 Tokenkeg (the WSOL side's program), X4 the token side's program
//!   (`== pool.token_x_program == market.token_program`: Tokenkeg or Token-2022), X5 the WSOL
//!   mint, X6 System, X7 `trader_wsol` (W). The token mint is the instruction's named `mint`.
//! - CPI (13 accounts): `[trader S (payer), authority, amm_config, pool W, input W, output W,
//!   input_vault W, output_vault W, input_program, output_program, input_mint, output_mint,
//!   observation W]`, then the extras; `swap_base_input(amount_in, minimum_amount_out)`
//!   (`8fbe5adac41e33de`; sell: input `pool_vault` with `trader` as its delegate, output
//!   `trader_wsol`) or `swap_base_output(max_amount_in, amount_out)` (`37d96256a34ab4ad`; buy:
//!   input `trader_wsol`, output `pool_vault`).
//!
//! **Pricing** ([`read_state`]; ARCH §4.1–§4.3): reserves `vault_i − (protocol_i + fund_i +
//! creator_i)`, `X` the token side and `Y` the WSOL side, real SOL `R = Y`; fees per direction
//! from the pool's `AmmConfig` (`fees::cpmm`: the trade fee `t`, the creator fee `c` only if the
//! pool enabled it, charged on the input or the output as `creator_fee_on` says), one-way `φ =
//! t + c` ppm; quotes `quote::cpmm`, which the dumped ELF executes exactly, creator fee on either
//! side included (T-RAY-MATH). The locked depth `D` from `lp_supply` @333 and the LP mint.
//!
//! **Venue predicate** (`venue_executable`, ARCH §5.3): the accounts parse and still describe
//! the adopted pool (its config, vaults, LP mint, observation, WSOL side, token mint and both
//! programs), the fees resolve (`t + c < 10⁶`, `creator_fee_on ≤ 2`), and the reader's predicate
//! holds (swap bit clear, `now ≥ open_time`, both vaults at or above their counters, both
//! reserves non-zero, and a buy can split its fee: not `t + c_eff = 0` with the creator fee on
//! the buy's input, where every buy reverts with `ZeroTradingTokens` — M3-SEC-1;
//! `shortwin_venue_state::raydium_cpmm`). Anything else is "not executable" (A63).
//!
//! **Risk view** ([`fee_view`], `super::risk_pricing`): the fees capped at the position's hard
//! cap (`CpmmFees::capped`), under which a zero fee is fee-free on either side, so a zero-fee
//! pool never makes a risk quote (a CR leg, the box, FV) infinite (M3-SEC-1, ARCH §4.3).
//!
//! **Verification** ([`verify`]; ARCH §2.6): owner, exact length 637 and discriminator;
//! `{token_0_mint, token_1_mint} = {WSOL, mint}`; the WSOL side under Tokenkeg (the Token-2022
//! native mint is never WSOL), the token side under the mint's own program; the passed config,
//! vaults and LP mint are the pool's (the config a CPMM `AmmConfig`, the vaults accounts of the
//! right mints and programs under the authority); then `eligible()`. Age `min(now, open_time)`
//! when `open_time ≥ T_CLAMP`, else the listing time (SPEC §4 O13, R3B-12; R-RAY §7).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use shortwin_math::fees::{self, CpmmConfig, CpmmFees, CpmmPoolFees, Direction};
use shortwin_math::pool_select;
use shortwin_math::quote::Pricing;
use shortwin_venue_state::raydium_cpmm::{self, AmmConfig, PoolState};
use shortwin_venue_state::token_account::TokenAccount;

use super::raydium::{
    at, cached, check_eligible, check_keys, delegate_sell, direct_buy, invoke_as_trader, lp_supply,
    sol_first, token_of, Adoption,
};
use super::{BuyOut, Leg, VenueState};
use crate::constants::{
    RAYDIUM_CPMM_AUTHORITY, RAYDIUM_CPMM_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
};
use crate::errors::ShortwinError;
use crate::state::{raydium_cache, Market, Params, VenueKind};

/// ReadSet length.
pub const READ_LEN: usize = 5;
/// TradeExtras length.
pub const TRADE_LEN: usize = 8;

/// ReadSet slots.
pub const R_POOL: usize = 0;
pub const R_AMM_CONFIG: usize = 1;
pub const R_TOKEN_0_VAULT: usize = 2;
pub const R_TOKEN_1_VAULT: usize = 3;
pub const R_LP_MINT: usize = 4;
/// TradeExtras slots.
pub const X_PROGRAM: usize = 0;
pub const X_AUTHORITY: usize = 1;
pub const X_OBSERVATION: usize = 2;
pub const X_WSOL_PROGRAM: usize = 3;
pub const X_TOKEN_SIDE_PROGRAM: usize = 4;
pub const X_WSOL_MINT: usize = 5;
pub const X_SYSTEM: usize = 6;
pub const X_TRADER_WSOL: usize = 7;

/// `swap_base_input`: `sha256("global:swap_base_input")[..8]`.
pub const IX_SWAP_BASE_INPUT: [u8; 8] = [0x8f, 0xbe, 0x5a, 0xda, 0xc4, 0x1e, 0x33, 0xde];
/// `swap_base_output`: `sha256("global:swap_base_output")[..8]`.
pub const IX_SWAP_BASE_OUTPUT: [u8; 8] = [0x37, 0xd9, 0x62, 0x56, 0xa3, 0x4a, 0xb4, 0xad];

/// `(token_0_vault, token_1_vault)` of the market's pool.
fn vaults(market: &Market) -> (Pubkey, Pubkey) {
    let (sol, tok) = (
        cached(market, raydium_cache::RAY_VAULT_SOL),
        cached(market, raydium_cache::RAY_VAULT_TOK),
    );
    if sol_first(market) {
        (sol, tok)
    } else {
        (tok, sol)
    }
}

/// The ReadSet keys of `market` in order: pool, config, token 0 vault, token 1 vault, LP mint.
pub fn read_keys(market: &Market) -> [Pubkey; READ_LEN] {
    let (v0, v1) = vaults(market);
    [
        cached(market, raydium_cache::RAY_POOL),
        cached(market, raydium_cache::RAY_AMM_CONFIG),
        v0,
        v1,
        cached(market, raydium_cache::RAY_LP_MINT),
    ]
}

/// The TradeExtras keys of `market` in order (X2 `observation` and X7 `trader_wsol` are
/// writable in the CPI).
pub fn trade_keys(market: &Market) -> [Pubkey; TRADE_LEN] {
    [
        RAYDIUM_CPMM_PROGRAM_ID,
        RAYDIUM_CPMM_AUTHORITY,
        cached(market, raydium_cache::RAY_OBSERVATION),
        TOKEN_PROGRAM_ID,
        market.token_program,
        WSOL_MINT,
        anchor_lang::system_program::ID,
        market.trader_wsol,
    ]
}

/// Key-checks the ReadSet against the market's pool cache.
pub fn check_read(market: &Market, read: &[AccountInfo]) -> Result<()> {
    check_keys(read, &read_keys(market))
}

/// Key-checks the TradeExtras against the compile-time constants and the market; the program
/// slot must be executable.
pub fn check_trade(market: &Market, trade: &[AccountInfo]) -> Result<()> {
    check_keys(trade, &trade_keys(market))?;
    require!(
        at(trade, X_PROGRAM)?.executable,
        ShortwinError::VenueAccountMismatch
    );
    Ok(())
}

/// A parsed ReadSet: the pool, its config, both vaults (keyed and minted as the pool records
/// them, owned by the pool's token programs) and the LP mint's supply.
struct Parsed {
    pool: PoolState,
    config: AmmConfig,
    v0: TokenAccount,
    v1: TokenAccount,
    lp_supply: u64,
}

fn pool_state(info: &AccountInfo) -> Option<PoolState> {
    let data = info.try_borrow_data().ok()?;
    raydium_cpmm::read_pool(&info.owner.to_bytes(), &data).ok()
}

fn amm_config(info: &AccountInfo) -> Option<AmmConfig> {
    let data = info.try_borrow_data().ok()?;
    raydium_cpmm::read_config(&info.owner.to_bytes(), &data).ok()
}

fn parse(read: &[AccountInfo]) -> Option<Parsed> {
    let (pool_i, cfg_i, v0_i, v1_i, lp_i) = (
        read.get(R_POOL)?,
        read.get(R_AMM_CONFIG)?,
        read.get(R_TOKEN_0_VAULT)?,
        read.get(R_TOKEN_1_VAULT)?,
        read.get(R_LP_MINT)?,
    );
    let pool = pool_state(pool_i)?;
    if pool.amm_config != cfg_i.key.to_bytes()
        || pool.token_0_vault != v0_i.key.to_bytes()
        || pool.token_1_vault != v1_i.key.to_bytes()
        || pool.lp_mint != lp_i.key.to_bytes()
    {
        return None;
    }
    let config = amm_config(cfg_i)?;
    let v0 = token_of(v0_i, &Pubkey::new_from_array(pool.token_0_program))?;
    let v1 = token_of(v1_i, &Pubkey::new_from_array(pool.token_1_program))?;
    if v0.mint != pool.token_0_mint || v1.mint != pool.token_1_mint {
        return None;
    }
    Some(Parsed {
        pool,
        config,
        v0,
        v1,
        lp_supply: lp_supply(lp_i)?,
    })
}

/// `fees::cpmm` of `pool` on `config` for direction `dir`, WSOL being token 0 or not.
fn fees_for(
    pool: &PoolState,
    config: &AmmConfig,
    sol_is_token0: bool,
    dir: Direction,
) -> Option<CpmmFees> {
    fees::cpmm(
        &CpmmConfig {
            trade_fee_rate: config.trade_fee_rate,
            protocol_fee_rate: config.protocol_fee_rate,
            fund_fee_rate: config.fund_fee_rate,
            creator_fee_rate: config.creator_fee_rate,
        },
        &CpmmPoolFees {
            enable_creator_fee: pool.enable_creator_fee,
            creator_fee_on: pool.creator_fee_on,
            sol_is_token0,
        },
        dir,
    )
    .ok()
}

/// The state of an executable pool (module docs), `None` otherwise.
fn state_of(p: &Parsed, now: i64) -> Option<VenueState> {
    let sol0 = p.pool.sol_is_token0()?;
    let fee_ppm = p.pool.fee_ppm(&p.config)?;
    let buy = fees_for(&p.pool, &p.config, sol0, Direction::Buy)?;
    let sell = fees_for(&p.pool, &p.config, sol0, Direction::Sell)?;
    // The reader's predicate includes M3-SEC-1's fee split: a buy whose creator fee is on its
    // input at `t + c = 0` reverts in the venue (`ZeroTradingTokens`), so no exit can execute.
    if !p
        .pool
        .venue_executable(&p.config, now, p.v0.amount, p.v1.amount)
    {
        return None;
    }
    let (r0, r1) = p.pool.reserves(p.v0.amount, p.v1.amount)?;
    let (x, y) = if sol0 { (r1, r0) } else { (r0, r1) };
    let locked_depth = pool_select::locked_depth(p.pool.lp_supply, p.lp_supply, x, y).ok()?;
    Some(VenueState {
        kind: VenueKind::RaydiumCpmm,
        executable: true,
        x,
        y,
        r: y,
        pricing: Some(Pricing::Cpmm { buy, sell }),
        fee_ppm,
        locked_depth,
        curve: None,
        pool: None,
    })
}

/// `(sol mint, token mint, sol program, token program)` of `pool` with WSOL on token 0 or not.
fn sides(pool: &PoolState, sol0: bool) -> ([u8; 32], [u8; 32], [u8; 32], [u8; 32]) {
    if sol0 {
        (
            pool.token_0_mint,
            pool.token_1_mint,
            pool.token_0_program,
            pool.token_1_program,
        )
    } else {
        (
            pool.token_1_mint,
            pool.token_0_mint,
            pool.token_1_program,
            pool.token_0_program,
        )
    }
}

/// The pool still describes the market's adoption: the WSOL side the market recorded under
/// Tokenkeg, the market's mint under the market's token program on the other side, the
/// observation account the market cached.
fn consistent(market: &Market, pool: &PoolState) -> bool {
    let sol0 = sol_first(market);
    let (_, tok_mint, sol_prog, tok_prog) = sides(pool, sol0);
    pool.sol_is_token0() == Some(sol0)
        && tok_mint == market.mint.to_bytes()
        && sol_prog == TOKEN_PROGRAM_ID.to_bytes()
        && tok_prog == market.token_program.to_bytes()
        && pool.observation_key == cached(market, raydium_cache::RAY_OBSERVATION).to_bytes()
}

/// The venue state from a key-checked ReadSet at clock time `now` (module docs). Never fails:
/// whatever does not parse or no longer matches the adoption is "not executable" (A63).
pub fn read_state(market: &Market, read: &[AccountInfo], now: i64) -> VenueState {
    parse(read)
        .filter(|p| consistent(market, &p.pool))
        .and_then(|p| state_of(&p, now))
        .unwrap_or_else(|| VenueState::not_executable(VenueKind::RaydiumCpmm))
}

/// The fee view for risk quotes whether or not the pool can execute (`super::risk_pricing`):
/// the pool's resolved fees while the pool and its config parse (a halted pool keeps its fee),
/// else the cap itself as a trade fee (`φ_live` unknown: a layout change); capped at
/// `cap_bps` (`CpmmFees::capped`, which prices a zero fee fee-free: a config whose trade fee
/// the admin set to 0 leaves every risk quote defined, M3-SEC-1). Reads only the key-checked
/// pool and config slots.
pub fn fee_view(market: &Market, read: &[AccountInfo], cap_bps: u64) -> Pricing {
    live_fees(market, read)
        .unwrap_or_else(|| {
            let cap = CpmmFees {
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
        .capped(cap_bps)
}

/// The pool's resolved fees for both directions while the pool and the config it names parse.
fn live_fees(market: &Market, read: &[AccountInfo]) -> Option<Pricing> {
    let sol0 = sol_first(market);
    let (pool_i, cfg_i) = (read.get(R_POOL)?, read.get(R_AMM_CONFIG)?);
    let pool = pool_state(pool_i)?;
    if pool.amm_config != cfg_i.key.to_bytes() {
        return None;
    }
    let config = amm_config(cfg_i)?;
    Some(Pricing::Cpmm {
        buy: fees_for(&pool, &config, sol0, Direction::Buy)?,
        sell: fees_for(&pool, &config, sol0, Direction::Sell)?,
    })
}

/// `create_market(venue_hint = RaydiumCpmm)` and pool selection: verifies the candidate ReadSet
/// `read` for `mint` under `token_program` (module docs, ARCH §2.6) and checks `eligible()` at
/// the reference price `p_ref` (`raydium::check_eligible`).
///
/// Errors: `BadRemainingAccounts` (not exactly the ReadSet); `PoolIneligible` (not a CPMM
/// `PoolState` of 637 bytes, a config that is not a CPMM `AmmConfig`, not a WSOL pool of
/// `mint`, WSOL not under Tokenkeg or the token side not under the mint's program, vaults that
/// are not the authority's accounts of the pool's mints and programs, an LP mint that does not
/// parse, a pool that cannot execute or whose fee is above `venue_fee_cap_bps`);
/// `VenueAccountMismatch` (the config, a vault or the LP mint is not the pool's); `PoolTooThin`
/// (the locked depth under the floor, M4-SEC-1 / M4-SEC-3).
pub fn verify(
    read: &[AccountInfo],
    mint: &Pubkey,
    token_program: &Pubkey,
    params: &Params,
    now: i64,
    p_ref: Option<u128>,
) -> Result<Adoption> {
    require!(read.len() == READ_LEN, ShortwinError::BadRemainingAccounts);
    let pool_i = at(read, R_POOL)?;
    require_keys_eq!(
        *pool_i.owner,
        RAYDIUM_CPMM_PROGRAM_ID,
        ShortwinError::PoolIneligible
    );
    require!(
        pool_i.data_len() == raydium_cpmm::POOL_LEN,
        ShortwinError::PoolIneligible
    );
    let pool = pool_state(pool_i).ok_or(ShortwinError::PoolIneligible)?;
    let (cfg_i, v0_i, v1_i, lp_i) = (
        at(read, R_AMM_CONFIG)?,
        at(read, R_TOKEN_0_VAULT)?,
        at(read, R_TOKEN_1_VAULT)?,
        at(read, R_LP_MINT)?,
    );
    require!(
        cfg_i.key.to_bytes() == pool.amm_config
            && v0_i.key.to_bytes() == pool.token_0_vault
            && v1_i.key.to_bytes() == pool.token_1_vault
            && lp_i.key.to_bytes() == pool.lp_mint,
        ShortwinError::VenueAccountMismatch
    );
    require_keys_eq!(
        *cfg_i.owner,
        RAYDIUM_CPMM_PROGRAM_ID,
        ShortwinError::PoolIneligible
    );
    amm_config(cfg_i).ok_or(ShortwinError::PoolIneligible)?;
    let sol0 = pool.sol_is_token0().ok_or(ShortwinError::PoolIneligible)?;
    let (_, tok_mint, sol_prog, tok_prog) = sides(&pool, sol0);
    require!(
        tok_mint == mint.to_bytes()
            && sol_prog == TOKEN_PROGRAM_ID.to_bytes()
            && tok_prog == token_program.to_bytes(),
        ShortwinError::PoolIneligible
    );
    let authority = RAYDIUM_CPMM_AUTHORITY.to_bytes();
    for (vault, want_mint, program) in [
        (v0_i, pool.token_0_mint, pool.token_0_program),
        (v1_i, pool.token_1_mint, pool.token_1_program),
    ] {
        let t = token_of(vault, &Pubkey::new_from_array(program))
            .ok_or(ShortwinError::PoolIneligible)?;
        require!(
            t.mint == want_mint && t.owner == authority,
            ShortwinError::PoolIneligible
        );
    }
    let parsed = parse(read).ok_or(ShortwinError::PoolIneligible)?;
    let state = state_of(&parsed, now).ok_or(ShortwinError::PoolIneligible)?;
    check_eligible(&state, params, p_ref)?;
    let (vault_sol, vault_tok) = if sol0 {
        (v0_i.key(), v1_i.key())
    } else {
        (v1_i.key(), v0_i.key())
    };
    Ok(Adoption {
        kind: VenueKind::RaydiumCpmm,
        pool: pool_i.key(),
        vault_sol,
        vault_tok,
        amm_config: cfg_i.key(),
        observation: Pubkey::new_from_array(pool.observation_key),
        lp_mint: lp_i.key(),
        tok_program: *token_program,
        sol_first: sol0,
        state,
        age_start: raydium_cpmm::age_start(pool.open_time, now),
    })
}

/// One direction of a CPMM swap.
struct Sides<'a, 'info> {
    input: &'a AccountInfo<'info>,
    output: &'a AccountInfo<'info>,
    input_vault: &'a AccountInfo<'info>,
    output_vault: &'a AccountInfo<'info>,
    input_program: &'a AccountInfo<'info>,
    output_program: &'a AccountInfo<'info>,
    input_mint: &'a AccountInfo<'info>,
    output_mint: &'a AccountInfo<'info>,
}

/// `(sol vault, token vault)` of the leg's ReadSet.
fn leg_vaults<'a, 'info>(
    leg: &'a Leg<'info>,
) -> Result<(&'a AccountInfo<'info>, &'a AccountInfo<'info>)> {
    let (v0, v1) = (
        at(leg.read, R_TOKEN_0_VAULT)?,
        at(leg.read, R_TOKEN_1_VAULT)?,
    );
    Ok(if leg.sol_first { (v0, v1) } else { (v1, v0) })
}

/// The swap CPI (module docs), `trader` signing as the payer (the input's owner or delegate).
fn swap<'info>(
    leg: &Leg<'info>,
    disc: [u8; 8],
    a: u64,
    b: u64,
    s: &Sides<'_, 'info>,
) -> Result<()> {
    let pool = at(leg.read, R_POOL)?;
    let config = at(leg.read, R_AMM_CONFIG)?;
    let program = at(leg.trade, X_PROGRAM)?;
    let authority = at(leg.trade, X_AUTHORITY)?;
    let observation = at(leg.trade, X_OBSERVATION)?;
    let trader = &leg.trader.trader;
    let metas = vec![
        AccountMeta::new_readonly(trader.key(), true),
        AccountMeta::new_readonly(authority.key(), false),
        AccountMeta::new_readonly(config.key(), false),
        AccountMeta::new(pool.key(), false),
        AccountMeta::new(s.input.key(), false),
        AccountMeta::new(s.output.key(), false),
        AccountMeta::new(s.input_vault.key(), false),
        AccountMeta::new(s.output_vault.key(), false),
        AccountMeta::new_readonly(s.input_program.key(), false),
        AccountMeta::new_readonly(s.output_program.key(), false),
        AccountMeta::new_readonly(s.input_mint.key(), false),
        AccountMeta::new_readonly(s.output_mint.key(), false),
        AccountMeta::new(observation.key(), false),
    ];
    let infos = vec![
        trader.clone(),
        authority.clone(),
        config.clone(),
        pool.clone(),
        s.input.clone(),
        s.output.clone(),
        s.input_vault.clone(),
        s.output_vault.clone(),
        s.input_program.clone(),
        s.output_program.clone(),
        s.input_mint.clone(),
        s.output_mint.clone(),
        observation.clone(),
        program.clone(),
    ];
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&disc);
    data.extend_from_slice(&a.to_le_bytes());
    data.extend_from_slice(&b.to_le_bytes());
    invoke_as_trader(leg, RAYDIUM_CPMM_PROGRAM_ID, data, metas, infos)
}

/// The sell leg of an open (ARCH §2.4.1; `super::raydium::delegate_sell`):
/// `swap_base_input(q, min_out)` from `pool_vault` (trader as delegate for exactly `q`) into
/// `trader_wsol`.
pub fn sell<'info>(
    leg: &Leg<'info>,
    borrower: &AccountInfo<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    min_out: u64,
) -> Result<u64> {
    let (sol_vault, tok_vault) = leg_vaults(leg)?;
    let sides = Sides {
        input: &leg.pool_vault,
        output: leg.trader.trader_wsol()?,
        input_vault: tok_vault,
        output_vault: sol_vault,
        input_program: &leg.token_program,
        output_program: at(leg.trade, X_WSOL_PROGRAM)?,
        input_mint: &leg.mint,
        output_mint: at(leg.trade, X_WSOL_MINT)?,
    };
    delegate_sell(leg, borrower, position, q, || {
        swap(leg, IX_SWAP_BASE_INPUT, q, min_out, &sides)
    })
}

/// The buy leg of an exit (ARCH §2.4.1; `super::raydium::direct_buy`):
/// `swap_base_output(max_in, q)` from `trader_wsol` straight into `pool_vault`.
pub fn buy<'info>(
    leg: &Leg<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    max_in: u64,
) -> Result<BuyOut> {
    let (sol_vault, tok_vault) = leg_vaults(leg)?;
    let sides = Sides {
        input: leg.trader.trader_wsol()?,
        output: &leg.pool_vault,
        input_vault: sol_vault,
        output_vault: tok_vault,
        input_program: at(leg.trade, X_WSOL_PROGRAM)?,
        output_program: &leg.token_program,
        input_mint: at(leg.trade, X_WSOL_MINT)?,
        output_mint: &leg.mint,
    };
    direct_buy(leg, position, q, max_in, || {
        swap(leg, IX_SWAP_BASE_OUTPUT, max_in, q, &sides)
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use crate::constants::{TOKEN_2022_PROGRAM_ID, T_CLAMP};
    use bytemuck::Zeroable;
    use std::str::FromStr;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(
                "../../../../tests/fixtures/accounts/raydium/",
                $name,
                ".bin"
            ))
        };
    }

    fn key(s: &str) -> Pubkey {
        Pubkey::from_str(s).unwrap()
    }

    const NOW: i64 = 1_790_563_273;

    /// USELESS (`Q2sPHPdU…`): pool, config 0, WSOL vault (token 0), USELESS vault, LP mint.
    fn useless() -> Vec<(Pubkey, Pubkey, Vec<u8>)> {
        vec![
            (
                key("Q2sPHPdUWFMg7M7wwrQKLrn619cAucfRsmhVJffodSp"),
                RAYDIUM_CPMM_PROGRAM_ID,
                fixture!("cpmm_useless_pool").to_vec(),
            ),
            (
                key("D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2"),
                RAYDIUM_CPMM_PROGRAM_ID,
                fixture!("cpmm_config_00").to_vec(),
            ),
            (
                key("HgNPDD8bpbSrGyHegiCT5xrYxHTfwLfZydwGkjNCJRKA"),
                TOKEN_PROGRAM_ID,
                fixture!("cpmm_useless_token_0_vault").to_vec(),
            ),
            (
                key("9xsCiNwYQXM3ZeHFSVj9JQdP1vREJREpN23f6wvxA1ty"),
                TOKEN_PROGRAM_ID,
                fixture!("cpmm_useless_token_1_vault").to_vec(),
            ),
            (
                key("5rASbyrUYh4eVmZpgN6MxVY2w83dC4PFg9U9WYc9HW7g"),
                TOKEN_PROGRAM_ID,
                fixture!("cpmm_useless_lp_mint").to_vec(),
            ),
        ]
    }

    const USELESS_MINT: &str = "Dz9mQ9NzkBcCsuGPFJ3r1bS4wgqKMHBPiVuniW8Mbonk";

    fn with_infos<R>(
        accounts: &mut [(Pubkey, Pubkey, Vec<u8>)],
        f: impl FnOnce(&[AccountInfo]) -> R,
    ) -> R {
        let mut lamports: Vec<u64> = vec![1; accounts.len()];
        let infos: Vec<AccountInfo> = accounts
            .iter_mut()
            .zip(lamports.iter_mut())
            .map(|((k, o, d), l)| AccountInfo::new(k, false, false, l, d.as_mut_slice(), o, false))
            .collect();
        f(&infos)
    }

    #[test]
    fn verifies_adopts_and_reads_useless() {
        let mint = key(USELESS_MINT);
        let mut accounts = useless();
        let a = with_infos(&mut accounts, |r| {
            verify(r, &mint, &TOKEN_PROGRAM_ID, &Params::defaults(), NOW, None)
        })
        .unwrap();
        assert_eq!(a.kind, VenueKind::RaydiumCpmm);
        assert!(a.sol_first, "WSOL is token 0");
        assert_eq!((a.vault_sol, a.vault_tok), (accounts[2].0, accounts[3].0));
        assert_eq!(a.amm_config, accounts[1].0);
        assert_eq!(
            a.observation,
            key("4UdSz2kMddtX4woMmdgkWg75fdBP8FgYwqfkh4ri7mnD")
        );
        assert_eq!(a.age_start, 1_746_886_997, "the pool's open_time (O13)");
        assert_eq!(a.state.fee_ppm, 2_500);
        let mut m = Market::zeroed();
        m.mint = mint;
        m.token_program = TOKEN_PROGRAM_ID;
        a.apply(&mut m, NOW).unwrap();
        assert_eq!(
            read_keys(&m).to_vec(),
            accounts.iter().map(|(k, _, _)| *k).collect::<Vec<_>>()
        );
        let st = with_infos(&mut accounts, |r| read_state(&m, r, NOW));
        assert!(st.executable);
        assert_eq!((st.x, st.y), (a.state.x, a.state.y));
        // An open time before T_CLAMP proves no creation time: the age counts from the
        // listing (O13).
        let mut acc = useless();
        acc[0].2[raydium_cpmm::OFF_OPEN_TIME..raydium_cpmm::OFF_OPEN_TIME + 8]
            .copy_from_slice(&1_600_000_000u64.to_le_bytes());
        let a = with_infos(&mut acc, |r| {
            verify(r, &mint, &TOKEN_PROGRAM_ID, &Params::defaults(), NOW, None)
        })
        .unwrap();
        assert_eq!(a.age_start, NOW);
        // From T_CLAMP on, the open time is a valid lower bound on the pool's age.
        let mut acc = useless();
        acc[0].2[raydium_cpmm::OFF_OPEN_TIME..raydium_cpmm::OFF_OPEN_TIME + 8]
            .copy_from_slice(&T_CLAMP.unsigned_abs().to_le_bytes());
        let a = with_infos(&mut acc, |r| {
            verify(r, &mint, &TOKEN_PROGRAM_ID, &Params::defaults(), NOW, None)
        })
        .unwrap();
        assert_eq!(a.age_start, T_CLAMP);
    }

    #[test]
    fn halts_and_breaks_are_not_executable() {
        let mint = key(USELESS_MINT);
        let mut accounts = useless();
        let a = with_infos(&mut accounts, |r| {
            verify(r, &mint, &TOKEN_PROGRAM_ID, &Params::defaults(), NOW, None)
        })
        .unwrap();
        let mut m = Market::zeroed();
        m.mint = mint;
        m.token_program = TOKEN_PROGRAM_ID;
        a.apply(&mut m, NOW).unwrap();
        for status in [4u8, 5, 6, 7] {
            let mut acc = useless();
            acc[0].2[raydium_cpmm::OFF_STATUS] = status;
            assert!(!with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
            assert_eq!(
                with_infos(&mut acc, |r| fee_view(&m, r, 500)).oneway_ppm(),
                2_500
            );
        }
        for status in [1u8, 2, 3] {
            let mut acc = useless();
            acc[0].2[raydium_cpmm::OFF_STATUS] = status;
            assert!(with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
        }
        // A future open time; creator_fee_on outside {0, 1, 2}; the WSOL side under Token-2022.
        let mut acc = useless();
        acc[0].2[raydium_cpmm::OFF_OPEN_TIME..raydium_cpmm::OFF_OPEN_TIME + 8]
            .copy_from_slice(&((NOW + 1) as u64).to_le_bytes());
        assert!(!with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
        let mut acc = useless();
        acc[0].2[raydium_cpmm::OFF_CREATOR_FEE_ON] = 3;
        assert!(!with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
        let mut acc = useless();
        acc[0].2[raydium_cpmm::OFF_TOKEN_0_PROGRAM..raydium_cpmm::OFF_TOKEN_0_PROGRAM + 32]
            .copy_from_slice(TOKEN_2022_PROGRAM_ID.as_ref());
        assert!(!with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
        // A config that no longer parses: not executable, and the fee view falls back to the cap.
        let mut acc = useless();
        acc[1].2[0] ^= 1;
        assert!(!with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
        assert_eq!(
            with_infos(&mut acc, |r| fee_view(&m, r, 500)).oneway_ppm(),
            50_000
        );
    }

    /// USELESS's config with its trade fee at `trade` ppm and creator fee at `creator`, and the
    /// pool's creator-fee fields.
    fn with_rates(
        trade: u64,
        creator: u64,
        enable: bool,
        on: u8,
    ) -> Vec<(Pubkey, Pubkey, Vec<u8>)> {
        let mut acc = useless();
        let t = raydium_cpmm::OFF_CONFIG_TRADE_FEE_RATE;
        acc[1].2[t..t + 8].copy_from_slice(&trade.to_le_bytes());
        let c = raydium_cpmm::OFF_CONFIG_CREATOR_FEE_RATE;
        acc[1].2[c..c + 8].copy_from_slice(&creator.to_le_bytes());
        acc[0].2[raydium_cpmm::OFF_ENABLE_CREATOR_FEE] = u8::from(enable);
        acc[0].2[raydium_cpmm::OFF_CREATOR_FEE_ON] = on;
        acc
    }

    /// M3-SEC-1: `update_amm_config(0, 0)` on USELESS's config 0 (no creator fee): every buy
    /// reverts in the venue (`ZeroTradingTokens`), so the pool is not executable (no execution
    /// quote, the S3 chain runs), not eligible, and the risk view — halted or not — prices it
    /// fee-free instead of `+∞`. A zero fee that only the sell direction cannot split (creator
    /// fee on the token input) keeps the route: buys quote, the sale is undefined.
    #[test]
    fn a_zero_fee_on_the_buy_input_is_not_executable() {
        let mint = key(USELESS_MINT);
        let mut accounts = useless();
        let a = with_infos(&mut accounts, |r| {
            verify(r, &mint, &TOKEN_PROGRAM_ID, &Params::defaults(), NOW, None)
        })
        .unwrap();
        let mut m = Market::zeroed();
        m.mint = mint;
        m.token_program = TOKEN_PROGRAM_ID;
        a.apply(&mut m, NOW).unwrap();
        let q = a.state.x / 1_000;
        let fee_free = CpmmFees {
            trade: 0,
            creator: 0,
            creator_on_input: false,
            protocol: 120_000,
            fund: 40_000,
        };
        for halted in [false, true] {
            let mut acc = with_rates(0, 500, false, 0);
            if halted {
                acc[0].2[raydium_cpmm::OFF_STATUS] = 4;
            }
            let st = with_infos(&mut acc, |r| read_state(&m, r, NOW));
            assert!(!st.executable, "halted {halted}");
            assert_eq!(st.buy(q), None);
            let risk = with_infos(&mut acc, |r| fee_view(&m, r, 500));
            assert_eq!(
                risk,
                Pricing::Cpmm {
                    buy: fee_free,
                    sell: fee_free
                }
            );
            let b = risk
                .buy(q, u128::from(a.state.x), u128::from(a.state.y))
                .expect("a defined risk quote");
            assert_eq!(b.cost, b.net, "fee-free");
        }
        // Not eligible for a listing.
        let mut acc = with_rates(0, 0, true, 0);
        let err = with_infos(&mut acc, |r| {
            verify(r, &mint, &TOKEN_PROGRAM_ID, &Params::defaults(), NOW, None)
        })
        .unwrap_err();
        let code = match err {
            anchor_lang::error::Error::AnchorError(e) => Some(e.error_code_number),
            anchor_lang::error::Error::ProgramError(_) => None,
        };
        assert_eq!(code, Some(u32::from(ShortwinError::PoolIneligible)));
        // WSOL is token 0: `creator_fee_on = 2` puts the (zero) creator fee on the token, i.e.
        // on the sale's input and the buy's output. Buys execute; sales cannot.
        let mut acc = with_rates(0, 0, true, 2);
        let st = with_infos(&mut acc, |r| read_state(&m, r, NOW));
        assert!(st.executable);
        assert!(st.buy(q).is_some());
        assert!(st.sell(q).is_none(), "the venue fails the sale");
        assert!(st.buy_risk(q, 500).is_some());
        // `creator_fee_on = 1` puts it on WSOL, the buy's input: not executable.
        let mut acc = with_rates(0, 0, true, 1);
        assert!(!with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
        // Any non-zero rate splits.
        for (t, c, enable) in [(1, 0, false), (0, 1, true)] {
            let mut acc = with_rates(t, c, enable, 0);
            assert!(with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
        }
    }
}
