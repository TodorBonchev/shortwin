//! `ShortPosition` (Borsh `Account<T>`, seeds `["position", market, borrower, nonce u64 LE]`,
//! 384 B; ARCH §2.3.5). Written by `open_short` (M2); M1 only defines the layout.
//!
//! Position SOL is extra lamports on this account: `lamports ≥ rent_min(384) + v` (I2, A4).
//! Everything in the four **locked** groups plus `deadline` and `r_min` never changes for the
//! life of the position (A26).

use anchor_lang::prelude::*;
use shortwin_math::bounds::ExitParams;
use shortwin_math::rent::{PosRent, RentTerms};
use shortwin_math::risk::{ExpiryKeeperTerms, SettleKeeperTerms};
use shortwin_math::settle::{ArmState, S3Terms};
use shortwin_math::waterfall::PenaltySplit;

use crate::errors::{CheckedExt, ShortwinError};
use crate::state::config::{Params, TierParams};

/// `ShortPosition.status`: open.
pub const POSITION_OPEN: u8 = 0;
/// `ShortPosition.status`: exit in progress (reserved for v1.1).
pub const POSITION_EXIT_IN_PROGRESS: u8 = 1;

/// A short position (ARCH §2.3.5).
#[account]
#[derive(InitSpace, Debug, PartialEq, Eq)]
pub struct ShortPosition {
    pub borrower: Pubkey,
    pub market: Pubkey,
    /// Chosen by the client; `init` fails if reused.
    pub nonce: u64,
    pub bump: u8,
    /// [`POSITION_OPEN`] (1 = ExitInProgress reserved for v1.1).
    pub status: u8,
    pub tier: u8,
    pub venue_at_open: u8,
    /// Tokens owed (constant: no partial close in the MVP).
    pub q: u64,
    /// Sale proceeds.
    pub s: u64,
    /// Margin.
    pub m: u64,
    /// Tracked vault lamports (changes only through `add_margin`).
    pub v: u64,
    pub n_open: u64,
    pub fee_open: u64,
    /// `S·WAD/Q`.
    pub entry_price: u128,
    /// Rent basis indices at open.
    pub j_open: [u128; 3],
    /// Minimum rent, fixed at open.
    pub r_min: u64,
    pub t_open: i64,
    /// `t_open + max_duration_s[tier]`.
    pub deadline: i64,
    // ---- locked tier terms (22) ----
    pub cr_liq_bps: u32,
    pub penalty_bps: u16,
    pub max_liq_slip_bps: u16,
    pub expiry_fee_bps: u16,
    pub rate_base_bps: u32,
    pub rate_slope1_bps: u32,
    pub rate_slope2_bps: u32,
    // ---- locked split, rent share and keeper terms (44) ----
    /// Lender / protocol / keeper.
    pub penalty_split: [u16; 3],
    pub rent_protocol_bps: u16,
    pub settle_keeper_fixed: u64,
    pub settle_keeper_bps: u16,
    pub settle_keeper_cap: u64,
    pub expiry_keeper_fixed: u64,
    pub expiry_keeper_bps: u16,
    pub expiry_keeper_cap: u64,
    // ---- locked exit-bound and persistence terms (14) ----
    pub delta_liq_bps: u16,
    pub delta_exp_bps: u16,
    pub kappa_liq_bps: u16,
    pub kappa_base_bps: u16,
    pub kappa_growth_n: u8,
    pub liq_persist_n: u8,
    pub settle_persist_n: u16,
    pub settle_arm_gap_s: u16,
    // ---- locked execution and settlement terms (27) ----
    /// ε of the expiry bound.
    pub exec_tol_bps: u16,
    /// Effective `exec_rent_tol` (0 on Raydium).
    pub ert: u64,
    pub ref_max_age_s: u8,
    pub fv_dev_bps: u16,
    pub venue_fee_hard_cap_bps: u16,
    pub s3_wait_s: u32,
    pub grad_grace_s: u32,
    pub max_migration_pause_s: u32,
    // ---- settlement arming state (25) ----
    pub s_armed: u8,
    /// `ema_count` at arm.
    pub s_count: u64,
    pub s_since: i64,
    pub s_last: i64,
    /// Room for the O1/O2 terms if the owner ever signs them off (not built, R3A-8 c).
    pub _reserved: [u8; 32],
}

