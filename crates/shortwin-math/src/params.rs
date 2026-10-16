//! Owner: integration (M0b). The protocol parameter set, its compiled hard bounds and its
//! relational checks (ECONOMICS §15; ARCHITECTURE §2.3.3, A8, A73).
//!
//! One source of truth (ARCH §2.3.3, ECON §15): `shortwin_sim.validate_params` checks every
//! ECON §15 compiled bound and relational check of the parameters the sim models — since
//! the M1 doc sync including the penalty split's parts (≤ 5,000 each), `cr_open_bps ≤
//! 100,000` and the other per-tier bounds (penalty, duration, impacts, slip, rates, minimum
//! rent) — and [`validate`] is that function exactly: every check the sim makes is a
//! [`Rule`] with [`Rule::in_sim`], [`check_sim`] reports exactly the sim's violations and
//! [`validate`] accepts exactly the configurations the sim accepts (T-CONFIG host part,
//! `tests/params_vectors.rs` ← `vectors/params/params.json`).
//!
//! The ECON §15 bounds of the parameters the sim does not model at all (market sizing and
//! creation fee, boost, warm-up (`≥ 10·EMA_TAU_S`), the EMA force delay, lender timing, θ
//! and `θ_B < θ_A`, oracle, Raydium pool selection and `pool_fast_min_bps`) are the other
//! rules (`in_sim() == false`), transcribed from the ECON §15 table — but for the floor of
//! `min_notional`, derived from the other compiled bounds ([`MIN_NOTIONAL_FLOOR`], AIR-06):
//! [`check_econ`] / [`validate_econ`] make them. [`check`] reports both sets, the sim's first.
//!
//! The program (`initialize_config`, `propose_config`), the keeper, the WASM facade and
//! T-CONFIG share these functions (ARCH §2.3.3, §3.1, §9 M0b): the program runs
//! [`validate`] and then [`validate_econ`], so it accepts a configuration exactly when the
//! sim does and every parameter the sim does not model lies inside its ECON §15 bounds. A
//! governance proposal must in addition pass `docs/design/sim/config_check.py`'s liveness
//! battery off-chain (A73), which applies `validate_params` first.
//!
//! Scopes (ARCH §2.3.3): τ is the constant [`EMA_TAU_S`] and not a parameter; the sim's
//! per-tier `ema_dtmax_s` / `ema_clamp_bps` are the market-level [`EmaParams`], so they are
//! reported once, without a tier; `util_kink_bps` lives in `GlobalConfig.kink` on-chain but
//! is validated with the rest.

use crate::bounds::ExitParams;
use crate::consts::{EMA_TAU_S, RING_LEN, S3_OBS_GAP_S};
use crate::error::MathError;
use crate::observe::EmaParams;
use crate::risk::{ExpiryKeeperTerms, SettleKeeperTerms};
use crate::settle::{S3Terms, SettleTerms};
use crate::tier::{Tier, TierParams};
use crate::waterfall::PenaltySplit;

/// Global parameters (ARCH §2.3.3 scopes G and G→P; ECON §15), in the order of the ECON §15
/// table. Positions snapshot the `→P` ones at open; the program stores this struct in
/// `GlobalConfig.params` (and `util_kink_bps` in `GlobalConfig.kink`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Params {
    /// Open fee, bps of `N_open`.
    pub open_fee_bps: u32,
    /// Protocol share of rent, bps (snapshot per position, R3A-5).
    pub rent_protocol_bps: u32,
    /// Penalty split: lenders' share, bps.
    pub penalty_lender_bps: u32,
    /// Penalty split: protocol share, bps.
    pub penalty_protocol_bps: u32,
    /// Penalty split: keeper share, bps.
    pub penalty_keeper_bps: u32,
    /// Expiry fee, bps of the base.
    pub expiry_fee_bps: u32,
    /// `K_s` fixed part, lamports.
    pub settle_keeper_fixed: u64,
    /// `K_s` bps of `V`.
    pub settle_keeper_bps: u32,
    /// `K_s` cap, lamports.
    pub settle_keeper_cap: u64,
    /// `K_e` fixed part, lamports.
    pub expiry_keeper_fixed: u64,
    /// `K_e` bps of `B̂_ref`.
    pub expiry_keeper_bps: u32,
    /// `K_e` cap, lamports.
    pub expiry_keeper_cap: u64,
    /// Rent-curve kink `k`, bps of utilisation (market-level with its own version).
    pub util_kink_bps: u32,
    /// Utilisation cap for opens, bps.
    pub util_cap_bps: u32,
    /// Per-wallet share of the OI cap, bps.
    pub wallet_cap_bps: u32,
    /// Tier D: `ΣQ ≤ γ·rT`, bps.
    pub curve_oi_gamma_bps: u32,
    /// Per-position fee-scaled cap, 1/100 × φ.
    pub fee_size_cap_x100: u32,
    /// Per-market fee-scaled cap, 1/100 × φ.
    pub fee_oi_cap_x100: u32,
    /// Smallest opening notional, lamports.
    pub min_notional: u64,
    /// Largest open notional per market, lamports.
    pub market_max_notional: u64,
    /// `create_market` fee, lamports.
    pub market_creation_fee: u64,
    /// S3 wait, seconds.
    pub s3_wait_s: u32,
    /// Graduation grace, seconds.
    pub grad_grace_s: u32,
    /// Longest pump migration pause before S3 counts, seconds.
    pub max_migration_pause_s: u32,
    /// PumpSwap boost dust threshold, lamports.
    pub boost_dust_lamports: u64,
    /// PumpSwap boost window, seconds.
    pub boost_window_s: u32,
    /// Opens refused while spot deviates from `P_ema` by more, bps.
    pub open_ema_dev_bps: u32,
    /// Market warm-up before the first open, seconds (≥ 10·`EMA_TAU_S`).
    pub market_warmup_s: u32,
    /// Forced adoption delay of new EMA parameters, seconds (A59).
    pub ema_force_delay_s: u32,
    /// Freshness of the completed bucket for the references, seconds.
    pub ref_max_age_s: u8,
    /// Liquidation EMA leg: newest ring entries below `L`.
    pub liq_persist_n: u8,
    /// Settlement persistence: EMA updates on one clock.
    pub settle_persist_n: u16,
    /// Settlement evaluations further apart re-arm, seconds.
    pub settle_arm_gap_s: u16,
    /// κ_max, bps.
    pub kappa_liq_bps: u16,
    /// κ_base, bps.
    pub kappa_base_bps: u16,
    /// Sustained-growth window, EMA updates.
    pub kappa_growth_n: u8,
    /// Fixed liquidation δ over `B̂_ref`, bps.
    pub delta_liq_bps: u16,
    /// Expiry δ, bps.
    pub delta_exp_bps: u16,
    /// ε, the expiry bound's rounding tolerance, bps.
    pub exec_tol_bps: u16,
    /// `ert`, venue-side rent tolerance on pump venues, lamports.
    pub exec_rent_tol: u64,
    /// Settlement-box tolerance of S2/S3 amounts, bps.
    pub fv_dev_bps: u32,
    /// Lump vesting period, seconds.
    pub lump_vest_s: u32,
    /// Lender lock grace after `max_release`, seconds.
    pub lock_grace_s: u32,
    /// Tier A liquidity threshold, USD·10⁶.
    pub theta_a_usd_e6: u64,
    /// Tier B liquidity threshold, USD·10⁶.
    pub theta_b_usd_e6: u64,
    /// Oracle cache usable for tiering if at most this old, seconds.
    pub oracle_max_age_s: u32,
    /// Oracle cache stale (graduated markets tier as C), seconds. Default 600 since the owner's
    /// decision of 2026-10-01 (OWNER-M7-15, SPEC §14 M7-15; was 3,600): ten of the feed's 55-s
    /// heartbeats, twenty of the keeper's ≤ 30-s `crank_sol_usd` periods.
    pub oracle_stale_s: u32,
    /// Oracle confidence limit, bps.
    pub oracle_max_conf_bps: u32,
    /// Oracle per-update step limit, bps.
    pub oracle_max_step_bps: u32,
    /// Opens refused above this one-way venue fee, bps (compared as `cap·100` ppm).
    pub venue_fee_cap_bps: u32,
    /// Risk quotes and FV use at most this fee; above it is S3 evidence, bps.
    pub venue_fee_hard_cap_bps: u32,
    /// Raydium eligibility: smallest real SOL, lamports.
    pub pool_min_real_sol: u64,
    /// λ_min: permanently locked LP share, bps.
    pub pool_lock_min_bps: u32,
    /// Raydium switch margin, bps.
    pub pool_switch_margin_bps: u32,
    /// Raydium listing window, seconds.
    pub pool_list_window_s: u32,
    /// Raydium listing margin, bps.
    pub pool_list_margin_bps: u32,
    /// Fast path: `pool_fail` continuously for this long, seconds.
    pub pool_fast_route_s: u32,
    /// Fast path: candidate depth, bps of the incumbent's `D`.
    pub pool_fast_min_bps: u32,
    /// Fast path: collection window, seconds.
    pub pool_fast_window_s: u32,
}

