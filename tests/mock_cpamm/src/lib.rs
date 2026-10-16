//! **TEST-ONLY** constant-product venue for short.win's M2 position engine (ARCHITECTURE
//! §1.2, §8.2, §9 M2; A49). Never deployed: short.win only knows it behind its `test-venue`
//! feature (`VenueKind::Mock`), which release builds never enable.
//!
//! A pool of a base token (SPL Token or Token-2022) against WSOL (SPL Token), fee on the
//! input, the arithmetic of `shortwin-math` `quote::cpmm` with a zero creator fee
//! ([`math`]), plus the knobs M2's venue tests need: disable, fee, a state-layout change,
//! exact-out over-delivery and required trailing accounts (an interface change). The
//! interface — account orders, byte offsets, errors, pricing — is `INTERFACE.md` next to this
//! crate; it is binding for every consumer.
//!
//! Instructions: [`mock_cpamm::initialize_pool`], [`mock_cpamm::add_liquidity`],
//! [`mock_cpamm::swap_exact_in`], [`mock_cpamm::swap_exact_out`], [`mock_cpamm::set_knobs`]
//! and (an addition to the M2 interface, for LP-pull scenarios) [`mock_cpamm::remove_liquidity`].

#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unwrap_used
)]

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked};

pub mod errors;
pub mod math;
pub mod state;

use errors::MockError;
use state::{MockPool, PoolState, AUTHORITY_SEED, MAX_EXTRAS, POOL_LEN, POOL_SEED, VAULT_SEED};

declare_id!("BPAZFPkmVtYEWEZHCzFUns7oybh1VWfAvmu3ofM2mQi7");

/// WSOL (the SPL Token native mint): the only quote mint.
pub const WSOL_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");
/// SPL Token.
pub const TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// Token-2022.
pub const TOKEN_2022_PROGRAM_ID: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

/// Number of fixed swap accounts (INTERFACE.md "Swap accounts"); remaining accounts follow.
pub const SWAP_FIXED_ACCOUNTS: usize = 11;

#[program]
pub mod mock_cpamm {
    use super::*;

    /// Creates the pool `["pool", base_mint, WSOL]` and its two vaults; the signer becomes
    /// the pool admin.
    pub fn initialize_pool(ctx: Context<InitializePool>, fee_ppm: u32) -> Result<()> {
        require_gt!(math::FEE_DENOMINATOR, u64::from(fee_ppm), MockError::BadFee);
        let st = PoolState {
            layout_version: 0,
            disabled: false,
            fee_ppm,
            over_delivery: 0,
            protocol_fee_base: 0,
            protocol_fee_quote: 0,
            required_extras_len: 0,
            required_extras: [Pubkey::default(); MAX_EXTRAS],
            base_vault: ctx.accounts.base_vault.key(),
            quote_vault: ctx.accounts.quote_vault.key(),
            bump: ctx.bumps.pool,
            admin: ctx.accounts.admin.key(),
            base_mint: ctx.accounts.base_mint.key(),
            quote_mint: ctx.accounts.quote_mint.key(),
            base_token_program: ctx.accounts.base_token_program.key(),
            authority_bump: ctx.bumps.pool_authority,
            base_vault_bump: ctx.bumps.base_vault,
            quote_vault_bump: ctx.bumps.quote_vault,
        };
        let info = ctx.accounts.pool.to_account_info();
        let mut data = info.try_borrow_mut_data()?;
        st.encode(&mut data)
    }

