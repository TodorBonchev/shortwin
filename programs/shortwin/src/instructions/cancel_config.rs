//! `cancel_config()` (ARCH §3.1): Governance or the Emergency vault clears the pending change.
//!
//! The Emergency vault (no timelock) may cancel pending parameter changes as a fast check
//! against a compromised Governance vault, but not `Authorities` or `Timelock` changes, so
//! Governance can always rotate a compromised Emergency key once the timelock has run (owner
//! decision on M1-SEC-1, SPEC §11). Governance may cancel any pending change.

use anchor_lang::prelude::*;

use crate::constants::CONFIG_VERSION;
use crate::errors::ShortwinError;
use crate::events::ConfigCancelled;
use crate::state::{ConfigChange, GlobalConfig};

/// `kind`s of the changes only Governance may cancel.
const GOVERNANCE_ONLY_CANCEL: [u8; 2] =
    [ConfigChange::KIND_AUTHORITIES, ConfigChange::KIND_TIMELOCK];

#[event_cpi]
#[derive(Accounts)]
pub struct CancelConfig<'info> {
    /// `governance` or `pause_authority`.
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
}

pub fn handler(ctx: Context<CancelConfig>) -> Result<()> {
    let clock = Clock::get()?;
    let (kind, hash) = {
        let mut c = ctx.accounts.config.load_mut()?;
        require!(c.pending.is_some(), ShortwinError::NoPendingChange);
        require!(
            *ctx.accounts.authority.key == c.governance
                || !GOVERNANCE_ONLY_CANCEL.contains(&c.pending.kind),
            ShortwinError::EmergencyCannotCancel
        );
        let cancelled = (c.pending.kind, c.pending.hash());
        c.pending.clear();
        cancelled
    };
    emit_cpi!(ConfigCancelled {
        market: Pubkey::default(),
        slot: clock.slot,
        ts: clock.unix_timestamp,
        kind,
        hash,
        by: ctx.accounts.authority.key(),
    });
    Ok(())
}