impl Params {
    /// ECON §15 / ARCH §2.3.3 defaults (= `shortwin_sim.DEFAULT` where the sim has the
    /// parameter).
    pub const DEFAULT: Self = Self {
        open_fee_bps: 25,
        rent_protocol_bps: 1_500,
        penalty_lender_bps: 5_000,
        penalty_protocol_bps: 2_000,
        penalty_keeper_bps: 3_000,
        expiry_fee_bps: 200,
        settle_keeper_fixed: 10_000_000,
        settle_keeper_bps: 100,
        settle_keeper_cap: 250_000_000,
        expiry_keeper_fixed: 1_000_000,
        expiry_keeper_bps: 10,
        expiry_keeper_cap: 50_000_000,
        util_kink_bps: 8_000,
        util_cap_bps: 9_000,
        wallet_cap_bps: 2_500,
        curve_oi_gamma_bps: 5_000,
        fee_size_cap_x100: 300,
        fee_oi_cap_x100: 750,
        min_notional: 100_000_000,
        market_max_notional: 25_000_000_000,
        market_creation_fee: 5_000_000,
        s3_wait_s: 600,
        grad_grace_s: 600,
        max_migration_pause_s: 3_600,
        boost_dust_lamports: 50_000_000,
        boost_window_s: 900,
        open_ema_dev_bps: 1_000,
        market_warmup_s: 50,
        ema_force_delay_s: 604_800,
        ref_max_age_s: 2,
        liq_persist_n: 3,
        settle_persist_n: 40,
        settle_arm_gap_s: 10,
        kappa_liq_bps: 2_500,
        kappa_base_bps: 0,
        kappa_growth_n: 3,
        delta_liq_bps: 200,
        delta_exp_bps: 100,
        exec_tol_bps: 10,
        exec_rent_tol: 6_500_000,
        fv_dev_bps: 100,
        lump_vest_s: 21_600,
        lock_grace_s: 86_400,
        theta_a_usd_e6: 500_000_000_000,
        theta_b_usd_e6: 100_000_000_000,
        oracle_max_age_s: 120,
        oracle_stale_s: 600,
        oracle_max_conf_bps: 200,
        oracle_max_step_bps: 2_000,
        venue_fee_cap_bps: 150,
        venue_fee_hard_cap_bps: 500,
        pool_min_real_sol: 25_000_000_000,
        pool_lock_min_bps: 5_000,
        pool_switch_margin_bps: 2_500,
        pool_list_window_s: 3_600,
        pool_list_margin_bps: 500,
        pool_fast_route_s: 120,
        pool_fast_min_bps: 2_500,
        pool_fast_window_s: 60,
    };

    /// The locked exit-bound terms of a position opened now (ARCH §2.3.5).
    pub const fn exit_params(&self) -> ExitParams {
        ExitParams {
            kappa_liq_bps: self.kappa_liq_bps,
            kappa_base_bps: self.kappa_base_bps,
            delta_liq_bps: self.delta_liq_bps,
            delta_exp_bps: self.delta_exp_bps,
            kappa_growth_n: self.kappa_growth_n,
            ref_max_age_s: self.ref_max_age_s,
        }
    }

    /// The locked settlement keeper terms (`K_s`).
    pub const fn settle_keeper(&self) -> SettleKeeperTerms {
        SettleKeeperTerms {
            fixed: self.settle_keeper_fixed,
            bps: self.settle_keeper_bps,
            cap: self.settle_keeper_cap,
        }
    }

    /// The locked expiry keeper terms (`K_e`).
    pub const fn expiry_keeper(&self) -> ExpiryKeeperTerms {
        ExpiryKeeperTerms {
            fixed: self.expiry_keeper_fixed,
            bps: self.expiry_keeper_bps,
            cap: self.expiry_keeper_cap,
        }
    }

    /// The locked penalty split.
    pub const fn penalty_split(&self) -> PenaltySplit {
        PenaltySplit {
            lender_bps: self.penalty_lender_bps,
            protocol_bps: self.penalty_protocol_bps,
            keeper_bps: self.penalty_keeper_bps,
        }
    }

    /// The locked S3 timing terms.
    pub const fn s3_terms(&self) -> S3Terms {
        S3Terms {
            s3_wait_s: self.s3_wait_s,
            grad_grace_s: self.grad_grace_s,
            max_migration_pause_s: self.max_migration_pause_s,
        }
    }
}

impl Default for Params {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Everything [`check`] reads: the global parameters, the four tiers and the
/// market-level EMA parameters (ARCH §2.3.2 `GlobalConfig.{params, tiers, ema_params,
/// kink}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParamSet {
    /// Global parameters.
    pub params: Params,
    /// `TierParams` of A, B, C, D ([`Tier::index`] order).
    pub tiers: [TierParams; 4],
    /// Market-level EMA parameters (`ema_dtmax_s`, `ema_clamp_bps`, `liq_slow_tau_s`,
    /// `liq_slow_dtmax_s`).
    pub ema: EmaParams,
}

impl ParamSet {
    /// The ECON §15 defaults; they validate (T-CONFIG).
    pub const DEFAULT: Self = Self {
        params: Params::DEFAULT,
        tiers: TierParams::DEFAULTS,
        ema: EmaParams::DEFAULT,
    };

    /// The terms `settle::evaluate` reads for a position of `tier` opened now.
    pub const fn settle_terms(&self, tier: Tier) -> SettleTerms {
        let t = &self.tiers[tier.index()];
        let p = &self.params;
        SettleTerms {
            cr_liq_bps: t.cr_liq_bps,
            sigma_max_bps: t.max_liq_slip_bps,
            penalty_bps: t.penalty_bps,
            expiry_fee_bps: p.expiry_fee_bps,
            fv_dev_bps: p.fv_dev_bps,
            persist_n: p.settle_persist_n,
            arm_gap_s: p.settle_arm_gap_s,
            s3: p.s3_terms(),
        }
    }

    /// Reads a parameter by its ECON §15 name (the [`Rule`] name of its bound). Tier
    /// parameters (`m_bps`, `cr_open_bps`, `cr_liq_bps`, `penalty_bps`, `max_duration_s`,
    /// `impact_pos_bps`, `impact_mkt_bps`, `max_liq_slip_bps`, `rate_base_bps`,
    /// `rate_slope1_bps`, `rate_slope2_bps`, `min_rent_s`) need `tier`; the others take
    /// `None`. `None` for an unknown name or a wrong `tier`.
    pub fn get(&self, name: &str, tier: Option<Tier>) -> Option<u64> {
        if let Some(t) = tier {
            let t = &self.tiers[t.index()];
            return Some(u64::from(match name {
                "m_bps" => t.m_bps,
                "cr_open_bps" => t.cr_open_bps,
                "cr_liq_bps" => t.cr_liq_bps,
                "penalty_bps" => t.penalty_bps,
                "max_duration_s" => t.max_duration_s,
                "impact_pos_bps" => t.impact_pos_bps,
                "impact_mkt_bps" => t.impact_mkt_bps,
                "max_liq_slip_bps" => t.max_liq_slip_bps,
                "rate_base_bps" => t.rate.base_bps,
                "rate_slope1_bps" => t.rate.slope1_bps,
                "rate_slope2_bps" => t.rate.slope2_bps,
                "min_rent_s" => t.min_rent_s,
                _ => return None,
            }));
        }
        let (p, e) = (&self.params, &self.ema);
        Some(match name {
            "open_fee_bps" => p.open_fee_bps.into(),
            "rent_protocol_bps" => p.rent_protocol_bps.into(),
            "penalty_lender_bps" => p.penalty_lender_bps.into(),
            "penalty_protocol_bps" => p.penalty_protocol_bps.into(),
            "penalty_keeper_bps" => p.penalty_keeper_bps.into(),
            "expiry_fee_bps" => p.expiry_fee_bps.into(),
            "settle_keeper_fixed" => p.settle_keeper_fixed,
            "settle_keeper_bps" => p.settle_keeper_bps.into(),
            "settle_keeper_cap" => p.settle_keeper_cap,
            "expiry_keeper_fixed" => p.expiry_keeper_fixed,
            "expiry_keeper_bps" => p.expiry_keeper_bps.into(),
            "expiry_keeper_cap" => p.expiry_keeper_cap,
            "util_kink_bps" => p.util_kink_bps.into(),
            "util_cap_bps" => p.util_cap_bps.into(),
            "wallet_cap_bps" => p.wallet_cap_bps.into(),
            "curve_oi_gamma_bps" => p.curve_oi_gamma_bps.into(),
            "fee_size_cap_x100" => p.fee_size_cap_x100.into(),
            "fee_oi_cap_x100" => p.fee_oi_cap_x100.into(),
            "min_notional" => p.min_notional,
            "market_max_notional" => p.market_max_notional,
            "market_creation_fee" => p.market_creation_fee,
            "s3_wait_s" => p.s3_wait_s.into(),
            "grad_grace_s" => p.grad_grace_s.into(),
            "max_migration_pause_s" => p.max_migration_pause_s.into(),
            "boost_dust_lamports" => p.boost_dust_lamports,
            "boost_window_s" => p.boost_window_s.into(),
            "open_ema_dev_bps" => p.open_ema_dev_bps.into(),
            "market_warmup_s" => p.market_warmup_s.into(),
            "ema_force_delay_s" => p.ema_force_delay_s.into(),
            "ref_max_age_s" => p.ref_max_age_s.into(),
            "liq_persist_n" => p.liq_persist_n.into(),
            "settle_persist_n" => p.settle_persist_n.into(),
            "settle_arm_gap_s" => p.settle_arm_gap_s.into(),
            "kappa_liq_bps" => p.kappa_liq_bps.into(),
            "kappa_base_bps" => p.kappa_base_bps.into(),
            "kappa_growth_n" => p.kappa_growth_n.into(),
            "delta_liq_bps" => p.delta_liq_bps.into(),
            "delta_exp_bps" => p.delta_exp_bps.into(),
            "exec_tol_bps" => p.exec_tol_bps.into(),
            "exec_rent_tol" => p.exec_rent_tol,
            "fv_dev_bps" => p.fv_dev_bps.into(),
            "lump_vest_s" => p.lump_vest_s.into(),
            "lock_grace_s" => p.lock_grace_s.into(),
            "theta_a_usd_e6" => p.theta_a_usd_e6,
            "theta_b_usd_e6" => p.theta_b_usd_e6,
            "oracle_max_age_s" => p.oracle_max_age_s.into(),
            "oracle_stale_s" => p.oracle_stale_s.into(),
            "oracle_max_conf_bps" => p.oracle_max_conf_bps.into(),
            "oracle_max_step_bps" => p.oracle_max_step_bps.into(),
            "venue_fee_cap_bps" => p.venue_fee_cap_bps.into(),
            "venue_fee_hard_cap_bps" => p.venue_fee_hard_cap_bps.into(),
            "pool_min_real_sol" => p.pool_min_real_sol,
            "pool_lock_min_bps" => p.pool_lock_min_bps.into(),
            "pool_switch_margin_bps" => p.pool_switch_margin_bps.into(),
            "pool_list_window_s" => p.pool_list_window_s.into(),
            "pool_list_margin_bps" => p.pool_list_margin_bps.into(),
            "pool_fast_route_s" => p.pool_fast_route_s.into(),
            "pool_fast_min_bps" => p.pool_fast_min_bps.into(),
            "pool_fast_window_s" => p.pool_fast_window_s.into(),
            "ema_dtmax_s" => e.dtmax_s.into(),
            "ema_clamp_bps" => e.clamp_bps.into(),
            "liq_slow_tau_s" => e.slow_tau_s.into(),
            "liq_slow_dtmax_s" => e.slow_dtmax_s.into(),
            _ => return None,
        })
    }

