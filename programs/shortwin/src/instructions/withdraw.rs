//! `withdraw(shares, min_tokens, min_sol)` (ARCH §3.3; ECON §9.3): pro-rata withdrawal with
//! locks, plus the SOL part.
//!
//! Checks (C0-9, C0-13), all `pool::withdraw`'s: `pending ≥ 0` after the touch
//! (`NegativePending`: `cover_deficit` first); the lender on the current share epoch and `0 <
//! shares ≤ lender.shares` (`InsufficientShares`); `tokens = ⌊shares·(T + ΣB_Q + VA)/(S + VS)⌋
//! ≤ T` (`InsufficientFreeInventory`) and `≤` the pro-rata allowance (`AboveProRataAllowance`,
//! with `cal_advance`, the lender's lock `lock_i(now)` and the aggregate `locked` of the lock
//! expiry calendar); then `tokens ≥ min_tokens` (`TokensBelowMin`). The mint guard runs on the
//! exit path: it never reverts on the filter; a mint that fails it now logs
//! `MintSafetyTripped` and the withdrawal goes ahead [R-MINT §6, §9.4].
//!
//! Program checks on top of `pool::withdraw` (the math crate stays `pool_model` v0.4.2):
//! - **the last shares stay while anything is borrowed** (ARCH §9 M0b carried-over note 4,
//!   implemented after the M1 review, M1-SEC-3): a withdrawal that would leave `S = 0` while
//!   `ΣB_Q > 0` is refused (`LastSharesBorrowed`). `S = 0 ∧ ΣB_Q > 0` would send every later
//!   rent credit to the protocol sink before any SOL exists for it, which underflows
//!   `lender_sol` and bricks every `Market`-writing instruction. The last lender can still
//!   withdraw all but its last share (whatever its allowance allows);
//! - **`pay ≥ min_sol`** (`SolBelowMin`, M1-SEC-2): the caller bounds the SOL part as
//!   `min_tokens` bounds the tokens. The last lender's full exit is paid the unreleased vest
//!   (R3B-2); a dust deposit landed just before it would make it "not last" and hand the vest to
//!   the depositor, so the frontend sets `min_sol` to the payout it quoted (flush included) and
//!   the front-run reverts instead.
//!
//! Effects, in order (ARCH §3.0 A16: prologue steps 1–5, then 10–11):
//! 1. params-version sync, the mint guard (exit), accrue (vest release, rent);
//! 2. `pool::withdraw`: burn `shares`, `T −= tokens`; the lock update (the borrowed part of
//!    the withdrawn fraction becomes a lock, `lock_until = ⌈(max(now, max_release) +
//!    lock_grace_s)/1 d⌉·1 d` when it grows, the live term moves between calendar buckets);
//!    the last lender (`shares == S`) is first paid the unreleased vest (R3B-2). Since KI-65
//!    (no rebase orphans) `shares == S` holds exactly when no other position holds or will
//!    hold a share of the epoch: a lender that has not converted through a rebase record yet
//!    still counts in `S`, so a JIT depositor never takes the vest while a holder remains
//!    (R4B-4);
//! 3. the SOL part exactly as `claim_sol` (`pool::claim`: pot draw up to the own deficit, pay
//!    `min(pending, real − claimed, lender_sol)`);
//! 4. **memo** (ARCH §3.3, R-MINT §8.4): if the destination is a Token-2022 account with
//!    `MemoTransfer.require_incoming_transfer_memos`, the optional `memo_program` (spl-memo,
//!    `MemoSq4g…`) is CPI'd first (`MemoProgramRequired` without it), so the transfer's
//!    previous sibling instruction is a memo. T-MEMO (U17) is verified on the mainnet build:
//!    Token-2022 slot 427,147,035, sha `7ea94a02…` = program@v11.0.0 (OtterSec `9bc02757`),
//!    spl-memo v3 `f520eaf0…` (`tests/litesvm/tests/security.rs`); the mainnet drill is M11's;
//! 5. `transfer_checked(pool_vault → owner_token_account, tokens)` signed by the `market` PDA
//!    (the vault's authority), with the market's token program;
//! 6. the SOL, a direct lamport debit of `market_vault` (never below rent + `lender_sol +
//!    protocol_fees`, I2);
//! 7. post-conditions: `pool_vault.amount ≥ T`, the vault floor;
//! 8. `lender` is closed to `owner` when `pool::closable` holds (current, `shares == 0 ∧ pending
//!    == 0 ∧ lock == 0`, no forward chain: always so for a current lender; KI-67, as `claim_sol`);
//! 9. events: the `Withdraw` `emit_cpi!` (one per instruction, ARCH §3.0 step 11); a mint failing
//!    the filter is a `MintSafetyTripped` **log** event (`emit!`, no trace entry, like
//!    `CoverageGap`), which reconciles ARCH §3.0 steps 3 and 11 (M1-SEC-11 / M1-FID-10).
//!
//! Works in every venue state (no venue accounts); no cooldown (ADOPTED Q22). Pausing opens
//! never blocks it.
//!
//! Remaining accounts: the `ShareEpoch` records the lender is behind on (key-checked);
//! writable when the lender converts through a rebase record, whose conversion remainder
//! `conv_rem` the touch writes (KI-65: `engine::lender::store_records`, else
//! `EpochRecordReadOnly`). A lender behind a rebase record converts to `⌊s/den⌋` shares or
//! one more, depending on who converted through the record before it: a client that quoted
//! the conversion re-reads the records when `InsufficientShares` / `SolBelowMin` refuse it.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::solana_program::program::invoke;
use anchor_spl::token_2022::spl_token_2022 as t22;
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked};
use shortwin_math::pool;
use t22::extension::memo_transfer::memo_required;
use t22::extension::StateWithExtensions;

