//! CR and the risk predicates, D4 margin, open fee, FV, keeper rewards, the exit bounds
//! (`max_in_liq`, `max_in_exp`, `π_ref`), caps and tiers
//! (`shortwin_math::{risk, bounds, caps, tier}`; ECON §3.3, §4, §6, §8; ARCH §3.6, §3.7,
//! §4.5–§4.7).

use shortwin_math::consts::{P_OPEN_MAX, PUMPSWAP_LOCK_MIN_BPS};
use shortwin_math::risk::{self as mrisk, ExpiryKeeperTerms, SettleKeeperTerms};
use shortwin_math::tier::{self, OraclePrice, Tier, TierVenue};
use shortwin_math::{MathError, bounds, caps};
use wasm_bindgen::JsError;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::or_throw;

/// `V_eff = V − R_due`, floored at 0 (ECON §3.3).
#[wasm_bindgen(js_name = vEff)]
pub fn v_eff(v: u64, r_due: u64) -> u64 {
    mrisk::v_eff(v, r_due)
}

/// `CR = ⌊V_eff·10⁴/B̂⌋` in bps; an undefined quote gives 0 (ECON §3.3).
#[wasm_bindgen(js_name = crBps)]
pub fn cr_bps(v_eff: u64, b_hat: Option<u64>) -> Result<u64, JsError> {
    Ok(mrisk::cr_bps(v_eff, b_hat)?)
}

/// `CR(V_eff, B̂) < L`, exactly as the program tests it (ECON §5.5).
#[wasm_bindgen(js_name = crBelow)]
pub fn cr_below(v_eff: u64, b_hat: Option<u64>, l_bps: u32) -> Result<bool, JsError> {
    Ok(mrisk::cr_below(v_eff, b_hat, l_bps)?)
}

/// S2 slippage σ in bps over the marginal price `P` (WAD); `n` is the quote's pre-fee
/// input, `undefined` for an undefined quote (σ = +∞ = `u64::MAX`) (ECON §5.6).
#[wasm_bindgen]
pub fn sigma(n: Option<u64>, q: u64, p_wad: u128) -> Result<u64, JsError> {
    Ok(mrisk::sigma(n, q, p_wad)?)
}

/// σ on the state `(x, y)` the quote was taken on.
#[wasm_bindgen(js_name = sigmaXy)]
pub fn sigma_xy(n: Option<u64>, q: u64, x: u128, y: u128) -> Result<u64, JsError> {
    Ok(mrisk::sigma_xy(n, q, x, y)?)
}

/// D4 margin `max(0, ⌈CR_open·max(B̂_spot, B̂_ema)/10⁴⌉ − S)` (ECON §4); an undefined EMA
/// quote (`undefined`) prices the spot quote alone, as the sim.
#[wasm_bindgen(js_name = marginD4)]
pub fn margin_d4(
    s: u64,
    b_spot: u64,
    b_ema: Option<u64>,
    cr_open_bps: u32,
) -> Result<u64, JsError> {
    Ok(mrisk::margin_d4(s, b_spot, b_ema, cr_open_bps)?)
}

/// Open fee `⌈open_fee_bps·N_open/10⁴⌉` (ECON §8.7).
#[wasm_bindgen(js_name = openFee)]
pub fn open_fee(n_open: u64, open_fee_bps: u32) -> Result<u64, JsError> {
    Ok(mrisk::open_fee(n_open, open_fee_bps)?)
}

/// Open guard `|P_spot − P_ema|·10⁴ ≤ devBps·P_ema` (ECON §4 "Open guards"; ARCH §3.4 step
/// 4): `P_spot` the live pre-trade spot, `P_ema` the post-roll EMA (WAD); `false` for a zero
/// `P_ema` (no price: no open).
#[wasm_bindgen(js_name = spotNearEma)]
pub fn spot_near_ema(p_spot: u128, p_ema: u128, dev_bps: u32) -> bool {
    caps::spot_near_ema(p_spot, p_ema, dev_bps)
}

/// `P_OPEN_MAX` (WAD, 1 SOL per raw unit; ECON §2.2): opens are refused while `P_ema` is
/// above it (`PriceAboveOpenMax`).
#[wasm_bindgen(js_name = pOpenMax)]
pub fn p_open_max() -> u128 {
    P_OPEN_MAX
}

