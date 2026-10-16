//! `cover_deficit(lamports)` (ARCH §3.3; C0-13): a lender pays SOL in, typically to bring a
//! negative `pending` (after a shortfall debit, ECON §9.4) back to ≥ 0 so it can withdraw.
//!
//! `pool::cover`: the lender's `touch`, then `lender_sol += lamports`, `pending += lamports`,
//! `real += lamports`; the lamports move from `owner` to `market_vault` by a System transfer.
//! What exceeds the lender's entitlement reaches the pot at its next touch (`pool_model.cover`
//! does not cap the amount). `lamports == 0` is refused (`ZeroAmount`; ARCH names no check,
//! and a zero cover would only be a touch). Does not write `Market` (C0-17). Event
//! `DeficitCovered` (`amount` = the lamports).
//!
//! Remaining accounts: the `ShareEpoch` records the lender is behind on (key-checked);
//! writable when the lender converts through a rebase record, whose conversion remainder
//! `conv_rem` the touch writes (KI-65: `engine::lender::store_records`, else
//! `EpochRecordReadOnly`).

use anchor_lang::prelude::*;
use anchor_lang::system_program::{self, Transfer};
use shortwin_math::pool;

use crate::constants::MARKET_VERSION;
use crate::engine::lender;
use crate::errors::{MathResultExt, ShortwinError};
use crate::events::DeficitCovered;
use crate::state::{LenderPosition, Market, MarketVault};

#[event_cpi]
#[derive(Accounts)]
pub struct CoverDeficit<'info> {
    /// The lender; pays the lamports.
    #[account(mut)]
    pub owner: Signer<'info>,
    /// Read-only (C0-17). Bound to its PDA `["market", mint]` with the stored bump
    /// (`Market::is_pda`; ARCH §2.3; M1-SEC-6 / M1-FID-6).
    #[account(
        constraint = market.load()?.is_pda(&market.key())
            @ anchor_lang::error::ErrorCode::ConstraintSeeds,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion,
        has_one = market_vault @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    #[account(
        mut,
        seeds = [LenderPosition::SEED, market.key().as_ref(), owner.key().as_ref()],
        bump = lender.bump,
        has_one = owner @ ShortwinError::Unauthorized,
        has_one = market @ ShortwinError::AccountMismatch
    )]
    pub lender: Box<Account<'info, LenderPosition>>,
    #[account(
        mut,
        seeds = [MarketVault::SEED, market.key().as_ref()],
        bump = market_vault.load()?.bump
    )]
    pub market_vault: AccountLoader<'info, MarketVault>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(ctx: Context<'info, CoverDeficit<'info>>, lamports: u64) -> Result<()> {
    require!(lamports > 0, ShortwinError::ZeroAmount);
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let market_key = ctx.accounts.market.key();

    let (t_after, s_after, after, tracked) = {
        let market = ctx.accounts.market.load()?;
        let mut vault_acc = ctx.accounts.market_vault.load_mut()?;
        let pool_state = market.pool();
        let mut vault = vault_acc.vault();
        let mut records = lender::load_records(&market_key, ctx.remaining_accounts, usize::MAX)?;
        let loaded = records.clone();
        let position = &mut ctx.accounts.lender;
        let mut l = position.lender();
        pool::cover(&pool_state, &mut vault, &mut l, &mut records, now, lamports).math()?;
        lender::store_records(ctx.remaining_accounts, &loaded, &records)?;

        vault_acc.set_vault(&vault);
        position.set_lender(&l);
        (
            pool_state.tokens_free,
            pool_state.total_shares,
            l,
            lender::tracked(&vault)?,
        )
    };

    system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.key(),
            Transfer {
                from: ctx.accounts.owner.to_account_info(),
                to: ctx.accounts.market_vault.to_account_info(),
            },
        ),
        lamports,
    )?;
    lender::check_vault_floor(&ctx.accounts.market_vault.to_account_info(), tracked)?;

    emit_cpi!(DeficitCovered {
        market: market_key,
        slot: clock.slot,
        ts: now,
        owner: ctx.accounts.owner.key(),
        amount: lamports,
        shares: 0,
        t_after,
        s_after,
        sol_paid: 0,
        pot_draw: 0,
        lock: after.lock,
        lock_until: after.lock_until,
        epoch: after.epoch,
    });
    Ok(())
}
