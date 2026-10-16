//! `GlobalConfig` (zero-copy, seeds `["config"]`, 1,448 B) and the parameter types (ARCH
//! §2.3.2, §2.3.3; ECON §15).
//!
//! Only governance writes the parameters, after the timelock (`propose_config` →
//! `apply_config`); Emergency writes the flags. `GlobalConfig` is never written on a hot path
//! or by a permissionless instruction other than `apply_config` (which only executes a
//! governance proposal whose timelock has run), and every instruction reads it.
//!
//! Layout (offsets include the 8-byte discriminator; asserted in `layout_asserts.rs`):
//!
//! | Offset | Bytes | Fields |
//! |---|---|---|
//! | 8 | 24 | `version` u16, `bump` u8, `flags` u8, `venue_enabled` u32, `venue_route_broken` u32, `timelock_s` u32, `params_version` u32, `_pad0` [u8;4] |
//! | 32 | 128 | `governance`, `pause_authority`, `coverage_authority`, `fee_recipient` |
//! | 160 | 64 | `pyth_sol_usd`, `pyth_feed_id` [u8;32] |
//! | 224 | 264 | `params` [`Params`] |
//! | 488 | 192 | `tiers` [`TierParams`; 4] (A, B, C, D) |
//! | 680 | 32 | `ema_params` [`EmaConfig`] |
//! | 712 | 8 | `kink` [`KinkConfig`] |
//! | 720 | 472 | `pending` [`PendingChange`] |
//! | 1,192 | 256 | `_reserved` |
//!
//! The parameter values are the math crate's (`shortwin_math::params`); these types are only
//! their account encoding. [`param_set`] converts back, and `params::validate` +
//! `params::validate_econ` decide validity (never restated here).

use anchor_lang::prelude::*;
use shortwin_math::observe::EmaParams as MathEmaParams;
use shortwin_math::params::{self as mparams, ParamSet};
use shortwin_math::rent::RentTerms;
use shortwin_math::tier::TierParams as MathTierParams;

use crate::constants::{MAX_TIMELOCK_S, MIN_TIMELOCK_S, VENUE_BITS_ENABLEABLE};
use crate::errors::ShortwinError;

/// `GlobalConfig.flags` bit 0: new opens paused on every market (SPEC §11).
pub const CONFIG_FLAG_PAUSED_OPENS: u8 = 1 << 0;

/// Size of [`PendingChange::payload`]: the largest [`ConfigChange`] variant (`Params`,
/// 264 + 4·48 bytes) without its Borsh tag. (Account field types spell array lengths as
/// literals for the IDL; `layout_asserts.rs` ties them to these constants.)
pub const PENDING_PAYLOAD_LEN: usize = 456;

/// The global configuration (ARCH §2.3.2).
#[account(zero_copy)]
pub struct GlobalConfig {
    /// Layout version ([`crate::constants::CONFIG_VERSION`]).
    pub version: u16,
    /// PDA bump of `["config"]`.
    pub bump: u8,
    /// Bit 0 [`CONFIG_FLAG_PAUSED_OPENS`].
    pub flags: u8,
    /// Venues enabled for opens (`VENUE_BIT_*`). Enabling is timelocked (`EnableVenues`),
    /// disabling immediate (`disable_venue`); affects opens only.
    pub venue_enabled: u32,
    /// Route-broken venues (same bits; Emergency, O6 — built in M2). Opens and S3 evidence
    /// only (A40).
    pub venue_route_broken: u32,
    /// Governance timelock, seconds (`MIN_TIMELOCK_S ≤ timelock_s ≤ MAX_TIMELOCK_S`).
    pub timelock_s: u32,
    /// Incremented by every applied `Params` change.
    pub params_version: u32,
    pub _pad0: [u8; 4],
    /// Squads Governance vault (48 h Squads time lock).
    pub governance: Pubkey,
    /// Squads Emergency vault (time lock 0).
    pub pause_authority: Pubkey,
    /// Keeper key that may only set/clear a market's `paused_coverage` (`set_coverage_pause`;
    /// A69, O17). `Pubkey::default()` = the role disabled (the `initialize_config` default);
    /// governance installs or rotates it with a timelocked `Authorities` change.
    pub coverage_authority: Pubkey,
    /// Destination of `sweep_protocol_fees`.
    pub fee_recipient: Pubkey,
    /// Pyth `PriceUpdateV2` account read by `crank_sol_usd`.
    pub pyth_sol_usd: Pubkey,
    /// Pyth feed id of SOL/USD.
    pub pyth_feed_id: [u8; 32],
    /// Global parameters (scopes G and G→P).
    pub params: Params,
    /// Tier parameters A, B, C, D.
    pub tiers: [TierParams; 4],
    /// Market-level EMA parameters with their version and forced-adoption time (A17, A59).
    pub ema_params: EmaConfig,
    /// Market-level rent-curve kink with its own version (R3A-9).
    pub kink: KinkConfig,
    /// The pending governance change (at most one).
    pub pending: PendingChange,
    pub _reserved: [u8; 256],
}

impl GlobalConfig {
    /// PDA seed.
    pub const SEED: &'static [u8] = b"config";
    /// Account size including the discriminator.
    pub const SPACE: usize = 8 + core::mem::size_of::<Self>();

    /// `true` while new opens are paused globally.
    pub fn paused_opens(&self) -> bool {
        self.flags & CONFIG_FLAG_PAUSED_OPENS != 0
    }

    /// `set_pause` / `cancel_config` signer check: the Emergency or the Governance vault.
    pub fn is_pause_or_governance(&self, key: &Pubkey) -> bool {
        *key == self.pause_authority || *key == self.governance
    }

    /// `true` once governance has installed a `coverage_authority` (A69; O17): until then
    /// `set_coverage_pause` is inert (`CoverageAuthorityDisabled`).
    pub fn coverage_enabled(&self) -> bool {
        self.coverage_authority != Pubkey::default()
    }

    /// `set_coverage_pause` signer check: the configured `coverage_authority`, never the default
    /// key (so an unset role matches no signer).
    pub fn is_coverage_authority(&self, key: &Pubkey) -> bool {
        self.coverage_enabled() && *key == self.coverage_authority
    }

    /// `set_coverage_pause`'s Emergency clear (★OWNER-M7-3 (b), owner decision of 2026-10-01;
    /// ARCH §3.1): `pause_authority` (the Emergency vault) **clearing** the flag — accepted whether
    /// or not the coverage role is installed, so a flag a lost, rotated or disabled role left set,
    /// or one a compromised coverage key keeps setting, is cleared at once. Never a set: the
    /// Emergency pauses a market's opens through `set_pause`, never through this flag.
    pub fn is_emergency_coverage_clear(&self, key: &Pubkey, paused: bool) -> bool {
        !paused && self.pause_authority != Pubkey::default() && *key == self.pause_authority
    }