/// The FV rate `p'` at one evaluated state: the penalty if `penalty_applies` (not expired,
/// or CR-liquidatable at that state), else the expiry fee (ECON §8.6, O15). The settlement
/// box decides it per corner (`pprime`, R4A-3).
#[wasm_bindgen(js_name = fvRateBps)]
pub fn fv_rate_bps(penalty_applies: bool, penalty_bps: u32, expiry_fee_bps: u32) -> u32 {
    mrisk::fv_rate_bps(penalty_applies, penalty_bps, expiry_fee_bps)
}

/// `FV = B̂_FV + ⌈p'·B̂_FV/10⁴⌉ + R_due` (D3; ECON §8.6); `undefined` if `B̂_FV` is.
#[wasm_bindgen(js_name = fairValue)]
pub fn fair_value(b_fv: Option<u64>, rate_bps: u32, r_due: u64) -> Option<u64> {
    mrisk::fair_value(b_fv, rate_bps, r_due)
}

/// SOL-settlement keeper reward `K_s = min(fixed + ⌊V·bps/10⁴⌋, cap, V)` (SPEC §6.3).
#[wasm_bindgen(js_name = settleKeeperReward)]
pub fn settle_keeper_reward(v: u64, fixed: u64, bps: u32, cap: u64) -> Result<u64, JsError> {
    Ok(mrisk::k_s(v, &SettleKeeperTerms { fixed, bps, cap })?)
}

/// Expiry keeper reward before truncation, `min(fixed + ⌊bps·B̂_ref/10⁴⌋, cap)` (ECON §8.4).
#[wasm_bindgen(js_name = expiryKeeperReward)]
pub fn expiry_keeper_reward(b_ref: u64, fixed: u64, bps: u32, cap: u64) -> Result<u64, JsError> {
    Ok(mrisk::k_e(b_ref, &ExpiryKeeperTerms { fixed, bps, cap })?)
}

/// Liquidation buyback bound `min(⌊B̂_ref·(10⁴ + δ_liq)/10⁴⌋, V − K_s − ert)` (ECON §8.2
/// v0.4.1; ARCH §3.6): no rent or penalty reserve. `undefined` if negative.
#[wasm_bindgen(js_name = maxInLiq)]
pub fn max_in_liq(b_ref: u64, delta_bps: u16, v: u64, k_s: u64, ert: u64) -> Option<u64> {
    bounds::max_in_liq(b_ref, delta_bps, v, k_s, ert)
}

/// Expiry buyback bound (ECON §8.2; ARCH §3.7): rent and the expiry fee `π_ref` stay
/// senior unless the fair cost itself leaves no room, `min(⌊B̂_ref·(10⁴ + δ_exp)/10⁴⌋,
/// V − K_s − ert, max(V − R − π_ref − ert, ⌊B̂_ref·(10⁴ + ε)/10⁴⌋))`. `undefined` if
/// negative.
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen(js_name = maxInExp)]
pub fn max_in_exp(
    b_ref: u64,
    delta_bps: u16,
    eps_bps: u16,
    v: u64,
    k_s: u64,
    r_due: u64,
    pi_ref: u64,
    ert: u64,
) -> Option<u64> {
    bounds::max_in_exp(b_ref, delta_bps, eps_bps, v, k_s, r_due, pi_ref, ert)
}

/// The expiry fee on the reference cost, `π_ref = ⌈B̂_ref·expiry_fee_bps/10⁴⌉` (ECON §8.2;
/// ARCH §3.7), the part of an expiry's reserve beyond rent.
#[wasm_bindgen(js_name = piRef)]
pub fn pi_ref(b_ref: u64, expiry_fee_bps: u16) -> Result<u64, JsError> {
    Ok(bounds::pi_ref(b_ref, expiry_fee_bps)?)
}

/// Per-position impact cap `⌊X₀·√(1 + ι_pos)⌋ − X₀` (ECON §6.1).
#[wasm_bindgen(js_name = maxQPosition)]
pub fn max_q_position(x0: u64, impact_pos_bps: u32) -> Result<u64, JsError> {
    Ok(caps::max_q_position(x0, impact_pos_bps)?)
}

