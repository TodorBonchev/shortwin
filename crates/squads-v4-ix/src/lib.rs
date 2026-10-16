//! Squads v4 for short.win's governance (M10 WP-7; ARCH §1.1 "Admin", §8.5): instruction
//! encoders, the Squads `TransactionMessage` compiler, PDAs and account decoders for the
//! program mainnet runs — `SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf`, immutable since slot
//! 302,582,236, program 2.1.0 (`tests/fixtures/` group `squads_programs`; source probably
//! `Squads-Protocol/v4@64af733`, UNVERIFIED).
//!
//! Host-only and dependency-light: `solana-pubkey` and `solana-instruction` at the versions
//! every lockfile that links it already holds, Borsh by hand ([`borsh`]). The LiteSVM harness
//! (`tests/litesvm/src/squads.rs`) drives the mainnet ELF with it; the M11 ops tool will too.
//!
//! | Module | What |
//! |---|---|
//! | [`ix`] | `multisig_create_v2`; `vault_transaction_create` / `_create_from_buffer` / `_execute`; `transaction_buffer_create` / `_extend` / `_close`; `proposal_create` / `_approve` / `_reject` / `_cancel` / `_cancel_v2`; `config_transaction_create` / `_execute` with every `ConfigAction` |
//! | [`message`] | the `TransactionMessage` compiler (the SDK's key order, the vault as payer; optional address lookup tables) and decoder; `vault_transaction_execute`'s remaining accounts |
//! | [`pda`] | program config, multisig, vault, transaction, proposal, transaction buffer, ephemeral signer |
//! | [`state`] | `Multisig`, `Proposal`, `TransactionBuffer`, `VaultTransaction` decoders; the instructions a `vault_transaction_execute` runs |
//! | [`error`] | `MultisigError` codes (6000 + variant index) |
//!
//! Everything is pinned against the vendored IDL `tests/fixtures/idl/squads_multisig_program.json`
//! (2.1.0) by `tests/idl.rs`: the discriminators are `sha256("global:<name>")[..8]` /
//! `sha256("account:<Name>")[..8]`, every builder's accounts are the IDL's (names, order,
//! `isMut`, `isSigner`) and its data is what an IDL-driven Borsh encoder writes for the same
//! arguments, the decoders read what that encoder writes, and the error codes are the IDL's.
//!
//! **Semantics the governance relies on** (Squads `64af733`; T-SQ-2 / T-SQ-7 prove them on the
//! ELF): the time lock runs from the approval that reaches the threshold and is checked at
//! execution against the multisig's **current** time lock (`now − approved_at ≥ time_lock`: a
//! later increase delays an approved transaction); an `Approved` vault transaction has no
//! expiry, and it survives a member, threshold or time-lock change that stales everything below
//! it (only its `Active` siblings and config-transaction counterparts go stale);
//! `proposal_cancel` needs `threshold` cancel votes on an `Approved` proposal.

pub mod borsh;
pub mod error;
pub mod ix;
pub mod message;
pub mod pda;
pub mod state;

pub use ix::{ConfigAction, Member, MultisigCreateArgs, Period, permission};
pub use message::{LookupTable, MessageError, VaultMessage, compile};
pub use solana_pubkey::Pubkey;

/// The Squads v4 program id.
pub const ID: Pubkey = solana_pubkey::pubkey!("SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf");

/// The System program.
pub const SYSTEM_PROGRAM: Pubkey = solana_pubkey::pubkey!("11111111111111111111111111111111");

/// The treasury mainnet's `ProgramConfig` names (`multisig_create_v2` must pass it; the
/// creation fee is 0 at slot 452,489,896).
pub const MAINNET_TREASURY: Pubkey =
    solana_pubkey::pubkey!("5DH2e3cJmFpyi6mk65EGFediunm4ui6BiKNUNrhWtD1b");

/// Squads' `MAX_TIME_LOCK` (90 days; `TimeLockExceedsMaxAllowed` above it).
pub const MAX_TIME_LOCK: u32 = 7_776_000;

/// Squads' `MAX_BUFFER_SIZE`: the largest message a transaction buffer holds.
pub const MAX_BUFFER_SIZE: usize = 10_128;

/// Anchor discriminators: `sha256("global:<instruction>")[..8]` and
/// `sha256("account:<Account>")[..8]` (checked in `tests/idl.rs`).
pub mod disc {
    pub const MULTISIG_CREATE_V2: [u8; 8] = [50, 221, 199, 93, 40, 245, 139, 233];
    pub const VAULT_TRANSACTION_CREATE: [u8; 8] = [48, 250, 78, 168, 208, 226, 218, 211];
    pub const VAULT_TRANSACTION_EXECUTE: [u8; 8] = [194, 8, 161, 87, 153, 164, 25, 171];
    pub const VAULT_TRANSACTION_CREATE_FROM_BUFFER: [u8; 8] = [222, 54, 149, 68, 87, 246, 48, 231];
    pub const TRANSACTION_BUFFER_CREATE: [u8; 8] = [245, 201, 113, 108, 37, 63, 29, 89];
    pub const TRANSACTION_BUFFER_EXTEND: [u8; 8] = [230, 157, 67, 56, 5, 238, 245, 146];
    pub const TRANSACTION_BUFFER_CLOSE: [u8; 8] = [17, 182, 208, 228, 136, 24, 178, 102];
    pub const PROPOSAL_CREATE: [u8; 8] = [220, 60, 73, 224, 30, 108, 79, 159];
    pub const PROPOSAL_APPROVE: [u8; 8] = [144, 37, 164, 136, 188, 216, 42, 248];
    pub const PROPOSAL_REJECT: [u8; 8] = [243, 62, 134, 156, 230, 106, 246, 135];
    pub const PROPOSAL_CANCEL: [u8; 8] = [27, 42, 127, 237, 38, 163, 84, 203];
    pub const PROPOSAL_CANCEL_V2: [u8; 8] = [205, 41, 194, 61, 220, 139, 16, 247];
    pub const CONFIG_TRANSACTION_CREATE: [u8; 8] = [155, 236, 87, 228, 137, 75, 81, 39];
    pub const CONFIG_TRANSACTION_EXECUTE: [u8; 8] = [114, 146, 244, 189, 252, 140, 36, 40];

    pub const ACCOUNT_MULTISIG: [u8; 8] = [224, 116, 121, 186, 68, 161, 79, 236];
    pub const ACCOUNT_PROPOSAL: [u8; 8] = [26, 94, 189, 187, 116, 136, 53, 33];
    pub const ACCOUNT_TRANSACTION_BUFFER: [u8; 8] = [90, 36, 35, 219, 93, 225, 110, 96];
    pub const ACCOUNT_VAULT_TRANSACTION: [u8; 8] = [168, 250, 162, 100, 81, 14, 162, 207];
}