    /// Anyone deposits `base` and `quote` into the vaults (no LP token: liquidity is a
    /// donation to the curve). Either amount may be zero, not both.
    pub fn add_liquidity(ctx: Context<AddLiquidity>, base: u64, quote: u64) -> Result<()> {
        require!(base > 0 || quote > 0, MockError::ZeroAmount);
        let st = load(&ctx.accounts.pool)?;
        let a = &ctx.accounts;
        require_keys_eq!(a.base_vault.key(), st.base_vault, MockError::VaultMismatch);
        require_keys_eq!(
            a.quote_vault.key(),
            st.quote_vault,
            MockError::VaultMismatch
        );
        require_keys_eq!(a.base_mint.key(), st.base_mint, MockError::MintMismatch);
        require_keys_eq!(a.quote_mint.key(), st.quote_mint, MockError::MintMismatch);
        if base > 0 {
            transfer(
                &a.base_token_program,
                &a.base_source,
                &a.base_mint,
                &a.base_vault,
                &a.depositor.to_account_info(),
                base,
                None,
            )?;
        }
        if quote > 0 {
            transfer(
                &a.quote_token_program,
                &a.quote_source,
                &a.quote_mint,
                &a.quote_vault,
                &a.depositor.to_account_info(),
                quote,
                None,
            )?;
        }
        Ok(())
    }

    /// The admin pulls up to the pricing reserves out of the vaults (LP-pull scenarios).
    pub fn remove_liquidity(ctx: Context<RemoveLiquidity>, base: u64, quote: u64) -> Result<()> {
        require!(base > 0 || quote > 0, MockError::ZeroAmount);
        let st = load(&ctx.accounts.pool)?;
        let a = &ctx.accounts;
        require_keys_eq!(a.admin.key(), st.admin, MockError::Unauthorized);
        require_keys_eq!(a.base_vault.key(), st.base_vault, MockError::VaultMismatch);
        require_keys_eq!(
            a.quote_vault.key(),
            st.quote_vault,
            MockError::VaultMismatch
        );
        require_keys_eq!(a.base_mint.key(), st.base_mint, MockError::MintMismatch);
        require_keys_eq!(a.quote_mint.key(), st.quote_mint, MockError::MintMismatch);
        let base_reserve = reserve(a.base_vault.amount, st.protocol_fee_base)?;
        let quote_reserve = reserve(a.quote_vault.amount, st.protocol_fee_quote)?;
        require_gte!(base_reserve, base, MockError::InsufficientLiquidity);
        require_gte!(quote_reserve, quote, MockError::InsufficientLiquidity);
        let bump = [st.authority_bump];
        let seeds: &[&[u8]] = &[AUTHORITY_SEED, &bump];
        if base > 0 {
            transfer(
                &a.base_token_program,
                &a.base_vault,
                &a.base_mint,
                &a.base_destination,
                &a.pool_authority.to_account_info(),
                base,
                Some(seeds),
            )?;
        }
        if quote > 0 {
            transfer(
                &a.quote_token_program,
                &a.quote_vault,
                &a.quote_mint,
                &a.quote_destination,
                &a.pool_authority.to_account_info(),
                quote,
                Some(seeds),
            )?;
        }
        Ok(())
    }

    /// Sells exactly `amount_in` of the input side for at least `min_out`.
    pub fn swap_exact_in<'info>(
        ctx: Context<'info, Swap<'info>>,
        amount_in: u64,
        min_out: u64,
    ) -> Result<()> {
        swap(ctx, SwapKind::ExactIn { amount_in, min_out })
    }

    /// Buys exactly `amount_out` of the output side (plus the pool's `over_delivery`) for at
    /// most `max_in`.
    pub fn swap_exact_out<'info>(
        ctx: Context<'info, Swap<'info>>,
        max_in: u64,
        amount_out: u64,
    ) -> Result<()> {
        swap(ctx, SwapKind::ExactOut { max_in, amount_out })
    }

    /// The admin sets every knob at once. A `layout_version` change moves the reserve
    /// counters to that version's slots (INTERFACE.md "Pool layout").
    pub fn set_knobs(
        ctx: Context<SetKnobs>,
        disabled: bool,
        fee_ppm: u32,
        layout_version: u8,
        over_delivery: u64,
        required_extras: Vec<Pubkey>,
    ) -> Result<()> {
        let mut st = load(&ctx.accounts.pool)?;
        require_keys_eq!(ctx.accounts.admin.key(), st.admin, MockError::Unauthorized);
        require_gt!(math::FEE_DENOMINATOR, u64::from(fee_ppm), MockError::BadFee);
        let _ = PoolState::counter_offsets(layout_version)?;
        require_gte!(MAX_EXTRAS, required_extras.len(), MockError::TooManyExtras);
        st.disabled = disabled;
        st.fee_ppm = fee_ppm;
        st.layout_version = layout_version;
        st.over_delivery = over_delivery;
        st.required_extras = [Pubkey::default(); MAX_EXTRAS];
        for (slot, k) in st.required_extras.iter_mut().zip(required_extras.iter()) {
            *slot = *k;
        }
        st.required_extras_len =
            u8::try_from(required_extras.len()).map_err(|_| MockError::TooManyExtras)?;
        store(&ctx.accounts.pool, &st)
    }
}