    /// The whole parameter set as the math crate sees it (for validation and for readers).
    pub fn param_set(&self) -> Result<ParamSet> {
        param_set(
            &self.params,
            &self.tiers,
            &self.ema_params.values(),
            self.kink.util_kink_bps,
        )
    }

    /// The parameter set as it would be after `change` (ARCH §3.1: `propose_config` validates
    /// "the would-be result"). Non-parameter changes leave it as it is.
    pub fn param_set_after(&self, change: &ConfigChange) -> Result<ParamSet> {
        let ema = self.ema_params.values();
        let kink = self.kink.util_kink_bps;
        match change {
            ConfigChange::Params { params, tiers } => param_set(params, tiers, &ema, kink),
            ConfigChange::EmaParams { ema } => param_set(&self.params, &self.tiers, ema, kink),
            ConfigChange::Kink { util_kink_bps } => {
                param_set(&self.params, &self.tiers, &ema, *util_kink_bps)
            }
            ConfigChange::Authorities { .. }
            | ConfigChange::Timelock { .. }
            | ConfigChange::EnableVenues { .. }
            | ConfigChange::OracleFeed { .. } => self.param_set(),
        }
    }

    /// Everything `propose_config` and `apply_config` require of a change: its own rules
    /// ([`ConfigChange::check_shape`]) and every compiled bound and relational check on the
    /// would-be parameter set ([`validate_param_set`]).
    pub fn check_change(&self, change: &ConfigChange) -> Result<()> {
        change.check_shape()?;
        validate_param_set(&self.param_set_after(change)?)
    }

    /// Applies `change` at `now` (ARCH §3.1 `apply_config`): a `Params` change bumps
    /// `params_version`; an `EmaParams` change bumps `ema_params.version` and sets `force_at =
    /// now + ema_force_delay_s` (A59); a `Kink` change bumps `kink.version` (R3A-9);
    /// `EnableVenues` ORs its bits in. The caller clears `pending`.
    pub fn apply(&mut self, change: &ConfigChange, now: i64) -> Result<()> {
        use crate::errors::CheckedExt;
        match change {
            ConfigChange::Params { params, tiers } => {
                self.params = *params;
                self.tiers = *tiers;
                self.params_version = self.params_version.checked_add(1).or_math()?;
            }
            ConfigChange::EmaParams { ema } => {
                self.ema_params.set_values(ema);
                self.ema_params.version = self.ema_params.version.checked_add(1).or_math()?;
                self.ema_params.force_at = now
                    .checked_add(i64::from(self.params.ema_force_delay_s))
                    .or_math()?;
            }
            ConfigChange::Kink { util_kink_bps } => {
                self.kink.util_kink_bps = *util_kink_bps;
                self.kink.version = self.kink.version.checked_add(1).or_math()?;
            }
            ConfigChange::Authorities {
                governance,
                pause_authority,
                coverage_authority,
                fee_recipient,
            } => {
                self.governance = *governance;
                self.pause_authority = *pause_authority;
                self.coverage_authority = *coverage_authority;
                self.fee_recipient = *fee_recipient;
            }
            ConfigChange::Timelock { timelock_s } => self.timelock_s = *timelock_s,
            ConfigChange::EnableVenues { bits } => self.venue_enabled |= *bits,
            ConfigChange::OracleFeed { account, feed_id } => {
                self.pyth_sol_usd = *account;
                self.pyth_feed_id = *feed_id;
            }
        }
        Ok(())
    }
}

/// Global parameters in account form (ARCH §2.3.3; the values of `shortwin_math::params::Params`
/// except `util_kink_bps`, which lives in [`KinkConfig`]).
///
/// Fields are grouped by width (u64, then u32, then u16) so the struct has no padding at all
/// (264 B) and its Borsh encoding (instruction arguments) equals its memory layout (account).
/// Five fields are wider here than in the math crate (`settle_persist_n`, `settle_arm_gap_s`
/// u32; `ref_max_age_s`, `liq_persist_n`, `kappa_growth_n` u16); [`Params::to_math`] narrows
/// them with a checked conversion, so an out-of-range value is `InvalidParams`.
#[derive(
    AnchorSerialize,
    AnchorDeserialize,
    Clone,
    Copy,
    PartialEq,
    Eq,
    bytemuck::Pod,
    bytemuck::Zeroable,
)]
#[repr(C)]
pub struct Params {
    // ---- u64 ----
    pub settle_keeper_fixed: u64,
    pub settle_keeper_cap: u64,
    pub expiry_keeper_fixed: u64,
    pub expiry_keeper_cap: u64,
    pub min_notional: u64,
    pub market_max_notional: u64,
    pub market_creation_fee: u64,
    pub boost_dust_lamports: u64,
    pub exec_rent_tol: u64,
    pub theta_a_usd_e6: u64,
    pub theta_b_usd_e6: u64,
    pub pool_min_real_sol: u64,
    // ---- u32 ----
    pub open_fee_bps: u32,
    pub rent_protocol_bps: u32,
    pub penalty_lender_bps: u32,
    pub penalty_protocol_bps: u32,
    pub penalty_keeper_bps: u32,
    pub expiry_fee_bps: u32,
    pub settle_keeper_bps: u32,
    pub expiry_keeper_bps: u32,
    pub util_cap_bps: u32,
    pub wallet_cap_bps: u32,
    pub curve_oi_gamma_bps: u32,
    pub fee_size_cap_x100: u32,
    pub fee_oi_cap_x100: u32,
    pub s3_wait_s: u32,
    pub grad_grace_s: u32,
    pub max_migration_pause_s: u32,
    pub boost_window_s: u32,
    pub open_ema_dev_bps: u32,
    pub market_warmup_s: u32,
    pub ema_force_delay_s: u32,
    pub settle_persist_n: u32,
    pub settle_arm_gap_s: u32,
    pub fv_dev_bps: u32,
    pub lump_vest_s: u32,
    pub lock_grace_s: u32,
    pub oracle_max_age_s: u32,
    pub oracle_stale_s: u32,
    pub oracle_max_conf_bps: u32,
    pub oracle_max_step_bps: u32,
    pub venue_fee_cap_bps: u32,
    pub venue_fee_hard_cap_bps: u32,
    pub pool_lock_min_bps: u32,
    pub pool_switch_margin_bps: u32,
    pub pool_list_window_s: u32,
    pub pool_list_margin_bps: u32,
    pub pool_fast_route_s: u32,
    pub pool_fast_min_bps: u32,
    pub pool_fast_window_s: u32,
    // ---- u16 ----
    pub kappa_liq_bps: u16,
    pub kappa_base_bps: u16,
    pub delta_liq_bps: u16,
    pub delta_exp_bps: u16,
    pub exec_tol_bps: u16,
    pub ref_max_age_s: u16,
    pub liq_persist_n: u16,
    pub kappa_growth_n: u16,
}

