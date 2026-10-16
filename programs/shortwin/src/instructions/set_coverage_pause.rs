//! `set_coverage_pause(paused, reason)` (ARCH §3.1; A69; **O17**, recorded by the owner in SPEC
//! §14 on 2026-09-30): the in-house keeper's `coverage_authority` sets or clears one market's
//! `paused_coverage` flag (`Market.flags` bit 6) — degraded mode: a market whose keeper
//! coverage it cannot keep (an outage, a keeper budget that can no longer cover it) stops taking
//! opens, visibly and at once, instead of waiting for a human-paged `set_pause`. Since the
//! owner's decision of 2026-10-01 (★OWNER-M7-3 (b), SPEC §14 M7-3) the Emergency vault
//! (`pause_authority`) can **clear** the flag too — never set it.
//!
//! **What it can do, stated fully.** The flag is read by one guard only, `open_short`'s static
//! step 1 (`engine::guard::open_static`, `CoveragePaused`). Closes, `add_margin`, liquidations,
//! expiries, settlements (S1–S3), cranks, deposits, withdrawals, claims, sweeps and pool
//! selection never read it (SPEC §11: never pause exits; I10). It moves no lamports and no
//! tokens and writes nothing but that one bit of one market and, when it clears a set flag, the
//! market's `coverage_cleared_at` (I4's coverage clause). It is independent of `set_pause`'s
//! `paused_opens` (bit 0): neither clears the other.
//!
//! **The clear delay (M7-SEC-3).** Clearing a set flag — by either signer — records the second
//! in `Market.coverage_cleared_at`, and opens keep refusing `CoveragePaused` for
//! `COVERAGE_CLEAR_DELAY_S` (5 s) after it. As built at M7 a clear took effect within the
//! transaction, so the key's holder (a hot key on the pauser instance) could land an open on a
//! market the keeper holds degraded with `[clear, open_short, set]` in one transaction — the flag
//! the keeper polls never changed between its reads and both events named the same key. With the
//! delay the flag is visibly cleared for at least five seconds before any open: the keeper alerts
//! a clear it did not send (`CoveragePauseCleared`) and sets the flag again, and alerts an open
//! that lands while it holds the market degraded (`OpenWhileDegraded`). A no-op clear (the flag
//! already clear) records nothing. The delay applies after an Emergency clear as well (owner
//! decision of 2026-10-01).
//!
//! **Checks, in order:** `config` is the `["config"]` PDA at `CONFIG_VERSION`; then
//! - **the Emergency clear** (★OWNER-M7-3 (b)): the signer is `pause_authority` and `paused`
//!   is `false` — accepted whether or not the role is installed (a flag a lost, rotated or
//!   disabled role left set, or one a compromised coverage key keeps setting, is cleared at
//!   once; each of its sets is undone by one Emergency clear), on to the market and reason checks;
//! - otherwise the role is enabled — `coverage_authority != Pubkey::default()`, else
//!   `CoverageAuthorityDisabled` (the role is inert until governance installs a key with a
//!   timelocked `Authorities` change; Governance never acts in its place, and the Emergency
//!   vault never sets the flag) — and the signer is `coverage_authority` (`Unauthorized`; so
//!   `pause_authority` setting the flag fails `Unauthorized`, or `CoverageAuthorityDisabled`
//!   while the role is unset);
//!
//! `market` is a `["market", mint]` PDA with its stored bump at `MARKET_VERSION`;
//! `reason == COVERAGE_REASON_KEEPER` (`InvalidParams`), for both signers. Setting the flag to
//! the value it has is accepted (the keeper retries until the flag matches) and emits the event
//! again. Event `CoveragePauseSet{paused, reason, by}` (`by`: the signer, the coverage key or the
//! Emergency vault).
//!
//! Who clears the flag: `coverage_authority` and the Emergency vault. A lost or rotated keeper
//! key leaves the flag set until the new key or the Emergency clears it; exits are unaffected
//! throughout. No new account, error or event: one more accepted signer for `paused = false`,
//! read from the `config` the instruction already loads.

use anchor_lang::prelude::*;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::errors::ShortwinError;
use crate::events::{CoveragePauseSet, COVERAGE_REASON_KEEPER};
use crate::state::{GlobalConfig, Market, MARKET_FLAG_PAUSED_COVERAGE};

#[event_cpi]
#[derive(Accounts)]
#[instruction(paused: bool)]
pub struct SetCoveragePause<'info> {
    /// `config.coverage_authority` (the keeper's pause-opens-only key; O17: sets or clears), or
    /// `config.pause_authority` (the Emergency vault; OWNER-M7-3 (b): clears only).
    pub coverage_authority: Signer<'info>,
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion,
        constraint = config.load()?.is_emergency_coverage_clear(coverage_authority.key, paused)
            || config.load()?.coverage_enabled()
            @ ShortwinError::CoverageAuthorityDisabled,
        constraint = config.load()?.is_emergency_coverage_clear(coverage_authority.key, paused)
            || config.load()?.is_coverage_authority(coverage_authority.key)
            @ ShortwinError::Unauthorized
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    /// The market, bound to its PDA `["market", mint]` with the stored bump (`Market::is_pda`;
    /// M1-SEC-6).
    #[account(
        mut,
        constraint = market.load()?.is_pda(&market.key())
            @ anchor_lang::error::ErrorCode::ConstraintSeeds,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion
    )]
    pub market: AccountLoader<'info, Market>,
}

pub fn handler(ctx: Context<SetCoveragePause>, paused: bool, reason: u8) -> Result<()> {
    require!(
        reason == COVERAGE_REASON_KEEPER,
        ShortwinError::InvalidParams
    );
    let clock = Clock::get()?;
    {
        let mut market = ctx.accounts.market.load_mut()?;
        // M7-SEC-3: a clear of a set flag — by the coverage key or the Emergency vault — starts
        // `COVERAGE_CLEAR_DELAY_S`, during which opens still refuse (`engine::guard::open_static`).
        if !paused && market.has_flag(MARKET_FLAG_PAUSED_COVERAGE) {
            market.coverage_cleared_at = clock.unix_timestamp;
        }
        market.set_flag(MARKET_FLAG_PAUSED_COVERAGE, paused);
    }
    emit_cpi!(CoveragePauseSet {
        market: ctx.accounts.market.key(),
        slot: clock.slot,
        ts: clock.unix_timestamp,
        paused,
        reason,
        by: ctx.accounts.coverage_authority.key(),
    });
    Ok(())
}
