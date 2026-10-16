//! Mint fixtures for the SPEC §3 filter (ARCH §9 M1 "one filter fixture per extension";
//! R-MINT §9.5).
//!
//! - [`t22_mint`]: a Token-2022 mint carrying the requested extensions, built with the real
//!   Token-2022 v11.0.0 program (mainnet's ELF, [`crate::fork::load_mainnet_spl`]), through
//!   [`Env::send`], with the instruction builders of `spl-token-2022-interface` 3.1 (the pinned
//!   test dependency, which knows every type up to `PermissionedBurn` = 28). Companion
//!   extensions the program requires are added automatically (TokenMetadata →
//!   MetadataPointer, TokenGroup → GroupPointer, TokenGroupMember → GroupMemberPointer and a
//!   group mint, ConfidentialTransferFeeConfig → TransferFeeConfig +
//!   ConfidentialTransferMint, ConfidentialMintBurn → ConfidentialTransferMint). The
//!   token-metadata and token-group instructions are encoded by hand (their interface crates
//!   are not dependencies here).
//! - [`pump_style_mint`]: the shape of a pump.fun `create_v2` coin (R-MINT §7.2):
//!   MetadataPointer (authority `None`, pointing at the mint) + TokenMetadata (update
//!   authority `None`), mint and freeze authorities `None`.
//! - [`forge_t22_mint`] / [`set_raw`]: bytes the Token-2022 program would never write (an
//!   account extension, an unknown type, a broken TLV, a flipped padding byte), written straight
//!   into LiteSVM — for the classification of every discriminant and for the tripwire tests
//!   that simulate a Token-2022 upgrade or a bug.
//! - [`native_mint`] / [`native_mint_2022`]: the two wrapped-SOL mints.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use bytemuck::Zeroable;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_program_pack::Pack;
use solana_signer::Signer;
use spl_token_2022_interface as t22;
use t22::extension::{
    BaseStateWithExtensions, ExtensionType, StateWithExtensions, confidential_mint_burn,
    confidential_transfer, confidential_transfer_fee, default_account_state, group_member_pointer,
    group_pointer, interest_bearing_mint, metadata_pointer, pausable, permissioned_burn,
    scaled_ui_amount, transfer_fee, transfer_hook,
};
use t22::instruction::{self as t22ix, AuthorityType};
use t22::state::{AccountState, Mint};

use crate::Env;
use crate::errors::ok;
use crate::tokens::TokenProgram;

/// Token-2022's id.
pub fn t22_id() -> Pubkey {
    TokenProgram::Token2022.id()
}

/// Every discriminant `spl-token-2022-interface` 3.1 knows (0 = `Uninitialized` … 28 =
/// `PermissionedBurn`).
pub fn known_extension_types() -> Vec<ExtensionType> {
    (0u16..=u16::MAX)
        .filter_map(|raw| ExtensionType::try_from(raw).ok())
        .collect()
}

/// A mint fixture.
pub struct MintFixture {
    pub mint: Pubkey,
    /// Mint authority, and every extension authority the fixture sets.
    pub authority: Keypair,
    /// The TLV types on the account, in order, as the 3.1 parser reads them.
    pub extensions: Vec<ExtensionType>,
}

/// The TLV types of a Token-2022 mint account (3.1 parser).
pub fn extension_types_of(env: &Env, mint: &Pubkey) -> Vec<ExtensionType> {
    let acc = env
        .svm
        .get_account(mint)
        .unwrap_or_else(|| panic!("no mint {mint}"));
    StateWithExtensions::<Mint>::unpack(&acc.data)
        .and_then(|s| s.get_extension_types())
        .unwrap_or_else(|e| panic!("mint {mint} does not parse: {e:?}"))
}

// The token-metadata / token-group instruction discriminators, `sha256(hash input)[..8]` as
// the interface crates declare them (spl-token-metadata-interface 1.0.1,
// spl-token-group-interface 0.7.2). Token-2022 v11 accepting every fixture that uses them,
// and the extensions read back afterwards, is their check.

