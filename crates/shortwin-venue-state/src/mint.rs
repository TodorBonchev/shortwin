//! SPL Token / Token-2022 mints: base fields (ARCH §4.1: `supply @36`, `decimals @44`,
//! `freeze_authority @46`; R-MINT §3.1) and a byte-level copy of the SPEC §3 allowlist filter
//! (R-MINT §9.2, §10.10).
//!
//! **Which filter is authoritative.** The program's filter
//! (`programs/shortwin/src/mint_filter.rs`, ARCH §2.1) parses the mint with
//! `spl-token-2022-interface` 2.x, as ARCH §3.2 check 1 specifies. [`check`] makes the same
//! decision on raw bytes, with no dependency, for the keeper, the indexer and the WASM
//! frontend ("show the filter verdict and reasons on the market page", R-MINT §10.10). A
//! differential test in the program crate (`mint_filter::tests`) runs both on every fixture
//! and on thousands of mutated mints and requires the same verdict and the same reason.
//!
//! **Layout** (R-MINT §3.1, §3.2):
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 36 | `mint_authority`: `COption<Pubkey>` (u32 LE tag 0/1, then the key) |
//! | 36 | 8 | `supply` u64 |
//! | 44 | 1 | `decimals` |
//! | 45 | 1 | `is_initialized` (0/1) |
//! | 46 | 36 | `freeze_authority`: `COption<Pubkey>` |
//! | 82 | 83 | Token-2022 only, if extended: zero padding up to `Account::LEN` = 165 |
//! | 165 | 1 | Token-2022 only: `AccountType` (1 = Mint) |
//! | 166 | … | Token-2022 only: TLV entries `u16 type ‖ u16 length ‖ value`, ended by type 0 or by fewer than 2 bytes left |
//!
//! An SPL Token mint is exactly 82 bytes; a Token-2022 mint is 82 bytes or at least 166, and
//! never exactly 355 (`Multisig::LEN`).

use crate::bytes::{key_at, u8_at, u16_at, u32_at, u64_at};
use crate::{Key, ReadError};

/// SPL Token (`TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`).
pub const TOKEN_PROGRAM: Key = [
    6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133, 237,
    95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169,
];

/// Token-2022 (`TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`).
pub const TOKEN_2022_PROGRAM: Key = [
    6, 221, 246, 225, 238, 117, 143, 222, 24, 66, 93, 188, 228, 108, 205, 218, 182, 26, 252, 77,
    131, 185, 13, 39, 254, 189, 249, 40, 216, 161, 139, 252,
];

/// SPL Token native mint (`So11111111111111111111111111111111111111112`).
pub const NATIVE_MINT: Key = [
    6, 155, 136, 87, 254, 171, 129, 132, 251, 104, 127, 99, 70, 24, 192, 53, 218, 196, 57, 220, 26,
    235, 59, 85, 152, 160, 240, 0, 0, 0, 0, 1,
];

/// Token-2022 native mint (`9pan9bMn5HatX4EJdBwg9VgCa7Uz5HL8N1m5D3NdXejP`).
pub const NATIVE_MINT_2022: Key = [
    131, 13, 252, 159, 222, 95, 230, 184, 170, 124, 4, 164, 118, 233, 30, 138, 198, 187, 38, 74,
    173, 144, 250, 25, 201, 223, 73, 216, 92, 62, 91, 94,
];

/// Length of a base mint (both programs).
pub const MINT_LEN: usize = 82;

/// Length of a base token account (`Account::LEN`); an extended mint is padded to it.
pub const ACCOUNT_LEN: usize = 165;

/// Length of a multisig account; never a valid extended account length.
pub const MULTISIG_LEN: usize = 355;

/// Offset of `mint_authority` (`COption<Pubkey>`).
pub const MINT_AUTHORITY_OFFSET: usize = 0;

/// Offset of `supply` (u64).
pub const SUPPLY_OFFSET: usize = 36;

