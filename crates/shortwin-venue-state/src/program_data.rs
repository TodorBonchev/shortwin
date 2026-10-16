//! BPF upgradeable loader `ProgramData` header (ARCH §4.1; §3.1 venue-upgrade latch, A7; C1-15)
//! and its trimmed ELF (AIR-01).
//!
//! `UpgradeableLoaderState::ProgramData` in bincode: `tag u32 @0 (== 3)`, `slot u64 @4` (the
//! last deploy slot), `upgrade_authority_address Option<Pubkey>` as `u8 @12` (0 = None, 1 =
//! Some) and the key `@13`; the ELF follows at 45 [R-PUMP §1; V9]. Only
//! `flag_venue_upgraded` and `revalidate_venue` read a venue's ProgramData (ARCH §6.5). The
//! header reader touches nothing past the header (the account can be 10 MiB).
//!
//! **The trimmed ELF (AIR-01).** The deploy slot moves on an `Upgrade` and on the loader's
//! `ExtendProgram`, which is permissionless since Agave 4.2 and appends zeros without touching the
//! ELF. `Upgrade` writes the new ELF after the header and zeroes the rest of the account, so two
//! deploys hold the same program exactly when their bytes after the header are equal once the
//! trailing zeros are dropped: [`trimmed_elf`], whose sha256 is the verifiable-build program hash
//! (`solana-verify get-program-hash`, OtterSec's on-chain hash). [`trimmed_len`] finds its end
//! with one `memcmp` per window, so the zero tail an extension appends (up to 10 MiB) costs the
//! program about 1 CU per 250 bytes (the `sol_memcmp_` syscall), never a byte loop.

use crate::bytes::{key_at, u8_at, u32_at, u64_at};
use crate::{Key, ReadError};

/// `BPFLoaderUpgradeab1e11111111111111111111111`, the owner of every `ProgramData` account.
pub const LOADER_UPGRADEABLE: Key = [
    2, 168, 246, 145, 78, 136, 161, 176, 226, 16, 21, 62, 247, 99, 174, 43, 0, 194, 185, 61, 22,
    193, 36, 210, 192, 83, 122, 16, 4, 128, 0, 0,
];

/// bincode enum tag of `UpgradeableLoaderState::ProgramData`.
pub const TAG_PROGRAM_DATA: u32 = 3;

/// Offset of the deploy slot.
pub const SLOT_OFFSET: usize = 4;

/// Offset of the upgrade authority's `Option` tag.
pub const AUTHORITY_OFFSET: usize = 12;

/// Length of the header (`tag + slot + Option<Pubkey>`); the ELF starts here.
pub const HEADER_LEN: usize = 45;

/// Smallest account this reader accepts: the tag and the deploy slot.
pub const MIN_LEN: usize = 12;

/// The `ProgramData` header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProgramDataHeader {
    /// Slot of the last deployment (upgrade) of the program.
    pub slot: u64,
    /// Upgrade authority, `None` once the program is immutable (or the field is absent in a
    /// truncated account: zero padding reads `None`).
    pub upgrade_authority: Option<Key>,
}

/// Reads the header of a `ProgramData` account: owner == the upgradeable loader, at least
/// [`MIN_LEN`] bytes, tag == 3, a valid `Option` tag.
pub fn read(owner: &Key, data: &[u8]) -> Result<ProgramDataHeader, ReadError> {
    if *owner != LOADER_UPGRADEABLE {
        return Err(ReadError::WrongOwner);
    }
    if data.len() < MIN_LEN {
        return Err(ReadError::TooShort);
    }
    if u32_at(data, 0) != TAG_PROGRAM_DATA {
        return Err(ReadError::WrongTag);
    }
    let upgrade_authority = match u8_at(data, AUTHORITY_OFFSET) {
        0 => None,
        1 => Some(key_at(data, AUTHORITY_OFFSET.saturating_add(1))),
        _ => return Err(ReadError::Malformed),
    };
    Ok(ProgramDataHeader {
        slot: u64_at(data, SLOT_OFFSET),
        upgrade_authority,
    })
}

/// The deploy slot of a `ProgramData` account (the header field the venue-upgrade latch reads).
pub fn deploy_slot(owner: &Key, data: &[u8]) -> Result<u64, ReadError> {
    read(owner, data).map(|h| h.slot)
}

/// Bytes of the windows [`trimmed_len`] tests for zeros from the end, one `memcmp` each.
pub const TRIM_WINDOW: usize = 64 * 1024;

/// Bytes of the steps [`trimmed_len`] narrows the last non-zero window with, before it scans
/// the last non-zero step byte by byte.
pub const TRIM_STEP: usize = 512;