fn load(pool: &AccountLoader<MockPool>) -> Result<PoolState> {
    let info = pool.to_account_info();
    let data = info.try_borrow_data()?;
    PoolState::decode(&data)
}

fn store(pool: &AccountLoader<MockPool>, st: &PoolState) -> Result<()> {
    let info = pool.to_account_info();
    let mut data = info.try_borrow_mut_data()?;
    st.encode(&mut data)
}

/// Pricing reserve of one side: `vault − counter` (the venue fails when the vault is below
/// its counter, as CPMM's `InsufficientVault`).
fn reserve(vault: u64, counter: u64) -> Result<u64> {
    vault
        .checked_sub(counter)
        .ok_or_else(|| error!(MockError::InsufficientVault))
}

fn transfer<'info>(
    program: &Interface<'info, TokenInterface>,
    from: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    to: &InterfaceAccount<'info, TokenAccount>,
    authority: &AccountInfo<'info>,
    amount: u64,
    signer_seeds: Option<&[&[u8]]>,
) -> Result<()> {
    let accounts = TransferChecked {
        from: from.to_account_info(),
        mint: mint.to_account_info(),
        to: to.to_account_info(),
        authority: authority.clone(),
    };
    match signer_seeds {
        Some(seeds) => {
            let signer: &[&[&[u8]]] = &[seeds];
            token_interface::transfer_checked(
                CpiContext::new_with_signer(program.key(), accounts, signer),
                amount,
                mint.decimals,
            )
        }
        None => token_interface::transfer_checked(
            CpiContext::new(program.key(), accounts),
            amount,
            mint.decimals,
        ),
    }
}

#[derive(Clone, Copy)]
enum SwapKind {
    ExactIn { amount_in: u64, min_out: u64 },
    ExactOut { max_in: u64, amount_out: u64 },
}

