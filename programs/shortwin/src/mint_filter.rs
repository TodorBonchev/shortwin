//! Mint safety filter (SPEC §3; ARCH §3.2 check 1, A3, ADOPTED Q7; R-MINT §9.2).
//!
//! An **allowlist**: a mint is listable only if
//!
//! 1. it is owned by SPL Token (`Tokenkeg…`) or Token-2022 (`Tokenz…`) — else
//!    `MintUnsupportedProgram`;
//! 2. it is not a native mint (`WSOL_MINT`, `WSOL_2022_MINT`) — else `MintNative`;
//! 3. an SPL Token mint is exactly 82 bytes, and the base mint unpacks through
//!    `anchor_spl::token_2022::spl_token_2022` (= `spl-token-2022-interface` 2.1.x, the
//!    version anchor-spl 1.2.0 links): valid `COption` tags, initialized, not multisig-sized,
//!    zero padding and `AccountType::Mint` when extended — else `MintMalformed`;
//! 4. `freeze_authority == None` — else `MintFreezeAuthority`;
//! 5. every TLV extension ∈ {MetadataPointer, TokenMetadata, GroupPointer, TokenGroup,
//!    GroupMemberPointer, TokenGroupMember}, and the TLV walk parses — else
//!    `MintDisallowedExtension`.
//!
//! **Fail closed.** `get_extension_types` errors on any type the linked interface does not
//! know (2.1.x ends at `PausableAccount` = 27, so `PermissionedBurn` = 28, live on mainnet
//! since Token-2022 v11.0.0, is "unknown") and on a truncated or overlong entry; every such
//! error is a rejection, never "no extensions" (R-MINT §3.3). Presence is enough: a zero
//! transfer fee or a hook with no program is still rejected (R-MINT §5).
//!
//! **Where it runs** (ARCH §3.0 step 3, R-MINT §6): `create_market` (hard failure, before any
//! token CPI touches the mint), then through `engine::guard::mint_guard` on every instruction
//! that moves the market's tokens — entry paths (`deposit`, `open_short`) require it, exit
//! paths emit `MintSafetyTripped` and continue — and `flag_unsafe_mint`, which persists a
//! failure as `MARKET_FLAG_MINT_UNSAFE`. Under current Token-2022 rules an accepted mint
//! cannot start failing (the freeze authority can only be removed, dangerous extensions can
//! only be added before `InitializeMint`, only the allowlisted metadata and group entries can
//! be appended), so the re-check is a tripwire for program upgrades and our own bugs.
//!
//! `shortwin_venue_state::mint::check` is the same decision on raw bytes for the keeper and
//! the frontend; `tests::differential_against_the_byte_level_filter` holds the two together.

use anchor_lang::prelude::*;
use anchor_spl::token_2022::spl_token_2022 as t22;
use t22::extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions};
use t22::state::Mint;

use crate::constants::{TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_2022_MINT, WSOL_MINT};
use crate::errors::ShortwinError;

/// Length of an SPL Token mint (and of a Token-2022 mint without extensions).
pub const MINT_LEN: usize = shortwin_venue_state::mint::MINT_LEN;

/// The v1 allowlist (R-MINT §4 "ALLOW" rows): display and collection data only. They cannot
/// freeze, seize, block, tax or rescale balances, and they require no extension on the
/// market's pool vault.
pub const ALLOWED_EXTENSIONS: [ExtensionType; 6] = [
    ExtensionType::MetadataPointer,
    ExtensionType::TokenMetadata,
    ExtensionType::GroupPointer,
    ExtensionType::TokenGroup,
    ExtensionType::GroupMemberPointer,
    ExtensionType::TokenGroupMember,
];

/// `true` if the extension type is on the allowlist.
pub fn is_allowlisted(t: ExtensionType) -> bool {
    ALLOWED_EXTENSIONS.contains(&t)
}