/// The trimmed ELF of a `ProgramData` account (module docs): the bytes after the header up to and
/// including the last non-zero one ([`trimmed_len`]); empty for a header-only account. The header
/// is checked as [`read`] checks it.
pub fn trimmed_elf<'a>(owner: &Key, data: &'a [u8]) -> Result<&'a [u8], ReadError> {
    read(owner, data)?;
    let elf = data.get(HEADER_LEN..).unwrap_or_default();
    Ok(elf.get(..trimmed_len(elf)).unwrap_or_default())
}

/// The length of `bytes` without its trailing zero bytes. From the end: [`TRIM_WINDOW`]-byte
/// windows while they are all zero, then [`TRIM_STEP`]-byte steps inside the first window that is
/// not, then that step's bytes — about one `memcmp` byte per byte of zero tail.
pub fn trimmed_len(bytes: &[u8]) -> usize {
    let mut end = bytes.len();
    while end > 0 {
        let start = end.saturating_sub(TRIM_WINDOW);
        let window = bytes.get(start..end).unwrap_or_default();
        if !all_zero(window) {
            return start.saturating_add(nonzero_len(window));
        }
        end = start;
    }
    0
}

/// [`trimmed_len`] inside one window: [`TRIM_STEP`]-byte steps from the end, then bytes.
fn nonzero_len(window: &[u8]) -> usize {
    let mut end = window.len();
    while end > 0 {
        let start = end.saturating_sub(TRIM_STEP);
        let step = window.get(start..end).unwrap_or_default();
        if !all_zero(step) {
            return step
                .iter()
                .rposition(|&b| b != 0)
                .map_or(start, |i| start.saturating_add(i).saturating_add(1));
        }
        end = start;
    }
    0
}

/// `true` if every byte of `w` is zero: its first 8 bytes are, and `w` shifted by 8 bytes equals
/// itself (`w[i + 8] == w[i]` for every `i`). Each slice comparison is one `memcmp`, which the
/// program's SBF build runs as the `sol_memcmp_` syscall (`max(10, n / 250)` CU).
fn all_zero(w: &[u8]) -> bool {
    match w.split_at_checked(8) {
        Some((head, rest)) => head == [0u8; 8] && w.get(..rest.len()) == Some(rest),
        None => w.iter().all(|&b| b == 0),
    }
}

