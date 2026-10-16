//! The SPEC §3 mint safety filter end to end (ARCH §3.2 check 1, §9 M1 "one filter fixture per
//! extension"; R-MINT §4, §9.2, §9.5).
//!
//! Every mint goes through the deployed program (`create_market`: listed or the exact error)
//! **and** through `shortwin_venue_state::mint::check`, the byte-level copy the keeper and the
//! frontend use; the two must agree on every fixture. A mint the program refuses is then
//! installed directly (as an older filter could have listed it) and `flag_unsafe_mint` must
//! flag it; a mint it accepts must get `MintSafe`.
//!
//! - [`every_known_extension_type_is_classified`]: the CI guard of R-MINT §9.5. The exhaustive
//!   `match` in [`decision`] stops compiling when the pinned `spl-token-2022-interface` gains a
//!   variant, and every discriminant it knows must be in the reviewed range.
//! - [`one_real_mint_per_mint_extension`]: each of the 19 mint extensions put on a mint by the
//!   real Token-2022 v11.0.0 program.
//! - [`one_forged_mint_per_discriminant`]: every discriminant 0..=28 (mint and account
//!   extensions) and unknown ones, as a single TLV entry.
//! - freeze authority, native mints, legacy SPL Token, the pump.fun shape, groups and
//!   members, malformed and foreign accounts, and a PYUSD-like combination.

#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use shortwin::errors::ShortwinError;
use shortwin::instructions::create_market::VENUE_HINT_AUTO;
use shortwin::state::MARKET_FLAG_MINT_UNSAFE;
use shortwin_litesvm_tests::errors::{custom_code, ok};
use shortwin_litesvm_tests::fixtures::{self, mints};
use shortwin_litesvm_tests::tokens::{self, TokenProgram};
use shortwin_litesvm_tests::{Env, ix, pda};
use shortwin_venue_state::mint::{self as vs, MintReject};
use solana_keypair::Keypair;
use solana_program_pack::Pack;
use solana_signer::Signer;
use spl_token_2022_interface::extension::{
    BaseStateWithExtensions, ExtensionType, StateWithExtensions, metadata_pointer::MetadataPointer,
};
use spl_token_2022_interface::state::Mint;

/// The filter's decision for one extension type (R-MINT §4 "v1 decision").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    /// Ends the TLV walk; not an extension.
    Terminator,
    /// Display or collection data only.
    Allow,
    /// Can freeze, seize, block, tax or rescale balances, hide supply, or close the mint.
    RejectMint,
    /// An account extension: cannot legitimately appear in a mint.
    RejectAccountType,
    /// Unknown to the interface the program links (2.1.x): a parse error, i.e. a rejection.
    RejectUnknownToProgram,
}

/// Exhaustive on purpose: a new `ExtensionType` variant in the pinned interface fails to
/// compile here until it is classified (R-MINT §9.5).
fn decision(t: ExtensionType) -> Decision {
    use Decision::*;
    use ExtensionType as E;
    match t {
        E::Uninitialized => Terminator,
        E::MetadataPointer
        | E::TokenMetadata
        | E::GroupPointer
        | E::TokenGroup
        | E::GroupMemberPointer
        | E::TokenGroupMember => Allow,
        E::TransferFeeConfig
        | E::MintCloseAuthority
        | E::ConfidentialTransferMint
        | E::DefaultAccountState
        | E::NonTransferable
        | E::InterestBearingConfig
        | E::PermanentDelegate
        | E::TransferHook
        | E::ConfidentialTransferFeeConfig
        | E::ConfidentialMintBurn
        | E::ScaledUiAmount
        | E::Pausable => RejectMint,
        E::PermissionedBurn => RejectUnknownToProgram,
        E::TransferFeeAmount
        | E::ConfidentialTransferAccount
        | E::ImmutableOwner
        | E::MemoTransfer
        | E::CpiGuard
        | E::NonTransferableAccount
        | E::TransferHookAccount
        | E::ConfidentialTransferFeeAmount
        | E::PausableAccount => RejectAccountType,
    }
}

/// Discriminants reviewed in R-MINT §4 (token-2022 main @ 23b03f8, program v11.x).
const REVIEWED: core::ops::RangeInclusive<u16> = 0..=28;

