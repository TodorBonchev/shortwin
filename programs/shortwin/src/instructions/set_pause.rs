//! `set_pause(scope, paused)` (ARCH §3.1): the Emergency vault or Governance sets or clears
//! `paused_opens` globally (`GlobalConfig.flags` bit 0) or on one market (`Market.flags` bit 0).
//! SPEC §11: new opens **only** — closes, liquidations, settlements, withdrawals and claims
//! never read it (I10).

use anchor_lang::prelude::*;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::errors::ShortwinError;
use crate::events::{PauseSet, SCOPE_GLOBAL, SCOPE_MARKET};
use crate::state::{GlobalConfig, Market, CONFIG_FLAG_PAUSED_OPENS, MARKET_FLAG_PAUSED_OPENS};

/// What `set_pause` pauses.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseScope {
    /// Every market (`GlobalConfig.flags`); `market` must be omitted.
    Global,
    /// One market (`Market.flags`); `market` must be passed.
    Market,
}

#[event_cpi]
#[derive(Accounts)]
pub struct SetPause<'info> {
    /// `pause_authority` or `governance`.
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion,
        constraint = config.load()?.is_pause_or_governance(authority.key)
            @ ShortwinError::Unauthorized
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    /// The market, for `PauseScope::Market` only; bound to its PDA `["market", mint]` with the
    /// stored bump (`Market::is_pda`; ARCH §2.3; M1-SEC-6).
    #[account(
        mut,
        constraint = market.load()?.is_pda(&market.key())
            @ anchor_lang::error::ErrorCode::ConstraintSeeds,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion
    )]
    pub market: Option<AccountLoader<'info, Market>>,
}

pub fn handler(ctx: Context<SetPause>, scope: PauseScope, paused: bool) -> Result<()> {
    let clock = Clock::get()?;
    let (scope_code, market_key) = match (scope, &ctx.accounts.market) {
        (PauseScope::Global, None) => {
            let mut c = ctx.accounts.config.load_mut()?;
            if paused {
                c.flags |= CONFIG_FLAG_PAUSED_OPENS;
            } else {
                c.flags &= !CONFIG_FLAG_PAUSED_OPENS;
            }
            (SCOPE_GLOBAL, Pubkey::default())
        }
        (PauseScope::Market, Some(market)) => {
            market
                .load_mut()?
                .set_flag(MARKET_FLAG_PAUSED_OPENS, paused);
            (SCOPE_MARKET, market.key())
        }
        _ => return err!(ShortwinError::AccountMismatch),
    };
    emit_cpi!(PauseSet {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        scope: scope_code,
        paused,
        by: ctx.accounts.authority.key(),
    });
    Ok(())
}