    /// Writes a parameter by name (see [`ParamSet::get`]); the value is not validated
    /// (use [`validate`]). `InvalidInput` for an unknown name, a wrong `tier`, or a value
    /// that does not fit the field's type.
    pub fn set(&mut self, name: &str, tier: Option<Tier>, value: u64) -> Result<(), MathError> {
        macro_rules! put {
            ($field:expr) => {{
                $field = fit(value)?;
                Ok(())
            }};
        }
        if let Some(t) = tier {
            let t = &mut self.tiers[t.index()];
            return match name {
                "m_bps" => put!(t.m_bps),
                "cr_open_bps" => put!(t.cr_open_bps),
                "cr_liq_bps" => put!(t.cr_liq_bps),
                "penalty_bps" => put!(t.penalty_bps),
                "max_duration_s" => put!(t.max_duration_s),
                "impact_pos_bps" => put!(t.impact_pos_bps),
                "impact_mkt_bps" => put!(t.impact_mkt_bps),
                "max_liq_slip_bps" => put!(t.max_liq_slip_bps),
                "rate_base_bps" => put!(t.rate.base_bps),
                "rate_slope1_bps" => put!(t.rate.slope1_bps),
                "rate_slope2_bps" => put!(t.rate.slope2_bps),
                "min_rent_s" => put!(t.min_rent_s),
                _ => Err(MathError::InvalidInput),
            };
        }
        let (p, e) = (&mut self.params, &mut self.ema);
        match name {
            "open_fee_bps" => put!(p.open_fee_bps),
            "rent_protocol_bps" => put!(p.rent_protocol_bps),
            "penalty_lender_bps" => put!(p.penalty_lender_bps),
            "penalty_protocol_bps" => put!(p.penalty_protocol_bps),
            "penalty_keeper_bps" => put!(p.penalty_keeper_bps),
            "expiry_fee_bps" => put!(p.expiry_fee_bps),
            "settle_keeper_fixed" => put!(p.settle_keeper_fixed),
            "settle_keeper_bps" => put!(p.settle_keeper_bps),
            "settle_keeper_cap" => put!(p.settle_keeper_cap),
            "expiry_keeper_fixed" => put!(p.expiry_keeper_fixed),
            "expiry_keeper_bps" => put!(p.expiry_keeper_bps),
            "expiry_keeper_cap" => put!(p.expiry_keeper_cap),
            "util_kink_bps" => put!(p.util_kink_bps),
            "util_cap_bps" => put!(p.util_cap_bps),
            "wallet_cap_bps" => put!(p.wallet_cap_bps),
            "curve_oi_gamma_bps" => put!(p.curve_oi_gamma_bps),
            "fee_size_cap_x100" => put!(p.fee_size_cap_x100),
            "fee_oi_cap_x100" => put!(p.fee_oi_cap_x100),
            "min_notional" => put!(p.min_notional),
            "market_max_notional" => put!(p.market_max_notional),
            "market_creation_fee" => put!(p.market_creation_fee),
            "s3_wait_s" => put!(p.s3_wait_s),
            "grad_grace_s" => put!(p.grad_grace_s),
            "max_migration_pause_s" => put!(p.max_migration_pause_s),
            "boost_dust_lamports" => put!(p.boost_dust_lamports),
            "boost_window_s" => put!(p.boost_window_s),
            "open_ema_dev_bps" => put!(p.open_ema_dev_bps),
            "market_warmup_s" => put!(p.market_warmup_s),
            "ema_force_delay_s" => put!(p.ema_force_delay_s),
            "ref_max_age_s" => put!(p.ref_max_age_s),
            "liq_persist_n" => put!(p.liq_persist_n),
            "settle_persist_n" => put!(p.settle_persist_n),
            "settle_arm_gap_s" => put!(p.settle_arm_gap_s),
            "kappa_liq_bps" => put!(p.kappa_liq_bps),
            "kappa_base_bps" => put!(p.kappa_base_bps),
            "kappa_growth_n" => put!(p.kappa_growth_n),
            "delta_liq_bps" => put!(p.delta_liq_bps),
            "delta_exp_bps" => put!(p.delta_exp_bps),
            "exec_tol_bps" => put!(p.exec_tol_bps),
            "exec_rent_tol" => put!(p.exec_rent_tol),
            "fv_dev_bps" => put!(p.fv_dev_bps),
            "lump_vest_s" => put!(p.lump_vest_s),
            "lock_grace_s" => put!(p.lock_grace_s),
            "theta_a_usd_e6" => put!(p.theta_a_usd_e6),
            "theta_b_usd_e6" => put!(p.theta_b_usd_e6),
            "oracle_max_age_s" => put!(p.oracle_max_age_s),
            "oracle_stale_s" => put!(p.oracle_stale_s),
            "oracle_max_conf_bps" => put!(p.oracle_max_conf_bps),
            "oracle_max_step_bps" => put!(p.oracle_max_step_bps),
            "venue_fee_cap_bps" => put!(p.venue_fee_cap_bps),
            "venue_fee_hard_cap_bps" => put!(p.venue_fee_hard_cap_bps),
            "pool_min_real_sol" => put!(p.pool_min_real_sol),
            "pool_lock_min_bps" => put!(p.pool_lock_min_bps),
            "pool_switch_margin_bps" => put!(p.pool_switch_margin_bps),
            "pool_list_window_s" => put!(p.pool_list_window_s),
            "pool_list_margin_bps" => put!(p.pool_list_margin_bps),
            "pool_fast_route_s" => put!(p.pool_fast_route_s),
            "pool_fast_min_bps" => put!(p.pool_fast_min_bps),
            "pool_fast_window_s" => put!(p.pool_fast_window_s),
            "ema_dtmax_s" => put!(e.dtmax_s),
            "ema_clamp_bps" => put!(e.clamp_bps),
            "liq_slow_tau_s" => put!(e.slow_tau_s),
            "liq_slow_dtmax_s" => put!(e.slow_dtmax_s),
            _ => Err(MathError::InvalidInput),
        }
    }
}