fn reason_code(r: MintReject) -> u32 {
    u32::from(match r {
        MintReject::UnsupportedProgram => ShortwinError::MintUnsupportedProgram,
        MintReject::Native => ShortwinError::MintNative,
        MintReject::Malformed => ShortwinError::MintMalformed,
        MintReject::FreezeAuthority => ShortwinError::MintFreezeAuthority,
        MintReject::DisallowedExtension(_) => ShortwinError::MintDisallowedExtension,
    })
}

/// Lists `mint` (the token program is the mint's owner when that is a token program, SPL
/// Token otherwise) and returns `None` if it was listed, else the error code. Asserts that
/// the byte-level filter agrees, and that `flag_unsafe_mint` agrees: `MintSafe` on a listed
/// mint; on a refused one, a directly installed market gets flagged.
fn listing(env: &mut Env, mint: &Pubkey) -> Option<u32> {
    let acc = env.svm.get_account(mint).unwrap_or_default();
    let program = if acc.owner == TokenProgram::Token2022.id() {
        TokenProgram::Token2022
    } else {
        TokenProgram::Spl
    };
    let bytes = vs::check(&mint.to_bytes(), &acc.owner.to_bytes(), &acc.data)
        .err()
        .map(reason_code);

    let creator = env.funded_keypair();
    let create = ix::create_market(
        &creator.pubkey(),
        mint,
        &program.id(),
        VENUE_HINT_AUTO,
        &pda::canonical_pool(mint),
    );
    let res = env.send(&[create], &[&creator]);
    let market = pda::market(mint).0;
    let on_chain = match &res {
        Ok(_) => None,
        Err(f) => Some(custom_code(&res).unwrap_or_else(|| {
            panic!(
                "create_market failed without a custom error: {:?}\n{}",
                f.err,
                f.meta.pretty_logs()
            )
        })),
    };
    assert_eq!(
        on_chain,
        bytes,
        "program and byte-level filter disagree on {mint} (owner {}, {} bytes)",
        acc.owner,
        acc.data.len()
    );

    if on_chain.is_none() {
        env.track_market(market);
        let res = env.send(&[ix::flag_unsafe_mint(mint)], &[]);
        assert_eq!(custom_code(&res), Some(u32::from(ShortwinError::MintSafe)));
    } else {
        assert!(
            env.svm.get_account(&market).is_none(),
            "a refused listing left a market"
        );
        let config = env.config();
        let now = env.now();
        let payer = env.payer.pubkey();
        let keys = fixtures::install_market(&mut env.svm, mint, 6, program, &payer, now, &config);
        env.track_market(keys.market);
        ok(env.send(&[ix::flag_unsafe_mint(mint)], &[]));
        assert!(env.market(&keys.market).has_flag(MARKET_FLAG_MINT_UNSAFE));
    }
    on_chain
}

fn code(e: ShortwinError) -> Option<u32> {
    Some(u32::from(e))
}

#[test]
fn every_known_extension_type_is_classified() {
    let known = mints::known_extension_types();
    for &t in &known {
        let raw = u16::from(t);
        assert!(
            REVIEWED.contains(&raw),
            "new Token-2022 extension type {raw} ({t:?}): classify it in \
             docs/research/mint-safety-filter.md §4 and in `decision`"
        );
        // The allowlist is the same everywhere.
        let allowed = decision(t) == Decision::Allow;
        assert_eq!(vs::is_allowed_extension(raw), allowed, "{t:?}");
        assert_eq!(
            shortwin::mint_filter::ALLOWED_EXTENSIONS
                .iter()
                .any(|&a| u16::from(a) == raw),
            allowed,
            "{t:?}"
        );
        // Mint extensions and account extensions as the interface sees them.
        let is_account =
            t.get_account_type() == spl_token_2022_interface::extension::AccountType::Account;
        assert_eq!(
            is_account,
            decision(t) == Decision::RejectAccountType,
            "{t:?}"
        );
    }
    assert_eq!(
        known.len(),
        29,
        "every discriminant 0..=28 and nothing else"
    );
}

