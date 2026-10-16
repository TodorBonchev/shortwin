//! Guards (ARCH §2.1 `engine/guard.rs`): the mint guard of ARCH §3.0 step 3 (M1) and the open
//! guards of ARCH §3.4 steps 1 and 3 (M2: global / market pauses, `paused_coverage`, the
//! route-broken flags, venue enabled, the `VenueGuard` mask, I17). **Exits never call the open
//! guards** (I10): closes, liquidations, expiries and settlements read no pause flag, no
//! route-broken flag, no latch and no oracle.
//!
//! The mint guard is shared by `create_market` / `deposit` / `open_short` (entry paths:
//! `check_mint_safety` must pass, and a market flagged `mint_unsafe` refuses) and by
//! `withdraw` / exits (exit paths: never revert on the filter; the caller emits
//! `MintSafetyTripped` when it reports `false`) [R-MINT §6, §9.4].

use anchor_lang::prelude::*;

use crate::constants::{COVERAGE_CLEAR_DELAY_S, GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4};
use crate::errors::ShortwinError;
use crate::mint_filter::check_mint_safety;
use crate::state::{
    GlobalConfig, Market, VenueGuard, VenueKind, MARKET_FLAG_MINT_UNSAFE,
    MARKET_FLAG_PAUSED_COVERAGE, MARKET_FLAG_PAUSED_OPENS,
};

/// Which side of the mint guard an instruction is on (ARCH §3.0 step 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MintPath {
    /// `create_market`, `deposit`, `open_short`: the filter must pass.
    Entry,
    /// `withdraw`, `close_short`, `liquidate`, `expire`, `sol_settle`: never blocked by the
    /// filter.
    Exit,
}

/// The mint guard: `mint.key == market.mint`, `mint.owner == market.token_program ==
/// token_program.key` (always; a mismatch is `AccountMismatch`), then on an entry path
/// `!mint_unsafe` (`MintUnsafeFlagged`) and `check_mint_safety` (its own error). Returns
/// whether the mint passes the filter (always `true` on an entry path; on an exit path `false`
/// means "emit `MintSafetyTripped` and continue").
pub fn mint_guard(
    market: &Market,
    mint: &AccountInfo,
    token_program: &Pubkey,
    path: MintPath,
) -> Result<bool> {
    require_keys_eq!(mint.key(), market.mint, ShortwinError::AccountMismatch);
    require_keys_eq!(
        *mint.owner,
        market.token_program,
        ShortwinError::AccountMismatch
    );
    require_keys_eq!(
        *token_program,
        market.token_program,
        ShortwinError::AccountMismatch
    );
    match path {
        MintPath::Entry => {
            require!(
                !market.has_flag(MARKET_FLAG_MINT_UNSAFE),
                ShortwinError::MintUnsafeFlagged
            );
            check_mint_safety(mint)?;
            Ok(true)
        }
        MintPath::Exit => Ok(check_mint_safety(mint).is_ok()),
    }
}

/// ARCH §3.4 step 1, the static open guards (in this order): opens paused globally or on the
/// market (`OpensPaused`, SPEC §11); paused for keeper coverage (`CoveragePaused`, A69), or
/// cleared less than `COVERAGE_CLEAR_DELAY_S` ago (M7-SEC-3: `now` against
/// `Market.coverage_cleared_at`); the
/// route-broken flag of the market or of its venue (`RouteBroken`, A40); a venue that takes
/// opens (`NoSupportedVenue` for `None`, `VenueMigrating` while `PumpMigrating`) and is enabled
/// (`VenueDisabled`). The mint guard (entry path, `MintUnsafeFlagged` first) is
/// [`mint_guard`]'s.
pub fn open_static(
    config: &GlobalConfig,
    market: &Market,
    kind: VenueKind,
    now: i64,
) -> Result<()> {
    require!(
        !config.paused_opens() && !market.has_flag(MARKET_FLAG_PAUSED_OPENS),
        ShortwinError::OpensPaused
    );
    let clear_delay_ends = market
        .coverage_cleared_at
        .saturating_add(COVERAGE_CLEAR_DELAY_S);
    require!(
        !market.has_flag(MARKET_FLAG_PAUSED_COVERAGE) && now >= clear_delay_ends,
        ShortwinError::CoveragePaused
    );
    require!(
        !crate::engine::route::route_broken(config, market, kind),
        ShortwinError::RouteBroken
    );
    match kind {
        VenueKind::None => return err!(ShortwinError::NoSupportedVenue),
        VenueKind::PumpMigrating => return err!(ShortwinError::VenueMigrating),
        _ => {}
    }
    require!(
        kind.venue_bit() != 0 && config.venue_enabled & kind.venue_bit() != 0,
        ShortwinError::VenueDisabled
    );
    Ok(())
}

/// ARCH §3.4 step 3, the venue-upgrade latch (A7; masks v0.4.1, R4B-7; I17): no open while a
/// bit of the venue's mask is set in `VenueGuard.upgraded` (`VenueUpgraded`).
pub fn latch(guard: &VenueGuard, kind: VenueKind) -> Result<()> {
    require!(
        !guard.blocks(kind.guard_mask()),
        ShortwinError::VenueUpgraded
    );
    Ok(())
}

