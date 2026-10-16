//! `disable_venue(bit)` (ARCH §3.1; AIR-24): the Emergency vault clears a `venue_enabled` bit at
//! once; re-enabling is timelocked (`ConfigChange::EnableVenues`). Opens only.
//!
//! **A pending re-enable is cancelled with it (AIR-24).** `apply_config` is permissionless once a
//! change's `not_before` has passed and ORs an `EnableVenues` change's bits into `venue_enabled`,
//! so a re-enable proposed before the disable undid it at `not_before` (anyone's `[apply_config,
//! open_short…]`) unless the Emergency also cancelled it in time. A pending `EnableVenues` change
//! that would set the cleared bit is therefore cancelled here, exactly as `cancel_config` cancels
//! it (event `ConfigCancelled`, `by` = the Emergency vault): the venue comes back only through a
//! new proposal and its full timelock. The whole change goes, other bits included (Governance
//! proposes them again): rewriting the pending payload would change the change's hash, which
//! `ConfigProposed` / `ConfigApplied` carry and indexers match on. Any other pending change stays.

use anchor_lang::prelude::*;

use crate::constants::{CONFIG_VERSION, VENUE_BITS_KNOWN};
use crate::errors::ShortwinError;
use crate::events::{ConfigCancelled, VenueDisabled};
use crate::state::{ConfigChange, GlobalConfig};

#[event_cpi]
#[derive(Accounts)]
pub struct DisableVenue<'info> {
    pub pause_authority: Signer<'info>,
    #[account(
        mut,
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion,
        constraint = config.load()?.pause_authority == pause_authority.key()
            @ ShortwinError::Unauthorized
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
}

pub fn handler(ctx: Context<DisableVenue>, bit: u8) -> Result<()> {
    let mask = 1u32
        .checked_shl(u32::from(bit))
        .filter(|m| m & VENUE_BITS_KNOWN != 0)
        .ok_or_else(|| error!(ShortwinError::InvalidParams))?;
    let clock = Clock::get()?;
    let (venue_enabled, cancelled) = {
        let mut c = ctx.accounts.config.load_mut()?;
        c.venue_enabled &= !mask;
        // AIR-24: a pending re-enable of this bit is cancelled (module docs).
        let re_enables = c.pending.kind == ConfigChange::KIND_ENABLE_VENUES
            && matches!(
                c.pending.change(),
                Ok(ConfigChange::EnableVenues { bits }) if bits & mask != 0
            );
        let cancelled = if re_enables {
            let kh = (c.pending.kind, c.pending.hash());
            c.pending.clear();
            Some(kh)
        } else {
            None
        };
        (c.venue_enabled, cancelled)
    };
    emit_cpi!(VenueDisabled {
        market: Pubkey::default(),
        slot: clock.slot,
        ts: clock.unix_timestamp,
        bit,
        venue_enabled,
    });
    if let Some((kind, hash)) = cancelled {
        emit_cpi!(ConfigCancelled {
            market: Pubkey::default(),
            slot: clock.slot,
            ts: clock.unix_timestamp,
            kind,
            hash,
            by: ctx.accounts.pause_authority.key(),
        });
    }
    Ok(())
}