#[test]
fn one_real_mint_per_mint_extension() {
    let mut env = Env::initialized();
    let mut covered = Vec::new();
    for t in mints::known_extension_types() {
        if !mints::is_mint_extension(t) {
            continue;
        }
        let fx = mints::t22_mint(&mut env, &[t], false);
        assert!(
            fx.extensions.contains(&t),
            "{t:?} not on {:?}",
            fx.extensions
        );
        let expect_listed = fx
            .extensions
            .iter()
            .all(|&e| decision(e) == Decision::Allow);
        let got = listing(&mut env, &fx.mint);
        if expect_listed {
            assert_eq!(got, None, "{t:?} ({:?}) should list", fx.extensions);
        } else {
            assert_eq!(
                got,
                code(ShortwinError::MintDisallowedExtension),
                "{t:?} ({:?}) should be refused",
                fx.extensions
            );
        }
        covered.push(t);
    }
    assert_eq!(covered.len(), 19, "every mint extension: {covered:?}");
}

#[test]
fn one_forged_mint_per_discriminant() {
    let mut env = Env::initialized();
    let unknown = [29u16, 30, 100, 0x7fff, 0xfffd, 0xfffe, u16::MAX];
    for raw in REVIEWED.chain(unknown) {
        let mint = mints::forge_t22_mint(&mut env, raw);
        let expect = match ExtensionType::try_from(raw).map(decision) {
            Ok(Decision::Terminator | Decision::Allow) => None,
            _ => code(ShortwinError::MintDisallowedExtension),
        };
        assert_eq!(listing(&mut env, &mint), expect, "type {raw}");
    }
}

#[test]
fn freeze_authority_is_refused_until_revoked() {
    let mut env = Env::initialized();
    for program in TokenProgram::BOTH {
        let authority = Keypair::new();
        env.fund(&authority.pubkey(), 1_000_000_000);
        let payer = env.payer.insecure_clone();
        let mint =
            tokens::create_mint_with_freeze(&mut env.svm, &payer, &authority.pubkey(), 6, program);
        let creator = env.funded_keypair();
        let res = env.send(
            &[ix::create_market(
                &creator.pubkey(),
                &mint,
                &program.id(),
                VENUE_HINT_AUTO,
                &pda::canonical_pool(&mint),
            )],
            &[&creator],
        );
        assert_eq!(custom_code(&res), code(ShortwinError::MintFreezeAuthority));
        mints::revoke_freeze_authority(&mut env, &mint, &authority, program);
        assert_eq!(listing(&mut env, &mint), None, "{program:?}");
    }
    // The freeze check comes before the extension check.
    let fx = mints::t22_mint(&mut env, &[ExtensionType::TransferHook], true);
    assert_eq!(
        listing(&mut env, &fx.mint),
        code(ShortwinError::MintFreezeAuthority)
    );
    // DefaultAccountState stays refused once the freeze authority that could flip it to
    // `Frozen` is revoked: presence, not configuration (R-MINT §5).
    let fx = mints::t22_mint(&mut env, &[ExtensionType::DefaultAccountState], true);
    mints::revoke_freeze_authority(&mut env, &fx.mint, &fx.authority, TokenProgram::Token2022);
    assert_eq!(
        listing(&mut env, &fx.mint),
        code(ShortwinError::MintDisallowedExtension)
    );
}

#[test]
fn native_mints_are_refused() {
    let mut env = Env::initialized();
    let spl = mints::native_mint(&mut env);
    let t22 = mints::native_mint_2022(&mut env);
    assert_eq!(t22, shortwin::constants::WSOL_2022_MINT);
    for mint in [spl, t22] {
        assert_eq!(listing(&mut env, &mint), code(ShortwinError::MintNative));
    }
}

#[test]
fn legacy_spl_and_plain_token_2022_mints_are_listed() {
    let mut env = Env::initialized();
    for program in TokenProgram::BOTH {
        let payer = env.payer.insecure_clone();
        let mint = tokens::create_mint(&mut env.svm, &payer, &Pubkey::new_unique(), 6, program);
        assert_eq!(env.svm.get_account(&mint).unwrap().data.len(), Mint::LEN);
        assert_eq!(listing(&mut env, &mint), None, "{program:?}");
    }
    // A legacy pump coin: SPL Token, both authorities None (R-MINT §7.2 `9BB6NF…`).
    let mint = Pubkey::new_unique();
    mints::set_raw(
        &mut env,
        &mint,
        &TokenProgram::Spl.id(),
        mints::base_mint_bytes(None, None, 999_999_999_000_000, 6),
    );
    assert_eq!(listing(&mut env, &mint), None);
}

