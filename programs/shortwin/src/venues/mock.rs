//! TEST-ONLY adapter of the mock venue `tests/mock_cpamm` (feature `test-venue`; ARCH §8.2,
//! §9 M2; A49). The binding interface is `tests/mock_cpamm/INTERFACE.md`.
//!
//! The mock is a CPMM stand-in and trades exactly like ARCH §2.4.1's Raydium row: the sell
//! leg spends `Q` straight out of `pool_vault` with `trader` as its delegate
//! (`approve_checked(Q)` signed by the market, cleared by the token program at a zero
//! allowance) into the ephemeral `trader_wsol`; the buy leg pays from `trader_wsol` and has the
//! venue deliver straight into `pool_vault` (the mock, like CPMM and AMM v4, does not check
//! the destination's owner). No venue-side rent: the effective `ert` is 0.
//!
//! **Account sets** (order binding; W = writable in trades):
//!
//! - ReadSet: R0 mock pool (W), R1 its base vault (W), R2 its quote (WSOL) vault (W) — keyed
//!   by the market's pool cache (`raydium_cache::{RAY_POOL, RAY_VAULT_TOK, RAY_VAULT_SOL}`).
//! - TradeExtras: T0 the mock program, T1 its `pool_authority` `PDA(mock, ["authority"])`, T2
//!   the WSOL mint, T3 SPL Token (the WSOL side's program), T4 `trader_wsol` (W). The base
//!   mint and its token program are the instruction's named `mint` and `token_program`.
//!
//! **Pricing** (INTERFACE.md "short.win's view"): `quote::cpmm` with `CpmmFees { trade:
//! fee_ppm, creator: 0, creator_on_input: false, protocol: 120,000, fund: 0 }`, on the pricing
//! reserves `vault − counter`; one-way fee `φ = fee_ppm`. The mock executes these quotes
//! exactly (its differential tests).
//!
//! **Venue predicate** (`venue_executable`): the pool parses under the reader contract
//! (owner, discriminator, `layout_version == 0`), records the passed vaults, is not disabled,
//! each vault holds at least its counter and both reserves are non-zero. Anything else is "not
//! executable" (A63): a layout change or an admin halt starts the S3 chain; it never reverts a
//! crank.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;
use anchor_spl::token_interface::{self, ApproveChecked};
use shortwin_math::fees::CpmmFees;
use shortwin_math::quote::Pricing;
use shortwin_venue_state::mock_cpamm;

use super::{BuyOut, Leg, VenueState};
use crate::constants::{
    MOCK_CPAMM_AUTHORITY, MOCK_CPAMM_PROGRAM_ID, TEST_VENUE_MARKER, TOKEN_PROGRAM_ID, WSOL_MINT,
};
use crate::engine::trader::{token_state, Funding};
use crate::errors::{CheckedExt, ShortwinError};
use crate::state::{raydium_cache, Market, VenueKind};

/// ReadSet length.
pub const READ_LEN: usize = 3;
/// TradeExtras length.
pub const TRADE_LEN: usize = 5;

/// ReadSet slots.
pub const R_POOL: usize = 0;
pub const R_BASE_VAULT: usize = 1;
pub const R_QUOTE_VAULT: usize = 2;
/// TradeExtras slots.
pub const T_PROGRAM: usize = 0;
pub const T_AUTHORITY: usize = 1;
pub const T_WSOL_MINT: usize = 2;
pub const T_WSOL_PROGRAM: usize = 3;
pub const T_TRADER_WSOL: usize = 4;

/// `swap_exact_in(amount_in, min_out)`: `sha256("global:swap_exact_in")[..8]`.
pub const IX_SWAP_EXACT_IN: [u8; 8] = [0x68, 0x68, 0x83, 0x56, 0xa1, 0xbd, 0xb4, 0xd8];
/// `swap_exact_out(max_in, amount_out)`: `sha256("global:swap_exact_out")[..8]`.
pub const IX_SWAP_EXACT_OUT: [u8; 8] = [0xfa, 0x49, 0x65, 0x21, 0x26, 0xcf, 0x4b, 0xb8];

/// The mock pool seed `["pool", base_mint, WSOL]`.
pub const POOL_SEED: &[u8] = b"pool";

fn cached(market: &Market, i: usize) -> Pubkey {
    market.pool_cache.get(i).copied().unwrap_or_default()
}

/// The market's mock pool.
pub fn pool(market: &Market) -> Pubkey {
    cached(market, raydium_cache::RAY_POOL)
}

/// The fees short.win prices a mock pool with (INTERFACE.md).
pub fn fees(fee_ppm: u32) -> CpmmFees {
    CpmmFees {
        trade: u64::from(fee_ppm),
        creator: 0,
        creator_on_input: false,
        protocol: mock_cpamm::PROTOCOL_FEE_RATE,
        fund: 0,
    }
}