/// Per-market OI cap `X₁ − isqrt(⌊X₁²·10⁴/(10⁴ + ι_mkt)⌋)` (ECON §6.1; the sim's integer
/// form, ARCH §9 M0b).
#[wasm_bindgen(js_name = maxTotalQ)]
pub fn max_total_q(x1: u64, impact_mkt_bps: u32) -> Result<u64, JsError> {
    Ok(caps::max_total_q(x1, impact_mkt_bps)?)
}

/// Fee-scaled per-position cap (ECON N2), `φ` in ppm; `undefined` when off.
#[wasm_bindgen(js_name = feeSizeCap)]
pub fn fee_size_cap(x0: u64, phi_ppm: u32, fee_size_cap_x100: u32) -> Result<Option<u64>, JsError> {
    Ok(caps::fee_size_cap(x0, phi_ppm, fee_size_cap_x100)?)
}

/// Fee-scaled per-market cap (ECON N2), `φ` in ppm; `undefined` when off.
#[wasm_bindgen(js_name = feeOiCap)]
pub fn fee_oi_cap(x1: u64, phi_ppm: u32, fee_oi_cap_x100: u32) -> Result<Option<u64>, JsError> {
    Ok(caps::fee_oi_cap(x1, phi_ppm, fee_oi_cap_x100)?)
}

/// The binding per-market cap `min(max_total_q, fee_oi_cap)` (ECON §6.1).
#[wasm_bindgen(js_name = oiCap)]
pub fn oi_cap(
    x1: u64,
    impact_mkt_bps: u32,
    phi_ppm: u32,
    fee_oi_cap_x100: u32,
) -> Result<u64, JsError> {
    Ok(caps::oi_cap(x1, impact_mkt_bps, phi_ppm, fee_oi_cap_x100)?)
}

/// Utilisation after the open within `util_cap_bps` (ECON §6.1).
#[wasm_bindgen(js_name = utilOk)]
pub fn util_ok(t_free: u64, borrowed: u64, q: u64, util_cap_bps: u32) -> Result<bool, JsError> {
    Ok(caps::util_ok(t_free, borrowed, q, util_cap_bps)?)
}

/// Tier D graduation guard `ΣQ + q ≤ γ·(rT + q)` (ECON §6.1).
#[wasm_bindgen(js_name = curveGammaOk)]
pub fn curve_gamma_ok(sum_q: u64, q: u64, rt: u64, gamma_bps: u32) -> Result<bool, JsError> {
    Ok(caps::curve_gamma_ok(sum_q, q, rt, gamma_bps)?)
}

/// Largest `q` passing the Tier D graduation guard.
#[wasm_bindgen(js_name = curveGammaMaxQ)]
pub fn curve_gamma_max_q(sum_q: u64, rt: u64, gamma_bps: u32) -> Result<u64, JsError> {
    Ok(caps::curve_gamma_max_q(sum_q, rt, gamma_bps)?)
}

/// Per-wallet cap `q_open + q ≤ wallet_cap_bps·OI_cap/10⁴` (ADOPTED Q10).
#[wasm_bindgen(js_name = walletOk)]
pub fn wallet_ok(q_open: u64, q: u64, oi_cap: u64, wallet_cap_bps: u32) -> Result<bool, JsError> {
    Ok(caps::wallet_ok(q_open, q, oi_cap, wallet_cap_bps)?)
}

/// Notional limits (ADOPTED Q11, Q21).
#[wasm_bindgen(js_name = notionalOk)]
pub fn notional_ok(
    n_open: u64,
    total_notional_open: u64,
    min_notional: u64,
    market_max_notional: u64,
) -> bool {
    caps::notional_ok(
        n_open,
        total_notional_open,
        min_notional,
        market_max_notional,
    )
}

/// The `λ_min` that divides the locked depth `D` of an open on `venue` (the [`TierInputs`]
/// codes): Raydium (2) the config's `pool_lock_min_bps`; PumpSwap (1) `PUMPSWAP_LOCK_MIN_BPS` =
/// 10,000 (100 %) whatever `pool_lock_min_bps` is — OWNER-M6D-1 (ECON #106, ARCH row 57):
/// PumpSwap credits its burned LP only, as the program's `VenueKind::lock_min_bps` (its sizing,
/// tier and exit reference); `None` on a pool without a locked depth (3). The curve (0) has no
/// locked depth and its cap reserves are not `X₀`'s: `InvalidInput`.
fn lock_min_bps(venue: u8, pool_lock_min_bps: u32) -> Result<Option<u32>, MathError> {
    match venue {
        1 => Ok(Some(PUMPSWAP_LOCK_MIN_BPS)),
        2 => Ok(Some(pool_lock_min_bps)),
        3 => Ok(None),
        _ => Err(MathError::InvalidInput),
    }
}

