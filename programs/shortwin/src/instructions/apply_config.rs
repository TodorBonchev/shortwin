//! `apply_config()` (ARCH §3.1): anyone, once `now ≥ not_before` and at the latest
//! `CONFIG_APPLY_WINDOW_S` (14 days) after it.
//!
//! Applies the pending change and clears it. The change is re-checked against the current
//! configuration first (the same rules as at proposal; nothing else can change the parameters
//! in between, but an upgrade could have changed the rules). Variants: `Params`, `EmaParams`
//! (new version, `force_at`), `Kink` (new kink version), `Authorities`, `Timelock`,
//! `EnableVenues`, `OracleFeed`; none changes a venue program id or PDA (C0-8).
//!
//! **After the M1 review** (ARCH §3.1 as synced after M1):
//! - a pending change lapses `CONFIG_APPLY_WINDOW_S` after `not_before`
//!   (`PendingChangeExpired`; governance proposes it again), so a forgotten proposal cannot be
//!   applied at a moment that suits whoever finds it (M1-SEC-9);
//! - an `Authorities` change needs the new `fee_recipient` as the optional `fee_recipient`
//!   account (`mut`: the runtime refuses to write-lock a sysvar, a program or a reserved key),
//!   which must be System-owned and not executable (`state::check_fee_recipient`; M1-SEC-5);
//! - an `OracleFeed` change needs the optional `oracle_cache` account and empties it (price,
//!   confidence, exponent, publish time, slot), so the old feed's price is no longer served as
//!   fresh and cannot gate the new feed's first crank through the step or ordering rules; until
//!   the next `crank_sol_usd` the cache reads as never cranked (stale: tiers fail safe to C,
//!   ARCH §4.7; M1-SEC-10).
//!
//! The optional accounts are ignored for the other variants (pass the program id, Anchor's
//! "none").

use anchor_lang::prelude::*;

use crate::constants::{CONFIG_APPLY_WINDOW_S, CONFIG_VERSION};
use crate::errors::{CheckedExt, ShortwinError};
use crate::events::ConfigApplied;
use crate::state::{check_fee_recipient, ConfigChange, GlobalConfig, OracleCache};

#[event_cpi]
#[derive(Accounts)]
pub struct ApplyConfig<'info> {
    #[account(
        mut,
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    /// CHECK: `Authorities` only: must equal the change's `fee_recipient` and pass
    /// `check_fee_recipient` (System-owned, not executable); `mut` proves the runtime lets it
    /// be written. Never read or written otherwise.
    #[account(mut)]
    pub fee_recipient: Option<UncheckedAccount<'info>>,
    /// `OracleFeed` only: emptied (the bump stays).
    #[account(
        mut,
        seeds = [OracleCache::SEED],
        bump = oracle_cache.bump
    )]
    pub oracle_cache: Option<Box<Account<'info, OracleCache>>>,
}

pub fn handler(ctx: Context<ApplyConfig>) -> Result<()> {
    let clock = Clock::get()?;
    let (kind, hash, change) = {
        let mut c = ctx.accounts.config.load_mut()?;
        require!(c.pending.is_some(), ShortwinError::NoPendingChange);
        require_gte!(
            clock.unix_timestamp,
            c.pending.not_before,
            ShortwinError::TimelockNotElapsed
        );
        let expires = c
            .pending
            .not_before
            .checked_add(CONFIG_APPLY_WINDOW_S)
            .or_math()?;
        require_gte!(
            expires,
            clock.unix_timestamp,
            ShortwinError::PendingChangeExpired
        );
        let change = c.pending.change()?;
        c.check_change(&change)?;
        let applied = (c.pending.kind, c.pending.hash());
        c.apply(&change, clock.unix_timestamp)?;
        c.pending.clear();
        (applied.0, applied.1, change)
    };
    match &change {
        ConfigChange::Authorities { fee_recipient, .. } => {
            let recipient = ctx
                .accounts
                .fee_recipient
                .as_ref()
                .ok_or_else(|| error!(ShortwinError::AccountMismatch))?;
            require_keys_eq!(
                recipient.key(),
                *fee_recipient,
                ShortwinError::AccountMismatch
            );
            check_fee_recipient(&recipient.to_account_info())?;
        }
        ConfigChange::OracleFeed { .. } => {
            let cache = ctx
                .accounts
                .oracle_cache
                .as_mut()
                .ok_or_else(|| error!(ShortwinError::AccountMismatch))?;
            let bump = cache.bump;
            cache.set_inner(OracleCache {
                bump,
                ..OracleCache::default()
            });
        }
        ConfigChange::Params { .. }
        | ConfigChange::EmaParams { .. }
        | ConfigChange::Kink { .. }
        | ConfigChange::Timelock { .. }
        | ConfigChange::EnableVenues { .. } => {}
    }
    emit_cpi!(ConfigApplied {
        market: Pubkey::default(),
        slot: clock.slot,
        ts: clock.unix_timestamp,
        kind,
        hash,
    });
    Ok(())
}