impl Default for ParamSet {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Lower bound of `market_warmup_s`: `10·EMA_TAU_S` (ECON §15).
const MARKET_WARMUP_MIN_S: u64 = 10 * EMA_TAU_S as u64;

/// `pool_fast_route_s + pool_fast_window_s` may not exceed the compiled minimum of
/// `s3_wait_s` minus `S3_OBS_GAP_S` (ECON §15, R3A-11 b): 540 s.
pub const POOL_FAST_MAX_S: u64 = 540;
const _: () = assert!(POOL_FAST_MAX_S as i64 + S3_OBS_GAP_S == Rule::S3WaitS.bounds().0 as i64);

/// The compiled floor of `min_notional`, lamports (AIR-06; ECON §5.7, #77: no in-bounds value
/// may lose liquidations). An open locks `V ≥ ⌈cr_open·N_open/10⁴⌉` (D4) with `N_open ≥
/// min_notional`; at the liquidation boundary the cheapest CR-liquidatable buyback is
/// `⌊V·10⁴/cr_liq⌋ + 1` (`risk::cr_below`, `R_due = 0`), and `liquidate` / `expire` buy back
/// only while `B̂ + ert ≤ V − K_s` (`risk::buyback_allowed`) with `K_s ≤ settle_keeper_fixed +
/// ⌊V·settle_keeper_bps/10⁴⌋`. `V·(10⁴·cr_liq − 10⁸ − bps·cr_liq) ≥ (fixed + ert + 1)·10⁴·cr_liq`
/// is therefore enough, and it is hardest at the tiers' SPEC minima (governance may only raise
/// them) with the keeper terms and `exec_rent_tol` at their compiled maxima:
/// [`MIN_NOTIONAL_BUYBACK_ROOM`] = 52,631,582 lamports (Tier A), rounded up to 0.053 SOL. The
/// floor was 10⁷ (ECON §15): there a Tier A–C position had no buyback room at the boundary, so
/// `liquidate` and `expire` refused `UseSolSettle` and every exit ended in S1, which pays the
/// keeper `K_s` (up to 51 % of `V`) and the lenders `V − K_s`.
pub const MIN_NOTIONAL_FLOOR: u64 = 53_000_000;

/// The smallest notional at which every configuration inside the compiled bounds keeps buyback
/// room at the liquidation boundary in every tier ([`MIN_NOTIONAL_FLOOR`]'s derivation): the
/// largest over the tiers of `⌈(fixed + ert + 1)·10⁸·cr_liq / (cr_open·(10⁴·cr_liq − 10⁸ −
/// bps·cr_liq))⌉` at the SPEC minima and the compiled maxima of `settle_keeper_fixed`,
/// `settle_keeper_bps` and `exec_rent_tol`, lamports.
pub const MIN_NOTIONAL_BUYBACK_ROOM: u128 = {
    let fixed = Rule::SettleKeeperFixed.bounds().1 as u128;
    let bps = Rule::SettleKeeperBps.bounds().1 as u128;
    let ert = Rule::ExecRentTol.bounds().1 as u128;
    let mut floor = 0u128;
    let mut i = 0;
    while i < Tier::ALL.len() {
        let (open, liq) = spec_cr(Tier::ALL[i]);
        let (open, liq) = (open as u128, liq as u128);
        let room = 10_000 * liq - 100_000_000 - bps * liq;
        let n = ((fixed + ert + 1) * 100_000_000 * liq).div_ceil(open * room);
        if n > floor {
            floor = n;
        }
        i += 1;
    }
    floor
};
const _: () = assert!(
    MIN_NOTIONAL_BUYBACK_ROOM <= MIN_NOTIONAL_FLOOR as u128
        && MIN_NOTIONAL_FLOOR as u128 - MIN_NOTIONAL_BUYBACK_ROOM < 1_000_000
);

/// One check of [`check`]: a compiled hard bound (named after its parameter) or a
/// relational check. Codes are stable; new rules are appended. [`Rule::in_sim`] tells the
/// rules of `shortwin_sim.validate_params` ([`check_sim`] / [`validate`]) from the bounds
/// of the parameters the sim does not model ([`check_econ`] / [`validate_econ`]); the two
/// groups interleave in code order because the penalty split's parts and the per-tier
/// bounds moved into the sim after their codes were assigned (M1 doc sync).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Rule {
    // ---- compiled bounds the sim checks (`COMPILED_BOUNDS`, in its order) ----
    /// `open_fee_bps` ∈ [0, 100].
    OpenFeeBps = 0,
    /// `rent_protocol_bps` ∈ [0, 3,000].
    RentProtocolBps,
    /// `expiry_fee_bps` ∈ [0, 500].
    ExpiryFeeBps,
    /// `settle_keeper_fixed` ∈ [0, 10⁷] (sim `settle_fixed`).
    SettleKeeperFixed,
    /// `settle_keeper_bps` ∈ [0, 100] (sim `settle_bps`).
    SettleKeeperBps,
    /// `settle_keeper_cap` ∈ [0, 2.5·10⁸] (sim `settle_cap`).
    SettleKeeperCap,
    /// `util_kink_bps` ∈ [0, 9,500].
    UtilKinkBps,
    /// `util_cap_bps` ∈ [0, 9,500].
    UtilCapBps,
    /// `wallet_cap_bps` ∈ [500, 10,000].
    WalletCapBps,
    /// `s3_wait_s` ∈ [600, 86,400].
    S3WaitS,
    /// `grad_grace_s` ∈ [600, 86,400].
    GradGraceS,
    /// `curve_oi_gamma_bps` ∈ [1,000, 9,000].
    CurveOiGammaBps,
    /// `max_migration_pause_s` ∈ [600, 86,400].
    MaxMigrationPauseS,
    /// `open_ema_dev_bps` ∈ [100, 5,000].
    OpenEmaDevBps,
    /// `fee_size_cap_x100` ∈ [100, 500].
    FeeSizeCapX100,
    /// `fee_oi_cap_x100` ∈ [300, 1,000].
    FeeOiCapX100,
    /// `pool_lock_min_bps` ∈ [2,000, 10,000].
    PoolLockMinBps,
    /// `venue_fee_cap_bps` ∈ [25, 300].
    VenueFeeCapBps,
    /// `venue_fee_hard_cap_bps` ∈ [300, 2,000].
    VenueFeeHardCapBps,
    /// `fv_dev_bps` ∈ [50, 200].
    FvDevBps,
    /// `liq_slow_tau_s` ∈ [600, 86,400].
    LiqSlowTauS,
    /// `liq_slow_dtmax_s` ∈ [1, 600].
    LiqSlowDtmaxS,
    /// `ref_max_age_s` ∈ [1, 3].
    RefMaxAgeS,
    /// `liq_persist_n` ∈ [2, 3].
    LiqPersistN,
    /// `settle_persist_n` ∈ [40, 600].
    SettlePersistN,
    /// `settle_arm_gap_s` ∈ [2, 60].
    SettleArmGapS,
    /// `kappa_liq_bps` ∈ [2,500, 3,000].
    KappaLiqBps,
    /// `kappa_base_bps` ∈ [0, 1,000].
    KappaBaseBps,
    /// `kappa_growth_n` ∈ [3, 4].
    KappaGrowthN,
    /// `delta_liq_bps` ∈ [200, 500].
    DeltaLiqBps,
    /// `delta_exp_bps` ∈ [50, 300].
    DeltaExpBps,
    /// `exec_tol_bps` ∈ [0, 100].
    ExecTolBps,
    /// `exec_rent_tol` ∈ [6·10⁶, 10⁷].
    ExecRentTol,
    /// `expiry_keeper_fixed` ∈ [0, 10⁷].
    ExpiryKeeperFixed,
    /// `expiry_keeper_bps` ∈ [0, 100].
    ExpiryKeeperBps,
    /// `expiry_keeper_cap` ∈ [0, 2.5·10⁸].
    ExpiryKeeperCap,
    /// `pool_fast_route_s` ∈ [30, 420].
    PoolFastRouteS,
    /// `pool_fast_window_s` ∈ [10, 120].
    PoolFastWindowS,
    // ---- the sim's `TIER_BOUNDS` (market-level `EmaParams` here) ----
    /// `ema_dtmax_s` ∈ [1, 2].
    EmaDtmaxS,
    /// `ema_clamp_bps` ∈ [5,000, 10,000].
    EmaClampBps,
    // ---- the sim's per-tier CR checks ----
    /// Per tier: `cr_open_bps ≥ cr_liq_bps + 2,000` and `cr_liq_bps ≥ 10,500`.
    TierCrGap,
    /// Per tier: `cr_open_bps` and `cr_liq_bps` at least the tier's SPEC §4 values
    /// (v0.4.1, R4A-4).
    TierSpecCr,
    // ---- the sim's relational checks ----
    /// `util_kink_bps ≤ util_cap_bps`.
    UtilKinkLeCap,
    /// The penalty split sums to 10,000.
    PenaltySplitSum,
    /// `kappa_base_bps ≤ kappa_liq_bps`.
    KappaBaseLeLiq,
    /// `delta_liq_bps ≥ delta_exp_bps`.
    DeltaLiqGeExp,
    /// `fee_size_cap_x100 ≤ fee_oi_cap_x100`.
    FeeSizeLeOi,
    /// `pool_fast_route_s + pool_fast_window_s ≤ 540` ([`POOL_FAST_MAX_S`]).
    PoolFastWithinS3,
    /// `venue_fee_cap_bps < venue_fee_hard_cap_bps`.
    VenueFeeCapLtHard,
    /// `settle_keeper_fixed ≤ settle_keeper_cap`.
    SettleKeeperFixedLeCap,
    /// `expiry_keeper_fixed ≤ expiry_keeper_cap`.
    ExpiryKeeperFixedLeCap,
    /// `RING_LEN ≥ max(liq_persist_n, kappa_growth_n + 1)`.
    RingLenCoversPersistence,
    // ---- the penalty split's parts (the sim's `COMPILED_BOUNDS` since the M1 doc sync) ----
    /// `penalty_protocol_bps ≤ 5,000` (sim `pen_protocol_bps`).
    PenaltyProtocolBps,
    /// `penalty_keeper_bps ≤ 5,000` (sim `pen_keeper_bps`).
    PenaltyKeeperBps,
    // ---- ECON §15 bounds of parameters the sim does not model ([`check_econ`]) ----
    /// `min_notional` ∈ [[`MIN_NOTIONAL_FLOOR`], 10⁹] (5.3·10⁷ since AIR-06, derived so that
    /// every tier keeps buyback room at the liquidation boundary; ECON §15's 10⁷ did not).
    MinNotional,
    /// `market_max_notional` ∈ [10⁹, 10¹⁴].
    MarketMaxNotional,
    /// `market_creation_fee` ∈ [0, 10⁸].
    MarketCreationFee,
    /// `boost_dust_lamports` ∈ [0, 10⁹].
    BoostDustLamports,
    /// `boost_window_s` ∈ [0, 7,200].
    BoostWindowS,
    /// `market_warmup_s` ∈ [10·`EMA_TAU_S`, 3,600].
    MarketWarmupS,
    /// `ema_force_delay_s` ∈ [86,400, 2,592,000].
    EmaForceDelayS,
    /// `lump_vest_s` ∈ [3,600, 604,800].
    LumpVestS,
    /// `lock_grace_s` ∈ [3,600, 604,800].
    LockGraceS,
    /// `theta_a_usd_e6` ∈ [$50 k, $50 M].
    ThetaAUsdE6,
    /// `theta_b_usd_e6 ≥ $10 k` (and below θ_A, [`Rule::ThetaBLtA`]).
    ThetaBUsdE6,
    /// `oracle_max_age_s` ∈ [60, 600].
    OracleMaxAgeS,
    /// `oracle_stale_s` ∈ [600, 86,400].
    OracleStaleS,
    /// `oracle_max_conf_bps` ∈ [10, 1,000].
    OracleMaxConfBps,
    /// `oracle_max_step_bps` ∈ [500, 5,000].
    OracleMaxStepBps,
    /// `pool_min_real_sol` ∈ [10⁹, 10¹³].
    PoolMinRealSol,
    /// `pool_switch_margin_bps` ∈ [500, 10,000].
    PoolSwitchMarginBps,
    /// `pool_list_window_s` ∈ [0, 86,400].
    PoolListWindowS,
    /// `pool_list_margin_bps` ∈ [0, 2,500].
    PoolListMarginBps,
    /// `pool_fast_min_bps` ∈ [1,000, 10,000].
    PoolFastMinBps,
    /// `theta_b_usd_e6 < theta_a_usd_e6`.
    ThetaBLtA,
    // ---- per-tier bounds (the sim's `TIER_BOUNDS` since the M1 doc sync) ----
    /// Per tier: `cr_open_bps ≤ 100,000`.
    TierCrOpenMax,
    /// Per tier: `penalty_bps` ∈ [0, 2,000].
    TierPenaltyBps,
    /// Per tier: `max_duration_s` ∈ [3,600, 7,776,000] (sim `max_dur_s`).
    TierMaxDurationS,
    /// Per tier: `impact_pos_bps` ∈ [0, 1,000].
    TierImpactPosBps,
    /// Per tier: `impact_mkt_bps` ∈ [0, 5,000].
    TierImpactMktBps,
    /// Per tier: `max_liq_slip_bps` ∈ [500, 5,000].
    TierMaxLiqSlipBps,
    /// Per tier: `rate_base_bps ≤ 50,000`.
    TierRateBaseBps,
    /// Per tier: `rate_slope1_bps ≤ 1,000,000` (sim `rate_s1_bps`).
    TierRateSlope1Bps,
    /// Per tier: `rate_slope2_bps ≤ 1,000,000` (sim `rate_s2_bps`).
    TierRateSlope2Bps,
    /// Per tier: `min_rent_s` ∈ [0, 86,400].
    TierMinRentS,
}