fn swap<'info>(ctx: Context<'info, Swap<'info>>, kind: SwapKind) -> Result<()> {
    let a = &ctx.accounts;
    let mut st = load(&a.pool)?;
    require!(!st.disabled, MockError::PoolDisabled);

    // Direction: the input vault names the side sold into the pool.
    let base_in = a.input_vault.key() == st.base_vault;
    let (want_in, want_out) = if base_in {
        (st.base_vault, st.quote_vault)
    } else {
        (st.quote_vault, st.base_vault)
    };
    require_keys_eq!(a.input_vault.key(), want_in, MockError::VaultMismatch);
    require_keys_eq!(a.output_vault.key(), want_out, MockError::VaultMismatch);
    require_keys_eq!(
        a.input_mint.key(),
        a.input_vault.mint,
        MockError::MintMismatch
    );
    require_keys_eq!(
        a.output_mint.key(),
        a.output_vault.mint,
        MockError::MintMismatch
    );
    require_keys_neq!(a.source.key(), want_in, MockError::VaultAsUserAccount);
    require_keys_neq!(a.source.key(), want_out, MockError::VaultAsUserAccount);
    require_keys_neq!(a.destination.key(), want_in, MockError::VaultAsUserAccount);
    require_keys_neq!(a.destination.key(), want_out, MockError::VaultAsUserAccount);

    // A venue interface change: the trailing accounts must be exactly the configured list.
    let extras = st.extras();
    require_eq!(
        ctx.remaining_accounts.len(),
        extras.len(),
        MockError::ExtrasMismatch
    );
    for (acc, want) in ctx.remaining_accounts.iter().zip(extras) {
        require_keys_eq!(acc.key(), *want, MockError::ExtrasMismatch);
    }

    let (in_counter, out_counter) = if base_in {
        (st.protocol_fee_base, st.protocol_fee_quote)
    } else {
        (st.protocol_fee_quote, st.protocol_fee_base)
    };
    let reserve_in = reserve(a.input_vault.amount, in_counter)?;
    let reserve_out = reserve(a.output_vault.amount, out_counter)?;

    let (s, delivered) = match kind {
        SwapKind::ExactIn { amount_in, min_out } => {
            let s = math::exact_in(amount_in, reserve_in, reserve_out, st.fee_ppm)?;
            require_gte!(s.amount_out, min_out, MockError::SlippageMinOut);
            (s, s.amount_out)
        }
        SwapKind::ExactOut { max_in, amount_out } => {
            let s = math::exact_out(amount_out, reserve_in, reserve_out, st.fee_ppm)?;
            require_gte!(max_in, s.amount_in, MockError::SlippageMaxIn);
            let delivered = amount_out
                .checked_add(st.over_delivery)
                .ok_or(MockError::MathOverflow)?;
            require_gt!(reserve_out, delivered, MockError::InsufficientLiquidity);
            (s, delivered)
        }
    };

    // Delegate-aware source: the authority owns it or is its delegate for the whole input
    // (the token program enforces the same; this names the failure).
    let src = &a.source;
    let authority = a.authority.key();
    let is_owner = src.owner == authority;
    let is_delegate = Option::<Pubkey>::from(src.delegate) == Some(authority)
        && src.delegated_amount >= s.amount_in;
    require!(is_owner || is_delegate, MockError::SourceAuthority);

    transfer(
        &a.input_token_program,
        &a.source,
        &a.input_mint,
        &a.input_vault,
        &a.authority.to_account_info(),
        s.amount_in,
        None,
    )?;
    let bump = [st.authority_bump];
    let seeds: &[&[u8]] = &[AUTHORITY_SEED, &bump];
    transfer(
        &a.output_token_program,
        &a.output_vault,
        &a.output_mint,
        &a.destination,
        &a.pool_authority.to_account_info(),
        delivered,
        Some(seeds),
    )?;

    let counter = if base_in {
        &mut st.protocol_fee_base
    } else {
        &mut st.protocol_fee_quote
    };
    *counter = counter
        .checked_add(s.protocol_fee)
        .ok_or(MockError::MathOverflow)?;
    store(&a.pool, &st)?;
    msg!(
        "mock_cpamm swap: in {} out {} fee {} protocol {}",
        s.amount_in,
        delivered,
        s.trade_fee,
        s.protocol_fee
    );
    Ok(())
}