/// `PUMPSWAP_LOCK_MIN_BPS` (10,000; OWNER-M6D-1, ECON #106): PumpSwap's compiled `λ_min` (its
/// sizing, tier and exit reference), which [`conservative_x0`] and [`TierInputs`] apply on venue 1
/// themselves.
#[wasm_bindgen(js_name = pumpswapLockMinBps)]
pub fn pumpswap_lock_min_bps() -> u32 {
    PUMPSWAP_LOCK_MIN_BPS
}

/// Conservative cap reserve `X₀` of an open on `venue` (ARCH §4.6; the [`TierInputs`] venue codes:
/// 1 = PumpSwap, 2 = Raydium, 3 = a pool without a locked depth; the curve, 0, sizes on its cap
/// reserves at the EMA price instead and throws `invalid input`): `min(X_now,
/// ⌊L_slow·10⁹/isqrt⁺(P_ema)⌋)`, on Raydium also `≤ ⌊D·10⁹·10⁴/(λ_min·isqrt⁺(P_ema))⌋` with
/// `λ_min = poolLockMinBps` (V2-8), on PumpSwap (M6R-1) `≤ ⌊D·10⁹/isqrt⁺(P_ema)⌋`, the burned LP's
/// own reserve at `P_ema`: its `λ_min` is `PUMPSWAP_LOCK_MIN_BPS` (100 %, OWNER-M6D-1, ECON #106)
/// whatever `poolLockMinBps` is. `lockedDepth` is `Market.pool_d` (ignored on venue 3);
/// `poolLockMinBps` the config's `pool_lock_min_bps`.
#[wasm_bindgen(js_name = conservativeX0)]
pub fn conservative_x0(
    venue: u8,
    x_now: u64,
    l_slow: u128,
    p_ema: u128,
    locked_depth: u128,
    pool_lock_min_bps: u32,
) -> Result<u64, JsError> {
    let lock = lock_min_bps(venue, pool_lock_min_bps)?.map(|l| (locked_depth, l));
    Ok(caps::conservative_x0(x_now, l_slow, p_ema, lock)?)
}

/// Inputs of the tier function (`tier::TierInputs`; ARCH §4.7). `venue`: 0 = pump
/// curve, 1 = PumpSwap, 2 = Raydium (both with `lockedDepth`; PumpSwap since M6R-1, ECON #105),
/// 3 = a pool without a locked depth (the program's test-venue mock). The locked-depth bound's
/// `λ_min` is `lambdaMinBps` on Raydium and `PUMPSWAP_LOCK_MIN_BPS` (100 %) on PumpSwap
/// (OWNER-M6D-1, ECON #106: `R_eff ≤ max(⌊D·isqrt(P_ema)/10⁹⌋ − max(VQR + buckets, 0), 0)`).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TierInputs {
    /// 0 = pump curve, 1 = PumpSwap, 2 = Raydium, 3 = unlocked pool.
    pub venue: u8,
    /// The locked depth `D` (`Market.pool_d`; Raydium and PumpSwap).
    #[wasm_bindgen(js_name = lockedDepth)]
    pub locked_depth: u128,
    /// The config's `pool_lock_min_bps` (λ_min), read on Raydium only: PumpSwap's is the compiled
    /// `PUMPSWAP_LOCK_MIN_BPS` (OWNER-M6D-1).
    #[wasm_bindgen(js_name = lambdaMinBps)]
    pub lambda_min_bps: u32,
    /// `R_now`: the pool's real SOL, lamports.
    #[wasm_bindgen(js_name = rNow)]
    pub r_now: u64,
    /// `Y(reprice(L_slow, P_ema))`, lamports.
    #[wasm_bindgen(js_name = ySlow)]
    pub y_slow: u128,
    /// The live virtual part `max(virtual_quote_reserves + buckets, 0)` (PumpSwap's boost; the
    /// program's `PoolVenue::virtual_part`, PUMP-2026-10-1; 0 on Raydium).
    pub vqr: u64,
    /// `P_ema`, WAD.
    #[wasm_bindgen(js_name = pEma)]
    pub p_ema: u128,
    /// Unix time.
    pub now: i64,
    /// `Market.graduated_at` (≤ 0: unknown).
    #[wasm_bindgen(js_name = graduatedAt)]
    pub graduated_at: i64,
    /// Pyth SOL/USD mantissa.
    #[wasm_bindgen(js_name = oraclePrice)]
    pub oracle_price: i64,
    /// Pyth exponent.
    #[wasm_bindgen(js_name = oracleExpo)]
    pub oracle_expo: i32,
    /// Pyth publish time.
    #[wasm_bindgen(js_name = oraclePublishTime)]
    pub oracle_publish_time: i64,
    /// Pyth confidence (same exponent): the tier converts the USD thresholds at
    /// `oraclePrice − oracleConf` (M7-SEC-7).
    #[wasm_bindgen(js_name = oracleConf)]
    pub oracle_conf: u64,
    /// `oracle_stale_s`.
    #[wasm_bindgen(js_name = oracleStaleS)]
    pub oracle_stale_s: u32,
    /// θ_A in USD·10⁶.
    #[wasm_bindgen(js_name = thetaAUsdE6)]
    pub theta_a_usd_e6: u64,
    /// θ_B in USD·10⁶.
    #[wasm_bindgen(js_name = thetaBUsdE6)]
    pub theta_b_usd_e6: u64,
}