/// The pump.fun `create_v2` shape observed on 123 of 123 mainnet coins (R-MINT §7.2).
#[test]
fn pump_style_token_2022_mint_is_listed() {
    let mut env = Env::initialized();
    let fx = mints::pump_style_mint(&mut env, 1_000_000_000_000_000);
    assert_eq!(
        fx.extensions,
        [ExtensionType::MetadataPointer, ExtensionType::TokenMetadata]
    );
    let acc = env.svm.get_account(&fx.mint).unwrap();
    assert_eq!(acc.owner, mints::t22_id());
    let state = StateWithExtensions::<Mint>::unpack(&acc.data).unwrap();
    assert!(state.base.mint_authority.is_none());
    assert!(state.base.freeze_authority.is_none());
    assert_eq!(state.base.supply, 1_000_000_000_000_000);
    let pointer = state.get_extension::<MetadataPointer>().unwrap();
    assert_eq!(Option::<Pubkey>::from(pointer.authority), None);
    assert_eq!(
        Option::<Pubkey>::from(pointer.metadata_address),
        Some(fx.mint)
    );
    // TokenMetadata value: update_authority (32, zero = None) then the mint.
    let tlv = &acc.data[vs::TLV_OFFSET..];
    let second = 4 + usize::from(u16::from_le_bytes([tlv[2], tlv[3]]));
    assert_eq!(u16::from_le_bytes([tlv[second], tlv[second + 1]]), 19);
    let value = &tlv[second + 4..];
    assert_eq!(
        &value[..32],
        &[0u8; 32],
        "metadata update authority is None"
    );
    assert_eq!(&value[32..64], fx.mint.as_ref());
    assert_eq!(listing(&mut env, &fx.mint), None);
}

#[test]
fn metadata_groups_and_members_are_listed() {
    let mut env = Env::initialized();
    // All six allowlisted extensions on one mint (a group that is also a member of another).
    let fx = mints::t22_mint(
        &mut env,
        &[
            ExtensionType::TokenMetadata,
            ExtensionType::TokenGroup,
            ExtensionType::TokenGroupMember,
        ],
        false,
    );
    let mut got = fx.extensions.clone();
    got.sort_by_key(|&t| u16::from(t));
    assert_eq!(
        got,
        [
            ExtensionType::MetadataPointer,
            ExtensionType::TokenMetadata,
            ExtensionType::GroupPointer,
            ExtensionType::TokenGroup,
            ExtensionType::GroupMemberPointer,
            ExtensionType::TokenGroupMember
        ]
    );
    assert_eq!(listing(&mut env, &fx.mint), None);
    // A metadata pointer alone, with an authority that can re-point it.
    let fx = mints::t22_mint(&mut env, &[ExtensionType::MetadataPointer], false);
    assert_eq!(listing(&mut env, &fx.mint), None);
}

