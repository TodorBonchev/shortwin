//! `set_route_broken(scope, venue_bit, broken)` (ARCH §3.1; A40, C0-4 b; **O6**, recorded in
//! SPEC §0, §11, §14): the Emergency vault (or Governance) sets or clears the route-broken flag
//! of a venue (`GlobalConfig.venue_route_broken` bit `venue_bit`, scope `Global`) or of one
//! market (`Market.flags` bit 2, scope `Market`). No timelock.
//!
//! **What it can do, stated fully (V1-7):** refuse opens (`RouteBroken`), and count as a
//! no-route observation in the S3 chain (`s3_no_route`, §5.3), which after `s3_wait_s` lets
//! positions that are liquidatable (spot leg evaluated normally) or expired settle in SOL at
//! `min(V − K_s, FV)` on the settlement box. It moves no funds and is **not** an input to
//! `venue_executable`, to any quote or leg, to close / liquidate / expire, or to `pool_fail`
//! (V2-1; I10, I16; T-S3-BROKEN).

use anchor_lang::prelude::*;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION, VENUE_BITS_KNOWN};
use crate::errors::ShortwinError;
use crate::events::{RouteBrokenSet, SCOPE_GLOBAL, SCOPE_MARKET};
use crate::state::{GlobalConfig, Market, MARKET_FLAG_ROUTE_BROKEN};
use crate::PauseScope;

#[event_cpi]
#[derive(Accounts)]
pub struct SetRouteBroken<'info> {
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
    /// The market, for `PauseScope::Market` only; bound to its PDA `["market", mint]` with
    /// the stored bump (`Market::is_pda`).
    #[account(
        mut,
        constraint = market.load()?.is_pda(&market.key())
            @ anchor_lang::error::ErrorCode::ConstraintSeeds,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion
    )]
    pub market: Option<AccountLoader<'info, Market>>,
}

pub fn handler(
    ctx: Context<SetRouteBroken>,
    scope: PauseScope,
    venue_bit: u8,
    broken: bool,
) -> Result<()> {
    let clock = Clock::get()?;
    let (scope_code, market_key) = match (scope, &ctx.accounts.market) {
        (PauseScope::Global, None) => {
            let mask = 1u32
                .checked_shl(u32::from(venue_bit))
                .filter(|m| m & VENUE_BITS_KNOWN != 0)
                .ok_or(ShortwinError::InvalidParams)?;
            let mut c = ctx.accounts.config.load_mut()?;
            if broken {
                c.venue_route_broken |= mask;
            } else {
                c.venue_route_broken &= !mask;
            }
            (SCOPE_GLOBAL, Pubkey::default())
        }
        (PauseScope::Market, Some(market)) => {
            market
                .load_mut()?
                .set_flag(MARKET_FLAG_ROUTE_BROKEN, broken);
            (SCOPE_MARKET, market.key())
        }
        _ => return err!(ShortwinError::AccountMismatch),
    };
    emit_cpi!(RouteBrokenSet {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        scope: scope_code,
        venue_bit,
        broken,
    });
    Ok(())
}
