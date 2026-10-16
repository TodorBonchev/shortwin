//! `VenueGuard` (zero-copy, seeds `["venue_guard"]`, 224 B): the event-driven venue-upgrade
//! latch (ARCH §2.3.2, §3.1, A7; decision 4; hardened in v0.4.1, R4B-7; AIR-01).
//!
//! The permissionless `flag_venue_upgraded` sets bit `venue` of `upgraded` when a venue
//! ProgramData's deploy slot differs from `guard_slot[venue]` and the change is an upgrade, and
//! while the bit is set refreshes `flagged_slot` / `flagged_at` whenever the deploy slot changes
//! again. The Emergency `revalidate_venue(venue, expected_slot, expected_elf_hash)` clears the
//! bit for the deploy it reviewed, records the current deploy slot as `guard_slot[venue]` and,
//! with a hash, the reviewed ELF's fingerprint. `open_short` refuses while a bit of its venue's
//! mask is set (`GUARD_MASK_*`). Kept out of `GlobalConfig` so a permissionless writer never
//! write-locks the account every instruction reads (V1-11).
//!
//! **The ELF fingerprint (AIR-01, from `_reserved`).** The loader's `ExtendProgram` is
//! permissionless (Agave 4.2) and moves the deploy slot without touching the ELF, so the slot alone
//! let anyone set the latch. For the four venue programs whose ProgramData can still grow (pump-fees,
//! AMM v4, CPMM, Jupiter: guard indices [`GUARD_ELF_FIRST`]`..`; pump and PumpSwap sit at the
//! loader's 10 MiB cap, V9, so only an `Upgrade` moves their slot) the guard keeps, per venue,
//! `guard_len` — the ProgramData's data length when it was revalidated — and `guard_elf` — the
//! first [`GUARD_ELF_LEN`] bytes of the sha256 of the reviewed trimmed ELF
//! (`shortwin_venue_state::program_data::trimmed_elf`). An unchanged length means only `Upgrade`
//! can have moved the slot (it never resizes the account; `ExtendProgram` always grows it); a
//! changed length means an extension, and then only a different trimmed ELF is an upgrade.
//! `guard_len == 0` means no fingerprint (the `initialize_config` state, or a revalidation without
//! a hash): the slot alone decides, as before.
//!
//! | Offset | Bytes | Fields |
//! |---|---|---|
//! | 8 | 8 | `version` u16, `bump` u8, `_pad0` u8, `upgraded` u32 |
//! | 16 | 48 | `guard_slot` [u64; 6] |
//! | 64 | 48 | `flagged_slot` [u64; 6] |
//! | 112 | 48 | `flagged_at` [i64; 6] |
//! | 160 | 16 | `guard_len` [u32; 4] (AIR-01, was `_reserved[0..16]`) |
//! | 176 | 48 | `guard_elf` [[u8; 12]; 4] (AIR-01, was `_reserved[16..64]`) |
//!
//! Index order (`GUARD_*` in `constants.rs`): pump, pAMM, pfee, AMM v4, CPMM, Jupiter.

use anchor_lang::prelude::*;

use crate::constants::{GUARD_PUMP_FEES, VENUE_PROGRAMS};

/// The first guard index with an ELF fingerprint (pump-fees; AIR-01).
pub const GUARD_ELF_FIRST: u8 = GUARD_PUMP_FEES;
/// Venue programs with an ELF fingerprint: pump-fees, AMM v4, CPMM, Jupiter.
pub const GUARD_ELF_VENUES: usize = VENUE_PROGRAMS - GUARD_ELF_FIRST as usize;
/// Bytes of the trimmed ELF's sha256 the guard keeps (96 bits: a second preimage of a recorded
/// fingerprint costs ~2^96 hashes).
pub const GUARD_ELF_LEN: usize = 12;

/// The fingerprint `VenueGuard::guard_elf` keeps of a trimmed ELF's sha256: its first
/// [`GUARD_ELF_LEN`] bytes (AIR-01).
pub fn elf_fingerprint_of(sha256: &[u8; 32]) -> [u8; GUARD_ELF_LEN] {
    let mut fp = [0u8; GUARD_ELF_LEN];
    for (d, s) in fp.iter_mut().zip(sha256.iter()) {
        *d = *s;
    }
    fp
}