/// Discriminator of `spl_token_metadata_interface::instruction::Initialize`
/// (`sha256("spl_token_metadata_interface:initialize_account")[..8]`).
const TOKEN_METADATA_INITIALIZE: [u8; 8] = [0xd2, 0xe1, 0x1e, 0xa2, 0x58, 0xb8, 0x4d, 0x8d];
/// `…:update_the_authority`.
const TOKEN_METADATA_UPDATE_AUTHORITY: [u8; 8] = [0xd7, 0xe4, 0xa6, 0xe4, 0x54, 0x64, 0x56, 0x7b];
/// `…:updating_field`.
const TOKEN_METADATA_UPDATE_FIELD: [u8; 8] = [0xdd, 0xe9, 0x31, 0x2d, 0xb5, 0xca, 0xdc, 0xc8];
/// `spl_token_group_interface:initialize_token_group`.
const TOKEN_GROUP_INITIALIZE: [u8; 8] = [0x79, 0x71, 0x6c, 0x27, 0x36, 0x33, 0x00, 0x04];
/// `spl_token_group_interface:initialize_member`.
const TOKEN_GROUP_INITIALIZE_MEMBER: [u8; 8] = [0x98, 0x20, 0xde, 0xb0, 0xdf, 0xed, 0x74, 0x86];

