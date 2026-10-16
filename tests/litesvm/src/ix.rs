//! Instruction builders for every M1 instruction, from anchor-lang's generated
//! `shortwin::instruction::*` (data) and `shortwin::accounts::*` (metas).
//!
//! Tracks refine the builders of their instructions together with the Accounts structs.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use shortwin::PauseScope;
use shortwin::constants::{ASSOCIATED_TOKEN_PROGRAM_ID, MEMO_PROGRAM_ID, VENUE_PROGRAM_DATA};
use shortwin::state::{ConfigChange, InitConfigArgs, VenueKind};

use crate::pda;

fn system_program() -> Pubkey {
    anchor_lang::system_program::ID
}

fn build(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction {
        program_id: shortwin::ID,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

/// Appends `remaining` accounts (read-only unless flagged) to an instruction.
pub fn with_remaining(mut ix: Instruction, remaining: &[AccountMeta]) -> Instruction {
    ix.accounts.extend_from_slice(remaining);
    ix
}

// ---- admin (ARCH §3.1) --------------------------------------------------------------------

/// `initialize_config(args)`, signed by short.win's upgrade authority.
pub fn initialize_config(authority: &Pubkey, args: InitConfigArgs) -> Instruction {
    build(
        shortwin::accounts::InitializeConfig {
            authority: *authority,
            config: pda::config(),
            venue_guard: pda::venue_guard(),
            venue_extras_pump_curve: pda::venue_extras(VenueKind::PumpCurve),
            venue_extras_pump_swap: pda::venue_extras(VenueKind::PumpSwap),
            venue_extras_raydium_v4: pda::venue_extras(VenueKind::RaydiumV4),
            venue_extras_raydium_cpmm: pda::venue_extras(VenueKind::RaydiumCpmm),
            oracle_cache: pda::oracle_cache(),
            program_data: pda::program_data(),
            system_program: system_program(),
        },
        shortwin::instruction::InitializeConfig { args },
    )
}

/// `propose_config(change)`, signed by `governance`.
pub fn propose_config(governance: &Pubkey, change: ConfigChange) -> Instruction {
    build(
        shortwin::accounts::ProposeConfig {
            governance: *governance,
            config: pda::config(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::ProposeConfig { change },
    )
}

/// `apply_config()` (no signer) without the optional accounts (enough for every change but
/// `Authorities` and `OracleFeed`; see [`apply_config_for`]).
pub fn apply_config() -> Instruction {
    apply_config_with(None, false)
}

/// `apply_config()` with the optional `fee_recipient` account (an `Authorities` change) and
/// the optional `oracle_cache` (an `OracleFeed` change).
pub fn apply_config_with(fee_recipient: Option<Pubkey>, oracle_cache: bool) -> Instruction {
    build(
        shortwin::accounts::ApplyConfig {
            config: pda::config(),
            fee_recipient,
            oracle_cache: oracle_cache.then(pda::oracle_cache),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::ApplyConfig {},
    )
}

/// `apply_config()` with the optional accounts `change` needs (M1-SEC-5, M1-SEC-10).
pub fn apply_config_for(change: &ConfigChange) -> Instruction {
    match change {
        ConfigChange::Authorities { fee_recipient, .. } => {
            apply_config_with(Some(*fee_recipient), false)
        }
        ConfigChange::OracleFeed { .. } => apply_config_with(None, true),
        _ => apply_config(),
    }
}

/// `cancel_config()`, signed by `governance` or `pause_authority`.
pub fn cancel_config(authority: &Pubkey) -> Instruction {
    build(
        shortwin::accounts::CancelConfig {
            authority: *authority,
            config: pda::config(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::CancelConfig {},
    )
}

/// `set_pause(scope, paused)`; `market` only for `PauseScope::Market`.
pub fn set_pause(
    authority: &Pubkey,
    scope: PauseScope,
    paused: bool,
    market: Option<Pubkey>,
) -> Instruction {
    build(
        shortwin::accounts::SetPause {
            authority: *authority,
            config: pda::config(),
            market,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SetPause { scope, paused },
    )
}

/// `set_coverage_pause(paused, reason)` on `market`, signed by `coverage_authority` (ARCH §3.1;
/// O17) — or, for a clear, by `pause_authority` (the Emergency vault; OWNER-M7-3 (b)). `reason`
/// is `shortwin::events::COVERAGE_REASON_KEEPER` for every accepted call.
pub fn set_coverage_pause(
    coverage_authority: &Pubkey,
    market: &Pubkey,
    paused: bool,
    reason: u8,
) -> Instruction {
    build(
        shortwin::accounts::SetCoveragePause {
            coverage_authority: *coverage_authority,
            config: pda::config(),
            market: *market,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SetCoveragePause { paused, reason },
    )
}

/// `disable_venue(bit)`, signed by `pause_authority`.
pub fn disable_venue(pause_authority: &Pubkey, bit: u8) -> Instruction {
    build(
        shortwin::accounts::DisableVenue {
            pause_authority: *pause_authority,
            config: pda::config(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::DisableVenue { bit },
    )
}

/// The ProgramData key a latch instruction for `venue` passes (a wrong index gets a default
/// key; override `program_data` to test key checks).
pub fn venue_program_data_key(venue: u8) -> Pubkey {
    VENUE_PROGRAM_DATA
        .get(usize::from(venue))
        .copied()
        .unwrap_or_default()
}

/// `flag_venue_upgraded(venue)` (no signer) with an explicit ProgramData account.
pub fn flag_venue_upgraded_with(venue: u8, program_data: Pubkey) -> Instruction {
    build(
        shortwin::accounts::FlagVenueUpgraded {
            venue_guard: pda::venue_guard(),
            program_data,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::FlagVenueUpgraded { venue },
    )
}

/// `flag_venue_upgraded(venue)` with the venue's ProgramData.
pub fn flag_venue_upgraded(venue: u8) -> Instruction {
    flag_venue_upgraded_with(venue, venue_program_data_key(venue))
}

/// `revalidate_venue(venue, expected_slot, [0; 32])` (no ELF hash: the exact slot) with an
/// explicit ProgramData account.
pub fn revalidate_venue_with(
    pause_authority: &Pubkey,
    venue: u8,
    expected_slot: u64,
    program_data: Pubkey,
) -> Instruction {
    build(
        shortwin::accounts::RevalidateVenue {
            pause_authority: *pause_authority,
            config: pda::config(),
            venue_guard: pda::venue_guard(),
            program_data,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::RevalidateVenue {
            venue,
            expected_slot,
            expected_elf_hash: [0; 32],
        },
    )
}

/// `revalidate_venue(venue, expected_slot, [0; 32])`, signed by `pause_authority`.
pub fn revalidate_venue(pause_authority: &Pubkey, venue: u8, expected_slot: u64) -> Instruction {
    revalidate_venue_with(
        pause_authority,
        venue,
        expected_slot,
        venue_program_data_key(venue),
    )
}

/// `revalidate_venue(venue, expected_slot, expected_elf_hash)` with the venue's ProgramData,
/// signed by `pause_authority`: the review names the sha256 of the deploy's trimmed ELF (AIR-01;
/// `shortwin::instructions::flag_venue_upgraded::trimmed_elf_hash_of`).
pub fn revalidate_venue_reviewed(
    pause_authority: &Pubkey,
    venue: u8,
    expected_slot: u64,
    expected_elf_hash: [u8; 32],
) -> Instruction {
    build(
        shortwin::accounts::RevalidateVenue {
            pause_authority: *pause_authority,
            config: pda::config(),
            venue_guard: pda::venue_guard(),
            program_data: venue_program_data_key(venue),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::RevalidateVenue {
            venue,
            expected_slot,
            expected_elf_hash,
        },
    )
}

/// `sweep_protocol_fees()` (no signer).
pub fn sweep_protocol_fees(market: &Pubkey, fee_recipient: &Pubkey) -> Instruction {
    build(
        shortwin::accounts::SweepProtocolFees {
            config: pda::config(),
            market: *market,
            market_vault: pda::market_vault(market).0,
            fee_recipient: *fee_recipient,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SweepProtocolFees {},
    )
}

// ---- markets (ARCH §3.2, §3.9) --------------------------------------------------------------

/// `create_market(venue_hint, u64::MAX)` (no bound on the creation fee) with the detection
/// set `[bonding_curve, canonical_pool]` as remaining accounts (C0-15).
pub fn create_market(
    creator: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
    venue_hint: u8,
    canonical_pool: &Pubkey,
) -> Instruction {
    create_market_max_fee(
        creator,
        mint,
        token_program,
        venue_hint,
        canonical_pool,
        u64::MAX,
    )
}

/// `create_market(venue_hint, max_creation_fee)` (M1-SEC-11).
pub fn create_market_max_fee(
    creator: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
    venue_hint: u8,
    canonical_pool: &Pubkey,
    max_creation_fee: u64,
) -> Instruction {
    let (market, _) = pda::market(mint);
    let (trader, _) = pda::trader(&market);
    let ix = build(
        shortwin::accounts::CreateMarket {
            creator: *creator,
            config: pda::config(),
            venue_guard: pda::venue_guard(),
            market,
            market_vault: pda::market_vault(&market).0,
            pool_vault: pda::pool_vault(&market).0,
            mint: *mint,
            trader,
            trader_base_ata: pda::ata(&trader, mint, token_program),
            token_program: *token_program,
            associated_token_program: ASSOCIATED_TOKEN_PROGRAM_ID,
            system_program: system_program(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::CreateMarket {
            venue_hint,
            max_creation_fee,
        },
    );
    with_remaining(
        ix,
        &[
            AccountMeta::new_readonly(pda::bonding_curve(mint), false),
            AccountMeta::new_readonly(*canonical_pool, false),
        ],
    )
}

/// `flag_unsafe_mint()` (no signer).
pub fn flag_unsafe_mint(mint: &Pubkey) -> Instruction {
    build(
        shortwin::accounts::FlagUnsafeMint {
            market: pda::market(mint).0,
            mint: *mint,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::FlagUnsafeMint {},
    )
}

/// `crank_sol_usd()` (no signer).
pub fn crank_sol_usd(price_update: &Pubkey) -> Instruction {
    build(
        shortwin::accounts::CrankSolUsd {
            config: pda::config(),
            oracle_cache: pda::oracle_cache(),
            price_update: *price_update,
        },
        shortwin::instruction::CrankSolUsd {},
    )
}

// ---- lenders (ARCH §3.3) --------------------------------------------------------------------

/// The accounts every lender token instruction shares.
#[derive(Clone, Copy, Debug)]
pub struct LenderKeys {
    pub owner: Pubkey,
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub owner_token_account: Pubkey,
}

/// `ShareEpoch` records `from..to` as writable remaining accounts: a lender converting through
/// a rebase record writes its `conv_rem` (KI-65; a read-only one is refused with
/// `EpochRecordReadOnly`, an unchanged one may stay read-only).
pub fn epoch_records(market: &Pubkey, from: u32, to: u32) -> Vec<AccountMeta> {
    (from..to)
        .map(|e| AccountMeta::new(pda::share_epoch(market, e).0, false))
        .collect()
}

/// `deposit(amount, min_shares)`.
pub fn deposit(k: &LenderKeys, amount: u64, min_shares: u128) -> Instruction {
    let market = pda::market(&k.mint).0;
    build(
        shortwin::accounts::Deposit {
            owner: k.owner,
            config: pda::config(),
            market,
            market_vault: pda::market_vault(&market).0,
            lender: pda::lender(&market, &k.owner).0,
            mint: k.mint,
            owner_token_account: k.owner_token_account,
            pool_vault: pda::pool_vault(&market).0,
            token_program: k.token_program,
            system_program: system_program(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::Deposit { amount, min_shares },
    )
}

/// `withdraw(shares, min_tokens, 0)` without the memo program.
pub fn withdraw(k: &LenderKeys, shares: u128, min_tokens: u64) -> Instruction {
    withdraw_with(k, shares, min_tokens, 0, false)
}

/// `withdraw(shares, min_tokens, min_sol)`; `memo` passes the memo program (a Token-2022
/// destination that requires memos, ARCH §3.3).
pub fn withdraw_with(
    k: &LenderKeys,
    shares: u128,
    min_tokens: u64,
    min_sol: u64,
    memo: bool,
) -> Instruction {
    let market = pda::market(&k.mint).0;
    build(
        shortwin::accounts::Withdraw {
            owner: k.owner,
            config: pda::config(),
            market,
            market_vault: pda::market_vault(&market).0,
            lender: pda::lender(&market, &k.owner).0,
            mint: k.mint,
            owner_token_account: k.owner_token_account,
            pool_vault: pda::pool_vault(&market).0,
            token_program: k.token_program,
            system_program: system_program(),
            memo_program: memo.then_some(MEMO_PROGRAM_ID),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::Withdraw {
            shares,
            min_tokens,
            min_sol,
        },
    )
}

/// `claim_sol()`.
pub fn claim_sol(owner: &Pubkey, market: &Pubkey) -> Instruction {
    build(
        shortwin::accounts::ClaimSol {
            owner: *owner,
            market: *market,
            lender: pda::lender(market, owner).0,
            market_vault: pda::market_vault(market).0,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::ClaimSol {},
    )
}

/// `cover_deficit(lamports)`.
pub fn cover_deficit(owner: &Pubkey, market: &Pubkey, lamports: u64) -> Instruction {
    build(
        shortwin::accounts::CoverDeficit {
            owner: *owner,
            market: *market,
            lender: pda::lender(market, owner).0,
            market_vault: pda::market_vault(market).0,
            system_program: system_program(),
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::CoverDeficit { lamports },
    )
}

/// `sync_lender(max_epochs)` for `owner`'s position, sent by anyone (`owner` does not sign):
/// the pot-only touch at a rebase record, which only the owner converts through (KI65-REV-SEC1,
/// KI-66).
pub fn sync_lender(owner: &Pubkey, market: &Pubkey, max_epochs: u8) -> Instruction {
    build(
        shortwin::accounts::SyncLender {
            market: *market,
            market_vault: pda::market_vault(market).0,
            lender: pda::lender(market, owner).0,
            owner: *owner,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::SyncLender { max_epochs },
    )
}

/// [`sync_lender`] signed by `owner`: it may convert the position through rebase records.
pub fn sync_lender_signed(owner: &Pubkey, market: &Pubkey, max_epochs: u8) -> Instruction {
    let mut ix = sync_lender(owner, market, max_epochs);
    for m in ix.accounts.iter_mut().filter(|m| m.pubkey == *owner) {
        m.is_signer = true;
    }
    ix
}