/// Offset of `decimals` (u8).
pub const DECIMALS_OFFSET: usize = 44;

/// Offset of `is_initialized` (u8, 0/1).
pub const IS_INITIALIZED_OFFSET: usize = 45;

/// Offset of `freeze_authority` (`COption<Pubkey>`: u32 tag + 32 bytes).
pub const FREEZE_AUTHORITY_OFFSET: usize = 46;

/// Offset of the Token-2022 `AccountType` byte of an extended account.
pub const ACCOUNT_TYPE_OFFSET: usize = ACCOUNT_LEN;

/// Offset of the first TLV entry of an extended Token-2022 account.
pub const TLV_OFFSET: usize = ACCOUNT_LEN + 1;

/// `AccountType::Mint`.
pub const ACCOUNT_TYPE_MINT: u8 = 1;

/// Token-2022 extension type discriminants (`ExtensionType` as `u16`; R-MINT §4). Only the
/// terminator and the allowlisted types are named; every other value is rejected.
pub mod ext {
    /// `Uninitialized`: ends the TLV walk.
    pub const UNINITIALIZED: u16 = 0;
    /// `MetadataPointer`.
    pub const METADATA_POINTER: u16 = 18;
    /// `TokenMetadata` (variable length).
    pub const TOKEN_METADATA: u16 = 19;
    /// `GroupPointer`.
    pub const GROUP_POINTER: u16 = 20;
    /// `TokenGroup`.
    pub const TOKEN_GROUP: u16 = 21;
    /// `GroupMemberPointer`.
    pub const GROUP_MEMBER_POINTER: u16 = 22;
    /// `TokenGroupMember`.
    pub const TOKEN_GROUP_MEMBER: u16 = 23;
}

/// The v1 allowlist (SPEC §3; ARCH §3.2 check 1; R-MINT §0, §4): display and collection data
/// only. Everything else — including types this crate does not know — is rejected.
pub const ALLOWED_EXTENSIONS: [u16; 6] = [
    ext::METADATA_POINTER,
    ext::TOKEN_METADATA,
    ext::GROUP_POINTER,
    ext::TOKEN_GROUP,
    ext::GROUP_MEMBER_POINTER,
    ext::TOKEN_GROUP_MEMBER,
];

/// `true` if `ext_type` is on the allowlist.
pub fn is_allowed_extension(ext_type: u16) -> bool {
    ALLOWED_EXTENSIONS.contains(&ext_type)
}

/// The base fields of an initialized mint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MintBase {
    /// `None` once minting is disabled for good.
    pub mint_authority: Option<Key>,
    /// Raw units in circulation.
    pub supply: u64,
    pub decimals: u8,
    /// `None` = no one can freeze token accounts of this mint (required by the filter).
    pub freeze_authority: Option<Key>,
}

/// A `COption<Pubkey>` at `offset`: tag 0 = `None`, 1 = `Some`, anything else is malformed.
fn coption_key_at(data: &[u8], offset: usize) -> Result<Option<Key>, ReadError> {
    match u32_at(data, offset) {
        0 => Ok(None),
        1 => Ok(Some(key_at(data, offset.saturating_add(4)))),
        _ => Err(ReadError::Malformed),
    }
}

/// The 82-byte base of an initialized mint, exactly as `Mint::unpack` reads it: both
/// `COption` tags valid, `is_initialized == 1`. The caller has checked the length.
fn base_of(data: &[u8]) -> Result<MintBase, ReadError> {
    let mint_authority = coption_key_at(data, MINT_AUTHORITY_OFFSET)?;
    let initialized = match u8_at(data, IS_INITIALIZED_OFFSET) {
        0 => false,
        1 => true,
        _ => return Err(ReadError::Malformed),
    };
    let freeze_authority = coption_key_at(data, FREEZE_AUTHORITY_OFFSET)?;
    if !initialized {
        return Err(ReadError::Malformed);
    }
    Ok(MintBase {
        mint_authority,
        supply: u64_at(data, SUPPLY_OFFSET),
        decimals: u8_at(data, DECIMALS_OFFSET),
        freeze_authority,
    })
}

