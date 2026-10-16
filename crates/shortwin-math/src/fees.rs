//! Owner: quotes track. Venue fee resolution, read on every quote and never cached (ARCH §4.2; ECON §1, §2.1, §3.1).
//!
//! The resolvers take the raw fields the venue-state readers decode (ARCH §4.1) and
//! reproduce each venue's own fee selection: pump-fees market-cap tiers with the
//! creator override for the pump curve and PumpSwap, the per-pool `num/den` of Raydium
//! AMM v4, and the per-config rates plus the creator-fee side of Raydium CPMM. They
//! return the fee **components** (the quotes charge them separately, R-PUMP §4, R-RAY
//! §3.3); every fee type resolves them to the one-way rate `φ` in **ppm** with
//! `oneway_ppm()` (ECON §2.1): pump `(lp + protocol + creator)·100` exactly, AMM v4
//! `⌈num·10⁶/den⌉`, CPMM `t + c`.
//!
//! **Gates compare ppm with `cap_bps·100`** ([`exceeds_cap`]; no rounding, R3A-3 iii):
//! the risk view `capped`, `venue_fee_cap_bps` eligibility, `s3_no_route` and `pool_fail`
//! against `venue_fee_hard_cap_bps`. `oneway_bps()` is `⌈φ_ppm/100⌉`, for display only.
//!
//! Every fee type offers the two views of ARCH §4.3 (V2-1): the live fee for execution
//! quotes and `capped(venue_fee_hard_cap_bps)` for risk quotes. Hypothetical states are
//! quoted with the fee resolved on the live state (the live tier; R3B-7 b, ARCH §4.2).

use crate::Rounding;
use crate::error::MathError;
use crate::num::{BPS, PPM, mul_div, mul_div_u64, saturating_u64};

/// `x` as `u32`, saturating: ECON §2.1 types `φ` as `u32` ppm (a saturated rate still
/// exceeds every cap a gate compares it with).
fn saturating_u32(x: u128) -> u32 {
    u32::try_from(x).unwrap_or(u32::MAX)
}

/// ppm per bp.
const PPM_PER_BPS: u64 = PPM / BPS;

/// `φ_ppm > cap_bps·100`, exactly: the fee gates of ECON §2.1 and ARCH §4.2 (R3A-3 iii),
/// e.g. `pool_fail` / `s3_no_route` against `venue_fee_hard_cap_bps` and pool eligibility
/// against `venue_fee_cap_bps`; `capped` uses the same comparison.
pub fn exceeds_cap(phi_ppm: u32, cap_bps: u64) -> bool {
    above_cap(u128::from(phi_ppm), cap_bps)
}

/// [`exceeds_cap`] on an unsaturated `u128` rate.
fn above_cap(phi_ppm: u128, cap_bps: u64) -> bool {
    phi_ppm > u128::from(cap_bps).saturating_mul(u128::from(PPM_PER_BPS))
}

/// The market-cap supply basis [`pump_curve`] takes: 10¹⁵ raw units (1 B tokens, 6 decimals),
/// R-PUMP §4 step 1. The deployed pump resolves the curve's tier on the mint's **live** supply
/// instead (R-PUMP Update U10, ARCH §4.2's open item); the two differ only after a burn and only
/// when a tier threshold lies between the two market caps, which pump's single curve tier
/// (threshold 0) rules out today. The synthetic exit's fee ([`pump_curve_at_supply`], pump-final
/// §5.2) takes the live supply.
pub const PUMP_CURVE_MCAP_SUPPLY: u128 = 1_000_000_000_000_000;

/// Fee rates charged on the SOL leg of a pump venue (pump curve, PumpSwap), in bps of
/// the pre-fee SOL amount; each component is rounded up separately (R-PUMP §4, R-VEN §2.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FeeBps {
    /// LP fee: stays in the pool's quote vault (PumpSwap; always 0 on the curve).
    pub lp: u64,
    /// Protocol fee, including the buyback split (the split does not change the cost).
    pub protocol: u64,
    /// Creator fee (0 if the coin has no creator).
    pub creator: u64,
}

impl FeeBps {
    /// No fee.
    pub const ZERO: Self = Self::new(0, 0, 0);

    /// `(lp, protocol, creator)` in bps.
    pub const fn new(lp: u64, protocol: u64, creator: u64) -> Self {
        Self {
            lp,
            protocol,
            creator,
        }
    }