impl TierInputs {
    fn inputs(&self) -> Result<tier::TierInputs, MathError> {
        let venue = match self.venue {
            0 => TierVenue::PumpCurve,
            1 => TierVenue::PumpSwap {
                locked_depth: self.locked_depth,
                lambda_min_bps: PUMPSWAP_LOCK_MIN_BPS,
            },
            2 => TierVenue::Raydium {
                locked_depth: self.locked_depth,
                lambda_min_bps: self.lambda_min_bps,
            },
            3 => TierVenue::Unlocked,
            _ => return Err(MathError::InvalidInput),
        };
        Ok(tier::TierInputs {
            venue,
            r_now: self.r_now,
            y_slow: self.y_slow,
            vqr: self.vqr,
            p_ema: self.p_ema,
            now: self.now,
            graduated_at: self.graduated_at,
            oracle: OraclePrice {
                price: self.oracle_price,
                conf: self.oracle_conf,
                expo: self.oracle_expo,
                publish_time: self.oracle_publish_time,
            },
            oracle_stale_s: self.oracle_stale_s,
            theta_a_usd_e6: self.theta_a_usd_e6,
            theta_b_usd_e6: self.theta_b_usd_e6,
        })
    }
}

#[wasm_bindgen]
impl TierInputs {
    /// All zero; set the fields, then call `tier()`.
    #[wasm_bindgen(constructor)]
    pub fn new() -> TierInputs {
        TierInputs::default()
    }

    /// The tier for new opens (`tier::assign`): 0 = A, 1 = B, 2 = C, 3 = D.
    pub fn tier(&self) -> Result<u8, JsError> {
        Ok(tier::assign(&self.inputs()?) as u8)
    }

    /// Pool liquidity `Λ = 2·R_eff` in lamports; `undefined` on the curve.
    #[wasm_bindgen(js_name = liquidityLamports)]
    pub fn liquidity_lamports(&self) -> Option<u128> {
        or_throw(self.inputs().and_then(|t| tier::liquidity_lamports(&t)))
    }
}

/// A tier's default parameters (`tier::TierParams::DEFAULTS`; ARCH §2.3.3).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TierParams(tier::TierParams);

/// The SPEC §4 / §6.1 defaults of tier 0 = A, 1 = B, 2 = C, 3 = D.
#[wasm_bindgen(js_name = tierParams)]
pub fn tier_params(tier: u8) -> Result<TierParams, JsError> {
    let t = Tier::from_u8(tier).ok_or(MathError::InvalidInput)?;
    Ok(TierParams(t.defaults()))
}