/// Narrows a widened field (`InvalidParams` if it does not fit).
fn narrow<T: TryFrom<u32>>(v: u32) -> Result<T> {
    T::try_from(v).map_err(|_| error!(ShortwinError::InvalidParams))
}

impl Params {
    /// The math crate's `Params` with `util_kink_bps` from [`KinkConfig`].
    pub fn to_math(&self, util_kink_bps: u32) -> Result<mparams::Params> {
        Ok(mparams::Params {
            open_fee_bps: self.open_fee_bps,
            rent_protocol_bps: self.rent_protocol_bps,
            penalty_lender_bps: self.penalty_lender_bps,
            penalty_protocol_bps: self.penalty_protocol_bps,
            penalty_keeper_bps: self.penalty_keeper_bps,
            expiry_fee_bps: self.expiry_fee_bps,
            settle_keeper_fixed: self.settle_keeper_fixed,
            settle_keeper_bps: self.settle_keeper_bps,
            settle_keeper_cap: self.settle_keeper_cap,
            expiry_keeper_fixed: self.expiry_keeper_fixed,
            expiry_keeper_bps: self.expiry_keeper_bps,
            expiry_keeper_cap: self.expiry_keeper_cap,
            util_kink_bps,
            util_cap_bps: self.util_cap_bps,
            wallet_cap_bps: self.wallet_cap_bps,
            curve_oi_gamma_bps: self.curve_oi_gamma_bps,
            fee_size_cap_x100: self.fee_size_cap_x100,
            fee_oi_cap_x100: self.fee_oi_cap_x100,
            min_notional: self.min_notional,
            market_max_notional: self.market_max_notional,
            market_creation_fee: self.market_creation_fee,
            s3_wait_s: self.s3_wait_s,
            grad_grace_s: self.grad_grace_s,
            max_migration_pause_s: self.max_migration_pause_s,
            boost_dust_lamports: self.boost_dust_lamports,
            boost_window_s: self.boost_window_s,
            open_ema_dev_bps: self.open_ema_dev_bps,
            market_warmup_s: self.market_warmup_s,
            ema_force_delay_s: self.ema_force_delay_s,
            ref_max_age_s: narrow(self.ref_max_age_s.into())?,
            liq_persist_n: narrow(self.liq_persist_n.into())?,
            settle_persist_n: narrow(self.settle_persist_n)?,
            settle_arm_gap_s: narrow(self.settle_arm_gap_s)?,
            kappa_liq_bps: self.kappa_liq_bps,
            kappa_base_bps: self.kappa_base_bps,
            kappa_growth_n: narrow(self.kappa_growth_n.into())?,
            delta_liq_bps: self.delta_liq_bps,
            delta_exp_bps: self.delta_exp_bps,
            exec_tol_bps: self.exec_tol_bps,
            exec_rent_tol: self.exec_rent_tol,
            fv_dev_bps: self.fv_dev_bps,
            lump_vest_s: self.lump_vest_s,
            lock_grace_s: self.lock_grace_s,
            theta_a_usd_e6: self.theta_a_usd_e6,
            theta_b_usd_e6: self.theta_b_usd_e6,
            oracle_max_age_s: self.oracle_max_age_s,
            oracle_stale_s: self.oracle_stale_s,
            oracle_max_conf_bps: self.oracle_max_conf_bps,
            oracle_max_step_bps: self.oracle_max_step_bps,
            venue_fee_cap_bps: self.venue_fee_cap_bps,
            venue_fee_hard_cap_bps: self.venue_fee_hard_cap_bps,
            pool_min_real_sol: self.pool_min_real_sol,
            pool_lock_min_bps: self.pool_lock_min_bps,
            pool_switch_margin_bps: self.pool_switch_margin_bps,
            pool_list_window_s: self.pool_list_window_s,
            pool_list_margin_bps: self.pool_list_margin_bps,
            pool_fast_route_s: self.pool_fast_route_s,
            pool_fast_min_bps: self.pool_fast_min_bps,
            pool_fast_window_s: self.pool_fast_window_s,
        })
    }

    /// The account encoding of the math crate's `Params` (drops `util_kink_bps`).
    pub fn from_math(p: &mparams::Params) -> Self {
        Self {
            settle_keeper_fixed: p.settle_keeper_fixed,
            settle_keeper_cap: p.settle_keeper_cap,
            expiry_keeper_fixed: p.expiry_keeper_fixed,
            expiry_keeper_cap: p.expiry_keeper_cap,
            min_notional: p.min_notional,
            market_max_notional: p.market_max_notional,
            market_creation_fee: p.market_creation_fee,
            boost_dust_lamports: p.boost_dust_lamports,
            exec_rent_tol: p.exec_rent_tol,
            theta_a_usd_e6: p.theta_a_usd_e6,
            theta_b_usd_e6: p.theta_b_usd_e6,
            pool_min_real_sol: p.pool_min_real_sol,
            open_fee_bps: p.open_fee_bps,
            rent_protocol_bps: p.rent_protocol_bps,
            penalty_lender_bps: p.penalty_lender_bps,
            penalty_protocol_bps: p.penalty_protocol_bps,
            penalty_keeper_bps: p.penalty_keeper_bps,
            expiry_fee_bps: p.expiry_fee_bps,
            settle_keeper_bps: p.settle_keeper_bps,
            expiry_keeper_bps: p.expiry_keeper_bps,
            util_cap_bps: p.util_cap_bps,
            wallet_cap_bps: p.wallet_cap_bps,
            curve_oi_gamma_bps: p.curve_oi_gamma_bps,
            fee_size_cap_x100: p.fee_size_cap_x100,
            fee_oi_cap_x100: p.fee_oi_cap_x100,
            s3_wait_s: p.s3_wait_s,
            grad_grace_s: p.grad_grace_s,
            max_migration_pause_s: p.max_migration_pause_s,
            boost_window_s: p.boost_window_s,
            open_ema_dev_bps: p.open_ema_dev_bps,
            market_warmup_s: p.market_warmup_s,
            ema_force_delay_s: p.ema_force_delay_s,
            settle_persist_n: p.settle_persist_n.into(),
            settle_arm_gap_s: p.settle_arm_gap_s.into(),
            fv_dev_bps: p.fv_dev_bps,
            lump_vest_s: p.lump_vest_s,
            lock_grace_s: p.lock_grace_s,
            oracle_max_age_s: p.oracle_max_age_s,
            oracle_stale_s: p.oracle_stale_s,
            oracle_max_conf_bps: p.oracle_max_conf_bps,
            oracle_max_step_bps: p.oracle_max_step_bps,
            venue_fee_cap_bps: p.venue_fee_cap_bps,
            venue_fee_hard_cap_bps: p.venue_fee_hard_cap_bps,
            pool_lock_min_bps: p.pool_lock_min_bps,
            pool_switch_margin_bps: p.pool_switch_margin_bps,
            pool_list_window_s: p.pool_list_window_s,
            pool_list_margin_bps: p.pool_list_margin_bps,
            pool_fast_route_s: p.pool_fast_route_s,
            pool_fast_min_bps: p.pool_fast_min_bps,
            pool_fast_window_s: p.pool_fast_window_s,
            kappa_liq_bps: p.kappa_liq_bps,
            kappa_base_bps: p.kappa_base_bps,
            delta_liq_bps: p.delta_liq_bps,
            delta_exp_bps: p.delta_exp_bps,
            exec_tol_bps: p.exec_tol_bps,
            ref_max_age_s: p.ref_max_age_s.into(),
            liq_persist_n: p.liq_persist_n.into(),
            kappa_growth_n: p.kappa_growth_n.into(),
        }
    }