fn get<'a, 'info>(accounts: &'a [AccountInfo<'info>], i: usize) -> Result<&'a AccountInfo<'info>> {
    accounts
        .get(i)
        .ok_or_else(|| error!(ShortwinError::BadRemainingAccounts))
}

/// Key-checks the ReadSet against the pool cache.
pub fn check_read(market: &Market, read: &[AccountInfo]) -> Result<()> {
    require!(read.len() == READ_LEN, ShortwinError::BadRemainingAccounts);
    let want = [
        pool(market),
        cached(market, raydium_cache::RAY_VAULT_TOK),
        cached(market, raydium_cache::RAY_VAULT_SOL),
    ];
    for (info, key) in read.iter().zip(want.iter()) {
        require_keys_eq!(info.key(), *key, ShortwinError::VenueAccountMismatch);
    }
    Ok(())
}

/// Key-checks the TradeExtras against the compile-time constants and the market.
pub fn check_trade(market: &Market, trade: &[AccountInfo]) -> Result<()> {
    require!(
        trade.len() == TRADE_LEN,
        ShortwinError::BadRemainingAccounts
    );
    let want = [
        MOCK_CPAMM_PROGRAM_ID,
        MOCK_CPAMM_AUTHORITY,
        WSOL_MINT,
        TOKEN_PROGRAM_ID,
        market.trader_wsol,
    ];
    for (info, key) in trade.iter().zip(want.iter()) {
        require_keys_eq!(info.key(), *key, ShortwinError::VenueAccountMismatch);
    }
    require!(
        get(trade, T_PROGRAM)?.executable,
        ShortwinError::VenueAccountMismatch
    );
    Ok(())
}

/// The venue state from a key-checked ReadSet (module docs: the venue predicate). Never
/// fails: whatever does not parse is "not executable" (A63).
pub fn read_state(read: &[AccountInfo]) -> VenueState {
    state_of(read).unwrap_or_else(|| VenueState::not_executable(VenueKind::Mock))
}

fn state_of(read: &[AccountInfo]) -> Option<VenueState> {
    let (pool_info, base_info, quote_info) = (
        read.get(R_POOL)?,
        read.get(R_BASE_VAULT)?,
        read.get(R_QUOTE_VAULT)?,
    );
    let pool = {
        let data = pool_info.try_borrow_data().ok()?;
        mock_cpamm::read(&pool_info.owner.to_bytes(), &data).ok()?
    };
    if pool.base_vault != base_info.key.to_bytes() || pool.quote_vault != quote_info.key.to_bytes()
    {
        return None;
    }
    let base = token_state(base_info).ok()?;
    let quote = token_state(quote_info).ok()?;
    if base.mint != pool.base_mint || quote.mint != pool.quote_mint {
        return None;
    }
    let (x, y) = mock_cpamm::reserves(&pool, base.amount, quote.amount)?;
    let f = fees(pool.fee_ppm);
    Some(VenueState {
        kind: VenueKind::Mock,
        executable: true,
        x,
        y,
        r: y,
        pricing: Some(Pricing::Cpmm { buy: f, sell: f }),
        fee_ppm: pool.fee_ppm,
        locked_depth: 0,
        curve: None,
        pool: None,
    })
}

/// The mock's fee view for risk quotes whether or not it can execute
/// (`super::risk_pricing`): the pool's `fee_ppm` while the pool account parses (a disabled
/// pool keeps its fee), else the cap itself (a layout change: `φ_live` unknown); capped at
/// `cap_bps`. Reads only the key-checked pool slot of the ReadSet.
pub fn fee_view(read: &[AccountInfo], cap_bps: u64) -> Pricing {
    let parsed = read.get(R_POOL).and_then(|info| {
        let data = info.try_borrow_data().ok()?;
        mock_cpamm::read(&info.owner.to_bytes(), &data).ok()
    });
    let cap_ppm = u32::try_from(cap_bps.saturating_mul(100)).unwrap_or(u32::MAX);
    let f = fees(parsed.map_or(cap_ppm, |p| p.fee_ppm));
    Pricing::Cpmm { buy: f, sell: f }.capped(cap_bps)
}