#[test]
fn malformed_and_foreign_accounts_are_refused() {
    let mut env = Env::initialized();
    let t22 = mints::t22_id();
    let spl = TokenProgram::Spl.id();
    let good = mints::base_mint_bytes(Some(&Pubkey::new_unique()), None, 1, 6);
    let cases: Vec<(&str, Pubkey, Vec<u8>, ShortwinError)> = vec![
        (
            "owned by another program",
            Pubkey::new_unique(),
            good.clone(),
            ShortwinError::MintUnsupportedProgram,
        ),
        (
            "a system account",
            anchor_lang::system_program::ID,
            good.clone(),
            ShortwinError::MintUnsupportedProgram,
        ),
        (
            "SPL mint of 83 bytes",
            spl,
            [good.clone(), vec![0]].concat(),
            ShortwinError::MintMalformed,
        ),
        (
            "SPL mint of 81 bytes",
            spl,
            good[..81].to_vec(),
            ShortwinError::MintMalformed,
        ),
        (
            "T22 mint of 100 bytes",
            t22,
            [good.clone(), vec![0; 18]].concat(),
            ShortwinError::MintMalformed,
        ),
        (
            "T22 mint of 165 bytes",
            t22,
            [good.clone(), vec![0; 83]].concat(),
            ShortwinError::MintMalformed,
        ),
        (
            "T22 multisig length",
            t22,
            {
                let mut d = mints::t22_mint_bytes(false, &[(18, vec![0; 64])]);
                d.resize(355, 0);
                d
            },
            ShortwinError::MintMalformed,
        ),
        (
            "T22 account type = Account",
            t22,
            {
                let mut d = mints::t22_mint_bytes(false, &[(18, vec![0; 64])]);
                d[vs::ACCOUNT_TYPE_OFFSET] = 2;
                d
            },
            ShortwinError::MintMalformed,
        ),
        (
            "T22 non-zero padding",
            t22,
            {
                let mut d = mints::t22_mint_bytes(false, &[(18, vec![0; 64])]);
                d[120] = 1;
                d
            },
            ShortwinError::MintMalformed,
        ),
        (
            "uninitialized mint",
            t22,
            {
                let mut d = good.clone();
                d[45] = 0;
                d
            },
            ShortwinError::MintMalformed,
        ),
        (
            "bad COption tag",
            spl,
            {
                let mut d = good.clone();
                d[0] = 2;
                d
            },
            ShortwinError::MintMalformed,
        ),
        (
            "TLV entry cut short",
            t22,
            {
                let mut d = mints::t22_mint_bytes(false, &[(18, vec![0; 64])]);
                d.truncate(vs::TLV_OFFSET + 40);
                d
            },
            ShortwinError::MintDisallowedExtension,
        ),
        (
            "TLV length past the end",
            t22,
            {
                let mut d = mints::t22_mint_bytes(false, &[(18, vec![0; 64])]);
                d[vs::TLV_OFFSET + 2..vs::TLV_OFFSET + 4].copy_from_slice(&1_000u16.to_le_bytes());
                d
            },
            ShortwinError::MintDisallowedExtension,
        ),
        (
            "TLV type without a length",
            t22,
            {
                let mut d = mints::t22_mint_bytes(false, &[(18, vec![0; 64])]);
                d.extend_from_slice(&[19, 0, 5]);
                d
            },
            ShortwinError::MintDisallowedExtension,
        ),
    ];
    for (what, owner, data, want) in cases {
        let mint = Pubkey::new_unique();
        mints::set_raw(&mut env, &mint, &owner, data);
        assert_eq!(listing(&mut env, &mint), code(want), "{what}");
    }
    // A token account passed as a mint, for both programs.
    for program in TokenProgram::BOTH {
        let payer = env.payer.insecure_clone();
        let mint = tokens::create_mint(&mut env.svm, &payer, &Pubkey::new_unique(), 6, program);
        let account =
            tokens::create_token_account(&mut env.svm, &payer, &mint, &payer.pubkey(), program);
        assert_eq!(
            listing(&mut env, &account),
            code(ShortwinError::MintMalformed),
            "{program:?}"
        );
    }
    // A trailing byte after the last entry is realloc slack, not an error (same as the parser).
    let mint = Pubkey::new_unique();
    let mut d = mints::t22_mint_bytes(false, &[(18, vec![0; 64])]);
    d.push(0xff);
    mints::set_raw(&mut env, &mint, &t22, d);
    assert_eq!(listing(&mut env, &mint), None);
}

/// A PYUSD-like mint (R-MINT §4: freeze authority, MintCloseAuthority, PermanentDelegate,
/// TransferFeeConfig, ConfidentialTransferMint, ConfidentialTransferFeeConfig, TransferHook,
/// MetadataPointer, TokenMetadata), with and without its freeze authority.
#[test]
fn pyusd_like_mint_is_refused() {
    let mut env = Env::initialized();
    let exts = [
        ExtensionType::MintCloseAuthority,
        ExtensionType::PermanentDelegate,
        ExtensionType::ConfidentialTransferFeeConfig,
        ExtensionType::TransferHook,
        ExtensionType::TokenMetadata,
    ];
    let fx = mints::t22_mint(&mut env, &exts, true);
    assert_eq!(fx.extensions.len(), 8, "{:?}", fx.extensions);
    assert_eq!(
        listing(&mut env, &fx.mint),
        code(ShortwinError::MintFreezeAuthority)
    );
    let fx = mints::t22_mint(&mut env, &exts, false);
    assert_eq!(
        listing(&mut env, &fx.mint),
        code(ShortwinError::MintDisallowedExtension)
    );
}