fn borsh_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&u32::try_from(s.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Token-2022 `TokenMetadata` initialize (metadata = the mint itself).
pub fn token_metadata_initialize(
    mint: &Pubkey,
    update_authority: &Pubkey,
    mint_authority: &Pubkey,
    name: &str,
    symbol: &str,
    uri: &str,
) -> Instruction {
    let mut data = TOKEN_METADATA_INITIALIZE.to_vec();
    borsh_string(&mut data, name);
    borsh_string(&mut data, symbol);
    borsh_string(&mut data, uri);
    Instruction {
        program_id: t22_id(),
        accounts: vec![
            AccountMeta::new(*mint, false),
            AccountMeta::new_readonly(*update_authority, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(*mint_authority, true),
        ],
        data,
    }
}

/// Token-2022 `TokenMetadata` update-authority (`None` = revoke for good).
pub fn token_metadata_update_authority(
    mint: &Pubkey,
    current: &Pubkey,
    new_authority: Option<&Pubkey>,
) -> Instruction {
    let mut data = TOKEN_METADATA_UPDATE_AUTHORITY.to_vec();
    data.extend_from_slice(&new_authority.copied().unwrap_or_default().to_bytes());
    Instruction {
        program_id: t22_id(),
        accounts: vec![
            AccountMeta::new(*mint, false),
            AccountMeta::new_readonly(*current, true),
        ],
        data,
    }
}

/// Token-2022 `TokenMetadata` update of the `name` field.
pub fn token_metadata_update_name(mint: &Pubkey, authority: &Pubkey, name: &str) -> Instruction {
    let mut data = TOKEN_METADATA_UPDATE_FIELD.to_vec();
    data.push(0); // Field::Name
    borsh_string(&mut data, name);
    Instruction {
        program_id: t22_id(),
        accounts: vec![
            AccountMeta::new(*mint, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

/// Token-2022 `TokenGroup` initialize (group = the mint itself).
pub fn token_group_initialize(
    mint: &Pubkey,
    mint_authority: &Pubkey,
    update_authority: &Pubkey,
    max_size: u64,
) -> Instruction {
    let mut data = TOKEN_GROUP_INITIALIZE.to_vec();
    data.extend_from_slice(&update_authority.to_bytes());
    data.extend_from_slice(&max_size.to_le_bytes());
    Instruction {
        program_id: t22_id(),
        accounts: vec![
            AccountMeta::new(*mint, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(*mint_authority, true),
        ],
        data,
    }
}

/// Token-2022 `TokenGroupMember` initialize (member = the member mint itself).
pub fn token_group_initialize_member(
    member_mint: &Pubkey,
    member_mint_authority: &Pubkey,
    group: &Pubkey,
    group_update_authority: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: t22_id(),
        accounts: vec![
            AccountMeta::new(*member_mint, false),
            AccountMeta::new_readonly(*member_mint, false),
            AccountMeta::new_readonly(*member_mint_authority, true),
            AccountMeta::new(*group, false),
            AccountMeta::new_readonly(*group_update_authority, true),
        ],
        data: TOKEN_GROUP_INITIALIZE_MEMBER.to_vec(),
    }
}

fn unwrap_ix(r: Result<Instruction, anchor_lang::prelude::ProgramError>) -> Instruction {
    r.unwrap_or_else(|e| panic!("instruction builder: {e:?}"))
}

/// The pre-`InitializeMint` instruction of a fixed-size mint extension.
fn pre_init_ix(t: ExtensionType, mint: &Pubkey, auth: &Pubkey) -> Instruction {
    let id = t22_id();
    unwrap_ix(match t {
        ExtensionType::TransferFeeConfig => {
            // A zero fee: presence alone must reject (R-MINT §5).
            transfer_fee::instruction::initialize_transfer_fee_config(
                &id,
                mint,
                Some(auth),
                Some(auth),
                0,
                0,
            )
        }
        ExtensionType::MintCloseAuthority => {
            t22ix::initialize_mint_close_authority(&id, mint, Some(auth))
        }
        ExtensionType::ConfidentialTransferMint => {
            confidential_transfer::instruction::initialize_mint(&id, mint, Some(*auth), false, None)
        }
        ExtensionType::DefaultAccountState => {
            // `Initialized`: needs no freeze authority, so only the extension is on trial.
            default_account_state::instruction::initialize_default_account_state(
                &id,
                mint,
                &AccountState::Initialized,
            )
        }
        ExtensionType::NonTransferable => t22ix::initialize_non_transferable_mint(&id, mint),
        ExtensionType::InterestBearingConfig => {
            interest_bearing_mint::instruction::initialize(&id, mint, Some(*auth), 0)
        }
        ExtensionType::PermanentDelegate => t22ix::initialize_permanent_delegate(&id, mint, auth),
        ExtensionType::TransferHook => {
            // Authority set, no hook program: inactive today, switchable later (PYUSD shape).
            transfer_hook::instruction::initialize(&id, mint, Some(*auth), None)
        }
        ExtensionType::ConfidentialTransferFeeConfig => {
            confidential_transfer_fee::instruction::initialize_confidential_transfer_fee_config(
                &id,
                mint,
                Some(*auth),
                &Zeroable::zeroed(),
            )
        }
        ExtensionType::MetadataPointer => {
            metadata_pointer::instruction::initialize(&id, mint, Some(*auth), Some(*mint))
        }
        ExtensionType::GroupPointer => {
            group_pointer::instruction::initialize(&id, mint, Some(*auth), Some(*mint))
        }
        ExtensionType::GroupMemberPointer => {
            group_member_pointer::instruction::initialize(&id, mint, Some(*auth), Some(*mint))
        }
        ExtensionType::ConfidentialMintBurn => {
            confidential_mint_burn::instruction::initialize_mint(
                &id,
                mint,
                &Zeroable::zeroed(),
                &Zeroable::zeroed(),
            )
        }
        ExtensionType::ScaledUiAmount => {
            scaled_ui_amount::instruction::initialize(&id, mint, Some(*auth), 1.0)
        }
        ExtensionType::Pausable => pausable::instruction::initialize(&id, mint, auth),
        ExtensionType::PermissionedBurn => {
            permissioned_burn::instruction::initialize(&id, mint, auth)
        }
        other => panic!("{other:?} is not a fixed-size mint extension"),
    })
}

/// Whether `t` can be put on a real mint by the Token-2022 program.
pub fn is_mint_extension(t: ExtensionType) -> bool {
    t != ExtensionType::Uninitialized && t.get_account_type() == t22::extension::AccountType::Mint
}

/// The extensions `exts` need on the same mint (companions first, no duplicates).
fn with_companions(exts: &[ExtensionType]) -> Vec<ExtensionType> {
    use ExtensionType as E;
    let mut want: Vec<E> = Vec::new();
    for &t in exts {
        assert!(is_mint_extension(t), "{t:?} cannot be on a real mint");
        let companions: &[E] = match t {
            E::TokenMetadata => &[E::MetadataPointer],
            E::TokenGroup => &[E::GroupPointer],
            E::TokenGroupMember => &[E::GroupMemberPointer],
            E::ConfidentialTransferFeeConfig => {
                &[E::TransferFeeConfig, E::ConfidentialTransferMint]
            }
            E::ConfidentialMintBurn => &[E::ConfidentialTransferMint],
            _ => &[],
        };
        for &c in companions.iter().chain(core::iter::once(&t)) {
            if !want.contains(&c) {
                want.push(c);
            }
        }
    }
    want
}

/// A Token-2022 mint with (at least) `exts`, built with real instructions. Mint and extension
/// authority: a new funded keypair; freeze authority: that keypair if `freeze`; 6 decimals.
pub fn t22_mint(env: &mut Env, exts: &[ExtensionType], freeze: bool) -> MintFixture {
    let authority = Keypair::new();
    env.fund(&authority.pubkey(), 1_000_000_000);
    t22_mint_with(env, &authority, exts, freeze)
}

/// [`t22_mint`] with a given (funded) authority.
pub fn t22_mint_with(
    env: &mut Env,
    authority: &Keypair,
    exts: &[ExtensionType],
    freeze: bool,
) -> MintFixture {
    use ExtensionType as E;
    let want = with_companions(exts);
    // A member needs a group: a mint with GroupPointer + TokenGroup under the same authority.
    let group = want
        .contains(&E::TokenGroupMember)
        .then(|| t22_mint_with(env, authority, &[E::TokenGroup], false).mint);
    let post = [E::TokenMetadata, E::TokenGroup, E::TokenGroupMember];
    let pre: Vec<E> = want.iter().copied().filter(|t| !post.contains(t)).collect();
    let mint = t22_mint_inner(env, authority, &pre, &want, freeze, group);
    MintFixture {
        mint,
        authority: authority.insecure_clone(),
        extensions: extension_types_of(env, &mint),
    }
}

fn t22_mint_inner(
    env: &mut Env,
    authority: &Keypair,
    pre: &[ExtensionType],
    want: &[ExtensionType],
    freeze: bool,
    group: Option<Pubkey>,
) -> Pubkey {
    use ExtensionType as E;
    let id = t22_id();
    let mint_kp = Keypair::new();
    let mint = mint_kp.pubkey();
    let auth = authority.pubkey();
    let space = if pre.is_empty() {
        Mint::LEN
    } else {
        ExtensionType::try_calculate_account_len::<Mint>(pre)
            .unwrap_or_else(|e| panic!("account len of {pre:?}: {e:?}"))
    };
    // Headroom for the variable-length entries appended after InitializeMint (the program
    // reallocates the mint; the lamports must already be there).
    let lamports = env.svm.minimum_balance_for_rent_exemption(space + 2_048);
    let payer = env.payer.pubkey();
    let mut ixs = vec![
        anchor_lang::solana_program::system_instruction::create_account(
            &payer,
            &mint,
            lamports,
            space as u64,
            &id,
        ),
    ];
    for &t in pre {
        ixs.push(pre_init_ix(t, &mint, &auth));
    }
    ixs.push(unwrap_ix(t22ix::initialize_mint2(
        &id,
        &mint,
        &auth,
        freeze.then_some(&auth),
        6,
    )));
    if want.contains(&E::TokenMetadata) {
        ixs.push(token_metadata_initialize(
            &mint,
            &auth,
            &auth,
            "Fixture",
            "FIX",
            "https://example.invalid/fixture.json",
        ));
    }
    if want.contains(&E::TokenGroup) {
        ixs.push(token_group_initialize(&mint, &auth, &auth, 100));
    }
    if let Some(group) = group {
        ixs.push(token_group_initialize_member(&mint, &auth, &group, &auth));
    }
    // The authority signs only the metadata / group instructions.
    let authority_signs = ixs
        .iter()
        .any(|ix| ix.accounts.iter().any(|a| a.is_signer && a.pubkey == auth));
    if authority_signs {
        ok(env.send(&ixs, &[&mint_kp, authority]));
    } else {
        ok(env.send(&ixs, &[&mint_kp]));
    }
    mint
}

/// A pump.fun `create_v2`-shaped Token-2022 mint (R-MINT §7.2): MetadataPointer (authority
/// `None` → the mint) and TokenMetadata (update authority `None`); `supply` minted to a holder,
/// then mint authority `None`; no freeze authority; 6 decimals.
pub fn pump_style_mint(env: &mut Env, supply: u64) -> MintFixture {
    let id = t22_id();
    let authority = Keypair::new();
    env.fund(&authority.pubkey(), 1_000_000_000);
    let auth = authority.pubkey();
    let mint_kp = Keypair::new();
    let mint = mint_kp.pubkey();
    let space = ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::MetadataPointer])
        .unwrap_or_else(|e| panic!("{e:?}"));
    let lamports = env.svm.minimum_balance_for_rent_exemption(space + 1_024);
    let payer = env.payer.pubkey();
    let ixs = vec![
        anchor_lang::solana_program::system_instruction::create_account(
            &payer,
            &mint,
            lamports,
            space as u64,
            &id,
        ),
        unwrap_ix(metadata_pointer::instruction::initialize(
            &id,
            &mint,
            None,
            Some(mint),
        )),
        unwrap_ix(t22ix::initialize_mint2(&id, &mint, &auth, None, 6)),
        token_metadata_initialize(
            &mint,
            &auth,
            &auth,
            "Pump Fixture",
            "PFIX",
            "https://ipfs.io/ipfs/fixture",
        ),
        token_metadata_update_authority(&mint, &auth, None),
    ];
    ok(env.send(&ixs, &[&mint_kp, &authority]));
    if supply > 0 {
        let payer_kp = env.payer.insecure_clone();
        let holder = crate::tokens::create_token_account(
            &mut env.svm,
            &payer_kp,
            &mint,
            &payer,
            TokenProgram::Token2022,
        );
        crate::tokens::mint_to(
            &mut env.svm,
            &payer_kp,
            &mint,
            &holder,
            &authority,
            supply,
            TokenProgram::Token2022,
        );
    }
    let revoke = unwrap_ix(t22ix::set_authority(
        &id,
        &mint,
        None,
        AuthorityType::MintTokens,
        &auth,
        &[],
    ));
    ok(env.send(&[revoke], &[&authority]));
    MintFixture {
        mint,
        authority,
        extensions: extension_types_of(env, &mint),
    }
}

/// Removes the freeze authority of `mint` (`authority` signs).
pub fn revoke_freeze_authority(
    env: &mut Env,
    mint: &Pubkey,
    authority: &Keypair,
    program: TokenProgram,
) {
    let ix = unwrap_ix(t22ix::set_authority(
        &program.id(),
        mint,
        None,
        AuthorityType::FreezeAccount,
        &authority.pubkey(),
        &[],
    ));
    ok(env.send(&[ix], &[authority]));
}

/// The 82-byte base of an initialized mint (optional mint and freeze authorities).
pub fn base_mint_bytes(
    mint_authority: Option<&Pubkey>,
    freeze_authority: Option<&Pubkey>,
    supply: u64,
    decimals: u8,
) -> Vec<u8> {
    let state = Mint {
        mint_authority: mint_authority.copied().into(),
        supply,
        decimals,
        is_initialized: true,
        freeze_authority: freeze_authority.copied().into(),
    };
    let mut data = vec![0u8; Mint::LEN];
    state.pack_into_slice(&mut data);
    data
}

/// An extended Token-2022 mint image: base, zero padding, `AccountType::Mint`, then the given
/// TLV entries `(type, value)` verbatim.
pub fn t22_mint_bytes(freeze: bool, entries: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let auth = Pubkey::new_unique();
    let mut d = base_mint_bytes(Some(&auth), freeze.then_some(&auth), 1_000_000, 6);
    d.resize(shortwin_venue_state::mint::TLV_OFFSET, 0);
    d[shortwin_venue_state::mint::ACCOUNT_TYPE_OFFSET] =
        shortwin_venue_state::mint::ACCOUNT_TYPE_MINT;
    for (t, value) in entries {
        d.extend_from_slice(&t.to_le_bytes());
        d.extend_from_slice(&u16::try_from(value.len()).unwrap_or(u16::MAX).to_le_bytes());
        d.extend_from_slice(value);
    }
    d
}

/// The value length of a forged entry of type `raw`: the real size for a sized type the
/// 3.1 interface knows, 64 bytes otherwise (TokenMetadata, unknown types).
pub fn forged_value_len(raw: u16) -> usize {
    ExtensionType::try_from(raw)
        .ok()
        .filter(|t| *t != ExtensionType::Uninitialized)
        .and_then(|t| ExtensionType::try_calculate_account_len::<Mint>(&[t]).ok())
        .map_or(64, |len| len - shortwin_venue_state::mint::TLV_OFFSET - 4)
}

/// Writes `data` at `key`, owned by `owner`, rent-exempt.
pub fn set_raw(env: &mut Env, key: &Pubkey, owner: &Pubkey, data: Vec<u8>) {
    let lamports = env
        .svm
        .minimum_balance_for_rent_exemption(data.len())
        .max(1);
    env.svm
        .set_account(
            *key,
            Account {
                lamports,
                data,
                owner: *owner,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap_or_else(|e| panic!("set_raw: {e:?}"));
}

/// A forged Token-2022 mint with one TLV entry of type `raw` (zero value of
/// [`forged_value_len`] bytes); returns its key.
pub fn forge_t22_mint(env: &mut Env, raw: u16) -> Pubkey {
    let key = Pubkey::new_unique();
    let data = t22_mint_bytes(false, &[(raw, vec![0u8; forged_value_len(raw)])]);
    set_raw(env, &key, &t22_id(), data);
    key
}

/// Rewrites the data of an existing account in place (e.g. a mint after a simulated
/// Token-2022 upgrade).
pub fn edit_account(env: &mut Env, key: &Pubkey, f: impl FnOnce(&mut Account)) {
    let mut acc = env
        .svm
        .get_account(key)
        .unwrap_or_else(|| panic!("no account {key}"));
    f(&mut acc);
    let rent = env.svm.minimum_balance_for_rent_exemption(acc.data.len());
    acc.lamports = acc.lamports.max(rent);
    env.svm
        .set_account(*key, acc)
        .unwrap_or_else(|e| panic!("edit_account: {e:?}"));
}

/// The SPL Token native mint `So111…112` as on mainnet (82 bytes, no authorities, 9
/// decimals), written into LiteSVM.
pub fn native_mint(env: &mut Env) -> Pubkey {
    let spl_native = shortwin::constants::WSOL_MINT;
    set_raw(
        env,
        &spl_native,
        &TokenProgram::Spl.id(),
        base_mint_bytes(None, None, 0, 9),
    );
    spl_native
}

/// The Token-2022 native mint `9pan…`, created by Token-2022's own `CreateNativeMint`.
pub fn native_mint_2022(env: &mut Env) -> Pubkey {
    let payer = env.payer.insecure_clone();
    let ix = unwrap_ix(t22ix::create_native_mint(&t22_id(), &payer.pubkey()));
    ok(env.send(&[ix], &[]));
    t22::native_mint::id()
}