#[wasm_bindgen]
impl TierParams {
    /// Opening CR (D4), bps.
    #[wasm_bindgen(getter, js_name = crOpenBps)]
    pub fn cr_open_bps(&self) -> u32 {
        self.0.cr_open_bps
    }

    /// Liquidation CR, bps.
    #[wasm_bindgen(getter, js_name = crLiqBps)]
    pub fn cr_liq_bps(&self) -> u32 {
        self.0.cr_liq_bps
    }

    /// Liquidation penalty, bps.
    #[wasm_bindgen(getter, js_name = penaltyBps)]
    pub fn penalty_bps(&self) -> u32 {
        self.0.penalty_bps
    }

    /// Max duration, seconds.
    #[wasm_bindgen(getter, js_name = maxDurationS)]
    pub fn max_duration_s(&self) -> u32 {
        self.0.max_duration_s
    }

    /// Per-position impact cap, bps.
    #[wasm_bindgen(getter, js_name = impactPosBps)]
    pub fn impact_pos_bps(&self) -> u32 {
        self.0.impact_pos_bps
    }

    /// Per-market impact cap, bps.
    #[wasm_bindgen(getter, js_name = impactMktBps)]
    pub fn impact_mkt_bps(&self) -> u32 {
        self.0.impact_mkt_bps
    }

    /// S2 slippage threshold σ_max, bps.
    #[wasm_bindgen(getter, js_name = maxLiqSlipBps)]
    pub fn max_liq_slip_bps(&self) -> u32 {
        self.0.max_liq_slip_bps
    }

    /// Rent base APR, bps.
    #[wasm_bindgen(getter, js_name = rateBaseBps)]
    pub fn rate_base_bps(&self) -> u32 {
        self.0.rate.base_bps
    }

    /// Rent slope below the kink, bps.
    #[wasm_bindgen(getter, js_name = rateSlope1Bps)]
    pub fn rate_slope1_bps(&self) -> u32 {
        self.0.rate.slope1_bps
    }

    /// Rent slope above the kink, bps.
    #[wasm_bindgen(getter, js_name = rateSlope2Bps)]
    pub fn rate_slope2_bps(&self) -> u32 {
        self.0.rate.slope2_bps
    }