/// The venue-upgrade latch.
#[account(zero_copy)]
pub struct VenueGuard {
    /// Layout version ([`crate::constants::VENUE_GUARD_VERSION`]).
    pub version: u16,
    /// PDA bump of `["venue_guard"]`.
    pub bump: u8,
    pub _pad0: u8,
    /// One bit per venue program: upgraded and not yet re-validated.
    pub upgraded: u32,
    /// Last validated ProgramData deploy slot per venue program.
    pub guard_slot: [u64; 6],
    /// The latest deploy slot seen while flagged.
    pub flagged_slot: [u64; 6],
    /// When `flagged_slot` was last recorded (unix seconds).
    pub flagged_at: [i64; 6],
    /// AIR-01: the ProgramData data length at the last revalidation with a hash, per venue from
    /// [`GUARD_ELF_FIRST`]; 0 = no fingerprint.
    pub guard_len: [u32; 4],
    /// AIR-01: the first [`GUARD_ELF_LEN`] bytes of the sha256 of the reviewed trimmed ELF, per
    /// venue from [`GUARD_ELF_FIRST`] (meaningful while `guard_len` is not 0).
    pub guard_elf: [[u8; 12]; 4],
}

impl VenueGuard {
    /// PDA seed.
    pub const SEED: &'static [u8] = b"venue_guard";
    /// Account size including the discriminator.
    pub const SPACE: usize = 8 + core::mem::size_of::<Self>();

    /// `true` if any bit of `mask` is set (an open on a venue with that mask is refused, I17).
    pub fn blocks(&self, mask: u32) -> bool {
        self.upgraded & mask != 0
    }

    /// `true` if venue program `venue` is flagged.
    pub fn is_flagged(&self, venue: u8) -> bool {
        (venue as usize) < VENUE_PROGRAMS && self.upgraded & (1u32 << venue) != 0
    }

    /// The fingerprint slot of `venue` (`guard_len` / `guard_elf`), `None` for pump and
    /// PumpSwap (AIR-01).
    pub fn elf_index(venue: u8) -> Option<usize> {
        venue
            .checked_sub(GUARD_ELF_FIRST)
            .map(usize::from)
            .filter(|&j| j < GUARD_ELF_VENUES)
    }

    /// The recorded `(data length, fingerprint)` of `venue`, if any (AIR-01).
    pub fn elf_fingerprint(&self, venue: u8) -> Option<(u32, [u8; GUARD_ELF_LEN])> {
        let j = Self::elf_index(venue)?;
        let len = *self.guard_len.get(j)?;
        let elf = *self.guard_elf.get(j)?;
        (len != 0).then_some((len, elf))
    }

    /// Records (`Some`) or clears (`None`) the fingerprint of `venue`; nothing for pump and
    /// PumpSwap (AIR-01).
    pub fn set_elf_fingerprint(&mut self, venue: u8, fp: Option<(u32, [u8; GUARD_ELF_LEN])>) {
        let Some(j) = Self::elf_index(venue) else {
            return;
        };
        let (len, elf) = fp.unwrap_or((0, [0; GUARD_ELF_LEN]));
        if let (Some(l), Some(e)) = (self.guard_len.get_mut(j), self.guard_elf.get_mut(j)) {
            *l = len;
            *e = elf;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{GUARD_JUPITER, GUARD_PUMP, GUARD_PUMP_AMM, GUARD_RAYDIUM_CPMM};

    #[test]
    fn the_fingerprinted_venues_are_those_below_the_cap() {
        assert_eq!(VenueGuard::elf_index(GUARD_PUMP), None);
        assert_eq!(VenueGuard::elf_index(GUARD_PUMP_AMM), None);
        assert_eq!(VenueGuard::elf_index(GUARD_PUMP_FEES), Some(0));
        assert_eq!(VenueGuard::elf_index(GUARD_RAYDIUM_CPMM), Some(2));
        assert_eq!(VenueGuard::elf_index(GUARD_JUPITER), Some(3));
        assert_eq!(VenueGuard::elf_index(GUARD_JUPITER + 1), None);
        assert_eq!(GUARD_ELF_VENUES, 4);
    }

    #[test]
    fn a_fingerprint_is_recorded_and_cleared() {
        let mut g: VenueGuard = bytemuck::Zeroable::zeroed();
        assert_eq!(g.elf_fingerprint(GUARD_RAYDIUM_CPMM), None, "none at init");
        g.set_elf_fingerprint(GUARD_RAYDIUM_CPMM, Some((793_869, [9; GUARD_ELF_LEN])));
        assert_eq!(
            g.elf_fingerprint(GUARD_RAYDIUM_CPMM),
            Some((793_869, [9; GUARD_ELF_LEN]))
        );
        assert_eq!(g.guard_len, [0, 0, 793_869, 0]);
        g.set_elf_fingerprint(GUARD_PUMP, Some((1, [1; GUARD_ELF_LEN])));
        assert_eq!(g.elf_fingerprint(GUARD_PUMP), None, "pump has no slot");
        g.set_elf_fingerprint(GUARD_RAYDIUM_CPMM, None);
        assert_eq!(g.elf_fingerprint(GUARD_RAYDIUM_CPMM), None);
        assert_eq!(g.guard_elf, [[0; GUARD_ELF_LEN]; 4]);
    }
}
