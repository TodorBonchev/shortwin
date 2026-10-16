//! `claim_sol()` (ARCH §3.3; C0-13, C0-17, V2-2, R3A-2): a lender claims its realised SOL.
//!
//! Does not accrue and does **not** write `Market` (C0-17): it pays against the last accrued
//! `acc`, `acc_r` and vest release. It runs the full `pool::touch` (share epochs from the
//! `ShareEpoch` records, pending/real, surplus to the pot, the lender's own lock expiry), which
//! writes only the lender and `MarketVault`, then `pool::claim`: `own = real − claimed`,
//! `draw = min(unmatched, max(0, pending − own))`, `unmatched −= draw`, `real += draw`, `pay =
//! min(pending, real − claimed, lender_sol)`. The lamports leave `market_vault` by direct
//! debit, never below rent + `lender_sol + protocol_fees` (I2). A second call in the same
//! state pays 0 (memoryful, V2-2), and no lender draws more than its own entitlement (I-P5).
//! A lender parked on a live full-wipe record claims from that record. Event `ClaimSol`
//! (`sol_paid`, and `pot_draw` of it).
//!
//! **Close (KI-67, owner decision of 2026-10-08).** A claim that leaves the position
//! `pool::closable` — current, no shares, `pending == 0` (so nothing realised is left either),
//! no lock and no forward chain — closes it and returns its rent (2,568,240 lamports) to the
//! owner, as `withdraw` does: a lender wiped out, floored to 0 shares by a rebase or fully paid
//! after its exit no longer keeps the rent locked. A `lock_term` without a `lock` does not keep
//! it open (it is never read again; the calendar holds its own copy until `lock_until`). After
//! the close a further `claim_sol` of the position (in the same transaction or later) fails with
//! `AccountNotInitialized` (3012) instead of paying 0; a later `deposit` re-creates the PDA from
//! `pool::Lender::new` (`init_if_needed`; within one transaction too), with no forward chain.
//! The indexer infers the close from the account, as for `withdraw`; the event is unchanged.
//!
//! Remaining accounts: the `ShareEpoch` records the lender is behind on (key-checked);
//! writable when the lender converts through a rebase record, whose conversion remainder
//! `conv_rem` the touch writes (KI-65: `engine::lender::store_records`, else
//! `EpochRecordReadOnly`). Apart from those records it writes no shared state but
//! `MarketVault`. A second call writes no record (the lender is current). A lender with a
//! forward chain (KI-66 full) passes the records from its own epoch, like any other; its
//! conversion takes the chain's accruals back exactly (`pool::touch`).

use anchor_lang::prelude::*;
use shortwin_math::pool;

use crate::constants::MARKET_VERSION;
use crate::engine::lender;
use crate::errors::{MathResultExt, ShortwinError};
use crate::events::ClaimSol as ClaimSolEvent;
use crate::state::{LenderPosition, Market, MarketVault};

#[event_cpi]
#[derive(Accounts)]
pub struct ClaimSol<'info> {
    /// The lender; receives the SOL, and the `LenderPosition` rent when the claim closes it
    /// (KI-67).
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
}

pub fn handler<'info>(ctx: Context<'info, ClaimSol<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let market_key = ctx.accounts.market.key();

    let (pay, pot_draw, t_after, s_after, after, tracked, closes) = {
        let market = ctx.accounts.market.load()?;
        let mut vault_acc = ctx.accounts.market_vault.load_mut()?;
        let pool_state = market.pool();
        let mut vault = vault_acc.vault();
        let mut records = lender::load_records(&market_key, ctx.remaining_accounts, usize::MAX)?;
        let loaded = records.clone();
        let position = &mut ctx.accounts.lender;
        let mut l = position.lender();
        pool::touch(&pool_state, &mut vault, &mut l, &mut records, now).math()?;
        let touched = l;
        let pay = pool::claim(&pool_state, &mut vault, &mut l, &mut records, now).math()?;
        lender::store_records(ctx.remaining_accounts, &loaded, &records)?;

        vault_acc.set_vault(&vault);
        position.set_lender(&l);
        (
            pay,
            lender::pot_draw(&touched, &l),
            pool_state.tokens_free,
            pool_state.total_shares,
            l,
            lender::tracked(&vault)?,
            pool::closable(&pool_state, &l),
        )
    };

    let vault_info = ctx.accounts.market_vault.to_account_info();
    lender::pay_sol(
        &vault_info,
        &ctx.accounts.owner.to_account_info(),
        pay,
        tracked,
    )?;
    // KI-67: a position the claim leaves empty closes; its rent returns to the owner.
    if closes {
        ctx.accounts
            .lender
            .close(ctx.accounts.owner.to_account_info())?;
    }

    emit_cpi!(ClaimSolEvent {
        market: market_key,
        slot: clock.slot,
        ts: now,
        owner: ctx.accounts.owner.key(),
        amount: 0,
        shares: 0,
        t_after,
        s_after,
        sol_paid: pay,
        pot_draw,
        lock: after.lock,
        lock_until: after.lock_until,
        epoch: after.epoch,
    });
    Ok(())
}
