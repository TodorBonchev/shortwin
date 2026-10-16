//! Account fixtures written straight into LiteSVM (ARCH §8.2 "scenario builders").
//!
//! - [`set_program_data`]: a mock venue `ProgramData` (tag 3, deploy slot, authority) at the
//!   venue's compile-time address — T-LATCH without the 10 MiB mainnet ELF (M1); M3+ load the
//!   dumped accounts from `tests/fixtures/` instead.
//! - [`install_market`]: a `None`-venue market (Market + MarketVault + pool vault) exactly as
//!   `create_market` lays it out (`tests/market_create.rs` checks the two are byte-identical);
//!   a cheap setup for tests about something else, and the base for adversarial state (e.g. a
//!   pre-set `lock_cal`, a live wipe record). Tests of listing use `create_market`.
//! - [`mints`]: mint fixtures for the SPEC §3 filter.

use anchor_lang::AccountSerialize;
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use bytemuck::Zeroable;
use litesvm::LiteSVM;
use shortwin::constants::{MARKET_VERSION, TOKEN_PROGRAM_ID, WSOL_MINT};
use shortwin::state::{
    GlobalConfig, MARKET_FLAG_WIPE_REWARDS_CLAIMED, MARKET_FLAG_WIPE_TRADER_SWEPT, Market,
    MarketVault, PodU128, VenueKind,
};
use solana_account::Account;
use solana_program_option::COption;
use solana_program_pack::Pack;
use spl_token_2022_interface::state::{Account as TokenAccountState, AccountState};

use crate::pda;
use crate::tokens::TokenProgram;

pub mod mints;