/// The latch mask of a pool-selection candidate on `kind` (M4-SEC-8): the bit of the
/// candidate's **own** program only — its compiled reader is the one a pending upgrade puts
/// at risk. The whole Raydium mask stays the rule for opens (R4B-7: a position may later exit
/// on either program), but on proposals and fast-path adoptions it would disable the fast path
/// exactly when an upgrade of the incumbent's program makes its pools fail `pool_fail`.
pub const fn candidate_guard_mask(kind: VenueKind) -> u32 {
    match kind {
        VenueKind::RaydiumV4 => 1 << GUARD_RAYDIUM_V4,
        VenueKind::RaydiumCpmm => 1 << GUARD_RAYDIUM_CPMM,
        k => k.guard_mask(),
    }
}

/// The venue-upgrade latch of `propose_pool` (M4-SEC-8; ARCH §2.6 rule 3, §3.9): no candidate
/// on `kind` while its program's upgrade awaits the Emergency review (`VenueUpgraded`).
pub fn candidate_latch(guard: &VenueGuard, kind: VenueKind) -> Result<()> {
    require!(
        !candidate_latched(guard, kind),
        ShortwinError::VenueUpgraded
    );
    Ok(())
}

/// Whether the program of a candidate of `kind` awaits the Emergency review of an upgrade
/// ([`candidate_guard_mask`]). `execute_pool_switch` treats such a window candidate as **not
/// adoptable now** and cancels the window instead of reverting (M4V-4): a reverting execution
/// left the window in place, and every other proposal — on the other, unflagged Raydium
/// program too — refused `PoolWindowClosed` until the review, so the fast path stayed frozen
/// while the halted incumbent's positions ran towards S3. The chain stays armed, so a proposal
/// on the other program opens a new window at once.
pub fn candidate_latched(guard: &VenueGuard, kind: VenueKind) -> bool {
    guard.blocks(candidate_guard_mask(kind))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::constants::TOKEN_PROGRAM_ID;
    use bytemuck::Zeroable;

    fn code(e: &anchor_lang::error::Error) -> Option<u32> {
        match e {
            anchor_lang::error::Error::AnchorError(a) => Some(a.error_code_number),
            anchor_lang::error::Error::ProgramError(_) => None,
        }
    }

    /// M4-SEC-8: a pool-selection candidate waits only for the review of its own program; an
    /// open on either Raydium venue waits for both (R4B-7).
    #[test]
    fn a_candidate_latches_on_its_own_program() {
        let mut g = VenueGuard::zeroed();
        for (flagged, v4_ok, cpmm_ok) in [
            (None, true, true),
            (Some(GUARD_RAYDIUM_V4), false, true),
            (Some(GUARD_RAYDIUM_CPMM), true, false),
        ] {
            g.upgraded = flagged.map_or(0, |b| 1 << b);
            assert_eq!(
                candidate_latch(&g, VenueKind::RaydiumV4).is_ok(),
                v4_ok,
                "{flagged:?}"
            );
            assert_eq!(
                candidate_latch(&g, VenueKind::RaydiumCpmm).is_ok(),
                cpmm_ok,
                "{flagged:?}"
            );
            let open_ok = flagged.is_none();
            assert_eq!(latch(&g, VenueKind::RaydiumV4).is_ok(), open_ok);
            assert_eq!(latch(&g, VenueKind::RaydiumCpmm).is_ok(), open_ok);
        }
        assert_eq!(
            candidate_guard_mask(VenueKind::RaydiumV4)
                | candidate_guard_mask(VenueKind::RaydiumCpmm),
            crate::constants::GUARD_MASK_RAYDIUM
        );
        assert_eq!(
            candidate_guard_mask(VenueKind::PumpSwap),
            VenueKind::PumpSwap.guard_mask()
        );
    }

    #[test]
    fn keys_first_then_the_path() {
        let mint_key = Pubkey::new_unique();
        let mut market = Market::zeroed();
        market.mint = mint_key;
        market.token_program = TOKEN_PROGRAM_ID;
        let (mut lamports, mut data) = (1u64, [0u8; 82]);
        let mint = AccountInfo::new(
            &mint_key,
            false,
            false,
            &mut lamports,
            &mut data,
            &TOKEN_PROGRAM_ID,
            false,
        );
        let mismatch = u32::from(ShortwinError::AccountMismatch);

        let wrong_program = Pubkey::new_unique();
        let e = mint_guard(&market, &mint, &wrong_program, MintPath::Exit).unwrap_err();
        assert_eq!(code(&e), Some(mismatch));

        let mut other = market;
        other.mint = Pubkey::new_unique();
        let e = mint_guard(&other, &mint, &TOKEN_PROGRAM_ID, MintPath::Exit).unwrap_err();
        assert_eq!(code(&e), Some(mismatch));

        // Exit never reverts on the filter: the zeroed 82-byte mint is uninitialized, so the
        // filter fails (`MintMalformed`) and the guard reports it tripped.
        assert!(matches!(
            mint_guard(&market, &mint, &TOKEN_PROGRAM_ID, MintPath::Exit),
            Ok(false)
        ));

        // Entry refuses a flagged market before running the filter.
        let mut flagged = market;
        flagged.set_flag(MARKET_FLAG_MINT_UNSAFE, true);
        let e = mint_guard(&flagged, &mint, &TOKEN_PROGRAM_ID, MintPath::Entry).unwrap_err();
        assert_eq!(code(&e), Some(u32::from(ShortwinError::MintUnsafeFlagged)));
    }
}
