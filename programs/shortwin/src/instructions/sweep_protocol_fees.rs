//! `sweep_protocol_fees()` (ARCH §3.1, A9): **anyone** moves a market's `protocol_fees` to the
//! configured `fee_recipient`.
//!
//! `protocol_fees` is set to 0 and that many lamports leave `market_vault` by direct debit, to
//! `config.fee_recipient` only, never taking the vault below rent + `lender_sol` (I2). The only
//! debit of protocol money and the only outflow of `MarketVault` that I4 allows an admin (the
//! instruction needs no signer at all). Sweeping 0 succeeds and moves nothing, so a keeper's
//! batch never fails on an empty market. Event `ProtocolFeesSwept`.
//!
//! **Recipient below its rent minimum** (M1-SEC-5 (b)): if crediting the fees would leave the
//! recipient with fewer lamports than the rent-exempt minimum of its data (a System account
//! that does not exist yet, fed less than 890,880 lamports), the runtime would refuse the
//! whole transaction (`InsufficientFundsForRent`). The sweep then moves nothing and keeps
//! `protocol_fees` (event amount 0); it goes through once the market's fees, or the
//! recipient's own balance, cover that minimum. `apply_config` refuses a recipient the sweep
//! could never credit (a sysvar, a program, any non-System account; M1-SEC-5 (a), (c)).

use anchor_lang::prelude::*;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::engine::lender;
use crate::errors::{CheckedExt, ShortwinError};
use crate::events::ProtocolFeesSwept;
use crate::lamports;
use crate::state::{GlobalConfig, Market, MarketVault};

#[event_cpi]
#[derive(Accounts)]
pub struct SweepProtocolFees<'info> {
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    /// Bound to its PDA `["market", mint]` with the stored bump
    /// (`Market::is_pda`; ARCH §2.3; M1-SEC-6).
    #[account(
        constraint = market.load()?.is_pda(&market.key())
            @ anchor_lang::error::ErrorCode::ConstraintSeeds,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion,
        has_one = market_vault @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    #[account(
        mut,
        seeds = [MarketVault::SEED, market.key().as_ref()],
        bump = market_vault.load()?.bump
    )]
    pub market_vault: AccountLoader<'info, MarketVault>,
    /// CHECK: must equal `config.fee_recipient`; only credited.
    #[account(
        mut,
        address = config.load()?.fee_recipient @ ShortwinError::AccountMismatch
    )]
    pub fee_recipient: UncheckedAccount<'info>,
}

pub fn handler(ctx: Context<SweepProtocolFees>) -> Result<()> {
    let clock = Clock::get()?;
    let recipient = ctx.accounts.fee_recipient.to_account_info();
    let recipient_floor = lamports::rent_min(recipient.data_len())?;
    let (amount, tracked) = {
        let mut vault_acc = ctx.accounts.market_vault.load_mut()?;
        let fees = vault_acc.protocol_fees;
        let credited = recipient.lamports().checked_add(fees).or_math()?;
        let amount = if credited < recipient_floor { 0 } else { fees };
        vault_acc.protocol_fees = fees.checked_sub(amount).or_math()?;
        // After the sweep only lender SOL (and fees left for later) stays tracked.
        let tracked = vault_acc
            .lender_sol
            .checked_add(vault_acc.protocol_fees)
            .or_math()?;
        (amount, tracked)
    };
    let vault_info = ctx.accounts.market_vault.to_account_info();
    lender::pay_sol(
        &vault_info,
        &ctx.accounts.fee_recipient.to_account_info(),
        amount,
        tracked,
    )?;
    lender::check_vault_floor(&vault_info, tracked)?;

    emit_cpi!(ProtocolFeesSwept {
        market: ctx.accounts.market.key(),
        slot: clock.slot,
        ts: clock.unix_timestamp,
        amount,
        recipient: ctx.accounts.fee_recipient.key(),
    });
    Ok(())
}