/// Writes a mock `ProgramData` at `key` (owner = upgradeable loader) with deploy `slot`.
/// `extra` bytes stand in for the ELF.
pub fn set_program_data(
    svm: &mut LiteSVM,
    key: &Pubkey,
    slot: u64,
    authority: Option<&Pubkey>,
    extra: usize,
) {
    let auth = authority.map(Pubkey::to_bytes);
    let header = shortwin_venue_state::program_data::encode_header(slot, auth.as_ref());
    let mut data = header.to_vec();
    data.resize(header.len() + extra, 0);
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(
        *key,
        Account {
            lamports,
            data,
            owner: bpf_loader_upgradeable::ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap_or_else(|e| panic!("set_program_data: {e:?}"));
}

/// Writes a zero-copy short.win account (discriminator + bytes), rent-exempt plus `extra`
/// lamports.
pub fn set_zc<T: anchor_lang::ZeroCopy>(svm: &mut LiteSVM, key: &Pubkey, value: &T, extra: u64) {
    let mut data = T::DISCRIMINATOR.to_vec();
    data.extend_from_slice(bytemuck::bytes_of(value));
    let lamports = svm.minimum_balance_for_rent_exemption(data.len()) + extra;
    svm.set_account(
        *key,
        Account {
            lamports,
            data,
            owner: shortwin::ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap_or_else(|e| panic!("set_zc: {e:?}"));
}

/// Writes a Borsh short.win account, rent-exempt plus `extra` lamports.
pub fn set_borsh<T: AccountSerialize>(
    svm: &mut LiteSVM,
    key: &Pubkey,
    value: &T,
    space: usize,
    extra: u64,
) {
    let mut data = Vec::with_capacity(space);
    value
        .try_serialize(&mut data)
        .unwrap_or_else(|e| panic!("set_borsh: {e:?}"));
    data.resize(space, 0);
    let lamports = svm.minimum_balance_for_rent_exemption(data.len()) + extra;
    svm.set_account(
        *key,
        Account {
            lamports,
            data,
            owner: shortwin::ID,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap_or_else(|e| panic!("set_borsh: {e:?}"));
}

/// Writes an initialized token account (165 bytes, no extensions) of `mint` owned by `owner`.
pub fn set_token_account(
    svm: &mut LiteSVM,
    key: &Pubkey,
    mint: &Pubkey,
    owner: &Pubkey,
    amount: u64,
    program: TokenProgram,
) {
    let state = TokenAccountState {
        mint: *mint,
        owner: *owner,
        amount,
        delegate: COption::None,
        state: AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    };
    let mut data = vec![0u8; TokenAccountState::LEN];
    state.pack_into_slice(&mut data);
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(
        *key,
        Account {
            lamports,
            data,
            owner: program.id(),
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap_or_else(|e| panic!("set_token_account: {e:?}"));
}

/// Keys of an installed market.
#[derive(Clone, Copy, Debug)]
pub struct MarketKeys {
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub market: Pubkey,
    pub market_vault: Pubkey,
    pub pool_vault: Pubkey,
    pub trader: Pubkey,
}

/// Installs a list-only (`VenueKind::None`) market for `mint` as `create_market` would leave it
/// (identity, bumps, own keys, detection keys, `Π = WAD`, both post-wipe bits set, the
/// market-level parameters and versions of `config`). `created_at = now`.
pub fn install_market(
    svm: &mut LiteSVM,
    mint: &Pubkey,
    decimals: u8,
    program: TokenProgram,
    creator: &Pubkey,
    now: i64,
    config: &GlobalConfig,
) -> MarketKeys {
    let token_program = program.id();
    let (market, bump) = pda::market(mint);
    let (market_vault, market_vault_bump) = pda::market_vault(&market);
    let (pool_vault, pool_vault_bump) = pda::pool_vault(&market);
    let (trader, trader_bump) = pda::trader(&market);
    let (trader_wsol, trader_wsol_bump) = pda::trader_wsol(&market);

    let mut m = Market::zeroed();
    m.version = MARKET_VERSION;
    m.flags = MARKET_FLAG_WIPE_TRADER_SWEPT | MARKET_FLAG_WIPE_REWARDS_CLAIMED;
    m.bump = bump;
    m.pool_vault_bump = pool_vault_bump;
    m.market_vault_bump = market_vault_bump;
    m.trader_bump = trader_bump;
    m.trader_wsol_bump = trader_wsol_bump;
    m.venue = VenueKind::None as u8;
    m.tier = shortwin_math::tier::Tier::C as u8;
    m.decimals = decimals;
    m.created_at = now;
    m.mint = *mint;
    m.token_program = token_program;
    m.creator = *creator;
    m.pool_vault = pool_vault;
    m.market_vault = market_vault;
    m.trader = trader;
    m.trader_base_ata = pda::ata(&trader, mint, &token_program);
    m.trader_wsol = trader_wsol;
    m.bonding_curve = pda::bonding_curve(mint);
    m.canonical_pool = pda::canonical_pool(mint);
    // The derived keys `create_market` caches (M5-SEC-6).
    m.trader_wsol_ata = pda::ata(&trader, &WSOL_MINT, &TOKEN_PROGRAM_ID);
    m.pump_pool_authority = pda::pump_pool_authority(mint);
    m.last_accrual_ts = now;
    m.ema_params_version = config.ema_params.version;
    m.ema_dtmax_s = config.ema_params.ema_dtmax_s;
    m.ema_clamp_bps = config.ema_params.ema_clamp_bps;
    m.liq_slow_tau_s = config.ema_params.liq_slow_tau_s;
    m.liq_slow_dtmax_s = config.ema_params.liq_slow_dtmax_s;
    m.kink_version = config.kink.version;
    m.util_kink_bps = config.kink.util_kink_bps;
    set_zc(svm, &market, &m, 0);

    let mut v = MarketVault::zeroed();
    v.pi = PodU128::new(shortwin_math::num::WAD);
    v.bump = market_vault_bump;
    set_zc(svm, &market_vault, &v, 0);

    set_token_account(svm, &pool_vault, mint, &market, 0, program);

    MarketKeys {
        mint: *mint,
        token_program,
        market,
        market_vault,
        pool_vault,
        trader,
    }
}