/// Narrows a parameter to the position's field width (`InvalidParams` if it does not fit;
/// the compiled bounds of ARCH §2.3.3 guarantee it does).
fn narrow<T: TryFrom<u32>>(v: u32) -> Result<T> {
    T::try_from(v).map_err(|_| error!(ShortwinError::InvalidParams))
}

impl ShortPosition {
    /// PDA seed prefix (`["position", market, borrower, nonce u64 LE]`).
    pub const SEED: &'static [u8] = b"position";
    /// Account size including the discriminator.
    pub const SPACE: usize = 8 + Self::INIT_SPACE;

    /// Writes every locked term of ARCH §2.3.5 (A26; ADOPTED Q9; C0-8, R3A-5, V1-5) from the
    /// current `params` and the position's tier: the tier terms, the penalty split, the
    /// protocol's rent share, both keeper reward terms, the exit-bound and persistence terms,
    /// the execution and settlement terms (`ert` is the **effective** value: 0 on Raydium and
    /// the mock, `exec_rent_tol` on pump venues) and the S3 timers. The arming state starts
    /// at 0. `deadline` and `r_min` are the caller's (they need `now` and the rent state).
    pub fn lock_terms(&mut self, params: &Params, tier: &TierParams, ert: u64) -> Result<()> {
        self.cr_liq_bps = tier.cr_liq_bps;
        self.penalty_bps = narrow(tier.penalty_bps)?;
        self.max_liq_slip_bps = narrow(tier.max_liq_slip_bps)?;
        self.expiry_fee_bps = narrow(params.expiry_fee_bps)?;
        self.rate_base_bps = tier.rate_base_bps;
        self.rate_slope1_bps = tier.rate_slope1_bps;
        self.rate_slope2_bps = tier.rate_slope2_bps;
        self.penalty_split = [
            narrow(params.penalty_lender_bps)?,
            narrow(params.penalty_protocol_bps)?,
            narrow(params.penalty_keeper_bps)?,
        ];
        self.rent_protocol_bps = narrow(params.rent_protocol_bps)?;
        self.settle_keeper_fixed = params.settle_keeper_fixed;
        self.settle_keeper_bps = narrow(params.settle_keeper_bps)?;
        self.settle_keeper_cap = params.settle_keeper_cap;
        self.expiry_keeper_fixed = params.expiry_keeper_fixed;
        self.expiry_keeper_bps = narrow(params.expiry_keeper_bps)?;
        self.expiry_keeper_cap = params.expiry_keeper_cap;
        self.delta_liq_bps = params.delta_liq_bps;
        self.delta_exp_bps = params.delta_exp_bps;
        self.kappa_liq_bps = params.kappa_liq_bps;
        self.kappa_base_bps = params.kappa_base_bps;
        self.kappa_growth_n = narrow(params.kappa_growth_n.into())?;
        self.liq_persist_n = narrow(params.liq_persist_n.into())?;
        self.settle_persist_n = narrow(params.settle_persist_n)?;
        self.settle_arm_gap_s = narrow(params.settle_arm_gap_s)?;
        self.exec_tol_bps = params.exec_tol_bps;
        self.ert = ert;
        self.ref_max_age_s = narrow(params.ref_max_age_s.into())?;
        self.fv_dev_bps = narrow(params.fv_dev_bps)?;
        self.venue_fee_hard_cap_bps = narrow(params.venue_fee_hard_cap_bps)?;
        self.s3_wait_s = params.s3_wait_s;
        self.grad_grace_s = params.grad_grace_s;
        self.max_migration_pause_s = params.max_migration_pause_s;
        self.set_arm_state(&ArmState::default());
        Ok(())
    }