    /// Minimum rent, seconds of rent.
    #[wasm_bindgen(getter, js_name = minRentS)]
    pub fn min_rent_s(&self) -> u32 {
        self.0.min_rent_s
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn risk_wrappers() {
        assert_eq!(cr_bps(200, Some(100)).unwrap(), 20_000);
        assert_eq!(cr_bps(200, None).unwrap(), 0);
        assert!(cr_below(124, Some(100), 12_500).unwrap());
        assert_eq!(margin_d4(100, 100, Some(120), 20_000).unwrap(), 140);
        assert_eq!(open_fee(1_001, 95).unwrap(), 10);
        assert_eq!(fair_value(None, 500, 7), None);
        assert_eq!(
            settle_keeper_reward(1_000_000_000, 10_000_000, 100, 250_000_000).unwrap(),
            20_000_000
        );
        assert_eq!(max_in_liq(1_000, 200, 10_000, 100, 0), Some(1_020));
        assert_eq!(max_in_liq(1_000, 200, 1_000, 100, 0), Some(900));
        assert_eq!(max_in_liq(1_000, 200, 100, 100, 1), None);
        let pi = pi_ref(1_000, 200).unwrap();
        assert_eq!(pi, 20);
        // Rent and fee senior: V − R − π − ert = 1,100 − 1,000 − 20 = 80 < ⌊B̂·(1 + ε)⌋ = 1,001.
        assert_eq!(
            max_in_exp(1_000, 100, 10, 1_100, 0, 1_000, pi, 0),
            Some(1_001)
        );
        assert_eq!(
            max_in_exp(1_000, 100, 10, 10_000, 100, 50, pi, 0),
            Some(1_010)
        );
        assert_eq!(v_eff(10, 11), 0);
    }

    #[test]
    fn open_guards() {
        let wad = 1_000_000_000_000_000_000u128;
        // 20 % deviation at dev 2,000 bps passes on both sides; a wei more does not.
        assert!(spot_near_ema(1_200_000_000_000_000_000, wad, 2_000));
        assert!(spot_near_ema(800_000_000_000_000_000, wad, 2_000));
        assert!(!spot_near_ema(1_200_000_000_000_000_001, wad, 2_000));
        assert!(!spot_near_ema(799_999_999_999_999_999, wad, 2_000));
        assert!(!spot_near_ema(wad, 0, 10_000));
        // No overflow at the u128 edge (the product is compared without being formed).
        assert!(spot_near_ema(u128::MAX, u128::MAX, 0));
        assert!(!spot_near_ema(0, u128::MAX, 9_999));
        assert_eq!(p_open_max(), 1_000_000_000_000_000_000_000_000_000);
    }

    #[test]
    fn caps_and_tiers() {
        let x = 1_000_000_000_000u64;
        assert_eq!(max_q_position(x, 500).unwrap(), 24_695_076_595);
        assert!(util_ok(1_000, 0, 900, 9_000).unwrap());
        assert!(!util_ok(1_000, 0, 901, 9_000).unwrap());
        let mut t = TierInputs::new();
        t.venue = 0;
        assert_eq!(t.tier().unwrap(), 3);
        t.venue = 1;
        t.r_now = 40_000_000_000_000;
        t.y_slow = 40_000_000_000_000;
        t.now = 10_000_000;
        t.graduated_at = 1;
        t.oracle_price = 12_100_000_000;
        t.oracle_expo = -8;
        t.oracle_publish_time = t.now;
        t.oracle_stale_s = 120;
        t.theta_a_usd_e6 = 500_000_000_000;
        t.theta_b_usd_e6 = 100_000_000_000;
        // PumpSwap (M6R-1): a burned-LP depth worth 40,000 SOL at P = 1 WAD over PumpSwap's
        // λ_min, 100 % (OWNER-M6D-1) whatever the config's pool_lock_min_bps (50 %) says.
        t.p_ema = 1_000_000_000_000_000_000;
        t.locked_depth = 40_000_000_000_000;
        t.lambda_min_bps = 5_000;
        assert_eq!(t.tier().unwrap(), 0);
        assert_eq!(t.liquidity_lamports(), Some(80_000_000_000_000));
        // 2,000 SOL locked: Λ = 4,000 SOL, under θ_A's 4,132 SOL at $121 (B) — at λ_min 50 % it
        // would credit 4,000 SOL of R_eff (Λ 8,000, A).
        t.locked_depth = 2_000_000_000_000;
        assert_eq!(t.liquidity_lamports(), Some(4_000_000_000_000));
        assert_eq!(t.tier().unwrap(), 1);
        // Raydium reads pool_lock_min_bps: the same depth credits 2× (Λ 8,000 SOL, A).
        t.venue = 2;
        assert_eq!(t.liquidity_lamports(), Some(8_000_000_000_000));
        assert_eq!(t.tier().unwrap(), 0);
        t.venue = 3;
        assert_eq!(t.liquidity_lamports(), Some(80_000_000_000_000));
        assert_eq!(t.tier().unwrap(), 0);
        // X₀ (ARCH §4.6): D = 10⁶ at P = 1 WAD, X_now and L_slow above it — PumpSwap sizes on D
        // itself, Raydium on D/λ_min, an unlocked pool on min(X_now, L_slow) alone.
        let wad = 1_000_000_000_000_000_000u128;
        assert_eq!(pumpswap_lock_min_bps(), 10_000);
        assert_eq!(
            conservative_x0(1, 9_000_000, 8_000_000, wad, 1_000_000, 5_000).ok(),
            Some(1_000_000)
        );
        assert_eq!(
            conservative_x0(1, 9_000_000, 8_000_000, wad, 1_000_000, 2_000).ok(),
            Some(1_000_000)
        );
        assert_eq!(
            conservative_x0(2, 9_000_000, 8_000_000, wad, 1_000_000, 5_000).ok(),
            Some(2_000_000)
        );
        assert_eq!(
            conservative_x0(3, 9_000_000, 8_000_000, wad, 1_000_000, 5_000).ok(),
            Some(8_000_000)
        );
        assert_eq!(lock_min_bps(0, 5_000), Err(MathError::InvalidInput));
        let d = tier_params(3).unwrap();
        assert_eq!((d.cr_open_bps(), d.max_duration_s()), (40_000, 259_200));
    }
}