    /// `(lp + protocol + creator)·100` on `u128` (exact for any inputs).
    fn ppm(&self) -> u128 {
        u128::from(self.lp)
            .saturating_add(u128::from(self.protocol))
            .saturating_add(u128::from(self.creator))
            .saturating_mul(u128::from(PPM_PER_BPS))
    }

    /// One-way fee φ in ppm (ECON §2.1 type `u32`), `(lp + protocol + creator)·100`
    /// exactly (ARCH §4.2): the `φ` of the fee gates and the fee-scaled caps. Saturates at
    /// `u32::MAX` (429,496 %: no venue fee comes near it, and every gate still trips).
    pub fn oneway_ppm(&self) -> u32 {
        saturating_u32(self.ppm())
    }

    /// One-way fee in bps, `lp + protocol + creator`, for display only (gates use
    /// [`oneway_ppm`](Self::oneway_ppm)). Saturates at `u64::MAX`.
    pub fn oneway_bps(&self) -> u64 {
        self.lp
            .saturating_add(self.protocol)
            .saturating_add(self.creator)
    }

    /// The risk view (ARCH §4.3, V2-1; ECON §3.1): unchanged if `φ_ppm ≤ cap_bps·100`,
    /// otherwise **one combined fee at the cap**, `B̂ = n + ⌈n·cap_bps/10⁴⌉` with one
    /// ceiling (the sims' `Venue._risk_fees`), carried in `protocol` with `lp = creator
    /// = 0` (a risk quote's post-trade reserves are never used). Since
    /// `Σᵢ⌈n·φᵢ/10⁴⌉ ≥ ⌈n·φ/10⁴⌉ ≥ ⌈n·cap/10⁴⌉` when `φ > cap`, a capped quote is never
    /// above the live one. The v0.3 split (lp, protocol, creator kept in that order up to
    /// the cap, three ceilings) could quote up to 2 lamports higher (ARCH §9 M0b).
    pub fn capped(&self, cap_bps: u64) -> Self {
        if above_cap(self.ppm(), cap_bps) {
            Self::new(0, cap_bps, 0)
        } else {
            *self
        }
    }
}

/// One pump-fees `FeeTier`: `fees` apply from market cap `threshold` (lamports) upwards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FeeTier {
    /// `market_cap_lamports_threshold`.
    pub threshold: u128,
    /// `{lp_fee_bps, protocol_fee_bps, creator_fee_bps}`.
    pub fees: FeeBps,
}

/// pump-fees `calculate_fee_tier` (R-PUMP §4 step 3, R-VEN §2.3): tier 0 below
/// `tiers[0].threshold`, otherwise the **last** tier (in account order) whose threshold
/// is ≤ `mcap`.
///
/// Errors: `InvalidInput` if `tiers` is empty (an invariant violation on-chain).
pub fn fee_tier(tiers: &[FeeTier], mcap: u128) -> Result<FeeBps, MathError> {
    let first = tiers.first().ok_or(MathError::InvalidInput)?;
    Ok(tiers
        .iter()
        .rev()
        .find(|t| mcap >= t.threshold)
        .unwrap_or(first)
        .fees)
}

/// Fee inputs of a SOL-quoted pump-curve coin (R-PUMP §2.1–§2.3, §4; ARCH §4.1).
#[derive(Clone, Copy, Debug)]
pub struct CurveFeeSchedule<'a> {
    /// pump `FeeConfig.fee_tiers` (the SOL schedule), in account order.
    pub tiers: &'a [FeeTier],
    /// `Global.creator_fee_configurable` (@1045).
    pub creator_fee_configurable: bool,
    /// `BondingCurve.creator_fee_bps` (@115; 0 = use the schedule).
    pub curve_creator_fee_bps: u64,
    /// `BondingCurve.creator == Pubkey::default()`.
    pub creator_is_default: bool,
    /// Risk-view cap on the one-way fee (`venue_fee_hard_cap_bps`); `None` = live fee.
    pub cap_bps: Option<u64>,
}

