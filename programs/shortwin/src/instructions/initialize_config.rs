//! `initialize_config(args)` (ARCH §3.1): once, by short.win's **upgrade authority**.
//!
//! Creates `GlobalConfig`, `VenueGuard`, the four `VenueExtras` (empty; used from M2, O6) and
//! `OracleCache`, and writes the authorities, parameters, tiers, EMA and kink parameters, the
//! timelock and `venue_guard.guard_slot` from the arguments. The signer must be the upgrade
//! authority recorded in short.win's own ProgramData, which prevents a front-run
//! initialisation. `timelock_s ≥ MIN_TIMELOCK_S`; the parameters pass `params::validate` and
//! `params::validate_econ`.
//!
//! No event (none in ARCH §3.10; with the two event accounts the transaction would exceed the
//! 1,232-byte legacy limit).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;

use crate::constants::{
    CONFIG_VERSION, VENUE_BITS_ENABLEABLE, VENUE_EXTRAS_VERSION, VENUE_GUARD_VERSION,
};
use crate::errors::ShortwinError;
use crate::state::{
    check_authorities, check_timelock, param_set, validate_param_set, GlobalConfig, InitConfigArgs,
    OracleCache, VenueExtras, VenueGuard, VenueKind,
};

/// `VenueExtras` seed byte of each CPI venue kind.
const EXTRAS_PUMP_CURVE: &[u8] = &[VenueKind::PumpCurve as u8];
const EXTRAS_PUMP_SWAP: &[u8] = &[VenueKind::PumpSwap as u8];
const EXTRAS_RAYDIUM_V4: &[u8] = &[VenueKind::RaydiumV4 as u8];
const EXTRAS_RAYDIUM_CPMM: &[u8] = &[VenueKind::RaydiumCpmm as u8];

#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    /// short.win's upgrade authority (checked against `program_data`); pays for the accounts.
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = GlobalConfig::SPACE,
        seeds = [GlobalConfig::SEED],
        bump
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    #[account(
        init,
        payer = authority,
        space = VenueGuard::SPACE,
        seeds = [VenueGuard::SEED],
        bump
    )]
    pub venue_guard: AccountLoader<'info, VenueGuard>,
    #[account(
        init,
        payer = authority,
        space = VenueExtras::SPACE,
        seeds = [VenueExtras::SEED, EXTRAS_PUMP_CURVE],
        bump
    )]
    pub venue_extras_pump_curve: AccountLoader<'info, VenueExtras>,
    #[account(
        init,
        payer = authority,
        space = VenueExtras::SPACE,
        seeds = [VenueExtras::SEED, EXTRAS_PUMP_SWAP],
        bump
    )]
    pub venue_extras_pump_swap: AccountLoader<'info, VenueExtras>,
    #[account(
        init,
        payer = authority,
        space = VenueExtras::SPACE,
        seeds = [VenueExtras::SEED, EXTRAS_RAYDIUM_V4],
        bump
    )]
    pub venue_extras_raydium_v4: AccountLoader<'info, VenueExtras>,
    #[account(
        init,
        payer = authority,
        space = VenueExtras::SPACE,
        seeds = [VenueExtras::SEED, EXTRAS_RAYDIUM_CPMM],
        bump
    )]
    pub venue_extras_raydium_cpmm: AccountLoader<'info, VenueExtras>,
    #[account(
        init,
        payer = authority,
        space = OracleCache::SPACE,
        seeds = [OracleCache::SEED],
        bump
    )]
    pub oracle_cache: Box<Account<'info, OracleCache>>,
    /// short.win's own ProgramData (`PDA(BPFLoaderUpgradeable, [program id])`); its upgrade
    /// authority must be the signer.
    #[account(
        seeds = [crate::ID.as_ref()],
        bump,
        seeds::program = bpf_loader_upgradeable::ID,
        constraint = program_data.upgrade_authority_address == Some(authority.key())
            @ ShortwinError::Unauthorized
    )]
    pub program_data: Box<Account<'info, ProgramData>>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<InitializeConfig>, args: InitConfigArgs) -> Result<()> {
    check_timelock(args.timelock_s)?;
    check_authorities(
        &args.governance,
        &args.pause_authority,
        &args.coverage_authority,
        &args.fee_recipient,
    )?;
    require_keys_neq!(
        args.pyth_sol_usd,
        Pubkey::default(),
        ShortwinError::InvalidParams
    );
    // Known bits only, and none whose lifecycle this build cannot finish (M5-SEC-4:
    // `constants::VENUE_BITS_UNFINISHED`; the pump curve in M5, empty since M6's `→ PumpSwap`).
    require!(
        args.venue_enabled & !VENUE_BITS_ENABLEABLE == 0,
        ShortwinError::InvalidParams
    );
    validate_param_set(&param_set(
        &args.params,
        &args.tiers,
        &args.ema,
        args.util_kink_bps,
    )?)?;

    {
        let mut c = ctx.accounts.config.load_init()?;
        c.version = CONFIG_VERSION;
        c.bump = ctx.bumps.config;
        c.flags = 0;
        c.venue_enabled = args.venue_enabled;
        c.venue_route_broken = 0;
        c.timelock_s = args.timelock_s;
        c.params_version = 1;
        c.governance = args.governance;
        c.pause_authority = args.pause_authority;
        c.coverage_authority = args.coverage_authority;
        c.fee_recipient = args.fee_recipient;
        c.pyth_sol_usd = args.pyth_sol_usd;
        c.pyth_feed_id = args.pyth_feed_id;
        c.params = args.params;
        c.tiers = args.tiers;
        c.ema_params.set_values(&args.ema);
        c.ema_params.version = 1;
        c.ema_params.force_at = 0;
        c.kink.util_kink_bps = args.util_kink_bps;
        c.kink.version = 1;
        c.pending.clear();
    }
    {
        let mut g = ctx.accounts.venue_guard.load_init()?;
        g.version = VENUE_GUARD_VERSION;
        g.bump = ctx.bumps.venue_guard;
        g.upgraded = 0;
        g.guard_slot = args.guard_slot;
    }
    let extras = [
        (
            &ctx.accounts.venue_extras_pump_curve,
            ctx.bumps.venue_extras_pump_curve,
            VenueKind::PumpCurve,
        ),
        (
            &ctx.accounts.venue_extras_pump_swap,
            ctx.bumps.venue_extras_pump_swap,
            VenueKind::PumpSwap,
        ),
        (
            &ctx.accounts.venue_extras_raydium_v4,
            ctx.bumps.venue_extras_raydium_v4,
            VenueKind::RaydiumV4,
        ),
        (
            &ctx.accounts.venue_extras_raydium_cpmm,
            ctx.bumps.venue_extras_raydium_cpmm,
            VenueKind::RaydiumCpmm,
        ),
    ];
    for (account, bump, kind) in extras {
        let mut e = account.load_init()?;
        e.version = VENUE_EXTRAS_VERSION;
        e.bump = bump;
        e.venue = kind as u8;
    }
    let oracle = &mut ctx.accounts.oracle_cache;
    oracle.set_inner(OracleCache {
        bump: ctx.bumps.oracle_cache,
        ..OracleCache::default()
    });
    Ok(())
}