    /// ECON §15 defaults.
    pub fn defaults() -> Self {
        Self::from_math(&mparams::Params::DEFAULT)
    }
}

/// Per-tier parameters (ARCH §2.3.3 `TierParams`; A, B, C, D), 12 × u32 = 48 B, no padding.
#[derive(
    AnchorSerialize,
    AnchorDeserialize,
    Clone,
    Copy,
    PartialEq,
    Eq,
    bytemuck::Pod,
    bytemuck::Zeroable,
)]
#[repr(C)]
pub struct TierParams {
    pub m_bps: u32,
    pub cr_open_bps: u32,
    pub cr_liq_bps: u32,
    pub penalty_bps: u32,
    pub max_duration_s: u32,
    pub impact_pos_bps: u32,
    pub impact_mkt_bps: u32,
    pub max_liq_slip_bps: u32,
    pub rate_base_bps: u32,
    pub rate_slope1_bps: u32,
    pub rate_slope2_bps: u32,
    pub min_rent_s: u32,
}

impl TierParams {
    /// The math crate's `TierParams`.
    pub fn to_math(&self) -> MathTierParams {
        MathTierParams {
            m_bps: self.m_bps,
            cr_open_bps: self.cr_open_bps,
            cr_liq_bps: self.cr_liq_bps,
            penalty_bps: self.penalty_bps,
            max_duration_s: self.max_duration_s,
            impact_pos_bps: self.impact_pos_bps,
            impact_mkt_bps: self.impact_mkt_bps,
            max_liq_slip_bps: self.max_liq_slip_bps,
            rate: RentTerms {
                base_bps: self.rate_base_bps,
                slope1_bps: self.rate_slope1_bps,
                slope2_bps: self.rate_slope2_bps,
            },
            min_rent_s: self.min_rent_s,
        }
    }

    /// The account encoding of the math crate's `TierParams`.
    pub fn from_math(t: &MathTierParams) -> Self {
        Self {
            m_bps: t.m_bps,
            cr_open_bps: t.cr_open_bps,
            cr_liq_bps: t.cr_liq_bps,
            penalty_bps: t.penalty_bps,
            max_duration_s: t.max_duration_s,
            impact_pos_bps: t.impact_pos_bps,
            impact_mkt_bps: t.impact_mkt_bps,
            max_liq_slip_bps: t.max_liq_slip_bps,
            rate_base_bps: t.rate.base_bps,
            rate_slope1_bps: t.rate.slope1_bps,
            rate_slope2_bps: t.rate.slope2_bps,
            min_rent_s: t.min_rent_s,
        }
    }

    /// SPEC §4 / §6.1 defaults of A, B, C, D.
    pub fn defaults() -> [Self; 4] {
        MathTierParams::DEFAULTS.map(|t| Self::from_math(&t))
    }
}

/// The market-level EMA parameter values (ARCH §2.3.2 `ema_params` without version and
/// `force_at`): the argument form of `initialize_config` and of `ConfigChange::EmaParams`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmaParamsArgs {
    pub ema_dtmax_s: u16,
    pub ema_clamp_bps: u32,
    pub liq_slow_tau_s: u32,
    pub liq_slow_dtmax_s: u32,
}

impl EmaParamsArgs {
    /// ECON §15 defaults.
    pub fn defaults() -> Self {
        let d = MathEmaParams::DEFAULT;
        Self {
            ema_dtmax_s: d.dtmax_s,
            ema_clamp_bps: d.clamp_bps,
            liq_slow_tau_s: d.slow_tau_s,
            liq_slow_dtmax_s: d.slow_dtmax_s,
        }
    }

    /// The math crate's `EmaParams`.
    pub fn to_math(&self) -> MathEmaParams {
        MathEmaParams::new(
            self.ema_dtmax_s,
            self.ema_clamp_bps,
            self.liq_slow_tau_s,
            self.liq_slow_dtmax_s,
        )
    }
}

/// `GlobalConfig.ema_params` (ARCH §2.3.2), 32 B: a market adopts a new `version` while it has
/// no open position, or at its first prologue after `force_at` (A17, A59; V2-13).
#[zero_copy]
#[derive(PartialEq, Eq)]
pub struct EmaConfig {
    pub ema_dtmax_s: u16,
    pub _pad0: [u8; 2],
    pub ema_clamp_bps: u32,
    pub liq_slow_tau_s: u32,
    pub liq_slow_dtmax_s: u32,
    pub version: u32,
    pub _pad1: [u8; 4],
    pub force_at: i64,
}

impl EmaConfig {
    /// The values without version and `force_at`.
    pub fn values(&self) -> EmaParamsArgs {
        EmaParamsArgs {
            ema_dtmax_s: self.ema_dtmax_s,
            ema_clamp_bps: self.ema_clamp_bps,
            liq_slow_tau_s: self.liq_slow_tau_s,
            liq_slow_dtmax_s: self.liq_slow_dtmax_s,
        }
    }

