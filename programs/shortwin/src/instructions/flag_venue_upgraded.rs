//! `flag_venue_upgraded(venue)` (ARCH §3.1, §3.9, A7; R4B-7; AIR-01): **anyone** records a venue
//! program upgrade in the `VenueGuard` latch.
//!
//! `venue` is a `VenueGuard` index (pump, pAMM, pfee, AMM v4, CPMM, Jupiter). The ProgramData
//! account must be the compile-time constant for that venue and owned by the upgradeable
//! loader, with tag 3. If the bit is clear, the deploy slot differs from `guard_slot` and the
//! change is an upgrade (below): set the bit, record `flagged_slot` / `flagged_at`. If the bit is
//! set and the deploy slot differs from `flagged_slot` (another upgrade during review): refresh
//! them. Either way emit `VenueUpgradeFlagged`. Otherwise fail with `VenueNotUpgraded`, writing
//! nothing.
//!
//! **An extension is not an upgrade (AIR-01).** The loader's `ExtendProgram` is permissionless
//! (Agave 4.2): it appends zeros, keeps the ELF and writes the current slot into the header, so
//! a moved slot alone let anyone set the latch and stop every open on the venue's markets. With
//! the bit clear and a fingerprint recorded for the venue (`VenueGuard::elf_fingerprint`:
//! pump-fees, AMM v4, CPMM and Jupiter, once `revalidate_venue` named the reviewed ELF's hash), a
//! moved slot is an upgrade if the ProgramData kept the length recorded with the fingerprint
//! (only `Upgrade` moves the slot without resizing the account; `ExtendProgram` always grows it),
//! or if the account grew and the sha256 of its trimmed ELF
//! (`shortwin_venue_state::program_data::trimmed_elf`) no longer starts with the fingerprint; a
//! grown account whose trimmed ELF still matches was only extended (`VenueNotUpgraded`). The hash
//! runs only after an extension and costs half a CU per trimmed-ELF byte plus about 1 CU per
//! 250 bytes of zero tail (pump-fees' 0.85 MB ELF ≈ 0.43 M CU: a flagger requests the compute
//! budget); a trimmed ELF above [`ELF_HASH_MAX_LEN`] is not hashed and counts as an upgrade
//! (fail-safe). Without a fingerprint — pump and PumpSwap, whose ProgramData sit at the loader's
//! 10 MiB cap so that only `Upgrade` moves their slot, and every venue until its first
//! revalidation with a hash — the slot alone decides. While the bit is set any later slot
//! refreshes the record (the program cannot tell an extension of the unreviewed ELF from another
//! upgrade; opens are blocked either way, and `revalidate_venue` binds to the reviewed ELF, not to
//! `flagged_slot`).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;

use crate::constants::{venue_program_data, VENUE_GUARD_VERSION, VENUE_PROGRAMS};
use crate::errors::ShortwinError;
use crate::events::VenueUpgradeFlagged;
use crate::state::{elf_fingerprint_of, VenueGuard, GUARD_ELF_LEN};

/// The largest trimmed ELF the latch hashes (AIR-01): 2 MiB, ≈ 1.05 M CU of `sol_sha256` (half a
/// CU per byte) plus ≤ 45 k CU to scan a 10 MiB zero tail, inside the 1.4 M CU transaction limit.
/// The fingerprinted venues' ELFs are 0.31–0.85 MB (AMM v4, CPMM, pump-fees); pump's 1.77 MB is
/// never hashed (no fingerprint).
pub const ELF_HASH_MAX_LEN: usize = 2 * 1024 * 1024;

#[event_cpi]
#[derive(Accounts)]
#[instruction(venue: u8)]
pub struct FlagVenueUpgraded<'info> {
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
    /// upgradeable loader; its header is read (`shortwin_venue_state`), and its ELF when the
    /// account grew since a fingerprinted revalidation (AIR-01).
    #[account(
        address = venue_program_data(venue).unwrap_or_default()
            @ ShortwinError::VenueAccountMismatch,
        owner = bpf_loader_upgradeable::ID @ ShortwinError::VenueAccountMismatch
    )]
    pub program_data: UncheckedAccount<'info>,
}