#[derive(Accounts)]
pub struct InitializePool<'info> {
    /// Pays for the pool and vaults; becomes the pool admin.
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(mint::token_program = base_token_program)]
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        address = WSOL_MINT @ MockError::QuoteNotWsol,
        constraint = quote_mint.key() != base_mint.key() @ MockError::SameMint,
        mint::token_program = quote_token_program
    )]
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = admin,
        space = POOL_LEN,
        seeds = [POOL_SEED, base_mint.key().as_ref(), quote_mint.key().as_ref()],
        bump
    )]
    pub pool: AccountLoader<'info, MockPool>,
    /// CHECK: the global PDA `["authority"]` that owns every vault; holds nothing.
    #[account(seeds = [AUTHORITY_SEED], bump)]
    pub pool_authority: UncheckedAccount<'info>,
    #[account(
        init,
        payer = admin,
        seeds = [VAULT_SEED, pool.key().as_ref(), base_mint.key().as_ref()],
        bump,
        token::mint = base_mint,
        token::authority = pool_authority,
        token::token_program = base_token_program
    )]
    pub base_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init,
        payer = admin,
        seeds = [VAULT_SEED, pool.key().as_ref(), quote_mint.key().as_ref()],
        bump,
        token::mint = quote_mint,
        token::authority = pool_authority,
        token::token_program = quote_token_program
    )]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub base_token_program: Interface<'info, TokenInterface>,
    #[account(address = TOKEN_PROGRAM_ID @ MockError::TokenProgramMismatch)]
    pub quote_token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AddLiquidity<'info> {
    /// Owner (or delegate) of both source accounts.
    pub depositor: Signer<'info>,
    pub pool: AccountLoader<'info, MockPool>,
    #[account(mut, token::mint = base_mint, token::token_program = base_token_program)]
    pub base_source: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = quote_mint, token::token_program = quote_token_program)]
    pub quote_source: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub base_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mint::token_program = base_token_program)]
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = quote_token_program)]
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    pub base_token_program: Interface<'info, TokenInterface>,
    pub quote_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct RemoveLiquidity<'info> {
    pub admin: Signer<'info>,
    pub pool: AccountLoader<'info, MockPool>,
    /// CHECK: the global PDA `["authority"]` (bump from the pool's record).
    #[account(seeds = [AUTHORITY_SEED], bump = pool.load()?.authority_bump())]
    pub pool_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub base_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = base_mint, token::token_program = base_token_program)]
    pub base_destination: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = quote_mint, token::token_program = quote_token_program)]
    pub quote_destination: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mint::token_program = base_token_program)]
    pub base_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = quote_token_program)]
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    pub base_token_program: Interface<'info, TokenInterface>,
    pub quote_token_program: Interface<'info, TokenInterface>,
}

/// Swap accounts; the order is binding (INTERFACE.md "Swap accounts"). Remaining accounts
/// must equal the pool's `required_extras` exactly.
#[derive(Accounts)]
pub struct Swap<'info> {
    /// 0: owner **or delegate** of `source`.
    pub authority: Signer<'info>,
    /// 1
    #[account(mut)]
    pub pool: AccountLoader<'info, MockPool>,
    /// CHECK: 2: the global PDA `["authority"]`, signs the output transfer (bump from the
    /// pool's record).
    #[account(seeds = [AUTHORITY_SEED], bump = pool.load()?.authority_bump())]
    pub pool_authority: UncheckedAccount<'info>,
    /// 3: the trader's input token account (any owner; see `authority`).
    #[account(
        mut,
        token::mint = input_mint,
        token::token_program = input_token_program
    )]
    pub source: Box<InterfaceAccount<'info, TokenAccount>>,
    /// 4: receives the output (its owner is not checked, as on CPMM and AMM v4).
    #[account(
        mut,
        token::mint = output_mint,
        token::token_program = output_token_program
    )]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
    /// 5: the pool's vault of the input side (base vault ⇒ base in).
    #[account(mut)]
    pub input_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// 6: the pool's other vault.
    #[account(mut)]
    pub output_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// 7
    #[account(mint::token_program = input_token_program)]
    pub input_mint: Box<InterfaceAccount<'info, Mint>>,
    /// 8
    #[account(mint::token_program = output_token_program)]
    pub output_mint: Box<InterfaceAccount<'info, Mint>>,
    /// 9
    pub input_token_program: Interface<'info, TokenInterface>,
    /// 10
    pub output_token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct SetKnobs<'info> {
    pub admin: Signer<'info>,
    #[account(mut)]
    pub pool: AccountLoader<'info, MockPool>,
}