    /// Stores new values (version and `force_at` are the caller's).
    pub fn set_values(&mut self, v: &EmaParamsArgs) {
        self.ema_dtmax_s = v.ema_dtmax_s;
        self.ema_clamp_bps = v.ema_clamp_bps;
        self.liq_slow_tau_s = v.liq_slow_tau_s;
        self.liq_slow_dtmax_s = v.liq_slow_dtmax_s;
    }
}

/// `GlobalConfig.kink` (ARCH §2.3.2), 8 B: market-level with its own version, adopted only
/// while a market has no open position (R3A-9).
#[zero_copy]
#[derive(PartialEq, Eq)]
pub struct KinkConfig {
    pub util_kink_bps: u32,
    pub version: u32,
}

/// `GlobalConfig.pending` (ARCH §2.3.2), 472 B: `kind` 0 = none, else the [`ConfigChange`]
/// variant index + 1; `payload` is the Borsh encoding of that variant's fields (the
/// `ConfigChange` encoding without its one-byte tag), zero-padded to [`PENDING_PAYLOAD_LEN`].
#[zero_copy]
pub struct PendingChange {
    pub kind: u8,
    pub _pad: [u8; 7],
    pub not_before: i64,
    pub payload: [u8; 456],
}

impl PendingChange {
    /// `true` if a change is pending.
    pub fn is_some(&self) -> bool {
        self.kind != 0
    }

    /// Clears the pending change.
    pub fn clear(&mut self) {
        self.kind = 0;
        self._pad = [0; 7];
        self.not_before = 0;
        self.payload = [0; PENDING_PAYLOAD_LEN];
    }

    /// Stores `change` with its timelock.
    pub fn set(&mut self, change: &ConfigChange, not_before: i64) -> Result<()> {
        let (kind, payload) = change.encode()?;
        self.kind = kind;
        self._pad = [0; 7];
        self.not_before = not_before;
        self.payload = payload;
        Ok(())
    }

    /// The pending change (`NoPendingChange` if none).
    pub fn change(&self) -> Result<ConfigChange> {
        ConfigChange::decode(self.kind, &self.payload)
    }

    /// `sha256(kind ‖ payload)`, the hash in `ConfigProposed` / `ConfigApplied` /
    /// `ConfigCancelled` (ARCH §3.10).
    pub fn hash(&self) -> [u8; 32] {
        change_hash(self.kind, &self.payload)
    }
}

/// `sha256(kind ‖ payload)`.
pub fn change_hash(kind: u8, payload: &[u8; PENDING_PAYLOAD_LEN]) -> [u8; 32] {
    solana_sha256_hasher::hashv(&[&[kind], payload.as_slice()]).to_bytes()
}

/// A governance change (ARCH §3.1 `propose_config`). **There is no variant that changes a
/// venue program id or a venue PDA (C0-8).**
// Only ever an instruction argument (deserialized once per `propose_config`) or decoded from
// `PendingChange`; the size difference between variants costs nothing.
#[allow(clippy::large_enum_variant)]
#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Eq)]
pub enum ConfigChange {
    /// Global and tier parameters (validated together with the current EMA and kink values).
    Params {
        params: Params,
        tiers: [TierParams; 4],
    },
    /// Market-level EMA parameters: a new `ema_params.version`, `force_at = applied_at +
    /// ema_force_delay_s` (A59).
    EmaParams { ema: EmaParamsArgs },
    /// The rent-curve kink: a new `kink.version` (R3A-9).
    Kink { util_kink_bps: u32 },
    /// All four authorities. `apply_config` takes the new `fee_recipient` as an account and
    /// checks it (`check_fee_recipient`, M1-SEC-5).
    Authorities {
        governance: Pubkey,
        pause_authority: Pubkey,
        coverage_authority: Pubkey,
        fee_recipient: Pubkey,
    },
    /// The timelock (`MIN_TIMELOCK_S ≤ timelock_s ≤ MAX_TIMELOCK_S`).
    Timelock { timelock_s: u32 },
    /// Enables venue bits for opens (`venue_enabled |= bits`).
    EnableVenues { bits: u32 },
    /// The Pyth account and feed id `crank_sol_usd` reads. `apply_config` also empties
    /// `OracleCache`, so the old feed's price neither lingers as fresh nor gates the new feed's
    /// first crank (step and ordering rules; M1-SEC-10).
    OracleFeed { account: Pubkey, feed_id: [u8; 32] },
}

/// Number of [`ConfigChange`] variants (`kind` ∈ 1..=this).
pub const CONFIG_CHANGE_KINDS: u8 = 7;

impl ConfigChange {
    /// `kind` of [`ConfigChange::Authorities`].
    pub const KIND_AUTHORITIES: u8 = 4;
    /// `kind` of [`ConfigChange::Timelock`].
    pub const KIND_TIMELOCK: u8 = 5;
    /// `kind` of [`ConfigChange::EnableVenues`] (`disable_venue` cancels a pending one that would
    /// set the bit it clears, AIR-24).
    pub const KIND_ENABLE_VENUES: u8 = 6;

    /// `kind` of this change (variant index + 1).
    pub fn kind(&self) -> u8 {
        match self {
            Self::Params { .. } => 1,
            Self::EmaParams { .. } => 2,
            Self::Kink { .. } => 3,
            Self::Authorities { .. } => Self::KIND_AUTHORITIES,
            Self::Timelock { .. } => Self::KIND_TIMELOCK,
            Self::EnableVenues { .. } => Self::KIND_ENABLE_VENUES,
            Self::OracleFeed { .. } => 7,
        }
    }

    /// `(kind, payload)` for [`PendingChange`].
    pub fn encode(&self) -> Result<(u8, [u8; PENDING_PAYLOAD_LEN])> {
        // Borsh into a fixed buffer (tag + fields); a variant that did not fit would fail
        // the write, but every variant fits by construction (`tests::largest_variant_fits`).
        let mut buf = [0u8; PENDING_PAYLOAD_LEN + 1];
        let mut w: &mut [u8] = &mut buf;
        self.serialize(&mut w)
            .map_err(|_| error!(ShortwinError::InvalidParams))?;
        let mut payload = [0u8; PENDING_PAYLOAD_LEN];
        if let Some(fields) = buf.get(1..) {
            payload.copy_from_slice(fields);
        }
        Ok((self.kind(), payload))
    }