impl Rule {
    /// Every rule, in code order.
    pub const ALL: [Self; 85] = [
        Self::OpenFeeBps,
        Self::RentProtocolBps,
        Self::ExpiryFeeBps,
        Self::SettleKeeperFixed,
        Self::SettleKeeperBps,
        Self::SettleKeeperCap,
        Self::UtilKinkBps,
        Self::UtilCapBps,
        Self::WalletCapBps,
        Self::S3WaitS,
        Self::GradGraceS,
        Self::CurveOiGammaBps,
        Self::MaxMigrationPauseS,
        Self::OpenEmaDevBps,
        Self::FeeSizeCapX100,
        Self::FeeOiCapX100,
        Self::PoolLockMinBps,
        Self::VenueFeeCapBps,
        Self::VenueFeeHardCapBps,
        Self::FvDevBps,
        Self::LiqSlowTauS,
        Self::LiqSlowDtmaxS,
        Self::RefMaxAgeS,
        Self::LiqPersistN,
        Self::SettlePersistN,
        Self::SettleArmGapS,
        Self::KappaLiqBps,
        Self::KappaBaseBps,
        Self::KappaGrowthN,
        Self::DeltaLiqBps,
        Self::DeltaExpBps,
        Self::ExecTolBps,
        Self::ExecRentTol,
        Self::ExpiryKeeperFixed,
        Self::ExpiryKeeperBps,
        Self::ExpiryKeeperCap,
        Self::PoolFastRouteS,
        Self::PoolFastWindowS,
        Self::EmaDtmaxS,
        Self::EmaClampBps,
        Self::TierCrGap,
        Self::TierSpecCr,
        Self::UtilKinkLeCap,
        Self::PenaltySplitSum,
        Self::KappaBaseLeLiq,
        Self::DeltaLiqGeExp,
        Self::FeeSizeLeOi,
        Self::PoolFastWithinS3,
        Self::VenueFeeCapLtHard,
        Self::SettleKeeperFixedLeCap,
        Self::ExpiryKeeperFixedLeCap,
        Self::RingLenCoversPersistence,
        Self::PenaltyProtocolBps,
        Self::PenaltyKeeperBps,
        Self::MinNotional,
        Self::MarketMaxNotional,
        Self::MarketCreationFee,
        Self::BoostDustLamports,
        Self::BoostWindowS,
        Self::MarketWarmupS,
        Self::EmaForceDelayS,
        Self::LumpVestS,
        Self::LockGraceS,
        Self::ThetaAUsdE6,
        Self::ThetaBUsdE6,
        Self::OracleMaxAgeS,
        Self::OracleStaleS,
        Self::OracleMaxConfBps,
        Self::OracleMaxStepBps,
        Self::PoolMinRealSol,
        Self::PoolSwitchMarginBps,
        Self::PoolListWindowS,
        Self::PoolListMarginBps,
        Self::PoolFastMinBps,
        Self::ThetaBLtA,
        Self::TierCrOpenMax,
        Self::TierPenaltyBps,
        Self::TierMaxDurationS,
        Self::TierImpactPosBps,
        Self::TierImpactMktBps,
        Self::TierMaxLiqSlipBps,
        Self::TierRateBaseBps,
        Self::TierRateSlope1Bps,
        Self::TierRateSlope2Bps,
        Self::TierMinRentS,
    ];