    /// `true` if `key` is this position's PDA `["position", market, borrower, nonce u64 LE]`
    /// with the stored bump (`create_program_address`, no bump search). The exits' Accounts
    /// constraint: a `seeds = [.., position.nonce]` constraint would put a seed read from the
    /// account itself into the IDL, which clients cannot resolve (as `Market::is_pda`).
    pub fn is_pda(&self, key: &Pubkey) -> bool {
        Pubkey::create_program_address(
            &[
                Self::SEED,
                self.market.as_ref(),
                self.borrower.as_ref(),
                &self.nonce.to_le_bytes(),
                &[self.bump],
            ],
            &crate::ID,
        )
        .is_ok_and(|pda| pda == *key)
    }

    /// The locked rate curve.
    pub fn rent_terms(&self) -> RentTerms {
        RentTerms {
            base_bps: self.rate_base_bps,
            slope1_bps: self.rate_slope1_bps,
            slope2_bps: self.rate_slope2_bps,
        }
    }

    /// What the position's rent depends on (`rent::{due, credited_for}`).
    pub fn pos_rent(&self) -> PosRent {
        PosRent {
            q: self.q,
            j_open: self.j_open,
            terms: self.rent_terms(),
            rent_protocol_bps: u32::from(self.rent_protocol_bps),
            r_min: self.r_min,
        }
    }

    /// The locked SOL-settlement keeper terms (`risk::k_s`).
    pub fn settle_keeper_terms(&self) -> SettleKeeperTerms {
        SettleKeeperTerms {
            fixed: self.settle_keeper_fixed,
            bps: u32::from(self.settle_keeper_bps),
            cap: self.settle_keeper_cap,
        }
    }

    /// The locked expiry keeper terms (`risk::k_e`).
    pub fn expiry_keeper_terms(&self) -> ExpiryKeeperTerms {
        ExpiryKeeperTerms {
            fixed: self.expiry_keeper_fixed,
            bps: u32::from(self.expiry_keeper_bps),
            cap: self.expiry_keeper_cap,
        }
    }

    /// The locked exit-bound terms (`bounds::*`).
    pub fn exit_params(&self) -> ExitParams {
        ExitParams {
            kappa_liq_bps: self.kappa_liq_bps,
            kappa_base_bps: self.kappa_base_bps,
            delta_liq_bps: self.delta_liq_bps,
            delta_exp_bps: self.delta_exp_bps,
            kappa_growth_n: self.kappa_growth_n,
            ref_max_age_s: self.ref_max_age_s,
        }
    }

    /// The locked penalty split (lender / protocol / keeper).
    pub fn penalty_split(&self) -> PenaltySplit {
        PenaltySplit {
            lender_bps: u32::from(self.penalty_split[0]),
            protocol_bps: u32::from(self.penalty_split[1]),
            keeper_bps: u32::from(self.penalty_split[2]),
        }
    }

    /// The locked S3 timers.
    pub fn s3_terms(&self) -> S3Terms {
        S3Terms {
            s3_wait_s: self.s3_wait_s,
            grad_grace_s: self.grad_grace_s,
            max_migration_pause_s: self.max_migration_pause_s,
        }
    }

    /// The settlement arming state.
    pub fn arm_state(&self) -> ArmState {
        ArmState {
            count: self.s_count,
            since: self.s_since,
            last: self.s_last,
            armed: self.s_armed,
            _pad: [0; 7],
        }
    }

    /// Stores the settlement arming state.
    pub fn set_arm_state(&mut self, a: &ArmState) {
        self.s_count = a.count;
        self.s_since = a.since;
        self.s_last = a.last;
        self.s_armed = a.armed;
    }

    /// `release_at = deadline + max_migration_pause_s + grad_grace_s + s3_wait_s` with the
    /// position's snapshot timers (ARCH §3.3, §3.4 step 13; R4A-8): the latest second this
    /// position can still hold borrowed tokens, which `Market.max_release` covers (I6).
    pub fn release_at(&self) -> Result<i64> {
        let tail = i64::from(self.max_migration_pause_s)
            .checked_add(i64::from(self.grad_grace_s))
            .and_then(|t| t.checked_add(i64::from(self.s3_wait_s)))
            .or_math()?;
        self.deadline.checked_add(tail).or_math()
    }
}