    /// Inverse of [`ConfigChange::encode`].
    pub fn decode(kind: u8, payload: &[u8; PENDING_PAYLOAD_LEN]) -> Result<Self> {
        require!(
            (1..=CONFIG_CHANGE_KINDS).contains(&kind),
            ShortwinError::NoPendingChange
        );
        let mut buf = [0u8; PENDING_PAYLOAD_LEN + 1];
        let (tag, rest) = buf.split_at_mut(1);
        tag.copy_from_slice(&[kind.saturating_sub(1)]);
        rest.copy_from_slice(payload);
        let mut slice: &[u8] = &buf;
        Self::deserialize(&mut slice).map_err(|_| error!(ShortwinError::InvalidParams))
    }

    /// Checks the variant's own rules (authorities set, timelock floor, known venue bits,
    /// oracle account set). Parameter variants are checked by [`validate_param_set`] on the
    /// would-be configuration.
    pub fn check_shape(&self) -> Result<()> {
        match self {
            Self::Authorities {
                governance,
                pause_authority,
                coverage_authority,
                fee_recipient,
            } => check_authorities(
                governance,
                pause_authority,
                coverage_authority,
                fee_recipient,
            ),
            Self::Timelock { timelock_s } => check_timelock(*timelock_s),
            Self::EnableVenues { bits } => {
                // Known bits only, and none this build cannot finish (M5-SEC-4:
                // `constants::VENUE_BITS_UNFINISHED`; the pump curve in M5, empty since M6).
                require!(
                    *bits != 0 && bits & !VENUE_BITS_ENABLEABLE == 0,
                    ShortwinError::InvalidParams
                );
                Ok(())
            }
            Self::OracleFeed { account, .. } => {
                require_keys_neq!(*account, Pubkey::default(), ShortwinError::InvalidParams);
                Ok(())
            }
            Self::Params { .. } | Self::EmaParams { .. } | Self::Kink { .. } => Ok(()),
        }
    }
}

/// Governance and the Emergency vault must be real keys (a default key could never sign, which
/// would freeze the config); the fee recipient must be a real account (`InvalidParams`).
///
/// **SQ-G8** (owner decision of 2026-10-02, Q4): `governance` (the Squads Governance vault 0),
/// `pause_authority` (the Emergency vault), `fee_recipient` (the Governance vault 1) and an
/// installed `coverage_authority` (a hot keeper key) must be **pairwise distinct**
/// (`AuthoritiesNotDistinct`), so one key never holds two roles — the Emergency 2-of-4 can
/// never act as the timelocked 3-of-5, nor the keeper key as either. An unset
/// `coverage_authority` (`Pubkey::default()`, A69/O17) is no key and is skipped. Checked by
/// `initialize_config` and by `check_change` on an `Authorities` change, so `propose_config`
/// refuses it early and `apply_config` again.
pub fn check_authorities(
    governance: &Pubkey,
    pause_authority: &Pubkey,
    coverage_authority: &Pubkey,
    fee_recipient: &Pubkey,
) -> Result<()> {
    let zero = Pubkey::default();
    require!(
        *governance != zero && *pause_authority != zero && *fee_recipient != zero,
        ShortwinError::InvalidParams
    );
    let keys = [
        governance,
        pause_authority,
        fee_recipient,
        coverage_authority,
    ];
    let set = if *coverage_authority == zero {
        &keys[..3]
    } else {
        &keys[..]
    };
    for (i, a) in set.iter().enumerate() {
        for b in set.iter().skip(i.saturating_add(1)) {
            require_keys_neq!(**a, **b, ShortwinError::AuthoritiesNotDistinct);
        }
    }
    Ok(())
}

/// `MIN_TIMELOCK_S ≤ timelock_s ≤ MAX_TIMELOCK_S` (A8; the ceiling is M1-SEC-4's): the
/// floor keeps the reaction window, the ceiling keeps a unit slip or a hostile `Timelock`
/// change from freezing every later change.
pub fn check_timelock(timelock_s: u32) -> Result<()> {
    require_gte!(timelock_s, MIN_TIMELOCK_S, ShortwinError::TimelockTooShort);
    require_gte!(MAX_TIMELOCK_S, timelock_s, ShortwinError::TimelockTooLong);
    Ok(())
}

/// The fee recipient an `Authorities` change installs, as an account (`apply_config`;
/// M1-SEC-5): it must be **System-owned** and **not executable**. That refuses sysvars
/// (owned by the sysvar program), programs and program data (loaders), token accounts and
/// every short.win account (a `MarketVault` as recipient would turn other markets' fees into
/// untracked lamports), none of which `sweep_protocol_fees` can credit safely. The caller's
/// Accounts struct marks it `mut`, so a key the runtime never lets be writable (reserved keys,
/// sysvars, program ids) fails there with `ConstraintMut`. A System account that does not
/// exist yet (0 lamports) is accepted: the sweep waits until it can fund its rent minimum.
/// `initialize_config` takes the recipient by key only (its transaction is 1,198 of 1,232
/// bytes); the deployer's initial choice is fixable by an `Authorities` change.
pub fn check_fee_recipient(recipient: &AccountInfo) -> Result<()> {
    require_keys_eq!(
        *recipient.owner,
        anchor_lang::system_program::ID,
        ShortwinError::InvalidParams
    );
    require!(!recipient.executable, ShortwinError::InvalidParams);
    Ok(())
}

/// The math crate's `ParamSet` from the account encodings.
pub fn param_set(
    params: &Params,
    tiers: &[TierParams; 4],
    ema: &EmaParamsArgs,
    util_kink_bps: u32,
) -> Result<ParamSet> {
    Ok(ParamSet {
        params: params.to_math(util_kink_bps)?,
        tiers: [
            tiers[0].to_math(),
            tiers[1].to_math(),
            tiers[2].to_math(),
            tiers[3].to_math(),
        ],
        ema: ema.to_math(),
    })
}

/// ARCH §2.3.3 / §3.1: every compiled bound and relational check — `params::validate`
/// (= `shortwin_sim.validate_params`) then `params::validate_econ` (the ECON §15 bounds the
/// sim does not check). A violation is `InvalidParams` (its rule is logged).
pub fn validate_param_set(ps: &ParamSet) -> Result<()> {
    if let Err(v) = mparams::validate(ps).and_then(|()| mparams::validate_econ(ps)) {
        msg!(
            "invalid params: {} (tier {:?})",
            v.rule.name(),
            v.tier.map(|t| t as u8)
        );
        return err!(ShortwinError::InvalidParams);
    }
    Ok(())
}

