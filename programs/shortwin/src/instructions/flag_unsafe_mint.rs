//! `flag_unsafe_mint()` (ARCH §3.9, A3, ADOPTED Q7; R-MINT §6, §10.4): anyone.
//!
//! The persistent tripwire of the mint filter. A revert cannot record anything, so an exit
//! path that finds the mint failing only emits `MintSafetyTripped` and continues (SPEC §11);
//! this instruction is how the failure is written down. If the market's mint **fails**
//! `check_mint_safety` — or is no longer owned by the market's token program — it sets
//! `MARKET_FLAG_MINT_UNSAFE`, which the entry paths refuse (`deposit`, `open_short`:
//! `MintUnsafeFlagged` in `engine::guard::mint_guard`), and emits `MintSafetyTripped`.
//! Exits are never gated on the flag.
//!
//! Errors: `MintSafe` if the mint still passes (nothing to flag); `MintUnsafeFlagged` if the
//! market is already flagged (nothing changes, so nothing is written or emitted). The flag is
//! one-way: no instruction clears it (ARCH has none; a governance-only unflag for a false
//! positive after a Token-2022 upgrade would be an owner decision).
//!
//! The mint is read raw (an `UncheckedAccount` keyed through `has_one`), so a mint that no
//! longer parses — or no longer exists — can still be flagged.

use anchor_lang::prelude::*;

use crate::constants::MARKET_VERSION;
use crate::errors::ShortwinError;
use crate::events::MintSafetyTripped;
use crate::mint_filter::check_mint_safety;
use crate::state::{Market, MARKET_FLAG_MINT_UNSAFE};

#[event_cpi]
#[derive(Accounts)]
pub struct FlagUnsafeMint<'info> {
    #[account(
        mut,
        seeds = [Market::SEED, mint.key().as_ref()],
        bump = market.load()?.bump,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion,
        has_one = mint @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    /// CHECK: key-checked by `has_one`; read raw so a broken or closed mint can be flagged.
    /// The market's mint, parsed by the filter.
    pub mint: UncheckedAccount<'info>,
}

pub fn handler(ctx: Context<FlagUnsafeMint>) -> Result<()> {
    let market_key = ctx.accounts.market.key();
    let mint = ctx.accounts.mint.to_account_info();
    {
        let mut m = ctx.accounts.market.load_mut()?;
        require!(
            !m.has_flag(MARKET_FLAG_MINT_UNSAFE),
            ShortwinError::MintUnsafeFlagged
        );
        // Unsafe = the filter fails, or the mint moved to another program than the one the
        // market transfers with (only possible through a close and re-creation, which the
        // filter's MintCloseAuthority rejection otherwise rules out).
        let safe = *mint.owner == m.token_program && check_mint_safety(&mint).is_ok();
        require!(!safe, ShortwinError::MintSafe);
        m.set_flag(MARKET_FLAG_MINT_UNSAFE, true);
    }
    let clock = Clock::get()?;
    emit_cpi!(MintSafetyTripped {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        mint: mint.key(),
    });
    Ok(())
}
