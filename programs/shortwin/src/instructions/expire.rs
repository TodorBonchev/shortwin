//! `expire(extras)` (ARCH §3.7; ECON §8.2, §8.4; SPEC §4 "Duration"): a keeper (anyone) buys
//! back an expired position that is **not** CR-liquidatable, within the expiry bound, and
//! splits what is left by the expiry waterfall: a 2 % expiry fee to lenders instead of the
//! liquidation penalty, and the expiry keeper reward `K_e` from the borrower's remainder.
//!
//! **Signer:** `keeper` (anyone; receives `K_e`, C0-1 (4)). **Named accounts:** as
//! `liquidate` (`close_short`'s plus `borrower`, mut, `== position.borrower`, which receives
//! the position close, A18). **Remaining:** `ReadSet ++ TradeExtras ++ Extras` (buy set).
//! Exits never check the venue guard, pause flags, the route-broken flag or the oracle (I10).
//!
//! **Preconditions** (ARCH §3.7, in this order): the prologue and the exit-path mint guard;
//! the venue trades and is executable (`NoRoute` / `VenueMigrating`); the post-change warm-up is
//! over (`EmaWarmingUp`, `liquidate`'s precondition 1b; M4R); `now ≥ deadline`
//! (`NotExpired`); a fresh completed bucket (`ReferenceStale`); **not** CR-liquidatable
//! (`UseLiquidate`: an expired position whose spot and EMA legs hold pays the penalty through
//! `liquidate`, ECON §8.4; `liquidate`'s own `assess`, the spot leg judged at `max(L_live,
//! L_ema')`, FITS-2 (a): a keeper cannot drain `L_live` inside its transaction to turn an expiry
//! into a penalised liquidation — a low `L_ema` reached by a genuine held withdrawal and frozen by
//! withdraw-wrapped cranks can, wherever it could force the liquidation: FITX-1, ECON §12.12
//! residual 10); normal buyback allowed (`UseSolSettle`); the expiry reference
//!
//! ```text
//! P_ref  = min(P_ema, min over the ring of p_i, lb_lo)          (nothing is urgent at expiry)
//! B̂_ref  = B̂(Q; reprice(max(L_live, min(max(L_ema', L_slow), cap)), P_ref))
//!                                            (risk quote; L_ema' = min(L_ema, L_mlo) while the
//!                                             post-change guard is set, M4R; cap =
//!                                             ⌊pool_d·10⁴/λ_min⌋ on Raydium and PumpSwap,
//!                                             none elsewhere, M4F-1, M6R-1)
//! π_ref  = ⌈expiry_fee_bps·B̂_ref/10⁴⌉
//! max_in = min(⌊B̂_ref·(10⁴ + δ_exp)/10⁴⌋, V − K_s − ert, max(V − R − π_ref − ert, ⌊B̂_ref·(10⁴ + ε)/10⁴⌋))
//! ```
//!
//! (`engine::bounds::{expiry_reference, max_in_exp}`): rent and the expiry fee stay senior
//! wherever the fair cost leaves room for them (v0.4.1 keeps this reserve for `expire` only);
//! `B̂_live ≤ max_in`, else `ExitBoundNotMet`.
//!
//! **Execution** (the machinery of `liquidate`, [`super::liquidate::keeper_exit`]): trader
//! normalisation, the buy leg with `max_in`, the waterfall `waterfall::expire` (`B` → `r =
//! min(R_due, V − B)` → the expiry fee `e = min(⌈expiry_fee_bps·min(B̂_live,risk,
//! B̂_ref)/10⁴⌉, V − B − r)`, 100 % to lenders, vesting → `K_e = min(fixed + ⌊bps·B̂_ref/10⁴⌋,
//! cap, V − B − r − e)` → the borrower), `pool::repay(r, e)`, the position aggregates, the
//! exposure. Event `Expire`.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use super::liquidate::{close_exposure, keeper_exit, ExitAccounts, ExitKind};
use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::errors::ShortwinError;
use crate::events::{Expire as ExpireEvent, MintSafetyTripped, TraderSwept, SWEEP_TO_VEST};
use crate::state::{
    GlobalConfig, Market, MarketVault, ShortPosition, WalletExposure, POOL_VAULT_SEED,
};
use crate::venues::ExtrasSel;