/// Fees of a pump-curve trade on the curve state `(vT, vQ)` (ARCH §4.2; R-PUMP §4):
/// `mcap = ⌊vQ·10¹⁵/vT⌋`, tier by [`fee_tier`], the curve's creator override if
/// `creator_fee_configurable ∧ curve_creator_fee_bps ≠ 0`, creator 0 for a default
/// creator. The curve charges only the protocol and creator components; `lp` is 0.
/// [`pump_curve_at_supply`] with [`PUMP_CURVE_MCAP_SUPPLY`] (see there for the live supply).
///
/// Errors: `DivByZero` if `vt == 0` (a migrated curve); `InvalidInput` for empty tiers;
/// `Overflow` if the market cap exceeds `u128`.
pub fn pump_curve(vt: u128, vq: u128, s: &CurveFeeSchedule<'_>) -> Result<FeeBps, MathError> {
    curve_fees_at(vt, vq, PUMP_CURVE_MCAP_SUPPLY, s)
}

/// [`pump_curve`] with the market cap on `supply`: `mcap = ⌊vQ·supply/vT⌋`. With the mint's
/// **live** supply this is the tier the deployed pump charges (R-PUMP Update U10): on both parts
/// of a buy past `rT` by synthetic migration, each at the curve's **pre-trade** market cap
/// (pump-final §3.4 (e), measured on pump's 2026-10-08 and 2026-10-13 ELFs), so it is the `φ` of
/// [`curve_synthetic`](crate::quote::curve_synthetic)'s two legs (P1 correction (a): at the fixed
/// 10¹⁵ a burned supply under a multi-tier `FeeConfig` gave a cheaper tier than pump charged, and
/// `B̂_v3` fell 59,057,853 lamports below the charge in G0's tripwire).
///
/// Errors: as [`pump_curve`].
pub fn pump_curve_at_supply(
    vt: u128,
    vq: u128,
    supply: u64,
    s: &CurveFeeSchedule<'_>,
) -> Result<FeeBps, MathError> {
    curve_fees_at(vt, vq, u128::from(supply), s)
}

fn curve_fees_at(
    vt: u128,
    vq: u128,
    supply: u128,
    s: &CurveFeeSchedule<'_>,
) -> Result<FeeBps, MathError> {
    let mcap = mul_div(vq, supply, vt, Rounding::Floor)?;
    let tier = fee_tier(s.tiers, mcap)?;
    let creator = if s.creator_is_default {
        0
    } else if s.creator_fee_configurable && s.curve_creator_fee_bps != 0 {
        s.curve_creator_fee_bps
    } else {
        tier.creator
    };
    Ok(apply_cap(FeeBps::new(0, tier.protocol, creator), s.cap_bps))
}

/// Fee inputs of a canonical, SOL-quoted PumpSwap pool (R-VEN §2.3; ARCH §4.1).
#[derive(Clone, Copy, Debug)]
pub struct PammFeeSchedule<'a> {
    /// pAMM `FeeConfig.fee_tiers` (canonical SOL-like pools), in account order. A
    /// non-canonical pool pays `flat_fees`: pass it as a single tier with threshold 0.
    pub tiers: &'a [FeeTier],
    /// Live base-mint supply, the market-cap basis (mayhem pools use a fixed 10¹⁵ and are
    /// rejected by short.win). 10¹⁵ for a coin that has not graduated yet.
    pub base_supply: u64,
    /// `GlobalConfig.creator_fee_configurable` (@940).
    pub creator_fee_configurable: bool,
    /// `Pool.creator_fee_bps` (@261; 0 = use the schedule).
    pub pool_creator_fee_bps: u64,
    /// `Pool.coin_creator == Pubkey::default()`.
    pub coin_creator_is_default: bool,
    /// Risk-view cap on the one-way fee (`venue_fee_hard_cap_bps`); `None` = live fee.
    pub cap_bps: Option<u64>,
}

/// Fees of a PumpSwap trade on pre-trade reserves (ARCH §4.2; R-VEN §2.3):
/// `mcap = ⌊effective_quote·base_supply/base_reserve⌋` with `effective_quote` = quote
/// vault + `virtual_quote_reserves`, tier by [`fee_tier`], the pool's creator override
/// if `creator_fee_configurable ∧ pool_creator_fee_bps > 0`, creator 0 for a default
/// coin creator.
///
/// Errors: `DivByZero` if `base_reserve == 0`; `InvalidInput` for empty tiers;
/// `Overflow` if the market cap exceeds `u128`.
pub fn pump_amm(
    effective_quote: u128,
    base_reserve: u128,
    s: &PammFeeSchedule<'_>,
) -> Result<FeeBps, MathError> {
    let mcap = mul_div(
        effective_quote,
        u128::from(s.base_supply),
        base_reserve,
        Rounding::Floor,
    )?;
    let tier = fee_tier(s.tiers, mcap)?;
    let creator = if s.coin_creator_is_default {
        0
    } else if s.creator_fee_configurable && s.pool_creator_fee_bps > 0 {
        s.pool_creator_fee_bps
    } else {
        tier.creator
    };
    Ok(apply_cap(
        FeeBps::new(tier.lp, tier.protocol, creator),
        s.cap_bps,
    ))
}