/// Reads the base fields of an SPL Token or Token-2022 mint: owner ∈ {SPL Token,
/// Token-2022}, at least [`MINT_LEN`] bytes, valid `COption` tags, initialized. It does not
/// look at the extensions (use [`check`] for the filter).
pub fn read_base(owner: &Key, data: &[u8]) -> Result<MintBase, ReadError> {
    if *owner != TOKEN_PROGRAM && *owner != TOKEN_2022_PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if data.len() < MINT_LEN {
        return Err(ReadError::TooShort);
    }
    base_of(data)
}

/// Why [`check`] rejects a mint. One variant per program error of ARCH §3.11's mint group, in
/// the order the program checks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MintReject {
    /// `MintUnsupportedProgram`: not owned by SPL Token or Token-2022.
    UnsupportedProgram,
    /// `MintNative`: a native (wrapped SOL) mint.
    Native,
    /// `MintMalformed`: the base mint does not unpack (length, `COption` tags, uninitialized,
    /// non-zero padding, wrong account type, multisig length).
    Malformed,
    /// `MintFreezeAuthority`: `freeze_authority` is set.
    FreezeAuthority,
    /// `MintDisallowedExtension`: a TLV entry that is not allowlisted (`Some(type)`), or a TLV
    /// walk that does not parse (`None`: a truncated entry or a value past the end).
    DisallowedExtension(Option<u16>),
}

/// Walks the TLV entries after [`TLV_OFFSET`] exactly as `get_extension_types` of
/// `spl-token-2022-interface` does, but stops at the first entry that is not allowlisted.
fn check_tlv(tlv: &[u8]) -> Result<(), MintReject> {
    let mut at = 0usize;
    while at < tlv.len() {
        // Fewer than 2 bytes left: the walk ends (a trailing byte may be realloc slack).
        let Some(length_at) = at.checked_add(2).filter(|&l| l <= tlv.len()) else {
            return Ok(());
        };
        let ext_type = u16_at(tlv, at);
        if ext_type == ext::UNINITIALIZED {
            return Ok(());
        }
        if !is_allowed_extension(ext_type) {
            return Err(MintReject::DisallowedExtension(Some(ext_type)));
        }
        let Some(value_at) = length_at.checked_add(2).filter(|&v| v <= tlv.len()) else {
            return Err(MintReject::DisallowedExtension(None));
        };
        let len = usize::from(u16_at(tlv, length_at));
        let Some(end) = value_at.checked_add(len).filter(|&e| e <= tlv.len()) else {
            return Err(MintReject::DisallowedExtension(None));
        };
        at = end;
    }
    Ok(())
}

/// The SPEC §3 allowlist filter on raw bytes (R-MINT §9.2), with the same decision and
/// reason as the program's `check_mint_safety`:
///
/// 1. owner ∈ {SPL Token, Token-2022} ([`MintReject::UnsupportedProgram`]);
/// 2. `key` is not a native mint ([`MintReject::Native`]);
/// 3. SPL Token: exactly 82 bytes; both programs: the base unpacks (`COption` tags,
///    initialized), and a Token-2022 mint longer than 82 bytes is not 355 bytes, is at least
///    166, has zero padding and `AccountType::Mint` ([`MintReject::Malformed`]);
/// 4. no freeze authority ([`MintReject::FreezeAuthority`]);
/// 5. every TLV entry is allowlisted and the walk parses
///    ([`MintReject::DisallowedExtension`]).
///
/// Returns the base fields of a mint that passes.
pub fn check(key: &Key, owner: &Key, data: &[u8]) -> Result<MintBase, MintReject> {
    let legacy = *owner == TOKEN_PROGRAM;
    if !legacy && *owner != TOKEN_2022_PROGRAM {
        return Err(MintReject::UnsupportedProgram);
    }
    if *key == NATIVE_MINT || *key == NATIVE_MINT_2022 {
        return Err(MintReject::Native);
    }
    if (legacy && data.len() != MINT_LEN) || data.len() < MINT_LEN || data.len() == MULTISIG_LEN {
        return Err(MintReject::Malformed);
    }
    let base = base_of(data).map_err(|_| MintReject::Malformed)?;
    let tlv = match data.get(MINT_LEN..) {
        Some(rest) if !rest.is_empty() => {
            let padding = data
                .get(MINT_LEN..ACCOUNT_TYPE_OFFSET)
                .ok_or(MintReject::Malformed)?;
            if padding.iter().any(|&b| b != 0) {
                return Err(MintReject::Malformed);
            }
            if data.get(ACCOUNT_TYPE_OFFSET) != Some(&ACCOUNT_TYPE_MINT) {
                return Err(MintReject::Malformed);
            }
            data.get(TLV_OFFSET..).unwrap_or(&[])
        }
        _ => &[],
    };
    if base.freeze_authority.is_some() {
        return Err(MintReject::FreezeAuthority);
    }
    check_tlv(tlv)?;
    Ok(base)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::unwrap_used)]
