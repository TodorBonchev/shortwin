//! SPL Token and Token-2022 mints and accounts (litesvm-token 0.16 with the `token-2022`
//! feature, whose builders speak both programs through `spl-token-2022-interface`).
//!
//! SPL Token is the p-token LiteSVM 0.16 bundles under the mainnet feature set (R-TOOL §2);
//! Token-2022 is the ELF mainnet runs, v11.0.0 from its dumped ProgramData
//! ([`crate::fork::load_mainnet_spl`], M10; LiteSVM's bundled v11.0.0 build with
//! `SHORTWIN_BUNDLED_SPL=1`). Mints with Token-2022 extensions (the filter's one
//! fixture per extension) are built by the market track with `spl-token-2022-interface`
//! instructions and [`crate::Env::send`].
//!
//! Every helper here sends through [`send_direct`], which expires the blockhash afterwards, so
//! the same call may be repeated (two equal `mint_to`s are byte-identical; FUZZ-M7-24H-2).

use anchor_lang::prelude::Pubkey;
use litesvm::LiteSVM;
use litesvm_token::{CreateAccount, CreateAssociatedTokenAccount, CreateMint, MintTo};
use shortwin::constants::{TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::env::send_direct;

/// Which token program a mint lives under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenProgram {
    /// SPL Token (`Tokenkeg…`).
    Spl,
    /// Token-2022 (`Tokenz…`).
    Token2022,
}

impl TokenProgram {
    /// The program id.
    pub fn id(self) -> Pubkey {
        match self {
            Self::Spl => TOKEN_PROGRAM_ID,
            Self::Token2022 => TOKEN_2022_PROGRAM_ID,
        }
    }

    /// Both programs, for tests that must hold on each.
    pub const BOTH: [Self; 2] = [Self::Spl, Self::Token2022];
}

/// Creates a mint with `authority` as mint authority and no freeze authority (a mint that
/// passes the SPEC §3 filter). Returns the mint key.
pub fn create_mint(
    svm: &mut LiteSVM,
    payer: &Keypair,
    authority: &Pubkey,
    decimals: u8,
    program: TokenProgram,
) -> Pubkey {
    let id = program.id();
    send_direct(svm, |svm| {
        CreateMint::new(svm, payer)
            .authority(authority)
            .decimals(decimals)
            .token_program_id(&id)
            .send()
    })
    .unwrap_or_else(|e| panic!("create_mint: {:?}", e.err))
}

/// Creates a mint **with** a freeze authority (fails the filter).
pub fn create_mint_with_freeze(
    svm: &mut LiteSVM,
    payer: &Keypair,
    authority: &Pubkey,
    decimals: u8,
    program: TokenProgram,
) -> Pubkey {
    let id = program.id();
    send_direct(svm, |svm| {
        CreateMint::new(svm, payer)
            .authority(authority)
            .freeze_authority(authority)
            .decimals(decimals)
            .token_program_id(&id)
            .send()
    })
    .unwrap_or_else(|e| panic!("create_mint_with_freeze: {:?}", e.err))
}

/// Creates a (non-ATA) token account of `mint` owned by `owner`.
pub fn create_token_account(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    owner: &Pubkey,
    program: TokenProgram,
) -> Pubkey {
    let id = program.id();
    send_direct(svm, |svm| {
        CreateAccount::new(svm, payer, mint)
            .owner(owner)
            .token_program_id(&id)
            .send()
    })
    .unwrap_or_else(|e| panic!("create_token_account: {:?}", e.err))
}

/// Creates `ATA(owner, mint, program)` (anyone can: the ATA program checks only the address).
/// Returns its key.
pub fn create_ata(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Pubkey,
    mint: &Pubkey,
    program: TokenProgram,
) -> Pubkey {
    let id = program.id();
    send_direct(svm, |svm| {
        CreateAssociatedTokenAccount::new(svm, payer, mint)
            .owner(owner)
            .token_program_id(&id)
            .send()
    })
    .unwrap_or_else(|e| panic!("create_ata: {:?}", e.err))
}

/// Mints `amount` raw units of `mint` to `destination` (`authority` signs).
pub fn mint_to(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    destination: &Pubkey,
    authority: &Keypair,
    amount: u64,
    program: TokenProgram,
) {
    let id = program.id();
    send_direct(svm, |svm| {
        MintTo::new(svm, payer, mint, destination, amount)
            .owner(authority)
            .token_program_id(&id)
            .send()
    })
    .unwrap_or_else(|e| panic!("mint_to: {:?}", e.err));
}

/// A funded token holder: a keypair, its token account and the amount minted to it.
pub struct Holder {
    pub owner: Keypair,
    pub token_account: Pubkey,
}

/// Creates a holder of `mint` with `amount` raw units (the mint authority is `authority`).
pub fn holder(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    authority: &Keypair,
    amount: u64,
    program: TokenProgram,
) -> Holder {
    let owner = Keypair::new();
    send_direct(svm, |svm| svm.airdrop(&owner.pubkey(), 10_000_000_000))
        .unwrap_or_else(|e| panic!("airdrop: {:?}", e.err));
    let token_account = create_token_account(svm, payer, mint, &owner.pubkey(), program);
    if amount > 0 {
        mint_to(svm, payer, mint, &token_account, authority, amount, program);
    }
    Holder {
        owner,
        token_account,
    }
}