    /// Stable numeric code.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// The parameter a bound constrains (its ECON §15 / [`ParamSet::get`] name), or a
    /// short snake_case name for a relational check.
    pub const fn name(self) -> &'static str {
        match self {
            Self::OpenFeeBps => "open_fee_bps",
            Self::RentProtocolBps => "rent_protocol_bps",
            Self::ExpiryFeeBps => "expiry_fee_bps",
            Self::SettleKeeperFixed => "settle_keeper_fixed",
            Self::SettleKeeperBps => "settle_keeper_bps",
            Self::SettleKeeperCap => "settle_keeper_cap",
            Self::UtilKinkBps => "util_kink_bps",
            Self::UtilCapBps => "util_cap_bps",
            Self::WalletCapBps => "wallet_cap_bps",
            Self::S3WaitS => "s3_wait_s",
            Self::GradGraceS => "grad_grace_s",
            Self::CurveOiGammaBps => "curve_oi_gamma_bps",
            Self::MaxMigrationPauseS => "max_migration_pause_s",
            Self::OpenEmaDevBps => "open_ema_dev_bps",
            Self::FeeSizeCapX100 => "fee_size_cap_x100",
            Self::FeeOiCapX100 => "fee_oi_cap_x100",
            Self::PoolLockMinBps => "pool_lock_min_bps",
            Self::VenueFeeCapBps => "venue_fee_cap_bps",
            Self::VenueFeeHardCapBps => "venue_fee_hard_cap_bps",
            Self::FvDevBps => "fv_dev_bps",
            Self::LiqSlowTauS => "liq_slow_tau_s",
            Self::LiqSlowDtmaxS => "liq_slow_dtmax_s",
            Self::RefMaxAgeS => "ref_max_age_s",
            Self::LiqPersistN => "liq_persist_n",
            Self::SettlePersistN => "settle_persist_n",
            Self::SettleArmGapS => "settle_arm_gap_s",
            Self::KappaLiqBps => "kappa_liq_bps",
            Self::KappaBaseBps => "kappa_base_bps",
            Self::KappaGrowthN => "kappa_growth_n",
            Self::DeltaLiqBps => "delta_liq_bps",
            Self::DeltaExpBps => "delta_exp_bps",
            Self::ExecTolBps => "exec_tol_bps",
            Self::ExecRentTol => "exec_rent_tol",
            Self::ExpiryKeeperFixed => "expiry_keeper_fixed",
            Self::ExpiryKeeperBps => "expiry_keeper_bps",
            Self::ExpiryKeeperCap => "expiry_keeper_cap",
            Self::PoolFastRouteS => "pool_fast_route_s",
            Self::PoolFastWindowS => "pool_fast_window_s",
            Self::EmaDtmaxS => "ema_dtmax_s",
            Self::EmaClampBps => "ema_clamp_bps",
            Self::TierCrGap => "tier_cr_gap",
            Self::TierSpecCr => "tier_spec_cr",
            Self::UtilKinkLeCap => "util_kink_le_cap",
            Self::PenaltySplitSum => "penalty_split_sum",
            Self::KappaBaseLeLiq => "kappa_base_le_liq",
            Self::DeltaLiqGeExp => "delta_liq_ge_exp",
            Self::FeeSizeLeOi => "fee_size_le_oi",
            Self::PoolFastWithinS3 => "pool_fast_within_s3",
            Self::VenueFeeCapLtHard => "venue_fee_cap_lt_hard",
            Self::SettleKeeperFixedLeCap => "settle_keeper_fixed_le_cap",
            Self::ExpiryKeeperFixedLeCap => "expiry_keeper_fixed_le_cap",
            Self::RingLenCoversPersistence => "ring_len_covers_persistence",
            Self::PenaltyProtocolBps => "penalty_protocol_bps",
            Self::PenaltyKeeperBps => "penalty_keeper_bps",
            Self::MinNotional => "min_notional",
            Self::MarketMaxNotional => "market_max_notional",
            Self::MarketCreationFee => "market_creation_fee",
            Self::BoostDustLamports => "boost_dust_lamports",
            Self::BoostWindowS => "boost_window_s",
            Self::MarketWarmupS => "market_warmup_s",
            Self::EmaForceDelayS => "ema_force_delay_s",
            Self::LumpVestS => "lump_vest_s",
            Self::LockGraceS => "lock_grace_s",
            Self::ThetaAUsdE6 => "theta_a_usd_e6",
            Self::ThetaBUsdE6 => "theta_b_usd_e6",
            Self::OracleMaxAgeS => "oracle_max_age_s",
            Self::OracleStaleS => "oracle_stale_s",
            Self::OracleMaxConfBps => "oracle_max_conf_bps",
            Self::OracleMaxStepBps => "oracle_max_step_bps",
            Self::PoolMinRealSol => "pool_min_real_sol",
            Self::PoolSwitchMarginBps => "pool_switch_margin_bps",
            Self::PoolListWindowS => "pool_list_window_s",
            Self::PoolListMarginBps => "pool_list_margin_bps",
            Self::PoolFastMinBps => "pool_fast_min_bps",
            Self::ThetaBLtA => "theta_b_lt_a",
            Self::TierCrOpenMax => "cr_open_bps",
            Self::TierPenaltyBps => "penalty_bps",
            Self::TierMaxDurationS => "max_duration_s",
            Self::TierImpactPosBps => "impact_pos_bps",
            Self::TierImpactMktBps => "impact_mkt_bps",
            Self::TierMaxLiqSlipBps => "max_liq_slip_bps",
            Self::TierRateBaseBps => "rate_base_bps",
            Self::TierRateSlope1Bps => "rate_slope1_bps",
            Self::TierRateSlope2Bps => "rate_slope2_bps",
            Self::TierMinRentS => "min_rent_s",
        }
    }

    /// Whether `shortwin_sim.validate_params` makes this check: [`check_sim`] /
    /// [`validate`] if so; [`check_econ`] / [`validate_econ`] (a bound of a parameter the
    /// sim does not model) otherwise.
    pub const fn in_sim(self) -> bool {
        !matches!(
            self,
            Self::MinNotional
                | Self::MarketMaxNotional
                | Self::MarketCreationFee
                | Self::BoostDustLamports
                | Self::BoostWindowS
                | Self::MarketWarmupS
                | Self::EmaForceDelayS
                | Self::LumpVestS
                | Self::LockGraceS
                | Self::ThetaAUsdE6
                | Self::ThetaBUsdE6
                | Self::OracleMaxAgeS
                | Self::OracleStaleS
                | Self::OracleMaxConfBps
                | Self::OracleMaxStepBps
                | Self::PoolMinRealSol
                | Self::PoolSwitchMarginBps
                | Self::PoolListWindowS
                | Self::PoolListMarginBps
                | Self::PoolFastMinBps
                | Self::ThetaBLtA
        )
    }

    /// Whether the rule is checked once per tier ([`Violation::tier`] is set).
    pub const fn per_tier(self) -> bool {
        matches!(
            self,
            Self::TierCrGap
                | Self::TierSpecCr
                | Self::TierCrOpenMax
                | Self::TierPenaltyBps
                | Self::TierMaxDurationS
                | Self::TierImpactPosBps
                | Self::TierImpactMktBps
                | Self::TierMaxLiqSlipBps
                | Self::TierRateBaseBps
                | Self::TierRateSlope1Bps
                | Self::TierRateSlope2Bps
                | Self::TierMinRentS
        )
    }

    /// The inclusive compiled hard bounds `(lo, hi)` of a bound rule; `(0, u64::MAX)` for
    /// a relational check or a tier-dependent bound ([`Rule::TierCrGap`],
    /// [`Rule::TierSpecCr`]).
    pub const fn bounds(self) -> (u64, u64) {
        match self {
            Self::OpenFeeBps => (0, 100),
            Self::RentProtocolBps => (0, 3_000),
            Self::ExpiryFeeBps => (0, 500),
            Self::SettleKeeperFixed => (0, 10_000_000),
            Self::SettleKeeperBps => (0, 100),
            Self::SettleKeeperCap => (0, 250_000_000),
            Self::UtilKinkBps | Self::UtilCapBps => (0, 9_500),
            Self::WalletCapBps => (500, 10_000),
            Self::S3WaitS | Self::GradGraceS | Self::MaxMigrationPauseS => (600, 86_400),
            Self::CurveOiGammaBps => (1_000, 9_000),
            Self::OpenEmaDevBps => (100, 5_000),
            Self::FeeSizeCapX100 => (100, 500),
            Self::FeeOiCapX100 => (300, 1_000),
            Self::PoolLockMinBps => (2_000, 10_000),
            Self::VenueFeeCapBps => (25, 300),
            Self::VenueFeeHardCapBps => (300, 2_000),
            Self::FvDevBps => (50, 200),
            Self::LiqSlowTauS => (600, 86_400),
            Self::LiqSlowDtmaxS => (1, 600),
            Self::RefMaxAgeS => (1, 3),
            Self::LiqPersistN => (2, 3),
            Self::SettlePersistN => (40, 600),
            Self::SettleArmGapS => (2, 60),
            Self::KappaLiqBps => (2_500, 3_000),
            Self::KappaBaseBps => (0, 1_000),
            Self::KappaGrowthN => (3, 4),
            Self::DeltaLiqBps => (200, 500),
            Self::DeltaExpBps => (50, 300),
            Self::ExecTolBps => (0, 100),
            Self::ExecRentTol => (6_000_000, 10_000_000),
            Self::ExpiryKeeperFixed => (0, 10_000_000),
            Self::ExpiryKeeperBps => (0, 100),
            Self::ExpiryKeeperCap => (0, 250_000_000),
            Self::PoolFastRouteS => (30, 420),
            Self::PoolFastWindowS => (10, 120),
            Self::EmaDtmaxS => (1, 2),
            Self::EmaClampBps => (5_000, 10_000),
            Self::PenaltyProtocolBps | Self::PenaltyKeeperBps => (0, 5_000),
            Self::MinNotional => (MIN_NOTIONAL_FLOOR, 1_000_000_000),
            Self::MarketMaxNotional => (1_000_000_000, 100_000_000_000_000),
            Self::MarketCreationFee => (0, 100_000_000),
            Self::BoostDustLamports => (0, 1_000_000_000),
            Self::BoostWindowS => (0, 7_200),
            Self::MarketWarmupS => (MARKET_WARMUP_MIN_S, 3_600),
            Self::EmaForceDelayS => (86_400, 2_592_000),
            Self::LumpVestS | Self::LockGraceS => (3_600, 604_800),
            Self::ThetaAUsdE6 => (50_000_000_000, 50_000_000_000_000),
            Self::ThetaBUsdE6 => (10_000_000_000, u64::MAX),
            Self::OracleMaxAgeS => (60, 600),
            Self::OracleStaleS => (600, 86_400),
            Self::OracleMaxConfBps => (10, 1_000),
            Self::OracleMaxStepBps => (500, 5_000),
            Self::PoolMinRealSol => (1_000_000_000, 10_000_000_000_000),
            Self::PoolSwitchMarginBps => (500, 10_000),
            Self::PoolListWindowS => (0, 86_400),
            Self::PoolListMarginBps => (0, 2_500),
            Self::PoolFastMinBps => (1_000, 10_000),
            Self::TierCrOpenMax => (0, 100_000),
            Self::TierPenaltyBps => (0, 2_000),
            Self::TierMaxDurationS => (3_600, 7_776_000),
            Self::TierImpactPosBps => (0, 1_000),
            Self::TierImpactMktBps => (0, 5_000),
            Self::TierMaxLiqSlipBps => (500, 5_000),
            Self::TierRateBaseBps => (0, 50_000),
            Self::TierRateSlope1Bps | Self::TierRateSlope2Bps => (0, 1_000_000),
            Self::TierMinRentS => (0, 86_400),
            Self::TierCrGap
            | Self::TierSpecCr
            | Self::UtilKinkLeCap
            | Self::PenaltySplitSum
            | Self::KappaBaseLeLiq
            | Self::DeltaLiqGeExp
            | Self::FeeSizeLeOi
            | Self::PoolFastWithinS3
            | Self::VenueFeeCapLtHard
            | Self::SettleKeeperFixedLeCap
            | Self::ExpiryKeeperFixedLeCap
            | Self::RingLenCoversPersistence
            | Self::ThetaBLtA => (0, u64::MAX),
        }
    }
}

/// A failed check: the rule and, for a per-tier rule, the tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Violation {
    /// The rule that failed.
    pub rule: Rule,
    /// The tier, for [`Rule::per_tier`] rules.
    pub tier: Option<Tier>,
}

impl From<Violation> for MathError {
    fn from(_: Violation) -> Self {
        Self::InvalidInput
    }
}

/// The SPEC §4 opening and liquidation CRs of a tier, its compiled minima since v0.4.1
/// (sim `SPEC_CR`).
pub const fn spec_cr(tier: Tier) -> (u32, u32) {
    let d = TierParams::DEFAULTS[tier.index()];
    (d.cr_open_bps, d.cr_liq_bps)
}

/// Widens a field for the bound checks.
fn wide<T: Into<u64>>(v: T) -> u64 {
    v.into()
}

/// Narrows a value into a field's type (`InvalidInput` if it does not fit).
fn fit<T: TryFrom<u64>>(v: u64) -> Result<T, MathError> {
    T::try_from(v).map_err(|_| MathError::InvalidInput)
}

/// Reports `rule` (for `tier`) unless `ok`.
fn expect<F: FnMut(Violation)>(report: &mut F, rule: Rule, tier: Option<Tier>, ok: bool) {
    if !ok {
        report(Violation { rule, tier });
    }
}

/// Reports a bound rule unless `v` lies inside its inclusive [`Rule::bounds`].
fn bounded<F: FnMut(Violation)>(report: &mut F, rule: Rule, tier: Option<Tier>, v: u64) {
    let (lo, hi) = rule.bounds();
    expect(report, rule, tier, lo <= v && v <= hi);
}

