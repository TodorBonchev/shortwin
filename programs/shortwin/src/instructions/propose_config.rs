//! `propose_config(change)` (ARCH §3.1): Governance only.
//!
//! `pending = { change, not_before = now + timelock_s }`, replacing any pending change. The
//! would-be configuration must pass `params::validate` and `params::validate_econ` (compiled
//! bounds and relational checks, A8) and the change its own rules (timelock floor, authorities
//! set, known venue bits). Off-chain gate (A73): the proposal's overrides pass
//! `docs/design/sim/config_check.py` before the multisig signs (`scripts/config-gate.sh`).

use anchor_lang::prelude::*;

use crate::constants::CONFIG_VERSION;
use crate::errors::{CheckedExt, ShortwinError};
use crate::events::ConfigProposed;
use crate::state::{ConfigChange, GlobalConfig};

#[event_cpi]
#[derive(Accounts)]
pub struct ProposeConfig<'info> {
    pub governance: Signer<'info>,
    #[account(
        mut,
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion,
        constraint = config.load()?.governance == governance.key() @ ShortwinError::Unauthorized
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
}

pub fn handler(ctx: Context<ProposeConfig>, change: ConfigChange) -> Result<()> {
    let clock = Clock::get()?;
    let (kind, hash, not_before) = {
        let mut c = ctx.accounts.config.load_mut()?;
        c.check_change(&change)?;
        let not_before = clock
            .unix_timestamp
            .checked_add(i64::from(c.timelock_s))
            .or_math()?;
        c.pending.set(&change, not_before)?;
        (c.pending.kind, c.pending.hash(), not_before)
    };
    emit_cpi!(ConfigProposed {
        market: Pubkey::default(),
        slot: clock.slot,
        ts: clock.unix_timestamp,
        kind,
        hash,
        not_before,
    });
    Ok(())
}