fn apply_cap(f: FeeBps, cap: Option<u64>) -> FeeBps {
    match cap {
        Some(c) => f.capped(c),
        None => f,
    }
}

/// Raydium AMM v4 swap fee `swap_fee_numerator / swap_fee_denominator` of the input
/// (AmmInfo @176/@184; R-RAY §2.3). The whole fee stays in the pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RayV4Fee {
    /// `swap_fee_numerator`.
    pub num: u64,
    /// `swap_fee_denominator`.
    pub den: u64,
}

impl RayV4Fee {
    /// One-way fee φ in ppm, `⌈num·10⁶/den⌉` (ECON §2.1, ARCH §4.2). `num < den` (checked
    /// by [`ray_v4`]) keeps it below 10⁶; otherwise it saturates at `u32::MAX` (also for a
    /// zero `den`).
    pub fn oneway_ppm(&self) -> u32 {
        mul_div(
            u128::from(self.num),
            u128::from(PPM),
            u128::from(self.den),
            Rounding::Ceil,
        )
        .map_or(u32::MAX, saturating_u32)
    }

    /// One-way fee in bps, `⌈num·10⁴/den⌉ = ⌈φ_ppm/100⌉`, for display only.
    pub fn oneway_bps(&self) -> u64 {
        mul_div_u64(self.num, BPS, self.den, Rounding::Ceil).unwrap_or(u64::MAX)
    }

    /// The risk view (ARCH §4.3, V2-1): unchanged if `φ_ppm ≤ cap_bps·100`, else the rate
    /// `cap_bps/10⁴`, which is strictly below the live rate; the quote then grosses up at
    /// `min(rate, cap)` (`⌈pre·10⁴/(10⁴ − cap)⌉`, the sims' `RaydiumPool._gross`).
    pub fn capped(&self, cap_bps: u64) -> Self {
        if exceeds_cap(self.oneway_ppm(), cap_bps) {
            Self {
                num: cap_bps,
                den: BPS,
            }
        } else {
            *self
        }
    }
}

/// Resolves the AMM v4 fee of a pool (ARCH §4.2; R-RAY §2.3, §3.3).
///
/// Errors: `InvalidInput` unless `num < den` — every swap on such a pool reverts
/// (division by zero, `den − num` underflow, or zero output).
pub fn ray_v4(swap_fee_numerator: u64, swap_fee_denominator: u64) -> Result<RayV4Fee, MathError> {
    if swap_fee_numerator >= swap_fee_denominator {
        return Err(MathError::InvalidInput);
    }
    Ok(RayV4Fee {
        num: swap_fee_numerator,
        den: swap_fee_denominator,
    })
}

/// Raydium CPMM `AmmConfig` rates in ppm (@12, @20, @28, @108; R-RAY §2.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CpmmConfig {
    /// `trade_fee_rate` `t`.
    pub trade_fee_rate: u64,
    /// `protocol_fee_rate` `p` (share of the trade fee; excluded from reserves).
    pub protocol_fee_rate: u64,
    /// `fund_fee_rate` `f` (share of the trade fee; excluded from reserves).
    pub fund_fee_rate: u64,
    /// `creator_fee_rate` `c` (charged only if the pool enabled it).
    pub creator_fee_rate: u64,
}

/// The fee-relevant fields of a Raydium CPMM `PoolState` (@389, @390; ARCH §2.6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CpmmPoolFees {
    /// `enable_creator_fee`.
    pub enable_creator_fee: bool,
    /// `creator_fee_on`: 0 = both tokens (input side), 1 = only token 0, 2 = only token 1.
    pub creator_fee_on: u8,
    /// WSOL is `token_0` (market flag `sol_is_token0_or_coin`).
    pub sol_is_token0: bool,
}

/// Trade direction on a SOL pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// SOL in, token out (buyback).
    Buy,
    /// Token in, SOL out (open).
    Sell,
}