/// The deploy slot of a venue ProgramData (`VenueAccountMismatch` if the header does not
/// parse: wrong owner, too short, not tag 3).
pub fn read_deploy_slot(program_data: &AccountInfo) -> Result<u64> {
    let data = program_data.try_borrow_data()?;
    shortwin_venue_state::program_data::deploy_slot(&program_data.owner.to_bytes(), &data)
        .map_err(|_| error!(ShortwinError::VenueAccountMismatch))
}

/// The sha256 of a venue ProgramData's trimmed ELF (AIR-01: what `solana-verify
/// get-program-hash` prints), `None` above [`ELF_HASH_MAX_LEN`]; `VenueAccountMismatch` if the
/// header does not parse.
pub fn trimmed_elf_hash(program_data: &AccountInfo) -> Result<Option<[u8; 32]>> {
    let data = program_data.try_borrow_data()?;
    trimmed_elf_hash_of(&program_data.owner.to_bytes(), &data)
        .map_err(|_| error!(ShortwinError::VenueAccountMismatch))
}

/// [`trimmed_elf_hash`] on an account's owner and bytes (host callers too: the tests, the
/// operations tool).
pub fn trimmed_elf_hash_of(
    owner: &[u8; 32],
    data: &[u8],
) -> core::result::Result<Option<[u8; 32]>, shortwin_venue_state::ReadError> {
    let elf = shortwin_venue_state::program_data::trimmed_elf(owner, data)?;
    Ok((elf.len() <= ELF_HASH_MAX_LEN).then(|| solana_sha256_hasher::hashv(&[elf]).to_bytes()))
}

/// AIR-01: whether a ProgramData whose deploy slot moved since the revalidation that recorded
/// `(len, fingerprint)` holds an upgrade (module docs): its length kept, or its trimmed ELF
/// changed (or too large to hash).
fn is_upgrade(
    program_data: &AccountInfo,
    (len, fingerprint): (u32, [u8; GUARD_ELF_LEN]),
) -> Result<bool> {
    if u32::try_from(program_data.data_len()).ok() == Some(len) {
        return Ok(true);
    }
    Ok(trimmed_elf_hash(program_data)?.map(|h| elf_fingerprint_of(&h)) != Some(fingerprint))
}

pub fn handler(ctx: Context<FlagVenueUpgraded>, venue: u8) -> Result<()> {
    let slot = read_deploy_slot(&ctx.accounts.program_data)?;
    let clock = Clock::get()?;
    let i = usize::from(venue);
    let bit = 1u32
        .checked_shl(u32::from(venue))
        .ok_or_else(|| error!(ShortwinError::InvalidParams))?;
    let guard_slot = {
        let mut g = ctx.accounts.venue_guard.load_mut()?;
        let guard_slot = *g
            .guard_slot
            .get(i)
            .ok_or_else(|| error!(ShortwinError::InvalidParams))?;
        let flagged_slot = *g
            .flagged_slot
            .get(i)
            .ok_or_else(|| error!(ShortwinError::InvalidParams))?;
        if g.upgraded & bit == 0 {
            require!(slot != guard_slot, ShortwinError::VenueNotUpgraded);
            // AIR-01: an extension moves the slot too; with a fingerprint it is told apart.
            if let Some(fp) = g.elf_fingerprint(venue) {
                require!(
                    is_upgrade(&ctx.accounts.program_data, fp)?,
                    ShortwinError::VenueNotUpgraded
                );
            }
            g.upgraded |= bit;
        } else {
            require!(slot != flagged_slot, ShortwinError::VenueNotUpgraded);
        }
        if let Some(s) = g.flagged_slot.get_mut(i) {
            *s = slot;
        }
        if let Some(t) = g.flagged_at.get_mut(i) {
            *t = clock.unix_timestamp;
        }
        guard_slot
    };
    emit_cpi!(VenueUpgradeFlagged {
        market: Pubkey::default(),
        slot: clock.slot,
        ts: clock.unix_timestamp,
        venue,
        guard_slot,
        programdata_slot: slot,
        flagged_at: clock.unix_timestamp,
    });
    Ok(())
}