mod tests {
    use super::*;

    const KEY: Key = [7; 32];

    fn base(freeze: bool) -> [u8; MINT_LEN] {
        let mut d = [0u8; MINT_LEN];
        d[0] = 1; // mint authority Some
        d[4..36].copy_from_slice(&[9; 32]);
        d[36..44].copy_from_slice(&1_000u64.to_le_bytes());
        d[44] = 6;
        d[45] = 1;
        if freeze {
            d[46] = 1;
            d[50..82].copy_from_slice(&[8; 32]);
        }
        d
    }

    fn extended(entries: &[(u16, u16)]) -> [u8; 512] {
        let mut d = [0u8; 512];
        d[..MINT_LEN].copy_from_slice(&base(false));
        d[ACCOUNT_TYPE_OFFSET] = ACCOUNT_TYPE_MINT;
        let mut at = TLV_OFFSET;
        for &(t, len) in entries {
            d[at..at + 2].copy_from_slice(&t.to_le_bytes());
            d[at + 2..at + 4].copy_from_slice(&len.to_le_bytes());
            at += 4 + usize::from(len);
        }
        d
    }

    #[test]
    fn reads_the_base() {
        let d = base(true);
        let b = read_base(&TOKEN_PROGRAM, &d).unwrap();
        assert_eq!(b.mint_authority, Some([9; 32]));
        assert_eq!(b.supply, 1_000);
        assert_eq!(b.decimals, 6);
        assert_eq!(b.freeze_authority, Some([8; 32]));
        assert_eq!(read_base(&[1; 32], &d), Err(ReadError::WrongOwner));
        assert_eq!(
            read_base(&TOKEN_PROGRAM, &d[..81]),
            Err(ReadError::TooShort)
        );
        let mut bad = d;
        bad[0] = 2;
        assert_eq!(read_base(&TOKEN_PROGRAM, &bad), Err(ReadError::Malformed));
        let mut uninit = d;
        uninit[45] = 0;
        assert_eq!(
            read_base(&TOKEN_PROGRAM, &uninit),
            Err(ReadError::Malformed)
        );
    }

    #[test]
    fn legacy_and_plain_mints() {
        assert!(check(&KEY, &TOKEN_PROGRAM, &base(false)).is_ok());
        assert!(check(&KEY, &TOKEN_2022_PROGRAM, &base(false)).is_ok());
        assert_eq!(
            check(&KEY, &TOKEN_PROGRAM, &base(true)),
            Err(MintReject::FreezeAuthority)
        );
        assert_eq!(
            check(&KEY, &[3; 32], &base(false)),
            Err(MintReject::UnsupportedProgram)
        );
        assert_eq!(
            check(&NATIVE_MINT, &TOKEN_PROGRAM, &base(false)),
            Err(MintReject::Native)
        );
        assert_eq!(
            check(&NATIVE_MINT_2022, &TOKEN_2022_PROGRAM, &base(false)),
            Err(MintReject::Native)
        );
        // SPL Token has no extensions: any other length is malformed.
        let d = extended(&[]);
        assert_eq!(
            check(&KEY, &TOKEN_PROGRAM, &d[..166]),
            Err(MintReject::Malformed)
        );
    }

