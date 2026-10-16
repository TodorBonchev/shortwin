//! `revalidate_venue(venue, expected_slot, expected_elf_hash)` (ARCH §3.1, A7; R4B-7; AIR-01):
//! the Emergency vault clears the latch for a venue program after reviewing the upgrade.
//!
//! `expected_slot` is the deploy slot the Emergency signers reviewed and `expected_elf_hash` the
//! sha256 of that deploy's trimmed ELF (`shortwin_venue_state::program_data::trimmed_elf`, what
//! `solana-verify get-program-hash` prints; all zero: no hash). The ProgramData's current deploy
//! slot must equal `expected_slot` — or, for a fingerprinted venue (pump-fees, AMM v4, CPMM,
//! Jupiter) and a hash, the current trimmed ELF must hash to `expected_elf_hash` — else
//! `VenueSlotMismatch`, so an upgrade that lands while the Squads proposal collects signatures
//! cannot be validated silently. Then `guard_slot[venue]` = the current deploy slot and bit
//! `venue` is cleared; for a fingerprinted venue the guard records the ProgramData's length and
//! the hash's first 12 bytes (`VenueGuard::guard_len` / `guard_elf`), or clears them without a
//! hash. Same address and owner checks as `flag_venue_upgraded`.
//!
//! **Extensions during review (AIR-01).** The loader's permissionless `ExtendProgram` moves the
//! deploy slot without touching the ELF, so with the exact slot alone anyone could refuse every
//! revalidation by extending the venue before the Emergency's transaction executed. A later slot
//! is therefore accepted when the account still holds exactly the reviewed ELF; the check hashes
//! it (half a CU per trimmed-ELF byte: the executor requests the compute budget). With the slot
//! unchanged nothing is hashed and the hash is recorded as given: a wrong one can only make a
//! later extension count as an upgrade (fail-safe), never hide an ELF the Emergency did not name.
//! Pump and PumpSwap (no fingerprint; their ProgramData sit at the 10 MiB cap, which no extension
//! can pass) keep the exact slot.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;

use crate::constants::{venue_program_data, CONFIG_VERSION, VENUE_GUARD_VERSION, VENUE_PROGRAMS};
use crate::errors::ShortwinError;
use crate::events::VenueRevalidated;
use crate::instructions::flag_venue_upgraded::{read_deploy_slot, trimmed_elf_hash};
use crate::state::{elf_fingerprint_of, GlobalConfig, VenueGuard};

#[event_cpi]
#[derive(Accounts)]
#[instruction(venue: u8)]
pub struct RevalidateVenue<'info> {
    pub pause_authority: Signer<'info>,
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion,
        constraint = config.load()?.pause_authority == pause_authority.key()
            @ ShortwinError::Unauthorized
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    #[account(
        mut,
        seeds = [VenueGuard::SEED],
        bump = venue_guard.load()?.bump,
        constraint = venue_guard.load()?.version == VENUE_GUARD_VERSION
            @ ShortwinError::ConfigVersion,
        constraint = usize::from(venue) < VENUE_PROGRAMS @ ShortwinError::InvalidParams
    )]
    pub venue_guard: AccountLoader<'info, VenueGuard>,
    /// CHECK: the venue program's ProgramData: key == the compile-time constant, owner == the
    /// upgradeable loader; its header is read, and its ELF when the slot moved (AIR-01).
    #[account(
        address = venue_program_data(venue).unwrap_or_default()
            @ ShortwinError::VenueAccountMismatch,
        owner = bpf_loader_upgradeable::ID @ ShortwinError::VenueAccountMismatch
    )]
    pub program_data: UncheckedAccount<'info>,
}

pub fn handler(
    ctx: Context<RevalidateVenue>,
    venue: u8,
    expected_slot: u64,
    expected_elf_hash: [u8; 32],
) -> Result<()> {
    let slot = read_deploy_slot(&ctx.accounts.program_data)?;
    let hashed = expected_elf_hash != [0u8; 32];
    let fingerprinted = VenueGuard::elf_index(venue).is_some();
    if slot != expected_slot {
        // AIR-01: the reviewed deploy, only extended since, still holds the reviewed ELF.
        require!(hashed && fingerprinted, ShortwinError::VenueSlotMismatch);
        require!(
            trimmed_elf_hash(&ctx.accounts.program_data)? == Some(expected_elf_hash),
            ShortwinError::VenueSlotMismatch
        );
    }
    let len = u32::try_from(ctx.accounts.program_data.data_len())
        .map_err(|_| error!(ShortwinError::VenueAccountMismatch))?;
    let clock = Clock::get()?;
    let i = usize::from(venue);
    let bit = 1u32
        .checked_shl(u32::from(venue))
        .ok_or_else(|| error!(ShortwinError::InvalidParams))?;
    let (old_slot, flagged_at) = {
        let mut g = ctx.accounts.venue_guard.load_mut()?;
        let was_flagged = g.upgraded & bit != 0;
        let flagged_at = if was_flagged {
            g.flagged_at.get(i).copied().unwrap_or_default()
        } else {
            0
        };
        let slot_ref = g
            .guard_slot
            .get_mut(i)
            .ok_or_else(|| error!(ShortwinError::InvalidParams))?;
        let old = *slot_ref;
        *slot_ref = slot;
        g.set_elf_fingerprint(
            venue,
            hashed.then(|| (len, elf_fingerprint_of(&expected_elf_hash))),
        );
        g.upgraded &= !bit;
        (old, flagged_at)
    };
    emit_cpi!(VenueRevalidated {
        market: Pubkey::default(),
        slot: clock.slot,
        ts: clock.unix_timestamp,
        venue,
        guard_slot: old_slot,
        programdata_slot: slot,
        flagged_at,
        expected_slot,
    });
    Ok(())
}