use crate::constants::{CONFIG_VERSION, MARKET_VERSION, MEMO_PROGRAM_ID, TOKEN_2022_PROGRAM_ID};
use crate::engine::guard::{mint_guard, MintPath};
use crate::engine::{lender, prologue};
use crate::errors::{MathResultExt, ShortwinError};
use crate::events::{MintSafetyTripped, Withdraw as WithdrawEvent};
use crate::state::{GlobalConfig, LenderPosition, Market, MarketVault, POOL_VAULT_SEED};

#[event_cpi]
#[derive(Accounts)]
pub struct Withdraw<'info> {
    /// The lender; receives the SOL part and the `LenderPosition` rent when it closes.
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
        mut,
        seeds = [LenderPosition::SEED, market.key().as_ref(), owner.key().as_ref()],
        bump = lender.bump,
        has_one = owner @ ShortwinError::Unauthorized,
        has_one = market @ ShortwinError::AccountMismatch
    )]
    pub lender: Box<Account<'info, LenderPosition>>,
    #[account(mint::token_program = token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    /// The destination token account (any account of this mint the owner chooses).
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
    /// CHECK: address-checked; only invoked (with no accounts) when the destination requires
    /// memos. spl-memo v3/v4.
    /// The memo program, for a Token-2022 destination with `MemoTransfer` only (else pass the
    /// program id, Anchor's "none").
    #[account(address = MEMO_PROGRAM_ID @ ShortwinError::AccountMismatch)]
    pub memo_program: Option<UncheckedAccount<'info>>,
}

/// The memo `withdraw` writes before a transfer into a memo-required account.
pub const WITHDRAW_MEMO: &[u8] = b"short.win withdraw";

/// `true` if `destination` is a Token-2022 account that requires incoming-transfer memos.
/// A legacy SPL Token account has no extensions; an unparsable one cannot be a valid
/// destination (Anchor already deserialized it as a token account), so it counts as "no".
pub fn requires_memo(destination: &AccountInfo) -> Result<bool> {
    if *destination.owner != TOKEN_2022_PROGRAM_ID {
        return Ok(false);
    }
    let data = destination.try_borrow_data()?;
    Ok(StateWithExtensions::<t22::state::Account>::unpack(&data)
        .map(|state| memo_required(&state))
        .unwrap_or(false))
}