/// The initial configuration (ARCH §3.1 `initialize_config`).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Eq)]
pub struct InitConfigArgs {
    pub governance: Pubkey,
    pub pause_authority: Pubkey,
    /// `Pubkey::default()` = disabled (the default until O17).
    pub coverage_authority: Pubkey,
    pub fee_recipient: Pubkey,
    pub pyth_sol_usd: Pubkey,
    pub pyth_feed_id: [u8; 32],
    /// In `[MIN_TIMELOCK_S, MAX_TIMELOCK_S]`.
    pub timelock_s: u32,
    /// Venue bits enabled for opens (`VENUE_BIT_*`).
    pub venue_enabled: u32,
    pub params: Params,
    pub tiers: [TierParams; 4],
    pub ema: EmaParamsArgs,
    pub util_kink_bps: u32,
    /// `VenueGuard.guard_slot`: the ProgramData deploy slots validated at launch, in
    /// `VenueGuard` order (pump, pAMM, pfee, AMM v4, CPMM, Jupiter).
    pub guard_slot: [u64; 6],
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    fn default_config() -> GlobalConfig {
        let mut c = GlobalConfig::zeroed();
        c.params = Params::defaults();
        c.tiers = TierParams::defaults();
        c.ema_params.set_values(&EmaParamsArgs::defaults());
        c.ema_params.version = 1;
        c.kink.util_kink_bps = mparams::Params::DEFAULT.util_kink_bps;
        c.kink.version = 1;
        c.timelock_s = crate::constants::DEFAULT_TIMELOCK_S;
        c
    }

    fn every_variant() -> [ConfigChange; 7] {
        let mut params = Params::defaults();
        params.open_fee_bps = 30;
        let mut tiers = TierParams::defaults();
        tiers[3].penalty_bps = 1_100;
        [
            ConfigChange::Params { params, tiers },
            ConfigChange::EmaParams {
                ema: EmaParamsArgs {
                    ema_dtmax_s: 1,
                    ema_clamp_bps: 6_000,
                    liq_slow_tau_s: 1_800,
                    liq_slow_dtmax_s: 30,
                },
            },
            ConfigChange::Kink {
                util_kink_bps: 7_500,
            },
            ConfigChange::Authorities {
                governance: Pubkey::new_from_array([1; 32]),
                pause_authority: Pubkey::new_from_array([2; 32]),
                coverage_authority: Pubkey::default(),
                fee_recipient: Pubkey::new_from_array([3; 32]),
            },
            ConfigChange::Timelock { timelock_s: 90_000 },
            ConfigChange::EnableVenues { bits: 0b1100 },
            ConfigChange::OracleFeed {
                account: Pubkey::new_from_array([4; 32]),
                feed_id: [5; 32],
            },
        ]
    }

    #[test]
    fn math_round_trips() {
        let p = mparams::Params::DEFAULT;
        assert_eq!(Params::defaults().to_math(p.util_kink_bps).ok(), Some(p));
        for t in MathTierParams::DEFAULTS {
            assert_eq!(TierParams::from_math(&t).to_math(), t);
        }
        assert_eq!(EmaParamsArgs::defaults().to_math(), MathEmaParams::DEFAULT);
        let ps = default_config().param_set().ok();
        assert_eq!(ps, Some(ParamSet::DEFAULT));
        assert!(validate_param_set(&ParamSet::DEFAULT).is_ok());
    }

    /// O17 (A69): an unset `coverage_authority` enables nothing and matches no signer — not the
    /// default key, not Governance, not the Emergency vault; once installed only its own key
    /// matches, and an `Authorities` change rotates or disables it.
    #[test]
    fn coverage_authority_is_inert_until_installed() {
        let mut c = default_config();
        c.governance = Pubkey::new_from_array([1; 32]);
        c.pause_authority = Pubkey::new_from_array([2; 32]);
        let keeper = Pubkey::new_from_array([7; 32]);
        assert!(!c.coverage_enabled());
        for k in [Pubkey::default(), c.governance, c.pause_authority, keeper] {
            assert!(!c.is_coverage_authority(&k), "{k}");
        }
        let rotate = |c: &mut GlobalConfig, to: Pubkey| {
            let change = ConfigChange::Authorities {
                governance: c.governance,
                pause_authority: c.pause_authority,
                coverage_authority: to,
                fee_recipient: Pubkey::new_from_array([3; 32]),
            };
            assert!(c.check_change(&change).is_ok());
            assert!(c.apply(&change, 0).is_ok());
        };
        rotate(&mut c, keeper);
        assert!(c.coverage_enabled());
        assert!(c.is_coverage_authority(&keeper));
        for k in [Pubkey::default(), c.governance, c.pause_authority] {
            assert!(!c.is_coverage_authority(&k), "{k}");
        }
        let next = Pubkey::new_from_array([8; 32]);
        rotate(&mut c, next);
        assert!(!c.is_coverage_authority(&keeper));
        assert!(c.is_coverage_authority(&next));
        rotate(&mut c, Pubkey::default());
        assert!(!c.coverage_enabled());
        assert!(!c.is_coverage_authority(&Pubkey::default()));
        assert!(!c.is_coverage_authority(&next));
    }

    /// SQ-G8: every pair of the four authorities refused when equal (an installed coverage key
    /// included); an unset coverage key is skipped; a default governance, Emergency or recipient
    /// key stays `InvalidParams`.
    #[test]
    fn authorities_must_be_pairwise_distinct() {
        let k = |b: u8| Pubkey::new_from_array([b; 32]);
        let code = |r: Result<()>| match r {
            Ok(()) => None,
            Err(anchor_lang::error::Error::AnchorError(e)) => Some(e.error_code_number),
            Err(_) => Some(0),
        };
        let distinct = u32::from(ShortwinError::AuthoritiesNotDistinct);
        let base = [k(1), k(2), k(4), k(3)]; // governance, pause, coverage, fee recipient
        let check = |a: &[Pubkey; 4]| code(check_authorities(&a[0], &a[1], &a[2], &a[3]));
        assert_eq!(check(&base), None);
        for i in 0..4 {
            for j in (i + 1)..4 {
                let mut a = base;
                a[j] = a[i];
                assert_eq!(check(&a), Some(distinct), "roles {i} and {j}");
            }
        }
        let unset = [k(1), k(2), Pubkey::default(), k(3)];
        assert_eq!(check(&unset), None, "an unset coverage key is no key");
        let zero = u32::from(ShortwinError::InvalidParams);
        for i in [0, 1, 3] {
            let mut a = base;
            a[i] = Pubkey::default();
            assert_eq!(check(&a), Some(zero), "role {i} unset");
        }
    }