/// Resolved CPMM fees for one direction, in ppm (ARCH §4.2; R-RAY §3.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CpmmFees {
    /// Trade fee rate `t`.
    pub trade: u64,
    /// Effective creator fee rate `c` (0 if the pool did not enable it).
    pub creator: u64,
    /// The creator fee is charged on the input token of this direction (else on the output).
    pub creator_on_input: bool,
    /// Protocol share of the trade fee `p` (post-trade reserves only).
    pub protocol: u64,
    /// Fund share of the trade fee `f` (post-trade reserves only).
    pub fund: u64,
}

impl CpmmFees {
    /// `t + c` on `u128` (exact for any inputs).
    fn ppm(&self) -> u128 {
        u128::from(self.trade).saturating_add(u128::from(self.creator))
    }

    /// One-way fee φ in ppm, `t + c` exactly (ECON §2.1, ARCH §4.2; R3A-3 iii). Saturates
    /// at `u32::MAX` (valid configs have `t + c < 10⁶`).
    pub fn oneway_ppm(&self) -> u32 {
        saturating_u32(self.ppm())
    }

    /// One-way fee in bps, `⌈(t + c)/100⌉`, for display only. Saturates at `u64::MAX`.
    pub fn oneway_bps(&self) -> u64 {
        saturating_u64(self.ppm().div_ceil(u128::from(PPM_PER_BPS)))
    }

    /// The risk view (ARCH §4.3, V2-1): unchanged if `t + c ≤ cap_bps·100`; otherwise the
    /// trade rate is kept first and the creator rate takes the rest, so
    /// `t + c = cap_bps·100` and a fee on the input grosses up at `min(rate, cap)` (the
    /// sims' `RaydiumPool._gross`). The side of the creator fee and the protocol/fund
    /// shares are unchanged, except for a **zero** total fee (M3-SEC-1): it has no creator
    /// part to split, so it is carried on the output and the risk quote is the fee-free one.
    /// The venue fails a swap whose creator fee is on the input at `t + c = 0`
    /// (`split_creator_fee` divides by it), which the execution quote keeps (`None`); a risk
    /// quote — a CR leg, the settlement box, `B̂_ref`, FV — must stay defined, because the
    /// fee view never makes a quote infinite (ARCH §4.3).
    pub fn capped(&self, cap_bps: u64) -> Self {
        let mut out = if above_cap(self.ppm(), cap_bps) {
            let cap_ppm = cap_bps.saturating_mul(PPM_PER_BPS);
            let trade = self.trade.min(cap_ppm);
            Self {
                trade,
                creator: self.creator.min(cap_ppm.saturating_sub(trade)),
                ..*self
            }
        } else {
            *self
        };
        if out.trade == 0 && out.creator == 0 {
            out.creator_on_input = false;
        }
        out
    }
}