/// Encodes a `ProgramData` header (tests and fixtures; the inverse of [`read`]).
pub fn encode_header(slot: u64, upgrade_authority: Option<&Key>) -> [u8; HEADER_LEN] {
    let mut out = [0u8; HEADER_LEN];
    let (tag, rest) = out.split_at_mut(SLOT_OFFSET);
    tag.copy_from_slice(&TAG_PROGRAM_DATA.to_le_bytes());
    let (slot_bytes, rest) = rest.split_at_mut(8);
    slot_bytes.copy_from_slice(&slot.to_le_bytes());
    if let Some(key) = upgrade_authority {
        let (flag, key_bytes) = rest.split_at_mut(1);
        flag.copy_from_slice(&[1]);
        key_bytes.copy_from_slice(key);
    }
    out
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    const AUTH: Key = [7u8; 32];

    #[test]
    fn round_trip() {
        let h = encode_header(449_734_335, Some(&AUTH));
        assert_eq!(
            read(&LOADER_UPGRADEABLE, &h),
            Ok(ProgramDataHeader {
                slot: 449_734_335,
                upgrade_authority: Some(AUTH)
            })
        );
        let h = encode_header(1, None);
        assert_eq!(deploy_slot(&LOADER_UPGRADEABLE, &h), Ok(1));
        assert_eq!(
            read(&LOADER_UPGRADEABLE, &h).map(|h| h.upgrade_authority),
            Ok(None)
        );
    }

    #[test]
    fn ignores_the_elf_after_the_header() {
        let mut data = [0xabu8; 200];
        data[..HEADER_LEN].copy_from_slice(&encode_header(42, Some(&AUTH)));
        assert_eq!(deploy_slot(&LOADER_UPGRADEABLE, &data), Ok(42));
    }

    #[test]
    fn refusals() {
        let h = encode_header(5, None);
        assert_eq!(deploy_slot(&[0u8; 32], &h), Err(ReadError::WrongOwner));
        assert_eq!(
            deploy_slot(&LOADER_UPGRADEABLE, &h[..11]),
            Err(ReadError::TooShort)
        );
        let mut program = h;
        program[0] = 2; // UpgradeableLoaderState::Program
        assert_eq!(
            deploy_slot(&LOADER_UPGRADEABLE, &program),
            Err(ReadError::WrongTag)
        );
        let mut bad_option = h;
        bad_option[AUTHORITY_OFFSET] = 2;
        assert_eq!(
            read(&LOADER_UPGRADEABLE, &bad_option),
            Err(ReadError::Malformed)
        );
    }

    #[test]
    fn truncated_header_reads_no_authority() {
        let h = encode_header(9, Some(&AUTH));
        assert_eq!(
            read(&LOADER_UPGRADEABLE, &h[..MIN_LEN]),
            Ok(ProgramDataHeader {
                slot: 9,
                upgrade_authority: None
            })
        );
    }

    /// The reference: after the last non-zero byte.
    fn naive(bytes: &[u8]) -> usize {
        bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1)
    }

    /// Three windows and a bit: every boundary of the window / step scan is inside.
    const BIG: usize = 3 * TRIM_WINDOW + 3 * TRIM_STEP + 7;

    #[test]
    fn trimmed_len_small_cases() {
        for bytes in [
            &[][..],
            &[0],
            &[0; 9],
            &[1],
            &[1, 0, 0],
            &[0, 0, 5, 0],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
            &[7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ] {
            assert_eq!(trimmed_len(bytes), naive(bytes), "{bytes:?}");
        }
    }

    #[test]
    fn trimmed_len_finds_the_last_nonzero_byte_across_every_boundary() {
        let mut buf = [0u8; BIG];
        assert_eq!(trimmed_len(&buf), 0, "all zero");
        let mut marks = [0usize; 64];
        let mut n = 0;
        for base in [0, TRIM_STEP, TRIM_WINDOW, 2 * TRIM_WINDOW, 3 * TRIM_WINDOW] {
            for d in [0, 1, 7, 8, 9] {
                for p in [base.saturating_sub(d), base + d] {
                    if p < BIG && n < marks.len() {
                        marks[n] = p;
                        n += 1;
                    }
                }
            }
        }
        marks[n] = BIG - 1;
        n += 1;
        for &p in &marks[..n] {
            buf[p] = 0xa5;
            assert_eq!(trimmed_len(&buf), p + 1, "one byte at {p}");
            // Zeros after it and a non-zero run before it change nothing.
            if p > 3 {
                buf[p - 3] = 1;
                assert_eq!(trimmed_len(&buf), p + 1, "a run ending at {p}");
                buf[p - 3] = 0;
            }
            buf[p] = 0;
        }
        // Pseudo-random contents up to random ends.
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..40 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let end = (x as usize) % BIG;
            for (i, b) in buf.iter_mut().enumerate() {
                *b = if i < end && (x >> (i % 61)) & 3 == 0 {
                    (i % 251) as u8 | 1
                } else {
                    0
                };
            }
            assert_eq!(trimmed_len(&buf), naive(&buf), "end {end}");
        }
    }

    #[test]
    fn trimmed_elf_drops_the_header_and_the_zero_tail() {
        let mut data = [0u8; HEADER_LEN + 40];
        data[..HEADER_LEN].copy_from_slice(&encode_header(3, Some(&AUTH)));
        data[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(b"\x7fELF");
        data[HEADER_LEN + 9] = 2;
        assert_eq!(
            trimmed_elf(&LOADER_UPGRADEABLE, &data),
            Ok(&data[HEADER_LEN..HEADER_LEN + 10])
        );
        // An extension (more zeros) leaves the trimmed ELF as it was.
        let mut extended = [0u8; HEADER_LEN + 4_000];
        extended[..data.len()].copy_from_slice(&data);
        assert_eq!(
            trimmed_elf(&LOADER_UPGRADEABLE, &extended),
            trimmed_elf(&LOADER_UPGRADEABLE, &data)
        );
        // A header alone, or zeros only: empty.
        let h = encode_header(3, None);
        assert_eq!(trimmed_elf(&LOADER_UPGRADEABLE, &h), Ok(&[][..]));
        assert_eq!(trimmed_elf(&LOADER_UPGRADEABLE, &h[..MIN_LEN]), Ok(&[][..]));
        let mut zeros = [0u8; HEADER_LEN + 100];
        zeros[..HEADER_LEN].copy_from_slice(&h);
        assert_eq!(trimmed_elf(&LOADER_UPGRADEABLE, &zeros), Ok(&[][..]));
        // The header checks of `read`.
        assert_eq!(trimmed_elf(&[0u8; 32], &data), Err(ReadError::WrongOwner));
        let mut program = data;
        program[0] = 2;
        assert_eq!(
            trimmed_elf(&LOADER_UPGRADEABLE, &program),
            Err(ReadError::WrongTag)
        );
    }
}