/// `Ok` if `mint` passes the allowlist filter, else the reason (see the module docs). A pure
/// read: no account is written, nothing is cached.
pub fn check_mint_safety(mint: &AccountInfo) -> Result<()> {
    let owner = *mint.owner;
    let legacy = owner == TOKEN_PROGRAM_ID;
    require!(
        legacy || owner == TOKEN_2022_PROGRAM_ID,
        ShortwinError::MintUnsupportedProgram
    );
    require!(
        *mint.key != WSOL_MINT && *mint.key != WSOL_2022_MINT,
        ShortwinError::MintNative
    );
    let data = mint
        .try_borrow_data()
        .map_err(|_| error!(ShortwinError::MintMalformed))?;
    // SPL Token mints are exactly 82 bytes and carry no TLV.
    require!(
        !legacy || data.len() == MINT_LEN,
        ShortwinError::MintMalformed
    );
    // Rejects lengths < 82 and == 355, bad `COption` tags, uninitialized mints, non-zero
    // padding and a wrong account type.
    let state = StateWithExtensions::<Mint>::unpack(&data)
        .map_err(|_| error!(ShortwinError::MintMalformed))?;
    require!(
        state.base.freeze_authority.is_none(),
        ShortwinError::MintFreezeAuthority
    );
    // Errs on an unknown type or a malformed entry: fail closed.
    let types = state
        .get_extension_types()
        .map_err(|_| error!(ShortwinError::MintDisallowedExtension))?;
    require!(
        types.into_iter().all(is_allowlisted),
        ShortwinError::MintDisallowedExtension
    );
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    clippy::panic
)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::program_pack::Pack;
    use shortwin_venue_state::mint::{self as vs, MintReject};

    fn code_of(res: Result<()>) -> Option<u32> {
        match res {
            Ok(()) => None,
            Err(anchor_lang::error::Error::AnchorError(a)) => Some(a.error_code_number),
            Err(e) => panic!("not a short.win error: {e:?}"),
        }
    }

    fn run(key: &Pubkey, owner: &Pubkey, data: &[u8]) -> Option<u32> {
        let mut lamports = 1u64;
        let mut buf = data.to_vec();
        let info = AccountInfo::new(key, false, false, &mut lamports, &mut buf, owner, false);
        code_of(check_mint_safety(&info))
    }

    fn reason_code(r: MintReject) -> u32 {
        u32::from(match r {
            MintReject::UnsupportedProgram => ShortwinError::MintUnsupportedProgram,
            MintReject::Native => ShortwinError::MintNative,
            MintReject::Malformed => ShortwinError::MintMalformed,
            MintReject::FreezeAuthority => ShortwinError::MintFreezeAuthority,
            MintReject::DisallowedExtension(_) => ShortwinError::MintDisallowedExtension,
        })
    }

    #[test]
    fn constants_agree_with_the_interface() {
        assert_eq!(MINT_LEN, <Mint as Pack>::LEN);
        assert_eq!(TOKEN_2022_PROGRAM_ID, t22::ID);
        assert_eq!(TOKEN_PROGRAM_ID, anchor_spl::token::spl_token::ID);
        assert_eq!(WSOL_2022_MINT, t22::native_mint::ID);
        assert_eq!(WSOL_MINT, anchor_spl::token::spl_token::native_mint::ID);
        assert_eq!(vs::TOKEN_PROGRAM, TOKEN_PROGRAM_ID.to_bytes());
        assert_eq!(vs::TOKEN_2022_PROGRAM, TOKEN_2022_PROGRAM_ID.to_bytes());
        assert_eq!(vs::NATIVE_MINT, WSOL_MINT.to_bytes());
        assert_eq!(vs::NATIVE_MINT_2022, WSOL_2022_MINT.to_bytes());
        let ours: Vec<u16> = ALLOWED_EXTENSIONS.iter().map(|&t| u16::from(t)).collect();
        assert_eq!(ours, vs::ALLOWED_EXTENSIONS);
        assert_eq!(ours, [18, 19, 20, 21, 22, 23]);
    }

    /// The linked interface knows types 0..=27 only: `PermissionedBurn` (28, Token-2022
    /// v11.0.0) and anything newer parse as errors, i.e. rejections. If anchor-spl moves to a
    /// newer interface this fails, and the new types must be classified (R-MINT §4, §9.5).
    #[test]
    fn linked_interface_fails_closed_after_type_27() {
        for raw in 0u16..=27 {
            assert!(ExtensionType::try_from(raw).is_ok(), "{raw}");
        }
        for raw in [28u16, 29, 100, u16::MAX] {
            assert!(ExtensionType::try_from(raw).is_err(), "{raw}");
        }
    }

    fn base(mint_auth: bool, freeze: bool) -> Vec<u8> {
        let mut d = vec![0u8; MINT_LEN];
        if mint_auth {
            d[0] = 1;
            d[4..36].copy_from_slice(&[9; 32]);
        }
        d[36..44].copy_from_slice(&1_000_000u64.to_le_bytes());
        d[44] = 6;
        d[45] = 1;
        if freeze {
            d[46] = 1;
            d[50..82].copy_from_slice(&[8; 32]);
        }
        d
    }

    fn extended(freeze: bool, entries: &[(u16, u16)]) -> Vec<u8> {
        let mut d = base(true, freeze);
        d.resize(vs::TLV_OFFSET, 0);
        d[vs::ACCOUNT_TYPE_OFFSET] = vs::ACCOUNT_TYPE_MINT;
        for &(t, len) in entries {
            d.extend_from_slice(&t.to_le_bytes());
            d.extend_from_slice(&len.to_le_bytes());
            d.resize(d.len() + usize::from(len), 0xab);
        }
        d
    }

    #[test]
    fn decisions() {
        let key = Pubkey::new_unique();
        let t22 = TOKEN_2022_PROGRAM_ID;
        let spl = TOKEN_PROGRAM_ID;
        let e = |x: ShortwinError| Some(u32::from(x));

        assert_eq!(run(&key, &spl, &base(true, false)), None);
        assert_eq!(run(&key, &t22, &base(false, false)), None);
        assert_eq!(
            run(&key, &spl, &base(true, true)),
            e(ShortwinError::MintFreezeAuthority)
        );
        assert_eq!(
            run(&key, &Pubkey::new_unique(), &base(true, false)),
            e(ShortwinError::MintUnsupportedProgram)
        );
        assert_eq!(
            run(&WSOL_MINT, &spl, &base(false, false)),
            e(ShortwinError::MintNative)
        );
        assert_eq!(
            run(&WSOL_2022_MINT, &t22, &base(false, false)),
            e(ShortwinError::MintNative)
        );
        // pump.fun `create_v2` shape: MetadataPointer then TokenMetadata (R-MINT §7).
        assert_eq!(
            run(&key, &t22, &extended(false, &[(18, 64), (19, 152)])),
            None
        );
        // All six allowlisted types together.
        let all: Vec<(u16, u16)> = vs::ALLOWED_EXTENSIONS.iter().map(|&t| (t, 16)).collect();
        assert_eq!(run(&key, &t22, &extended(false, &all)), None);
        // Each spec rejection by presence (R-MINT §5), and PermissionedBurn (unknown to 2.1).
        for t in [
            1u16,
            3,
            4,
            6,
            9,
            10,
            12,
            14,
            16,
            24,
            25,
            26,
            28,
            29,
            u16::MAX,
        ] {
            assert_eq!(
                run(&key, &t22, &extended(false, &[(18, 64), (t, 8)])),
                e(ShortwinError::MintDisallowedExtension),
                "{t}"
            );
        }
        // Account extensions cannot appear in a mint.
        for t in [2u16, 5, 7, 8, 11, 13, 15, 17, 27] {
            assert_eq!(
                run(&key, &t22, &extended(false, &[(t, 8)])),
                e(ShortwinError::MintDisallowedExtension),
                "{t}"
            );
        }
        // A token account passed as a mint.
        let mut account = vec![0u8; 165];
        account[108] = 1;
        assert_eq!(run(&key, &spl, &account), e(ShortwinError::MintMalformed));
        account.resize(170, 0);
        account[165] = 2;
        assert_eq!(run(&key, &t22, &account), e(ShortwinError::MintMalformed));
        // Multisig length, uninitialized, truncated.
        let mut ms = extended(false, &[(18, 64)]);
        ms.resize(355, 0);
        assert_eq!(run(&key, &t22, &ms), e(ShortwinError::MintMalformed));
        let mut uninit = base(true, false);
        uninit[45] = 0;
        assert_eq!(run(&key, &spl, &uninit), e(ShortwinError::MintMalformed));
        assert_eq!(run(&key, &spl, &[]), e(ShortwinError::MintMalformed));
        // A malformed TLV walk is an extension rejection, as in the byte-level filter.
        let mut cut = extended(false, &[(18, 64)]);
        cut.truncate(vs::TLV_OFFSET + 20);
        assert_eq!(
            run(&key, &t22, &cut),
            e(ShortwinError::MintDisallowedExtension)
        );
    }

    /// Minimal xorshift64* generator (no dev-dependency).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn chance(&mut self, one_in: u64) -> bool {
            self.below(one_in) == 0
        }
    }

    fn random_mint(rng: &mut Rng) -> Vec<u8> {
        let mut d = base(rng.chance(2), rng.chance(6));
        // Base-field corruption: COption tags and the initialized byte.
        if rng.chance(12) {
            let at = [0usize, 46][rng.below(2) as usize];
            d[at + rng.below(4) as usize] = rng.below(3) as u8;
        }
        if rng.chance(12) {
            d[45] = rng.below(3) as u8;
        }
        if rng.chance(4) {
            return d;
        }
        d.resize(vs::TLV_OFFSET, 0);
        d[vs::ACCOUNT_TYPE_OFFSET] = if rng.chance(10) {
            rng.below(4) as u8
        } else {
            vs::ACCOUNT_TYPE_MINT
        };
        if rng.chance(15) {
            let at = MINT_LEN + rng.below(83) as usize;
            d[at] = 1;
        }
        for _ in 0..rng.below(5) {
            let t: u16 = match rng.below(6) {
                0..=2 => vs::ALLOWED_EXTENSIONS[rng.below(6) as usize],
                3 => 0,
                4 => rng.below(40) as u16,
                _ => rng.next() as u16,
            };
            let len: u16 = if rng.chance(3) {
                rng.below(300) as u16
            } else {
                rng.below(80) as u16
            };
            d.extend_from_slice(&t.to_le_bytes());
            d.extend_from_slice(&len.to_le_bytes());
            let fill = rng.next() as u8;
            d.resize(d.len() + usize::from(len), fill);
        }
        match rng.below(8) {
            0 => {
                let cut = rng.below(d.len() as u64 + 1) as usize;
                d.truncate(cut);
            }
            1 => d.resize(d.len() + rng.below(4) as usize, 0),
            2 => d.resize(355, 0),
            _ => {}
        }
        d
    }

    /// The program's filter and `shortwin_venue_state::mint::check` give the same verdict and
    /// reason on the fixtures above and on 20,000 random and mutated mints.
    #[test]
    fn differential_against_the_byte_level_filter() {
        let owners = [
            TOKEN_PROGRAM_ID,
            TOKEN_2022_PROGRAM_ID,
            TOKEN_2022_PROGRAM_ID,
            TOKEN_2022_PROGRAM_ID,
            Pubkey::new_unique(),
        ];
        let mut rng = Rng(0x5eed_5eed_1234_5678);
        let (mut pass, mut reject) = (0u32, 0u32);
        for i in 0..20_000u32 {
            let owner = owners[rng.below(owners.len() as u64) as usize];
            let key = match rng.below(50) {
                0 => WSOL_MINT,
                1 => WSOL_2022_MINT,
                _ => Pubkey::new_from_array(rng.next().to_le_bytes().repeat(4).try_into().unwrap()),
            };
            let data = random_mint(&mut rng);
            let program = run(&key, &owner, &data);
            let bytes = vs::check(&key.to_bytes(), &owner.to_bytes(), &data)
                .err()
                .map(reason_code);
            assert_eq!(program, bytes, "case {i}: owner {owner} data {data:?}");
            if program.is_none() {
                pass += 1;
            } else {
                reject += 1;
            }
        }
        // The generator reaches both outcomes often.
        assert!(
            pass > 1_000 && reject > 1_000,
            "pass {pass} reject {reject}"
        );
    }
}