    #[test]
    fn pump_style_mint_passes() {
        // MetadataPointer (64) then TokenMetadata (152): the 390-byte pump mint of R-MINT §3.2.
        let d = extended(&[(18, 64), (19, 152)]);
        let b = check(&KEY, &TOKEN_2022_PROGRAM, &d[..390]).unwrap();
        assert_eq!(b.decimals, 6);
    }

    #[test]
    fn every_other_type_is_rejected() {
        for t in 1u16..=64 {
            let d = extended(&[(t, 8)]);
            let got = check(&KEY, &TOKEN_2022_PROGRAM, &d[..TLV_OFFSET + 12]);
            if is_allowed_extension(t) {
                assert!(got.is_ok(), "{t}");
            } else {
                assert_eq!(got, Err(MintReject::DisallowedExtension(Some(t))), "{t}");
            }
        }
        let d = extended(&[(u16::MAX, 0)]);
        assert_eq!(
            check(&KEY, &TOKEN_2022_PROGRAM, &d[..TLV_OFFSET + 4]),
            Err(MintReject::DisallowedExtension(Some(u16::MAX)))
        );
    }

    #[test]
    fn tlv_walk_edges() {
        // The terminator, and a single trailing byte, end the walk.
        let d = extended(&[(18, 4), (0, 0)]);
        assert!(check(&KEY, &TOKEN_2022_PROGRAM, &d[..TLV_OFFSET + 16]).is_ok());
        assert!(check(&KEY, &TOKEN_2022_PROGRAM, &d[..TLV_OFFSET + 9]).is_ok());
        assert!(check(&KEY, &TOKEN_2022_PROGRAM, &d[..TLV_OFFSET]).is_ok());
        // A type with no room for its length, or a value past the end.
        assert_eq!(
            check(&KEY, &TOKEN_2022_PROGRAM, &d[..TLV_OFFSET + 3]),
            Err(MintReject::DisallowedExtension(None))
        );
        assert_eq!(
            check(&KEY, &TOKEN_2022_PROGRAM, &d[..TLV_OFFSET + 7]),
            Err(MintReject::DisallowedExtension(None))
        );
    }

    #[test]
    fn malformed_extended_mints() {
        let d = extended(&[(18, 64)]);
        for len in [83, 100, 165, MULTISIG_LEN] {
            assert_eq!(
                check(&KEY, &TOKEN_2022_PROGRAM, &d[..len]),
                Err(MintReject::Malformed),
                "{len}"
            );
        }
        let mut padding = d;
        padding[100] = 1;
        assert_eq!(
            check(&KEY, &TOKEN_2022_PROGRAM, &padding[..300]),
            Err(MintReject::Malformed)
        );
        for account_type in [0u8, 2, 3] {
            let mut t = d;
            t[ACCOUNT_TYPE_OFFSET] = account_type;
            assert_eq!(
                check(&KEY, &TOKEN_2022_PROGRAM, &t[..300]),
                Err(MintReject::Malformed)
            );
        }
        // Malformed wins over the freeze authority, which wins over the extensions (the
        // program's order).
        let mut both = extended(&[(12, 32)]);
        both[46] = 1;
        assert_eq!(
            check(&KEY, &TOKEN_2022_PROGRAM, &both[..300]),
            Err(MintReject::FreezeAuthority)
        );
        both[ACCOUNT_TYPE_OFFSET] = 2;
        assert_eq!(
            check(&KEY, &TOKEN_2022_PROGRAM, &both[..300]),
            Err(MintReject::Malformed)
        );
    }
}