/// Calls `report` once for every violated rule of `shortwin_sim.validate_params`
/// ([`Rule::in_sim`]), in the sim's order: its `COMPILED_BOUNDS`, its `TIER_BOUNDS` (the
/// EMA parameters are market-level here and reported once, without a tier, with the
/// global bounds; the other per-tier bounds tier by tier, each tier's before its CR
/// checks), the per-tier CR checks, then the relational checks.
pub fn check_sim<F: FnMut(Violation)>(ps: &ParamSet, mut report: F) {
    let (p, e) = (&ps.params, &ps.ema);
    for (rule, v) in [
        (Rule::OpenFeeBps, wide(p.open_fee_bps)),
        (Rule::RentProtocolBps, wide(p.rent_protocol_bps)),
        (Rule::PenaltyProtocolBps, wide(p.penalty_protocol_bps)),
        (Rule::PenaltyKeeperBps, wide(p.penalty_keeper_bps)),
        (Rule::ExpiryFeeBps, wide(p.expiry_fee_bps)),
        (Rule::SettleKeeperFixed, p.settle_keeper_fixed),
        (Rule::SettleKeeperBps, wide(p.settle_keeper_bps)),
        (Rule::SettleKeeperCap, p.settle_keeper_cap),
        (Rule::UtilKinkBps, wide(p.util_kink_bps)),
        (Rule::UtilCapBps, wide(p.util_cap_bps)),
        (Rule::WalletCapBps, wide(p.wallet_cap_bps)),
        (Rule::S3WaitS, wide(p.s3_wait_s)),
        (Rule::GradGraceS, wide(p.grad_grace_s)),
        (Rule::CurveOiGammaBps, wide(p.curve_oi_gamma_bps)),
        (Rule::MaxMigrationPauseS, wide(p.max_migration_pause_s)),
        (Rule::OpenEmaDevBps, wide(p.open_ema_dev_bps)),
        (Rule::FeeSizeCapX100, wide(p.fee_size_cap_x100)),
        (Rule::FeeOiCapX100, wide(p.fee_oi_cap_x100)),
        (Rule::PoolLockMinBps, wide(p.pool_lock_min_bps)),
        (Rule::VenueFeeCapBps, wide(p.venue_fee_cap_bps)),
        (Rule::VenueFeeHardCapBps, wide(p.venue_fee_hard_cap_bps)),
        (Rule::FvDevBps, wide(p.fv_dev_bps)),
        (Rule::LiqSlowTauS, wide(e.slow_tau_s)),
        (Rule::LiqSlowDtmaxS, wide(e.slow_dtmax_s)),
        (Rule::RefMaxAgeS, wide(p.ref_max_age_s)),
        (Rule::LiqPersistN, wide(p.liq_persist_n)),
        (Rule::SettlePersistN, wide(p.settle_persist_n)),
        (Rule::SettleArmGapS, wide(p.settle_arm_gap_s)),
        (Rule::KappaLiqBps, wide(p.kappa_liq_bps)),
        (Rule::KappaBaseBps, wide(p.kappa_base_bps)),
        (Rule::KappaGrowthN, wide(p.kappa_growth_n)),
        (Rule::DeltaLiqBps, wide(p.delta_liq_bps)),
        (Rule::DeltaExpBps, wide(p.delta_exp_bps)),
        (Rule::ExecTolBps, wide(p.exec_tol_bps)),
        (Rule::ExecRentTol, p.exec_rent_tol),
        (Rule::ExpiryKeeperFixed, p.expiry_keeper_fixed),
        (Rule::ExpiryKeeperBps, wide(p.expiry_keeper_bps)),
        (Rule::ExpiryKeeperCap, p.expiry_keeper_cap),
        (Rule::PoolFastRouteS, wide(p.pool_fast_route_s)),
        (Rule::PoolFastWindowS, wide(p.pool_fast_window_s)),
        (Rule::EmaDtmaxS, wide(e.dtmax_s)),
        (Rule::EmaClampBps, wide(e.clamp_bps)),
    ] {
        bounded(&mut report, rule, None, v);
    }
    for tier in Tier::ALL {
        let t = &ps.tiers[tier.index()];
        for (rule, v) in [
            (Rule::TierCrOpenMax, t.cr_open_bps),
            (Rule::TierPenaltyBps, t.penalty_bps),
            (Rule::TierMaxDurationS, t.max_duration_s),
            (Rule::TierImpactPosBps, t.impact_pos_bps),
            (Rule::TierImpactMktBps, t.impact_mkt_bps),
            (Rule::TierMaxLiqSlipBps, t.max_liq_slip_bps),
            (Rule::TierRateBaseBps, t.rate.base_bps),
            (Rule::TierRateSlope1Bps, t.rate.slope1_bps),
            (Rule::TierRateSlope2Bps, t.rate.slope2_bps),
            (Rule::TierMinRentS, t.min_rent_s),
        ] {
            bounded(&mut report, rule, Some(tier), wide(v));
        }
        let (cr_open, cr_liq) = (u64::from(t.cr_open_bps), u64::from(t.cr_liq_bps));
        expect(
            &mut report,
            Rule::TierCrGap,
            Some(tier),
            cr_open >= cr_liq.saturating_add(2_000) && cr_liq >= 10_500,
        );
        let (spec_open, spec_liq) = spec_cr(tier);
        expect(
            &mut report,
            Rule::TierSpecCr,
            Some(tier),
            t.cr_open_bps >= spec_open && t.cr_liq_bps >= spec_liq,
        );
    }
    let pen_sum = u64::from(p.penalty_lender_bps)
        .saturating_add(u64::from(p.penalty_protocol_bps))
        .saturating_add(u64::from(p.penalty_keeper_bps));
    let fast = u64::from(p.pool_fast_route_s).saturating_add(u64::from(p.pool_fast_window_s));
    let ring_need =
        usize::from(p.liq_persist_n).max(usize::from(p.kappa_growth_n).saturating_add(1));
    for (rule, ok) in [
        (Rule::UtilKinkLeCap, p.util_kink_bps <= p.util_cap_bps),
        (Rule::PenaltySplitSum, pen_sum == 10_000),
        (Rule::KappaBaseLeLiq, p.kappa_base_bps <= p.kappa_liq_bps),
        (Rule::DeltaLiqGeExp, p.delta_liq_bps >= p.delta_exp_bps),
        (Rule::FeeSizeLeOi, p.fee_size_cap_x100 <= p.fee_oi_cap_x100),
        (Rule::PoolFastWithinS3, fast <= POOL_FAST_MAX_S),
        (
            Rule::VenueFeeCapLtHard,
            p.venue_fee_cap_bps < p.venue_fee_hard_cap_bps,
        ),
        (
            Rule::SettleKeeperFixedLeCap,
            p.settle_keeper_fixed <= p.settle_keeper_cap,
        ),
        (
            Rule::ExpiryKeeperFixedLeCap,
            p.expiry_keeper_fixed <= p.expiry_keeper_cap,
        ),
        (Rule::RingLenCoversPersistence, RING_LEN >= ring_need),
    ] {
        expect(&mut report, rule, None, ok);
    }
}

/// Calls `report` once for every violated ECON §15 rule on a parameter
/// `shortwin_sim.validate_params` does not model (`!`[`Rule::in_sim`]; see the module
/// docs), in [`Rule`] code order: the bounds, then `θ_B < θ_A`.
pub fn check_econ<F: FnMut(Violation)>(ps: &ParamSet, mut report: F) {
    let p = &ps.params;
    for (rule, v) in [
        (Rule::MinNotional, p.min_notional),
        (Rule::MarketMaxNotional, p.market_max_notional),
        (Rule::MarketCreationFee, p.market_creation_fee),
        (Rule::BoostDustLamports, p.boost_dust_lamports),
        (Rule::BoostWindowS, wide(p.boost_window_s)),
        (Rule::MarketWarmupS, wide(p.market_warmup_s)),
        (Rule::EmaForceDelayS, wide(p.ema_force_delay_s)),
        (Rule::LumpVestS, wide(p.lump_vest_s)),
        (Rule::LockGraceS, wide(p.lock_grace_s)),
        (Rule::ThetaAUsdE6, p.theta_a_usd_e6),
        (Rule::ThetaBUsdE6, p.theta_b_usd_e6),
        (Rule::OracleMaxAgeS, wide(p.oracle_max_age_s)),
        (Rule::OracleStaleS, wide(p.oracle_stale_s)),
        (Rule::OracleMaxConfBps, wide(p.oracle_max_conf_bps)),
        (Rule::OracleMaxStepBps, wide(p.oracle_max_step_bps)),
        (Rule::PoolMinRealSol, p.pool_min_real_sol),
        (Rule::PoolSwitchMarginBps, wide(p.pool_switch_margin_bps)),
        (Rule::PoolListWindowS, wide(p.pool_list_window_s)),
        (Rule::PoolListMarginBps, wide(p.pool_list_margin_bps)),
        (Rule::PoolFastMinBps, wide(p.pool_fast_min_bps)),
    ] {
        bounded(&mut report, rule, None, v);
    }
    expect(
        &mut report,
        Rule::ThetaBLtA,
        None,
        p.theta_b_usd_e6 < p.theta_a_usd_e6,
    );
}

/// Calls `report` once for every violated rule: [`check_sim`], then [`check_econ`].
pub fn check<F: FnMut(Violation)>(ps: &ParamSet, mut report: F) {
    check_sim(ps, &mut report);
    check_econ(ps, &mut report);
}

/// `shortwin_sim.validate_params` (ARCH §2.3.3, §9 M0b; ECON §15; A8): `Ok` exactly when
/// the sim reports no violation, else the first violation [`check_sim`] reports.
/// `initialize_config` and `propose_config` run it, and [`validate_econ`], on the
/// would-be configuration.
pub fn validate(ps: &ParamSet) -> Result<(), Violation> {
    let mut first = None;
    check_sim(ps, |v| {
        first.get_or_insert(v);
    });
    first.map_or(Ok(()), Err)
}