#[event_cpi]
#[derive(Accounts)]
pub struct Expire<'info> {
    /// The keeper (anyone).
    #[account(mut)]
    pub keeper: Signer<'info>,
    /// CHECK: `== position.borrower` (constraint on `position`); receives the position close.
    #[account(mut)]
    pub borrower: UncheckedAccount<'info>,
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    #[account(
        mut,
        seeds = [Market::SEED, mint.key().as_ref()],
        bump = market.load()?.bump,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion,
        has_one = mint @ ShortwinError::AccountMismatch,
        has_one = market_vault @ ShortwinError::AccountMismatch,
        has_one = pool_vault @ ShortwinError::AccountMismatch,
        has_one = token_program @ ShortwinError::AccountMismatch,
        has_one = trader @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    #[account(
        mut,
        close = borrower,
        has_one = borrower @ ShortwinError::AccountMismatch,
        has_one = market @ ShortwinError::AccountMismatch,
        constraint = position.is_pda(&position.key()) @ anchor_lang::error::ErrorCode::ConstraintSeeds
    )]
    pub position: Box<Account<'info, ShortPosition>>,
    #[account(
        mut,
        seeds = [WalletExposure::SEED, market.key().as_ref(), borrower.key().as_ref()],
        bump = exposure.bump,
        has_one = market @ ShortwinError::AccountMismatch,
        constraint = exposure.wallet == borrower.key() @ ShortwinError::AccountMismatch
    )]
    pub exposure: Box<Account<'info, WalletExposure>>,
    #[account(mint::token_program = token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [POOL_VAULT_SEED, market.key().as_ref()],
        bump = market.load()?.pool_vault_bump
    )]
    pub pool_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [MarketVault::SEED, market.key().as_ref()],
        bump = market_vault.load()?.bump
    )]
    pub market_vault: AccountLoader<'info, MarketVault>,
    /// CHECK: `== market.trader` (`has_one`).
    #[account(mut)]
    pub trader: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(ctx: Context<'info, Expire<'info>>, extras: ExtrasSel) -> Result<()> {
    let clock = Clock::get()?;
    let pos = (**ctx.accounts.position).clone();
    let mut out = {
        let a = &ctx.accounts;
        let accs = ExitAccounts {
            keeper: a.keeper.to_account_info(),
            config: &a.config,
            market: &a.market,
            position: a.position.to_account_info(),
            exposure: a.exposure.key(),
            mint: a.mint.to_account_info(),
            decimals: a.mint.decimals,
            pool_vault: a.pool_vault.to_account_info(),
            market_vault: &a.market_vault,
            trader: a.trader.to_account_info(),
            token_program: a.token_program.to_account_info(),
            system_program: a.system_program.to_account_info(),
        };
        keeper_exit(
            &accs,
            &pos,
            ctx.remaining_accounts,
            extras,
            ExitKind::Expiry,
            &clock,
        )?
    };
    let borrower = ctx.accounts.borrower.to_account_info();
    close_exposure(&mut ctx.accounts.exposure, borrower, pos.q)?;

    let market_key = ctx.accounts.market.key();
    if !out.mint_ok {
        emit!(MintSafetyTripped {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            mint: ctx.accounts.mint.key(),
        });
    }
    if out.swept.any() {
        emit_cpi!(TraderSwept {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            lamports: out.swept.lamports,
            tokens: out.swept.tokens,
            to: SWEEP_TO_VEST,
            wipe_bits: 0,
        });
    }
    if let Some(ev) = out.completed.take() {
        emit_cpi!(ev);
    }
    let p = &out.payout;
    emit_cpi!(ExpireEvent {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        position: ctx.accounts.position.key(),
        keeper: ctx.accounts.keeper.key(),
        b_live: out.b_live,
        b_ref: out.b_ref,
        pi_ref: out.pi_ref,
        buyback_cost: out.leg.spent,
        rent: p.rent,
        expiry_fee: p.expiry_fee,
        keeper_reward: p.keeper,
        borrower_payout: p.borrower,
        b_live_risk: out.b_live_risk,
        max_in: out.max_in,
        token_surplus: out.token_surplus,
    });
    Ok(())
}