/// Resolves the CPMM fees of a pool for one direction (ARCH §4.2; R-RAY §2.5, §3.3):
/// `c` = `creator_fee_rate` if `enable_creator_fee`, else 0; the creator fee is on the
/// input iff `creator_fee_on == 0`, or it names the input token.
///
/// Errors: `InvalidInput` if `creator_fee_on > 2` (the venue rejects every swap with
/// `InvalidFeeModel`), or the config breaks the venue's own invariants
/// `t + c < 10⁶` and `p + f ≤ 10⁶` (`update_amm_config`).
pub fn cpmm(
    config: &CpmmConfig,
    pool: &CpmmPoolFees,
    direction: Direction,
) -> Result<CpmmFees, MathError> {
    let tc = config
        .trade_fee_rate
        .checked_add(config.creator_fee_rate)
        .ok_or(MathError::InvalidInput)?;
    let pf = config
        .protocol_fee_rate
        .checked_add(config.fund_fee_rate)
        .ok_or(MathError::InvalidInput)?;
    if tc >= PPM || pf > PPM {
        return Err(MathError::InvalidInput);
    }
    let input_is_token0 = (direction == Direction::Buy) == pool.sol_is_token0;
    let creator_on_input = match pool.creator_fee_on {
        0 => true,
        1 => input_is_token0,
        2 => !input_is_token0,
        _ => return Err(MathError::InvalidInput),
    };
    Ok(CpmmFees {
        trade: config.trade_fee_rate,
        creator: if pool.enable_creator_fee {
            config.creator_fee_rate
        } else {
            0
        },
        creator_on_input,
        protocol: config.protocol_fee_rate,
        fund: config.fund_fee_rate,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    const PAMM: [(u128, u64, u64, u64); 3] = [
        (0, 2, 93, 30),
        (420_000_000_000, 20, 5, 95),
        (98_240_000_000_000, 20, 5, 5),
    ];

    fn tiers() -> [FeeTier; 3] {
        PAMM.map(|(threshold, lp, pr, cr)| FeeTier {
            threshold,
            fees: FeeBps::new(lp, pr, cr),
        })
    }

    #[test]
    fn tier_selection_follows_pump_fees() {
        let t = tiers();
        assert_eq!(fee_tier(&t, 0).unwrap(), FeeBps::new(2, 93, 30));
        assert_eq!(
            fee_tier(&t, 419_999_999_999).unwrap(),
            FeeBps::new(2, 93, 30)
        );
        assert_eq!(
            fee_tier(&t, 420_000_000_000).unwrap(),
            FeeBps::new(20, 5, 95)
        );
        assert_eq!(fee_tier(&t, u128::MAX).unwrap(), FeeBps::new(20, 5, 5));
        // Below the first threshold: tier 0 even if it is not 0.
        let high = [FeeTier {
            threshold: 10,
            fees: FeeBps::new(1, 2, 3),
        }];
        assert_eq!(fee_tier(&high, 3).unwrap(), FeeBps::new(1, 2, 3));
        assert_eq!(fee_tier(&[], 3), Err(MathError::InvalidInput));
    }

    #[test]
    fn creator_rules() {
        let t = tiers();
        let mut s = PammFeeSchedule {
            tiers: &t,
            base_supply: 1_000_000_000_000_000,
            creator_fee_configurable: true,
            pool_creator_fee_bps: 0,
            coin_creator_is_default: false,
            cap_bps: None,
        };
        // Fresh graduate: mcap ≈ 410.8 SOL < 420 SOL -> 2/93/30 (R-VEN §2.3).
        let f = pump_amm(84_990_359_345, 206_900_000_000_000, &s).unwrap();
        assert_eq!(f, FeeBps::new(2, 93, 30));
        s.pool_creator_fee_bps = 150;
        assert_eq!(
            pump_amm(84_990_359_345, 206_900_000_000_000, &s)
                .unwrap()
                .creator,
            150
        );
        s.creator_fee_configurable = false;
        assert_eq!(
            pump_amm(84_990_359_345, 206_900_000_000_000, &s)
                .unwrap()
                .creator,
            30
        );
        s.coin_creator_is_default = true;
        assert_eq!(
            pump_amm(84_990_359_345, 206_900_000_000_000, &s)
                .unwrap()
                .creator,
            0
        );
        assert_eq!(pump_amm(1, 0, &s), Err(MathError::DivByZero));

        let curve = [FeeTier {
            threshold: 0,
            fees: FeeBps::new(7, 95, 30),
        }];
        let mut c = CurveFeeSchedule {
            tiers: &curve,
            creator_fee_configurable: true,
            curve_creator_fee_bps: 0,
            creator_is_default: false,
            cap_bps: None,
        };
        // The curve never charges an LP fee.
        assert_eq!(
            pump_curve(824_130_602_106_172, 39_059_343_535, &c).unwrap(),
            FeeBps::new(0, 95, 30)
        );
        c.curve_creator_fee_bps = 200;
        assert_eq!(pump_curve(1, 1, &c).unwrap().creator, 200);
        c.creator_is_default = true;
        assert_eq!(pump_curve(1, 1, &c).unwrap().creator, 0);
        c.cap_bps = Some(50);
        assert_eq!(pump_curve(1, 1, &c).unwrap(), FeeBps::new(0, 50, 0));
        assert_eq!(pump_curve(0, 1, &c), Err(MathError::DivByZero));
        assert_eq!(pump_curve_at_supply(0, 1, 1, &c), Err(MathError::DivByZero));
    }

    /// pump's curve tier on the mint's live supply (R-PUMP Update U10; pump-final §3.4 (e)):
    /// G0's tripwire on `near166` (supply 999,818,465,159,445 after burns, `(vT, vQ)` =
    /// (333,021,296,825,365, 96,660,487,854)): the pre-trade market cap is 290,200,481,257 at the
    /// live supply and 290,253,172,320 at 10¹⁵, on either side of the tier threshold
    /// 290,226,826,789, so pump charged 80 / 25 bps where the fixed supply reads 70 / 22.
    #[test]
    fn curve_tier_at_the_live_supply() {
        let tiers = [
            FeeTier {
                threshold: 0,
                fees: FeeBps::new(0, 90, 28),
            },
            FeeTier {
                threshold: 290_174_135_726,
                fees: FeeBps::new(0, 80, 25),
            },
            FeeTier {
                threshold: 290_226_826_789,
                fees: FeeBps::new(0, 70, 22),
            },
        ];
        let s = CurveFeeSchedule {
            tiers: &tiers,
            creator_fee_configurable: true,
            curve_creator_fee_bps: 0,
            creator_is_default: false,
            cap_bps: None,
        };
        let (vt, vq) = (333_021_296_825_365, 96_660_487_854);
        let live = pump_curve_at_supply(vt, vq, 999_818_465_159_445, &s).unwrap();
        assert_eq!(live, FeeBps::new(0, 80, 25));
        assert_eq!(pump_curve(vt, vq, &s).unwrap(), FeeBps::new(0, 70, 22));
        // The fixed basis is the 10¹⁵ supply exactly.
        assert_eq!(
            pump_curve_at_supply(vt, vq, 1_000_000_000_000_000, &s),
            pump_curve(vt, vq, &s)
        );
        assert_eq!(
            pump_curve_at_supply(
                vt,
                vq,
                999_818_465_159_445,
                &CurveFeeSchedule {
                    cap_bps: Some(50),
                    ..s
                }
            ),
            Ok(FeeBps::new(0, 50, 0))
        );
    }

    #[test]
    fn caps() {
        let f = FeeBps::new(20, 5, 95);
        assert_eq!((f.oneway_bps(), f.oneway_ppm()), (120, 12_000));
        assert_eq!(f.capped(500), f);
        assert_eq!(f.capped(120), f);
        // Above the cap: one combined fee at the cap (ECON §3.1, the sims' _risk_fees).
        assert_eq!(f.capped(119), FeeBps::new(0, 119, 0));
        assert_eq!(f.capped(100), FeeBps::new(0, 100, 0));
        assert_eq!(f.capped(10), FeeBps::new(0, 10, 0));
        assert_eq!(FeeBps::new(u64::MAX, 1, 0).oneway_bps(), u64::MAX);
        assert_eq!(FeeBps::new(u64::MAX, 1, 0).oneway_ppm(), u32::MAX);
        // Exact even where the u64 views saturate.
        let huge = FeeBps::new(u64::MAX, u64::MAX, 0);
        assert_eq!(huge.capped(u64::MAX), FeeBps::new(0, u64::MAX, 0));
        assert_eq!(
            FeeBps::new(u64::MAX, 0, 0).capped(u64::MAX),
            FeeBps::new(u64::MAX, 0, 0)
        );
        // n = 1,000: three ceilings 2 + 1 + 10 = 13 lamports; one at 100 bps: 10.
        let n = 1_000u64;
        let three: u64 = [20u64, 5, 95]
            .iter()
            .map(|b| (n * b).div_ceil(10_000))
            .sum();
        assert_eq!(three, 13);
        let c = f.capped(100);
        let one: u64 = [c.lp, c.protocol, c.creator]
            .iter()
            .map(|b| (n * b).div_ceil(10_000))
            .sum();
        assert_eq!(one, 10);

        assert!(exceeds_cap(12_001, 120) && !exceeds_cap(12_000, 120));
        assert!(!exceeds_cap(u32::MAX, u64::MAX) && exceeds_cap(u32::MAX, 42_949_672));

        let v4 = ray_v4(25, 10_000).unwrap();
        assert_eq!((v4.oneway_bps(), v4.oneway_ppm()), (25, 2_500));
        assert_eq!(v4.capped(300), v4);
        assert_eq!(v4.capped(25), v4);
        assert_eq!(ray_v4(1, 3).unwrap().oneway_bps(), 3_334);
        assert_eq!(ray_v4(1, 3).unwrap().oneway_ppm(), 333_334);
        // 333,334 ppm > 3,333 bps·100: capped, although ⌈⌉ in bps would say 3,334.
        assert_eq!(
            ray_v4(1, 3).unwrap().capped(3_333),
            RayV4Fee {
                num: 3_333,
                den: 10_000
            }
        );
        assert_eq!(ray_v4(1, 3).unwrap().capped(3_334), ray_v4(1, 3).unwrap());
        assert_eq!(RayV4Fee { num: 1, den: 0 }.oneway_ppm(), u32::MAX);
        assert_eq!(
            RayV4Fee {
                num: u64::MAX,
                den: 1
            }
            .oneway_ppm(),
            u32::MAX
        );
        assert_eq!(
            ray_v4(1, 3).unwrap().capped(500),
            RayV4Fee {
                num: 500,
                den: 10_000
            }
        );
        assert_eq!(ray_v4(3, 3), Err(MathError::InvalidInput));
        assert_eq!(ray_v4(0, 0), Err(MathError::InvalidInput));

        let cp = CpmmFees {
            trade: 2_500,
            creator: 500,
            creator_on_input: true,
            protocol: 120_000,
            fund: 40_000,
        };
        assert_eq!((cp.oneway_bps(), cp.oneway_ppm()), (30, 3_000));
        assert_eq!(CpmmFees { trade: 50, ..cp }.oneway_bps(), 6);
        assert_eq!(CpmmFees { trade: 50, ..cp }.oneway_ppm(), 550);
        // 550 ppm > 5 bps·100: capped at exactly 500 ppm (no rounding, R3A-3 iii).
        let c = CpmmFees { trade: 50, ..cp }.capped(5);
        assert_eq!((c.trade, c.creator), (50, 450));
        assert_eq!(
            CpmmFees { trade: 50, ..cp }.capped(6),
            CpmmFees { trade: 50, ..cp }
        );
        let big = CpmmFees {
            trade: 40_000,
            creator: 14_950,
            ..cp
        };
        let c = big.capped(500);
        assert_eq!((c.trade, c.creator, c.oneway_bps()), (40_000, 10_000, 500));
        let c = big.capped(300);
        assert_eq!((c.trade, c.creator), (30_000, 0));

        // M3-SEC-1: a zero total fee is carried on the output in the risk view, whatever the
        // cap; any non-zero fee keeps its side.
        let zero = CpmmFees {
            trade: 0,
            creator: 0,
            ..cp
        };
        for cap in [0u64, 1, 500, u64::MAX] {
            assert_eq!(
                zero.capped(cap),
                CpmmFees {
                    creator_on_input: false,
                    ..zero
                },
                "cap {cap}"
            );
        }
        assert_eq!(
            CpmmFees { trade: 1, ..zero }.capped(500),
            CpmmFees { trade: 1, ..zero }
        );
        assert_eq!(
            CpmmFees { creator: 1, ..zero }.capped(500),
            CpmmFees { creator: 1, ..zero }
        );
        // A cap of 0 caps every fee to zero: fee-free, on the output.
        let c = cp.capped(0);
        assert_eq!((c.trade, c.creator, c.creator_on_input), (0, 0, false));
    }

    #[test]
    fn cpmm_sides() {
        let cfg = CpmmConfig {
            trade_fee_rate: 2_500,
            protocol_fee_rate: 120_000,
            fund_fee_rate: 40_000,
            creator_fee_rate: 500,
        };
        let pool = |on, sol0| CpmmPoolFees {
            enable_creator_fee: true,
            creator_fee_on: on,
            sol_is_token0: sol0,
        };
        let side = |on, sol0, d| cpmm(&cfg, &pool(on, sol0), d).unwrap().creator_on_input;
        // BothToken: always the input.
        assert!(side(0, true, Direction::Buy) && side(0, false, Direction::Sell));
        // OnlyToken0 with WSOL = token 0: SOL side -> input on buys, output on sells.
        assert!(side(1, true, Direction::Buy) && !side(1, true, Direction::Sell));
        assert!(!side(1, false, Direction::Buy) && side(1, false, Direction::Sell));
        assert!(!side(2, true, Direction::Buy) && side(2, true, Direction::Sell));
        assert_eq!(
            cpmm(&cfg, &pool(3, true), Direction::Buy),
            Err(MathError::InvalidInput)
        );
        let off = CpmmPoolFees {
            enable_creator_fee: false,
            ..pool(1, true)
        };
        assert_eq!(cpmm(&cfg, &off, Direction::Buy).unwrap().creator, 0);
        let bad = CpmmConfig {
            trade_fee_rate: 999_500,
            ..cfg
        };
        assert_eq!(
            cpmm(&bad, &off, Direction::Buy),
            Err(MathError::InvalidInput)
        );
        let bad = CpmmConfig {
            fund_fee_rate: 880_001,
            ..cfg
        };
        assert_eq!(
            cpmm(&bad, &off, Direction::Buy),
            Err(MathError::InvalidInput)
        );
    }
}