/// `create_market(venue_hint = Mock)`: adopts the mock pool of `mint` (ARCH §2.5, §3.2 in
/// their test-venue form). The ReadSet must be the pool `PDA(mock, ["pool", mint, WSOL])` and
/// the vaults it records; the pool must parse, quote WSOL, price `mint` under
/// `token_program`, and be executable (`PoolIneligible` otherwise). Stores the keys in the
/// pool cache and returns the live state.
pub fn adopt(
    market: &mut Market,
    read: &[AccountInfo],
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Result<VenueState> {
    require!(read.len() == READ_LEN, ShortwinError::BadRemainingAccounts);
    let expected_pool = Pubkey::find_program_address(
        &[POOL_SEED, mint.as_ref(), WSOL_MINT.as_ref()],
        &MOCK_CPAMM_PROGRAM_ID,
    )
    .0;
    let pool_info = get(read, R_POOL)?;
    require_keys_eq!(
        pool_info.key(),
        expected_pool,
        ShortwinError::VenueAccountMismatch
    );
    let pool = {
        let data = pool_info.try_borrow_data()?;
        mock_cpamm::read(&pool_info.owner.to_bytes(), &data)
            .map_err(|_| error!(ShortwinError::PoolIneligible))?
    };
    require!(
        pool.base_mint == mint.to_bytes()
            && pool.quote_mint == WSOL_MINT.to_bytes()
            && pool.base_token_program == token_program.to_bytes(),
        ShortwinError::PoolIneligible
    );
    let base_vault = Pubkey::new_from_array(pool.base_vault);
    let quote_vault = Pubkey::new_from_array(pool.quote_vault);
    require_keys_eq!(
        get(read, R_BASE_VAULT)?.key(),
        base_vault,
        ShortwinError::VenueAccountMismatch
    );
    require_keys_eq!(
        get(read, R_QUOTE_VAULT)?.key(),
        quote_vault,
        ShortwinError::VenueAccountMismatch
    );
    let state = read_state(read);
    require!(state.price().is_some(), ShortwinError::PoolIneligible);
    let slot = |m: &mut Market, i: usize, k: Pubkey| -> Result<()> {
        *m.pool_cache.get_mut(i).ok_or(ShortwinError::MathOverflow)? = k;
        Ok(())
    };
    slot(market, raydium_cache::RAY_POOL, expected_pool)?;
    slot(market, raydium_cache::RAY_VAULT_TOK, base_vault)?;
    slot(market, raydium_cache::RAY_VAULT_SOL, quote_vault)?;
    msg!(TEST_VENUE_MARKER);
    Ok(state)
}

/// One direction of a mock swap.
struct Sides<'a, 'info> {
    source: &'a AccountInfo<'info>,
    destination: &'a AccountInfo<'info>,
    input_vault: &'a AccountInfo<'info>,
    output_vault: &'a AccountInfo<'info>,
    input_mint: &'a AccountInfo<'info>,
    output_mint: &'a AccountInfo<'info>,
    input_program: &'a AccountInfo<'info>,
    output_program: &'a AccountInfo<'info>,
}

/// CPI into the mock (INTERFACE.md "Swap accounts", then the resolved extras), `trader`
/// signing as the source's owner or delegate.
fn swap<'info>(
    leg: &Leg<'info>,
    disc: [u8; 8],
    a: u64,
    b: u64,
    s: &Sides<'_, 'info>,
) -> Result<()> {
    let pool_info = get(leg.read, R_POOL)?;
    let authority = get(leg.trade, T_AUTHORITY)?;
    let program = get(leg.trade, T_PROGRAM)?;
    let trader = &leg.trader.trader;
    let mut metas = vec![
        AccountMeta::new_readonly(trader.key(), true),
        AccountMeta::new(pool_info.key(), false),
        AccountMeta::new_readonly(authority.key(), false),
        AccountMeta::new(s.source.key(), false),
        AccountMeta::new(s.destination.key(), false),
        AccountMeta::new(s.input_vault.key(), false),
        AccountMeta::new(s.output_vault.key(), false),
        AccountMeta::new_readonly(s.input_mint.key(), false),
        AccountMeta::new_readonly(s.output_mint.key(), false),
        AccountMeta::new_readonly(s.input_program.key(), false),
        AccountMeta::new_readonly(s.output_program.key(), false),
    ];
    metas.extend(leg.extra_metas.iter().cloned());
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&disc);
    data.extend_from_slice(&a.to_le_bytes());
    data.extend_from_slice(&b.to_le_bytes());
    let ix = Instruction {
        program_id: MOCK_CPAMM_PROGRAM_ID,
        accounts: metas,
        data,
    };
    let mut infos = vec![
        trader.clone(),
        pool_info.clone(),
        authority.clone(),
        s.source.clone(),
        s.destination.clone(),
        s.input_vault.clone(),
        s.output_vault.clone(),
        s.input_mint.clone(),
        s.output_mint.clone(),
        s.input_program.clone(),
        s.output_program.clone(),
        program.clone(),
    ];
    infos.extend(leg.extra_infos.iter().cloned());
    let bump = [leg.trader.trader_bump];
    let seeds: &[&[u8]] = &[crate::state::TRADER_SEED, leg.trader.market.as_ref(), &bump];
    invoke_signed(&ix, &infos, &[seeds])?;
    Ok(())
}

