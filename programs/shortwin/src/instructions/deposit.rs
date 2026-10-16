//! `deposit(amount, min_shares)` (ARCH §3.3; ECON §9.2): a lender adds tokens to the pool.
//!
//! Checks: `amount > 0` (`ZeroAmount`); the mint guard on the entry path (`mint_unsafe` refuses
//! with `MintUnsafeFlagged`, then `check_mint_safety`); `minted > 0` (`ZeroAmount`); `minted ≥
//! min_shares` (`SharesBelowMin`); `S + minted ≤ u128::MAX` (`MathOverflow`); the pool is not
//! drained — with shares held and tokens lent out, more than 1/1,000 of the token claims is
//! unborrowed
//! (`PoolDrained`, AIR-07's residual: a deposit before a settlement that leaves dust would stop
//! the vest flush, `pool::VEST_FLUSH_RATIO`); while the pool has no shares — a live full-wipe
//! record (R4A-7) or, since AIR-08, a market every lender left — the sweeps since `S` reached 0
//! (`WipeSweepPending`).
//!
//! Effects, in order (ARCH §3.0 A16: prologue steps 1–5, then 10–11):
//! 1. the prologue (`engine::prologue`): the Accounts constraints; params-version sync; the
//!    mint guard; accrue (vest release, rent); the observation roll (a completed bucket is
//!    integrated; nothing is recorded, A16);
//! 2. a `LenderPosition` created by this instruction starts at the pool's current `acc`,
//!    `acc_r` and epoch (`pool::Lender::new`);
//! 3. `pool::deposit` (release, touch with the `ShareEpoch` records, mint `⌊amount·(S + VS)/(T +
//!    ΣB_Q + VA)⌋`, `T += amount`, `S += minted`; at a live wipe record it finalises the record
//!    and touches again);
//! 4. `transfer_checked(owner_token_account → pool_vault, amount)` signed by the owner, with
//!    the market's token program (SPL Token or Token-2022);
//! 5. post-conditions (step 10): `pool_vault.amount ≥ T` (a mint that takes a transfer fee
//!    would fail here; the filter rejects those anyway), `MarketVault` lamports ≥ rent +
//!    `lender_sol + protocol_fees`;
//! 6. event `Deposit`.
//!
//! Pausing opens never blocks deposits; only `mint_unsafe` does (ARCH §3.9). A lender whose
//! token account has `CpiGuard.lock_cpi` cannot deposit through our CPI (R-MINT §8.4).
//!
//! Remaining accounts: the `ShareEpoch` records `["share_epoch", market, e]` for the epochs
//! the lender is behind, in order (key-checked; `EpochRecordMismatch`); writable when the
//! lender converts through a rebase record, whose conversion remainder `conv_rem` the touch
//! writes (KI-65: `engine::lender::store_records`, else `EpochRecordReadOnly`).

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked};
use shortwin_math::pool;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION};
use crate::engine::guard::{mint_guard, MintPath};
use crate::engine::{lender, prologue};
use crate::errors::{MathResultExt, ShortwinError};
use crate::events::Deposit as DepositEvent;
use crate::state::{GlobalConfig, LenderPosition, Market, MarketVault, POOL_VAULT_SEED};

#[event_cpi]
#[derive(Accounts)]
pub struct Deposit<'info> {
    /// The lender; pays for its `LenderPosition` on first deposit.
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    #[account(
        mut,
        seeds = [Market::SEED, mint.key().as_ref()],
        bump = market.load()?.bump,
        constraint = market.load()?.version == MARKET_VERSION @ ShortwinError::ConfigVersion,
        has_one = mint @ ShortwinError::AccountMismatch,
        has_one = market_vault @ ShortwinError::AccountMismatch,
        has_one = pool_vault @ ShortwinError::AccountMismatch,
        has_one = token_program @ ShortwinError::AccountMismatch
    )]
    pub market: AccountLoader<'info, Market>,
    #[account(
        mut,
        seeds = [MarketVault::SEED, market.key().as_ref()],
        bump = market_vault.load()?.bump
    )]
    pub market_vault: AccountLoader<'info, MarketVault>,
    #[account(
        init_if_needed,
        payer = owner,
        space = LenderPosition::SPACE,
        seeds = [LenderPosition::SEED, market.key().as_ref(), owner.key().as_ref()],
        bump
    )]
    pub lender: Box<Account<'info, LenderPosition>>,
    #[account(mint::token_program = token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    /// The lender's source token account (its authority is checked by the token program).
    #[account(
        mut,
        token::mint = mint,
        token::token_program = token_program
    )]
    pub owner_token_account: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        seeds = [POOL_VAULT_SEED, market.key().as_ref()],
        bump = market.load()?.pool_vault_bump
    )]
    pub pool_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(
    ctx: Context<'info, Deposit<'info>>,
    amount: u64,
    min_shares: u128,
) -> Result<()> {
    require!(amount > 0, ShortwinError::ZeroAmount);
    let clock = Clock::get()?;
    let market_key = ctx.accounts.market.key();
    let owner_key = ctx.accounts.owner.key();

    // Steps 1–5 and the body; every account is stored only once every check passed.
    let (minted, t_after, s_after, after, tracked) = {
        let config = ctx.accounts.config.load()?;
        let mut market = ctx.accounts.market.load_mut()?;
        let mut vault_acc = ctx.accounts.market_vault.load_mut()?;
        let now = prologue::market_now(&market, clock.unix_timestamp);

        prologue::sync_market_params(&config, &mut market, now);
        mint_guard(
            &market,
            &ctx.accounts.mint.to_account_info(),
            ctx.accounts.token_program.key,
            MintPath::Entry,
        )?;
        let mut pool = market.pool();
        let mut vault = vault_acc.vault();
        prologue::accrue(&mut market, &mut pool, &mut vault, now)?;
        // A16 step 5: roll a completed bucket (no ReadSet, so no record and no chains).
        prologue::roll_only(&market_key, &mut market, now, clock.slot)?;

        let mut records = lender::load_records(&market_key, ctx.remaining_accounts, usize::MAX)?;
        let loaded = records.clone();
        let position = &mut ctx.accounts.lender;
        lender::init_if_new(position, &owner_key, &market_key, ctx.bumps.lender, &pool);
        let mut l = position.lender();
        let minted = pool::deposit(
            &mut pool,
            &mut vault,
            &mut l,
            &mut records,
            now,
            amount,
            lender::rewards_required(&market),
        )
        .math()?;
        require!(minted >= min_shares, ShortwinError::SharesBelowMin);

        lender::store_records(ctx.remaining_accounts, &loaded, &records)?;
        market.set_pool(&pool);
        vault_acc.set_vault(&vault);
        position.set_lender(&l);
        (
            minted,
            pool.tokens_free,
            pool.total_shares,
            l,
            lender::tracked(&vault)?,
        )
    };

    token_interface::transfer_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            TransferChecked {
                from: ctx.accounts.owner_token_account.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.pool_vault.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    // Step 10: post-conditions.
    ctx.accounts.pool_vault.reload()?;
    require_gte!(
        ctx.accounts.pool_vault.amount,
        t_after,
        ShortwinError::AccountMismatch
    );
    lender::check_vault_floor(&ctx.accounts.market_vault.to_account_info(), tracked)?;

    emit_cpi!(DepositEvent {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        owner: owner_key,
        amount,
        shares: minted,
        t_after,
        s_after,
        sol_paid: 0,
        pot_draw: 0,
        lock: after.lock,
        lock_until: after.lock_until,
        epoch: after.epoch,
    });
    Ok(())
}