/// The ECON §15 bounds of the parameters `shortwin_sim` does not model (module docs): `Ok`
/// if none is violated, else the first violation [`check_econ`] reports. The program runs
/// it after [`validate`] (ARCH §2.3.3, §3.1: every parameter inside its ECON §15 hard
/// bounds).
pub fn validate_econ(ps: &ParamSet) -> Result<(), Violation> {
    let mut first = None;
    check_econ(ps, |v| {
        first.get_or_insert(v);
    });
    first.map_or(Ok(()), Err)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn violations(ps: &ParamSet) -> Vec<Violation> {
        let mut out = Vec::new();
        check(ps, |v| out.push(v));
        out
    }

    #[test]
    fn defaults_validate() {
        assert_eq!(validate(&ParamSet::DEFAULT), Ok(()));
        assert_eq!(validate_econ(&ParamSet::DEFAULT), Ok(()));
        assert!(violations(&ParamSet::DEFAULT).is_empty());
    }

    /// AIR-06 (ECON §5.7, #77: no in-bounds value loses liquidations): at the compiled floor of
    /// `min_notional`, with the keeper terms and `exec_rent_tol` at their compiled maxima and
    /// every tier at its SPEC minima (its worst in-bounds CRs), the smallest vault an open can
    /// lock (`⌈cr_open·min_notional/10⁴⌉`, D4) and every larger one still pays the cheapest
    /// buyback that is CR-liquidatable (`risk::cr_below` at `cr_liq`), `ert` and `K_s`
    /// (`risk::buyback_allowed`). As built the floor was 10⁷: Tiers A–C had no such room, so
    /// `liquidate` and `expire` refused `UseSolSettle` and every exit ended in S1.
    #[test]
    fn min_notional_floor_keeps_buyback_room_at_the_liquidation_boundary() {
        use crate::risk::{self, Budget};
        // The derivation's figure: Tier A binds (B 32,095,099, C 20,618,558, D 11,945,394).
        assert_eq!(MIN_NOTIONAL_BUYBACK_ROOM, 52_631_582);
        let n = Rule::MinNotional.bounds().0;
        let terms = SettleKeeperTerms {
            fixed: Rule::SettleKeeperFixed.bounds().1,
            bps: u32::try_from(Rule::SettleKeeperBps.bounds().1).unwrap(),
            cap: Rule::SettleKeeperCap.bounds().1,
        };
        let ert = Rule::ExecRentTol.bounds().1;
        for tier in Tier::ALL {
            let (cr_open, cr_liq) = spec_cr(tier);
            let v0 = (u64::from(cr_open) * n).div_ceil(10_000);
            for v in (v0..v0 + 20_000).chain([v0 + 1_000_000, 2 * v0, 10 * v0]) {
                // The cheapest buyback at which the position is CR-liquidatable (R_due = 0).
                let b = v * 10_000 / u64::from(cr_liq) + 1;
                assert!(risk::cr_below(v, Some(b), cr_liq).unwrap());
                assert!(!risk::cr_below(v, Some(b - 1), cr_liq).unwrap());
                let budget = Budget {
                    v,
                    k_s: risk::k_s(v, &terms).unwrap(),
                    ert,
                };
                assert!(
                    risk::buyback_allowed(&budget, Some(b), 0, u32::MAX).unwrap(),
                    "{tier:?}: no buyback room at the liquidation boundary, V = {v}"
                );
            }
        }
        // The floor is what validate_econ enforces.
        let mut ps = ParamSet::DEFAULT;
        ps.params.min_notional = n;
        assert_eq!(validate_econ(&ps), Ok(()));
        ps.params.min_notional = n - 1;
        assert_eq!(
            validate_econ(&ps),
            Err(Violation {
                rule: Rule::MinNotional,
                tier: None
            })
        );
    }

    /// `validate` makes exactly the sim's rules and `validate_econ` exactly the others
    /// (M0B-S-1; the M1 doc sync moved the split's parts and the per-tier bounds into the
    /// sim), for every rule pushed past each of its bounds.
    #[test]
    fn validate_is_the_sims_rule_set() {
        for rule in Rule::ALL {
            let (lo, hi) = rule.bounds();
            if (lo, hi) == (0, u64::MAX) {
                continue;
            }
            let tier = rule.per_tier().then_some(Tier::B);
            for v in [lo.checked_sub(1), hi.checked_add(1)].into_iter().flatten() {
                let mut ps = ParamSet::DEFAULT;
                if ps.set(rule.name(), tier, v).is_err() {
                    continue;
                }
                let (mut sim, mut econ) = (Vec::new(), Vec::new());
                check_sim(&ps, |x| sim.push(x));
                check_econ(&ps, |x| econ.push(x));
                assert!(sim.iter().all(|x| x.rule.in_sim()), "{rule:?} {v}");
                assert!(econ.iter().all(|x| !x.rule.in_sim()), "{rule:?} {v}");
                let hit = Violation { rule, tier };
                assert_eq!(sim.contains(&hit), rule.in_sim(), "{rule:?} {v}");
                assert_eq!(econ.contains(&hit), !rule.in_sim(), "{rule:?} {v}");
                assert_eq!(validate(&ps).err(), sim.first().copied());
                assert_eq!(validate_econ(&ps).err(), econ.first().copied());
                assert_eq!(violations(&ps), [sim, econ].concat());
            }
        }
        // The review's reproducer (M0B-S-1): a keeper share above ECON's 5,000, which the
        // v0.4.1 sim accepted; the sim and `validate` refuse it now.
        let mut ps = ParamSet::DEFAULT;
        ps.params.penalty_lender_bps = 3_198;
        ps.params.penalty_protocol_bps = 166;
        ps.params.penalty_keeper_bps = 6_636;
        assert_eq!(
            validate(&ps),
            Err(Violation {
                rule: Rule::PenaltyKeeperBps,
                tier: None
            })
        );
        assert_eq!(validate_econ(&ps), Ok(()));
    }

    #[test]
    fn rule_table_is_consistent() {
        for (k, r) in Rule::ALL.iter().enumerate() {
            assert_eq!(usize::from(r.code()), k);
            assert!(!r.name().is_empty());
        }
        let names: std::collections::BTreeSet<_> =
            Rule::ALL.iter().map(|r| (r.name(), r.per_tier())).collect();
        assert_eq!(names.len(), Rule::ALL.len());
        assert_eq!(Rule::ALL.iter().filter(|r| r.in_sim()).count(), 64);
        // Every per-tier rule is the sim's; every rule the sim does not make bounds a
        // global parameter.
        assert!(
            Rule::ALL
                .iter()
                .filter(|r| r.per_tier())
                .all(|r| r.in_sim())
        );
    }

    /// Every bound rule of a parameter: `lo − 1` and `hi + 1` fail it, `lo` and `hi` pass it,
    /// through [`ParamSet::set`] by name.
    #[test]
    fn every_bound_by_name() {
        for rule in Rule::ALL {
            let (lo, hi) = rule.bounds();
            if (lo, hi) == (0, u64::MAX) {
                continue;
            }
            let tiers: Vec<Option<Tier>> = if rule.per_tier() {
                Tier::ALL.iter().copied().map(Some).collect()
            } else {
                std::vec![None]
            };
            for tier in tiers {
                let hits = |v: u64| {
                    let mut ps = ParamSet::DEFAULT;
                    ps.set(rule.name(), tier, v).ok()?;
                    assert_eq!(ps.get(rule.name(), tier), Some(v));
                    Some(violations(&ps).contains(&Violation { rule, tier }))
                };
                assert_eq!(hits(lo), Some(false), "{rule:?} {tier:?} lo");
                assert!(!hits(hi).unwrap_or(false), "{rule:?} {tier:?} hi");
                if lo > 0 {
                    assert_eq!(hits(lo - 1), Some(true), "{rule:?} {tier:?} lo - 1");
                }
                if let Some(v) = hi.checked_add(1) {
                    assert_ne!(hits(v), Some(false), "{rule:?} {tier:?} hi + 1");
                }
            }
        }
    }

    #[test]
    fn relational_checks() {
        let mut ps = ParamSet::DEFAULT;
        ps.params.theta_b_usd_e6 = ps.params.theta_a_usd_e6;
        assert_eq!(validate(&ps), Ok(()));
        assert_eq!(
            validate_econ(&ps),
            Err(Violation {
                rule: Rule::ThetaBLtA,
                tier: None
            })
        );
        // The compiled maxima (420 + 120) meet the relation exactly; one more second fails
        // both the bound and the relation.
        let mut ps = ParamSet::DEFAULT;
        ps.params.pool_fast_route_s = 420;
        ps.params.pool_fast_window_s = 120;
        assert_eq!(validate(&ps), Ok(()));
        ps.params.pool_fast_route_s = 421;
        assert_eq!(
            violations(&ps),
            [
                Violation {
                    rule: Rule::PoolFastRouteS,
                    tier: None
                },
                Violation {
                    rule: Rule::PoolFastWithinS3,
                    tier: None
                }
            ]
        );
        let mut ps = ParamSet::DEFAULT;
        ps.tiers[Tier::B.index()].cr_liq_bps = 13_499;
        assert_eq!(
            violations(&ps),
            [Violation {
                rule: Rule::TierSpecCr,
                tier: Some(Tier::B)
            }]
        );
        ps.tiers[Tier::B.index()].cr_liq_bps = 23_001;
        assert_eq!(
            violations(&ps),
            [Violation {
                rule: Rule::TierCrGap,
                tier: Some(Tier::B)
            }]
        );
        let mut ps = ParamSet::DEFAULT;
        ps.params.kappa_growth_n = 8;
        assert_eq!(
            violations(&ps),
            [
                Violation {
                    rule: Rule::KappaGrowthN,
                    tier: None
                },
                Violation {
                    rule: Rule::RingLenCoversPersistence,
                    tier: None
                }
            ]
        );
    }

    #[test]
    fn projections() {
        let p = Params::DEFAULT;
        assert_eq!(p.exit_params(), ExitParams::DEFAULT);
        assert_eq!(p.settle_keeper(), SettleKeeperTerms::DEFAULT);
        assert_eq!(p.expiry_keeper(), ExpiryKeeperTerms::DEFAULT);
        assert_eq!(p.penalty_split(), PenaltySplit::DEFAULT);
        assert_eq!(p.s3_terms(), S3Terms::DEFAULT);
        let t = ParamSet::DEFAULT.settle_terms(Tier::D);
        assert_eq!(
            (t.cr_liq_bps, t.sigma_max_bps, t.penalty_bps),
            (17_500, 3_000, 1_000)
        );
        let mut ps = ParamSet::DEFAULT;
        assert_eq!(ps.set("nope", None, 1), Err(MathError::InvalidInput));
        assert_eq!(ps.set("cr_liq_bps", None, 1), Err(MathError::InvalidInput));
        assert_eq!(
            ps.set("liq_persist_n", None, 256),
            Err(MathError::InvalidInput)
        );
        assert_eq!(ps.get("open_fee_bps", Some(Tier::A)), None);
    }
}