/// The sell leg of an open (ARCH §2.4.1 Raydium row, exact in): `approve_checked(pool_vault →
/// trader, q)` signed by the market; `trader_wsol` created with the borrower paying its rent;
/// `swap_exact_in(q, min_out)` from `pool_vault` into `trader_wsol`; `S = trader_wsol.amount`;
/// the pool vault must have lost exactly `q` (`VenueResultMismatch`) and have no delegate left
/// (`DelegateNotCleared`); `trader_wsol` is closed into `position`, which then holds `S` plus
/// the WSOL account's rent. Returns `S`; the caller refunds the borrower that rent from the
/// position after its last CPI that involves either account ([`super::SellOut`]). The caller
/// normalised the trader first.
pub fn sell<'info>(
    leg: &Leg<'info>,
    borrower: &AccountInfo<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    min_out: u64,
) -> Result<u64> {
    let market_seeds: &[&[u8]] = &[Market::SEED, leg.mint.key.as_ref(), &[leg.market_bump]];
    token_interface::approve_checked(
        CpiContext::new_with_signer(
            leg.token_program.key(),
            ApproveChecked {
                to: leg.pool_vault.clone(),
                mint: leg.mint.clone(),
                delegate: leg.trader.trader.clone(),
                authority: leg.market.clone(),
            },
            &[market_seeds],
        ),
        q,
        leg.decimals,
    )?;
    leg.trader.open_wsol(Funding::Signer(borrower), 0)?;
    let before = token_state(&leg.pool_vault)?.amount;
    let wsol = leg.trader.trader_wsol()?;
    swap(
        leg,
        IX_SWAP_EXACT_IN,
        q,
        min_out,
        &Sides {
            source: &leg.pool_vault,
            destination: wsol,
            input_vault: get(leg.read, R_BASE_VAULT)?,
            output_vault: get(leg.read, R_QUOTE_VAULT)?,
            input_mint: &leg.mint,
            output_mint: get(leg.trade, T_WSOL_MINT)?,
            input_program: &leg.token_program,
            output_program: get(leg.trade, T_WSOL_PROGRAM)?,
        },
    )?;
    let after = token_state(&leg.pool_vault)?;
    require!(
        before.checked_sub(after.amount) == Some(q),
        ShortwinError::VenueResultMismatch
    );
    require!(after.delegate.is_none(), ShortwinError::DelegateNotCleared);
    let s = leg.trader.wsol_amount()?;
    leg.trader.close_wsol(position)?;
    Ok(s)
}

/// The buy leg of an exit (ARCH §2.4.1 Raydium row, exact out): `trader_wsol` funded with
/// `rent(165) + max_in` straight from the position; `swap_exact_out(max_in, q)` from
/// `trader_wsol` into `pool_vault`; `trader_wsol` closed back into the position. Measures `B =
/// position lamports before − after` and the pool vault's token delta; requires `B ≤ max_in`
/// (`ert` = 0) and a delta of at least `q` (`VenueResultMismatch`; a surplus is a donation,
/// A62, V2-10).
pub fn buy<'info>(
    leg: &Leg<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    max_in: u64,
) -> Result<BuyOut> {
    let lamports_before = position.lamports();
    leg.trader.open_wsol(Funding::Position(position), max_in)?;
    let before = token_state(&leg.pool_vault)?.amount;
    let wsol = leg.trader.trader_wsol()?;
    swap(
        leg,
        IX_SWAP_EXACT_OUT,
        max_in,
        q,
        &Sides {
            source: wsol,
            destination: &leg.pool_vault,
            input_vault: get(leg.read, R_QUOTE_VAULT)?,
            output_vault: get(leg.read, R_BASE_VAULT)?,
            input_mint: get(leg.trade, T_WSOL_MINT)?,
            output_mint: &leg.mint,
            input_program: get(leg.trade, T_WSOL_PROGRAM)?,
            output_program: &leg.token_program,
        },
    )?;
    let received = token_state(&leg.pool_vault)?
        .amount
        .checked_sub(before)
        .ok_or(ShortwinError::VenueResultMismatch)?;
    require_gte!(received, q, ShortwinError::VenueResultMismatch);
    leg.trader.close_wsol(position)?;
    let spent = lamports_before.checked_sub(position.lamports()).or_math()?;
    require_gte!(max_in, spent, ShortwinError::VenueResultMismatch);
    Ok(BuyOut { spent, received })
}