    /// ★OWNER-M7-3 (b) (owner decision of 2026-10-01): the Emergency vault clears the flag —
    /// with the role installed, unset or rotated — and never sets it; no other key clears through
    /// this path (Governance, the coverage key and the default key included).
    #[test]
    fn the_emergency_clears_and_never_sets() {
        let mut c = default_config();
        c.governance = Pubkey::new_from_array([1; 32]);
        c.pause_authority = Pubkey::new_from_array([2; 32]);
        let keeper = Pubkey::new_from_array([7; 32]);
        for installed in [Pubkey::default(), keeper] {
            c.coverage_authority = installed;
            assert!(c.is_emergency_coverage_clear(&c.pause_authority, false));
            assert!(!c.is_emergency_coverage_clear(&c.pause_authority, true));
            for k in [Pubkey::default(), c.governance, keeper] {
                assert!(!c.is_emergency_coverage_clear(&k, false), "{k}");
                assert!(!c.is_emergency_coverage_clear(&k, true), "{k}");
            }
        }
        // An unset Emergency key (never accepted by `check_authorities`) matches nothing.
        c.pause_authority = Pubkey::default();
        assert!(!c.is_emergency_coverage_clear(&Pubkey::default(), false));
    }

    #[test]
    fn widened_fields_narrow_or_refuse() {
        let mut p = Params::defaults();
        p.ref_max_age_s = 256; // u8 in the math crate
        assert!(p.to_math(8_000).is_err());
        let mut p = Params::defaults();
        p.settle_persist_n = 70_000; // u16 in the math crate
        assert!(p.to_math(8_000).is_err());
    }

    #[test]
    fn largest_variant_fits_and_round_trips() {
        for change in every_variant() {
            let (kind, payload) = change
                .encode()
                .ok()
                .unwrap_or((0, [0; PENDING_PAYLOAD_LEN]));
            assert_eq!(kind, change.kind());
            assert!(ConfigChange::decode(kind, &payload).ok() == Some(change.clone()));
            let mut pending = PendingChange::zeroed();
            assert!(pending.set(&change, 42).is_ok());
            assert!(pending.is_some());
            assert_eq!(pending.not_before, 42);
            assert_eq!(pending.hash(), change_hash(kind, &payload));
            assert!(pending.change().ok() == Some(change.clone()));
            pending.clear();
            assert!(!pending.is_some());
            assert!(pending.change().is_err());
        }
        // The Params variant fills the payload exactly (its Borsh encoding equals its layout).
        let (_, payload) = every_variant()[0].encode().ok().unwrap_or((0, [0; 456]));
        assert_eq!(
            &payload[..264],
            bytemuck::bytes_of(&{
                let mut p = Params::defaults();
                p.open_fee_bps = 30;
                p
            })
        );
    }

    #[test]
    fn apply_sets_versions_and_values() {
        let now = 1_800_000_000;
        let mut c = default_config();
        for change in every_variant() {
            assert!(c.check_change(&change).is_ok(), "{}", change.kind());
            assert!(c.apply(&change, now).is_ok());
        }
        assert_eq!(c.params.open_fee_bps, 30);
        assert_eq!(c.tiers[3].penalty_bps, 1_100);
        assert_eq!(c.params_version, 1);
        assert_eq!(c.ema_params.version, 2);
        assert_eq!(c.ema_params.ema_clamp_bps, 6_000);
        assert_eq!(
            c.ema_params.force_at,
            now + i64::from(c.params.ema_force_delay_s)
        );
        assert_eq!((c.kink.util_kink_bps, c.kink.version), (7_500, 2));
        assert_eq!(c.governance, Pubkey::new_from_array([1; 32]));
        assert_eq!(c.timelock_s, 90_000);
        assert_eq!(c.venue_enabled, 0b1100);
        assert_eq!(c.pyth_feed_id, [5; 32]);
    }

    #[test]
    fn change_rules() {
        let c = default_config();
        let too_short = ConfigChange::Timelock {
            timelock_s: crate::constants::MIN_TIMELOCK_S - 1,
        };
        assert!(c.check_change(&too_short).is_err());
        // M1-SEC-4: the compiled ceiling (30 days) holds too; the ceiling itself is accepted.
        for too_long in [crate::constants::MAX_TIMELOCK_S + 1, 172_800_000, u32::MAX] {
            let change = ConfigChange::Timelock {
                timelock_s: too_long,
            };
            assert!(c.check_change(&change).is_err(), "{too_long}");
        }
        assert!(c
            .check_change(&ConfigChange::Timelock {
                timelock_s: crate::constants::MAX_TIMELOCK_S
            })
            .is_ok());
        // Bit 6 is unknown in every build (a `test-venue` build knows bit 5, the mock's).
        assert!(c
            .check_change(&ConfigChange::EnableVenues { bits: 1 << 6 })
            .is_err());
        assert!(c
            .check_change(&ConfigChange::EnableVenues { bits: 0 })
            .is_err());
        // M5-SEC-4's gate, emptied by M6 (the `→ PumpSwap` transition): the pump curve can be
        // enabled, alone or with other bits, and so can every release bit.
        for bits in [
            crate::constants::VENUE_BIT_PUMP_CURVE,
            crate::constants::VENUE_BIT_PUMP_CURVE | crate::constants::VENUE_BIT_RAYDIUM_V4,
            crate::constants::VENUE_BITS_RELEASE,
        ] {
            assert!(c.check_change(&ConfigChange::EnableVenues { bits }).is_ok());
        }
        assert!(c
            .check_change(&ConfigChange::Authorities {
                governance: Pubkey::default(),
                pause_authority: Pubkey::new_from_array([2; 32]),
                coverage_authority: Pubkey::default(),
                fee_recipient: Pubkey::new_from_array([3; 32]),
            })
            .is_err());
        // Relational check on the would-be set: kink above the current utilisation cap.
        assert!(c
            .check_change(&ConfigChange::Kink {
                util_kink_bps: 9_100
            })
            .is_err());
        // Compiled bound: τ-dependent warm-up below 10·EMA_TAU_S (ECON-only rule).
        let mut params = Params::defaults();
        params.market_warmup_s = 49;
        assert!(c
            .check_change(&ConfigChange::Params {
                params,
                tiers: TierParams::defaults()
            })
            .is_err());
        // Per-tier SPEC CR minimum (v0.4.1).
        let mut tiers = TierParams::defaults();
        tiers[0].cr_liq_bps = 12_499;
        assert!(c
            .check_change(&ConfigChange::Params {
                params: Params::defaults(),
                tiers
            })
            .is_err());
    }
}