pub fn handler<'info>(
    ctx: Context<'info, Withdraw<'info>>,
    shares: u128,
    min_tokens: u64,
    min_sol: u64,
) -> Result<()> {
    let clock = Clock::get()?;
    let market_key = ctx.accounts.market.key();
    let owner_key = ctx.accounts.owner.key();
    let mint_key = ctx.accounts.mint.key();

    // Steps 1–5 and the pool body; every account is stored only once every check passed.
    let (tokens, pay, pot_draw, t_after, s_after, after, tracked, market_bump, filter_ok, closes) = {
        let config = ctx.accounts.config.load()?;
        let mut market = ctx.accounts.market.load_mut()?;
        let mut vault_acc = ctx.accounts.market_vault.load_mut()?;
        let now = prologue::market_now(&market, clock.unix_timestamp);

        prologue::sync_market_params(&config, &mut market, now);
        let filter_ok = mint_guard(
            &market,
            &ctx.accounts.mint.to_account_info(),
            ctx.accounts.token_program.key,
            MintPath::Exit,
        )?;
        let mut pool = market.pool();
        let mut vault = vault_acc.vault();
        prologue::accrue(&mut market, &mut pool, &mut vault, now)?;
        // A16 step 5: roll a completed bucket (no ReadSet, so no record and no chains).
        prologue::roll_only(&market_key, &mut market, now, clock.slot)?;

        let mut records = lender::load_records(&market_key, ctx.remaining_accounts, usize::MAX)?;
        let loaded = records.clone();
        let position = &mut ctx.accounts.lender;
        let mut l = position.lender();
        let tokens = pool::withdraw(
            &mut pool,
            &mut vault,
            &mut l,
            &mut records,
            now,
            config.params.lock_grace_s,
            shares,
        )
        .math()?;
        require_gte!(tokens, min_tokens, ShortwinError::TokensBelowMin);
        // ARCH §9 M0b note 4 (M1-SEC-3): never `S = 0` while tokens are borrowed.
        require!(
            pool.total_shares != 0 || pool.borrowed_q == 0,
            ShortwinError::LastSharesBorrowed
        );
        // The SOL part, exactly as `claim_sol` (the lender is current: records unused).
        let touched = l;
        let pay = pool::claim(&pool, &mut vault, &mut l, &mut records, now).math()?;
        require_gte!(pay, min_sol, ShortwinError::SolBelowMin);

        lender::store_records(ctx.remaining_accounts, &loaded, &records)?;
        market.set_pool(&pool);
        vault_acc.set_vault(&vault);
        position.set_lender(&l);
        (
            tokens,
            pay,
            lender::pot_draw(&touched, &l),
            pool.tokens_free,
            pool.total_shares,
            l,
            lender::tracked(&vault)?,
            market.bump,
            filter_ok,
            pool::closable(&pool, &l),
        )
    };

    if tokens > 0 {
        let destination = ctx.accounts.owner_token_account.to_account_info();
        if requires_memo(&destination)? {
            let memo = ctx
                .accounts
                .memo_program
                .as_ref()
                .ok_or_else(|| error!(ShortwinError::MemoProgramRequired))?;
            invoke(
                &Instruction {
                    program_id: MEMO_PROGRAM_ID,
                    accounts: Vec::new(),
                    data: WITHDRAW_MEMO.to_vec(),
                },
                &[memo.to_account_info()],
            )?;
        }
        let seeds: &[&[u8]] = &[Market::SEED, mint_key.as_ref(), &[market_bump]];
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.pool_vault.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.owner_token_account.to_account_info(),
                    authority: ctx.accounts.market.to_account_info(),
                },
                &[seeds],
            ),
            tokens,
            ctx.accounts.mint.decimals,
        )?;
    }
    let vault_info = ctx.accounts.market_vault.to_account_info();
    lender::pay_sol(
        &vault_info,
        &ctx.accounts.owner.to_account_info(),
        pay,
        tracked,
    )?;

    // Step 10: post-conditions.
    ctx.accounts.pool_vault.reload()?;
    require_gte!(
        ctx.accounts.pool_vault.amount,
        t_after,
        ShortwinError::AccountMismatch
    );
    lender::check_vault_floor(&vault_info, tracked)?;

    if closes {
        ctx.accounts
            .lender
            .close(ctx.accounts.owner.to_account_info())?;
    }

    if !filter_ok {
        // A log event: the one `emit_cpi!` of this instruction is `Withdraw` (ARCH §3.0 step
        // 11; M1-SEC-11 / M1-FID-10).
        emit!(MintSafetyTripped {
            market: market_key,
            slot: clock.slot,
            ts: clock.unix_timestamp,
            mint: mint_key,
        });
    }
    emit_cpi!(WithdrawEvent {
        market: market_key,
        slot: clock.slot,
        ts: clock.unix_timestamp,
        owner: owner_key,
        amount: tokens,
        shares,
        t_after,
        s_after,
        sol_paid: pay,
        pot_draw,
        lock: after.lock,
        lock_until: after.lock_until,
        epoch: after.epoch,
    });
    Ok(())
}
