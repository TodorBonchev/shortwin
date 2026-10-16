//! Owner: caps track. Position and OI size caps, fee-scaled caps, utilisation, curve gamma and wallet caps (ECON §2.1, §6.1; ARCH §4.6).
//!
//! Caps are checked in `open_short` only and never force a close. `X₀` is the
//! conservative pre-trade token reserve ([`conservative_x0`]) and `X₁ = X₀ + q` the
//! post-open one (the sell adds `q` to the token side). Each cap is the integer form of
//! ECON §2.1 / the sims exactly (`sim/shortwin_sim.py` v0.4.1 `max_q_impact`,
//! `max_total_q_market`, `Market.oi_cap_q`, `Market.max_open_q`, `Market.conservative_x`);
//! predicates compare cross-multiplied integers with no rounding.
//!
//! Numerics (ARCH §2.2): `u128` throughout; `mul_div` falls back to a 256-bit product
//! only when `a·b` overflows, and square roots are `u128` (`isqrt_ratio` refines a
//! scaled `u128` root in the one case, `max_q_position` at `X₀` within 5 % of `2⁶⁴`,
//! where the radicand exceeds `u128`).

use crate::consts::CARRIED_FEE_CAP_PPM;
use crate::error::MathError;
use crate::num::{
    BPS, Rounding, SQRT_WAD, U256, mul_div, mul_le, overflow_to_max, saturating_u64, sqrt_u128,
    to_u64,
};

const BPS_U128: u128 = BPS as u128;
/// `100·10⁶`: the fee-scaled caps' `x100` and ppm denominators.
const FEE_CAP_DEN: u128 = 100_000_000;
/// `10⁹·10⁴`: the Raydium locked-depth bound's numerator scale (ECON §2.1).
const LOCK_SCALE: u128 = 10_000_000_000_000;

/// `⌊√⌊a·b/d⌋⌋` with `u128` square roots only (ARCH §2.2). When the quotient fits `u128`
/// (always for `b ≤ d`) it is `sqrt_u128` of it. Otherwise, with `q = ⌊a·b/d⌋` on 256
/// bits and the smallest `k` such that `q/4ᵏ < 2¹²⁸`: `⌊√⌊q/4ᵏ⌋⌋ = ⌊⌊√q⌋/2ᵏ⌋`, so the root
/// is `2ᵏ·s'` plus the `k` low bits, found bit by bit against `s² ≤ q` (`k = 1` for every
/// impact cap within the compiled bounds).
///
/// Errors: `DivByZero` if `d = 0`.
fn isqrt_ratio(a: u128, b: u128, d: u128) -> Result<u128, MathError> {
    match mul_div(a, b, d, Rounding::Floor) {
        Ok(n) => return Ok(sqrt_u128(n, Rounding::Floor)),
        Err(MathError::Overflow) => {}
        Err(e) => return Err(e),
    }
    let (q, _) = U256::from_product(a, b)
        .div_rem_u128(d)
        .ok_or(MathError::DivByZero)?;
    // q ≥ 2¹²⁸ here, so bits ∈ [129, 256] and k ∈ [1, 64].
    let k = q.bits().saturating_sub(127) / 2;
    let top = q
        .checked_shr(k.saturating_mul(2))
        .and_then(U256::to_u128)
        .ok_or(MathError::Overflow)?;
    let mut s = sqrt_u128(top, Rounding::Floor)
        .checked_shl(k)
        .ok_or(MathError::Overflow)?;
    for j in (0..k).rev() {
        let bit = 1u128.checked_shl(j).ok_or(MathError::Overflow)?;
        let cand = s | bit;
        if U256::from_product(cand, cand) <= q {
            s = cand;
        }
    }
    Ok(s)
}

/// Per-position impact cap (SPEC §4): buying `q` back from the post-open reserves
/// `X₀ + q` moves the marginal price by at most `ι_pos`, `q ≤ X₀·(√(1 + ι_pos) − 1)`
/// (ECON §6.1; ARCH §4.6), in the sims' integer form
/// `isqrt(⌊X₀²·(10⁴ + ι_pos)/10⁴⌋) − X₀` (`max_q_impact`): the largest `q` with
/// `(X₀ + q)²·10⁴ ≤ X₀²·(10⁴ + ι_pos)`. 2.470 % of `X₀` at 5 %, 1.489 % at 3 % (Tier D).
/// Saturates at `u64::MAX`.
///
/// Errors: none in practice.
pub fn max_q_position(x0: u64, impact_pos_bps: u32) -> Result<u64, MathError> {
    let x = u128::from(x0);
    let xx = x.checked_mul(x).ok_or(MathError::Overflow)?;
    let root = isqrt_ratio(
        xx,
        BPS_U128.saturating_add(u128::from(impact_pos_bps)),
        BPS_U128,
    )?;
    Ok(saturating_u64(root.saturating_sub(x)))
}

/// Per-market OI cap (SPEC §4), post-open: buying all of `ΣQ + q` back from `X₁` moves
/// the price by at most `ι_mkt`, `ΣQ + q ≤ X₁·(1 − 1/√(1 + ι_mkt))` (ECON §6.1; ARCH
/// §4.6), in the sims' integer form **exactly** (ARCH §9 M0b; the sim is normative):
/// `X₁ − isqrt(⌊X₁²·10⁴/(10⁴ + ι_mkt)⌋)` (`max_total_q_market`). 8.713 % of `X₁` at 20 %,
/// 4.654 % at 10 % (Tier D). The flooring inside the root can put it one raw unit above
/// the real-valued bound; v0.3's crate returned the largest integer within that bound.
///
/// Errors: none in practice.
pub fn max_total_q(x1: u64, impact_mkt_bps: u32) -> Result<u64, MathError> {
    let x = u128::from(x1);
    let xx = x.checked_mul(x).ok_or(MathError::Overflow)?;
    let root = isqrt_ratio(
        xx,
        BPS_U128,
        BPS_U128.saturating_add(u128::from(impact_mkt_bps)),
    )?;
    to_u64(x.checked_sub(root).ok_or(MathError::Underflow)?)
}

/// `⌊x100·φ_ppm·X/(100·10⁶)⌋` (saturating at `u64::MAX`), or `None` if the cap is off.
fn fee_cap(x: u64, phi_ppm: u32, x100: u32) -> Result<Option<u64>, MathError> {
    if x100 == 0 {
        return Ok(None);
    }
    let k = u128::from(phi_ppm)
        .checked_mul(u128::from(x100))
        .ok_or(MathError::Overflow)?;
    // A cap above `u64::MAX` tokens never binds: saturate instead of failing an open.
    let cap = overflow_to_max(mul_div(u128::from(x), k, FEE_CAP_DEN, Rounding::Floor))?;
    Ok(Some(saturating_u64(cap)))
}

/// Fee-scaled per-position cap (ECON §2.1, §6.1, N2): `q ≤ ⌊fee_size_cap_x100·φ·X₀/(100·10⁶)⌋`
/// with `φ` the live one-way venue fee in **ppm** (`oneway_ppm()` of the resolved fee,
/// ARCH §4.2, §4.6; the sims' `Market.max_open_q`): 3.0·φ·X₀ at the default 300. `None`
/// when `fee_size_cap_x100 = 0` (off; outside the compiled bounds [100, 500], kept as the
/// sims' switch); saturates at `u64::MAX`.
///
/// Errors: none in practice.
pub fn fee_size_cap(
    x0: u64,
    phi_ppm: u32,
    fee_size_cap_x100: u32,
) -> Result<Option<u64>, MathError> {
    fee_cap(x0, phi_ppm, fee_size_cap_x100)
}

/// Fee-scaled per-market cap (ECON §2.1, §6.1, N2), post-open:
/// `ΣQ + q ≤ ⌊fee_oi_cap_x100·φ·X₁/(100·10⁶)⌋`, `φ` in ppm (the sims' `Market.oi_cap_q`):
/// 7.5·φ·X₁ at the default 750. `None` when `fee_oi_cap_x100 = 0` (off; outside the
/// compiled bounds [300, 1,000]); saturates at `u64::MAX`.
///
/// Errors: none in practice.
pub fn fee_oi_cap(x1: u64, phi_ppm: u32, fee_oi_cap_x100: u32) -> Result<Option<u64>, MathError> {
    fee_cap(x1, phi_ppm, fee_oi_cap_x100)
}

/// The binding per-market cap `OI_cap = min(max_total_q, fee_oi_cap)` on `X₁`, `φ` in ppm
/// (ECON §6.1; ARCH §4.6; `shortwin_sim.Market.oi_cap_q` exactly), the base of the wallet
/// cap.
///
/// Errors: as [`max_total_q`] and [`fee_oi_cap`].
pub fn oi_cap(
    x1: u64,
    impact_mkt_bps: u32,
    phi_ppm: u32,
    fee_oi_cap_x100: u32,
) -> Result<u64, MathError> {
    let cap = max_total_q(x1, impact_mkt_bps)?;
    Ok(match fee_oi_cap(x1, phi_ppm, fee_oi_cap_x100)? {
        Some(f) => cap.min(f),
        None => cap,
    })
}

/// Utilisation after the open within the cap (SPEC §4, §10; ECON §6.1):
/// `⌈(ΣB_Q + q)·10⁴/(T + ΣB_Q)⌉ ≤ util_cap_bps`, evaluated exactly as
/// `(ΣB_Q + q)·10⁴ ≤ util_cap_bps·(T + ΣB_Q)`. `T` and `ΣB_Q` are the pre-open counters;
/// with a cap below 100 % this also implies `q < T`.
///
/// Errors: none in practice.
pub fn util_ok(t_free: u64, borrowed: u64, q: u64, util_cap_bps: u32) -> Result<bool, MathError> {
    let after = u128::from(borrowed)
        .checked_add(u128::from(q))
        .ok_or(MathError::Overflow)?;
    let total = u128::from(t_free)
        .checked_add(u128::from(borrowed))
        .ok_or(MathError::Overflow)?;
    Ok(mul_le(after, BPS_U128, u128::from(util_cap_bps), total))
}

/// Tier D graduation guard (ECON §6.1; ARCH §4.6): `ΣQ + q ≤ γ·(rT + q)/10⁴`, with `rT`
/// the curve's real token reserves before the open (the sell adds `q` to them) and
/// `γ = curve_oi_gamma_bps`. Exact: `(ΣQ + q)·10⁴ ≤ γ·(rT + q)`.
///
/// Errors: none in practice.
pub fn curve_gamma_ok(sum_q: u64, q: u64, rt: u64, gamma_bps: u32) -> Result<bool, MathError> {
    let lhs = u128::from(sum_q)
        .checked_add(u128::from(q))
        .ok_or(MathError::Overflow)?;
    let rhs = u128::from(rt)
        .checked_add(u128::from(q))
        .ok_or(MathError::Overflow)?;
    Ok(mul_le(lhs, BPS_U128, u128::from(gamma_bps), rhs))
}

/// Largest `q` that passes [`curve_gamma_ok`]:
/// `⌊(γ·rT − 10⁴·ΣQ)/(10⁴ − γ)⌋`, 0 if negative (`shortwin_sim.Market.max_open_q` with
/// `ΣQ = 0`: `⌊rT·γ/(10⁴ − γ)⌋`). Saturates at `u64::MAX` like every other cap (a cap
/// above `u64::MAX` never binds; only a non-standard curve with `rT` near `u64::MAX` gets
/// there).
///
/// Errors: `InvalidInput` if `γ ≥ 10⁴` (unbounded).
pub fn curve_gamma_max_q(sum_q: u64, rt: u64, gamma_bps: u32) -> Result<u64, MathError> {
    let g = u128::from(gamma_bps);
    let den = BPS_U128
        .checked_sub(g)
        .filter(|d| *d > 0)
        .ok_or(MathError::InvalidInput)?;
    let num = g
        .saturating_mul(u128::from(rt))
        .saturating_sub(BPS_U128.saturating_mul(u128::from(sum_q)));
    Ok(saturating_u64(
        num.checked_div(den).ok_or(MathError::DivByZero)?,
    ))
}

/// Per-wallet cap (ADOPTED Q10; ARCH §4.6): `q_open + q ≤ wallet_cap_bps·OI_cap/10⁴`,
/// exactly `(q_open + q)·10⁴ ≤ wallet_cap_bps·OI_cap`, with `OI_cap` from [`oi_cap`].
///
/// Errors: none in practice.
pub fn wallet_ok(q_open: u64, q: u64, oi_cap: u64, wallet_cap_bps: u32) -> Result<bool, MathError> {
    let lhs = u128::from(q_open)
        .checked_add(u128::from(q))
        .ok_or(MathError::Overflow)?;
    Ok(mul_le(
        lhs,
        BPS_U128,
        u128::from(wallet_cap_bps),
        u128::from(oi_cap),
    ))
}

/// Notional limits (ADOPTED Q11, Q21; ARCH §3.4 step 9): `N_open ≥ min_notional` and,
/// when `market_max_notional > 0`, `total_notional_open + N_open ≤ market_max_notional`.
pub fn notional_ok(
    n_open: u64,
    total_notional_open: u64,
    min_notional: u64,
    market_max_notional: u64,
) -> bool {
    n_open >= min_notional
        && (market_max_notional == 0
            || u128::from(total_notional_open).saturating_add(u128::from(n_open))
                <= u128::from(market_max_notional))
}

/// Conservative pre-trade token reserve `X₀` for the caps of an AMM market (ECON §2.1,
/// §6.1; ARCH §2.6 rule 5, §4.6; the sims' `Market.conservative_x`):
/// `min(X_now, ⌊L_slow·10⁹/isqrt⁺(P_ema)⌋)` — the token side of
/// `reprice(L_slow, P_ema)` ([`reprice_token`](crate::quote::reprice_token)) — and on
/// Raydium and (M6R-1, ECON #105) a canonical PumpSwap pool also
/// `≤ ⌊D·10⁹·10⁴/(λ_min_bps·isqrt⁺(P_ema))⌋`, the locked depth over `λ_min` (V2-8).
/// `raydium_lock` (the name predates PumpSwap's lock) is `(D, λ_min)`: on Raydium `(D,
/// pool_lock_min_bps)`; on PumpSwap `(pool_d,`
/// [`PUMPSWAP_LOCK_MIN_BPS`](crate::consts::PUMPSWAP_LOCK_MIN_BPS)`)` since OWNER-M6D-1 (ECON
/// #106), which makes the term `⌊D·10⁹/isqrt⁺(P_ema)⌋`, the burned LP's token reserve at
/// `P_ema`. The pump curve uses `vT` directly. A bound beyond `u128` does not bind.
///
/// Errors: `DivByZero` if `P_ema = 0` or `λ_min = 0`.
pub fn conservative_x0(
    x_now: u64,
    l_slow: u128,
    p_ema: u128,
    raydium_lock: Option<(u128, u32)>,
) -> Result<u64, MathError> {
    let sp = sqrt_u128(p_ema, Rounding::Ceil);
    let x_slow = overflow_to_max(mul_div(l_slow, SQRT_WAD, sp, Rounding::Floor))?;
    let mut x = u128::from(x_now).min(x_slow);
    if let Some((depth, lambda_min_bps)) = raydium_lock {
        let den = u128::from(lambda_min_bps)
            .checked_mul(sp)
            .ok_or(MathError::Overflow)?;
        let x_lock = overflow_to_max(mul_div(depth, LOCK_SCALE, den, Rounding::Floor))?;
        x = x.min(x_lock);
    }
    to_u64(x)
}

/// Open guard `|P_spot/P_ema − 1| ≤ open_ema_dev_bps` (ECON §4 "Open guards"; ARCH §3.4
/// step 4), evaluated exactly as `|P_spot − P_ema|·10⁴ ≤ dev_bps·P_ema` (no division, no
/// rounding). `P_spot` is the live pre-trade spot, `P_ema` the post-roll EMA. `false` for a
/// zero `P_ema` (no price: no open).
pub fn spot_near_ema(p_spot: u128, p_ema: u128, dev_bps: u32) -> bool {
    if p_ema == 0 {
        return false;
    }
    let diff = p_spot.abs_diff(p_ema);
    mul_le(diff, BPS_U128, u128::from(dev_bps), p_ema)
}

/// The binding per-position cap on `X₀`: `min(max_q_position(X₀, ι_pos), fee_size_cap(X₀, φ,
/// fee_size_cap_x100))` (the fee-scaled cap only when on), `φ` in ppm — exactly the two
/// per-position checks of an open ([`max_q_position`], [`fee_size_cap`]; ECON §6.1; ARCH §4.6).
///
/// Errors: as [`max_q_position`] and [`fee_size_cap`].
pub fn position_cap(
    x0: u64,
    impact_pos_bps: u32,
    phi_ppm: u32,
    fee_size_cap_x100: u32,
) -> Result<u64, MathError> {
    let cap = max_q_position(x0, impact_pos_bps)?;
    Ok(match fee_size_cap(x0, phi_ppm, fee_size_cap_x100)? {
        Some(f) => cap.min(f),
        None => cap,
    })
}

/// The O(1) upper bound on every open position's `q` that the carried-position fit
/// ([`carried_fit`]) checks: `min(max_q_hwm, ΣQ)` — `Market.max_q_hwm` is the high-water mark of
/// `q` over the positions opened since `open_positions` last returned to 0 (so at least every
/// open `q`), and `ΣQ = ΣB_Q` is their sum (I6, so at least each of them too). A zero
/// high-water mark with positions open (a market written before the field existed) bounds
/// nothing: then `ΣQ` alone. Conservative by design: a position that closed may have set the
/// mark.
pub fn carried_q_bound(max_q_hwm: u64, sum_q: u64) -> u64 {
    if max_q_hwm == 0 {
        sum_q
    } else {
        max_q_hwm.min(sum_q)
    }
}

/// `Market.min_open_fee_ppm` after an open at the one-way venue fee `phi_ppm` (the fee the open's
/// caps were computed at): `phi_ppm` for the first open after the market emptied (`mark = 0`),
/// else `min(mark, phi_ppm)` — the low-water mark of the fee the open positions posted their
/// margin at (FIT-2; reset to 0 with `max_q_hwm` when `open_positions` returns to 0).
pub fn open_fee_mark(mark: u32, phi_ppm: u32) -> u32 {
    if mark == 0 {
        phi_ppm
    } else {
        mark.min(phi_ppm)
    }
}

/// The fee φ the carried fit's fee-scaled caps ([`fee_size_cap`], [`fee_oi_cap`]) take (FIT-2,
/// FITS-1): `min(φ_inc, φ_cand, CARRIED_FEE_CAP_PPM)`, `φ_inc = Market.min_open_fee_ppm`
/// ([`open_fee_mark`]: the lowest one-way venue fee an open paid since `open_positions` last
/// returned to 0 — the fee the open positions posted their margin at), `φ_cand` the candidate's
/// one-way fee and [`CARRIED_FEE_CAP_PPM`] (**PROPOSED**, 25 bps) the ceiling. A position carried
/// onto a higher-fee pool stays in the size class of the fee it was margined at (0.75 % of `X₀`
/// at 25 bps, not 2.47 % at 1 %); a lower-fee candidate's own fee binds; and whatever both fees,
/// no carried position is above the 25-bps class (0.75 % of `X₀`) — a 1 % → 1 % switch no longer
/// carries positions up to the candidate's impact cap (FITS-1). A zero mark with positions open
/// (an account written before the field existed) bounds nothing: `min(φ_cand, CARRIED_FEE_CAP_PPM)`,
/// as [`carried_q_bound`]'s zero mark. The impact caps do not read φ.
pub fn carried_phi(min_open_fee_ppm: u32, phi_cand_ppm: u32) -> u32 {
    let phi = if min_open_fee_ppm == 0 {
        phi_cand_ppm
    } else {
        min_open_fee_ppm.min(phi_cand_ppm)
    };
    phi.min(CARRIED_FEE_CAP_PPM)
}

/// The candidate's opening caps of the carried-position fit ([`carried_fit`]): its conservative
/// pre-trade token reserve `X₀` ([`conservative_x0`] on the candidate), the impact caps of the
/// tier the market would have on it, the fee φ (ppm) of the fee-scaled caps — [`carried_phi`] of
/// the market's `min_open_fee_ppm` and the candidate's one-way fee — and the fee-scaled cap
/// multipliers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CarriedCaps {
    pub x0: u64,
    pub impact_pos_bps: u32,
    pub impact_mkt_bps: u32,
    pub phi_ppm: u32,
    pub fee_size_cap_x100: u32,
    pub fee_oi_cap_x100: u32,
}

/// The outcome of [`carried_fit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CarriedFit {
    /// Every open position fits the per-position caps and `ΣQ` the per-market caps.
    Fits,
    /// The position bound exceeds the per-position impact cap or the fee-scaled size cap.
    Position,
    /// `ΣQ` exceeds the per-market OI cap `min(SPEC impact cap, fee-scaled OI cap)`.
    Market,
}

impl CarriedFit {
    /// `true` for [`CarriedFit::Fits`].
    pub const fn fits(self) -> bool {
        matches!(self, Self::Fits)
    }
}

/// "Switch only if all fit" (owner decision of 2026-09-29; ARCH §2.6, ECON §6.1): a pool
/// adoption that carries open positions — a collection window's — is allowed only if every open
/// position fits the **candidate's normal opening caps**, evaluated as an open would be on it at
/// adoption time (ARCH §4.6; ECON §6.1):
///
/// - per position: `q_max ≤ min(max_q_position(X₀, ι_pos), fee_size_cap(X₀, φ))`
///   ([`position_cap`]), with `q_max` an upper bound on every open `q` ([`carried_q_bound`]);
/// - per market: `ΣQ ≤ oi_cap(X₀, ι_mkt, φ, fee_oi_cap_x100)` ([`oi_cap`]) — an open's
///   `ΣB_Q + q ≤ OI_cap(X₀ + q)` at `q = 0`: the carried tokens were sold into the old pool, so
///   the candidate's reserve holds none of them and a buyback of `ΣQ` comes out of `X₀` itself.
///
/// The per-position check comes first. Exact integers, no rounding beyond the caps' own.
///
/// Errors: as [`position_cap`] and [`oi_cap`] (none in practice).
pub fn carried_fit(q_max: u64, sum_q: u64, c: &CarriedCaps) -> Result<CarriedFit, MathError> {
    let pos = position_cap(c.x0, c.impact_pos_bps, c.phi_ppm, c.fee_size_cap_x100)?;
    if q_max > pos {
        return Ok(CarriedFit::Position);
    }
    let oi = oi_cap(c.x0, c.impact_mkt_bps, c.phi_ppm, c.fee_oi_cap_x100)?;
    if sum_q > oi {
        return Ok(CarriedFit::Market);
    }
    Ok(CarriedFit::Fits)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn spot_near_ema_is_exact() {
        let e = 1_000_000_000_000_000_000u128;
        assert!(spot_near_ema(e, e, 0));
        assert!(spot_near_ema(e * 11 / 10, e, 1_000));
        assert!(!spot_near_ema(e * 11 / 10 + 1, e, 1_000));
        assert!(spot_near_ema(e * 9 / 10, e, 1_000));
        assert!(!spot_near_ema(e * 9 / 10 - 1, e, 1_000));
        assert!(!spot_near_ema(e, 0, 1_000));
        // No overflow at the extremes (the 256-bit fallback of mul_le).
        assert!(spot_near_ema(u128::MAX, u128::MAX, 0));
        assert!(!spot_near_ema(u128::MAX, 1, 5_000));
    }

    #[test]
    fn econ_closed_forms() {
        let x = 150_000_000_000_000u64;
        // ECON §6.1 table: 2.470 % / 1.489 % of X₀, 8.713 % / 4.654 % of X₁.
        let p = max_q_position(x, 500).unwrap();
        assert_eq!(p * 100_000 / x, 2_469);
        assert_eq!(max_q_position(x, 300).unwrap() * 100_000 / x, 1_488);
        assert_eq!(max_total_q(x, 2_000).unwrap() * 100_000 / x, 8_712);
        assert_eq!(max_total_q(x, 1_000).unwrap() * 100_000 / x, 4_653);
        assert_eq!(max_q_position(0, 500), Ok(0));
        assert_eq!(max_total_q(0, 2_000), Ok(0));
        assert_eq!(max_q_position(100, 0), Ok(0));
        assert_eq!(max_total_q(100, 0), Ok(0));
        // The sims' integer form: X − isqrt(⌊X²·10⁴/(10⁴ + ι)⌋).
        let xx = u128::from(x) * u128::from(x);
        let want = u128::from(x) - (xx * 10_000 / 12_000).isqrt();
        assert_eq!(u128::from(max_total_q(x, 2_000).unwrap()), want);
        // 25 bps pool (2,500 ppm): 3.0·φ = 0.75 % of X₀; 7.5·φ = 1.875 % of X₁.
        assert_eq!(fee_size_cap(x, 2_500, 300), Ok(Some(x * 75 / 10_000)));
        assert_eq!(fee_oi_cap(x, 2_500, 750), Ok(Some(x * 1_875 / 100_000)));
        assert_eq!(fee_size_cap(x, 2_500, 0), Ok(None));
        assert_eq!(oi_cap(x, 2_000, 2_500, 750), Ok(x * 1_875 / 100_000));
        assert_eq!(oi_cap(x, 2_000, 2_500, 0), max_total_q(x, 2_000));
        // A CPMM rate that is not a whole bp enters exactly (R3A-3 iii): 2,550 ppm.
        let want = u64::try_from(u128::from(x) * 2_550 * 300 / 100_000_000).unwrap();
        assert_eq!(fee_size_cap(x, 2_550, 300), Ok(Some(want)));
        // Absurd rates saturate instead of failing an open.
        assert_eq!(
            fee_size_cap(u64::MAX, u32::MAX, u32::MAX),
            Ok(Some(u64::MAX))
        );
    }

    /// The radicand of `max_q_position` exceeds `u128` for `X₀` near `2⁶⁴`: the scaled
    /// `u128` root plus its low bits equals the 256-bit root.
    #[test]
    fn isqrt_ratio_matches_the_wide_root() {
        let cases = [
            (
                u128::from(u64::MAX) * u128::from(u64::MAX),
                11_000u128,
                10_000u128,
            ),
            (u128::from(u64::MAX) * u128::from(u64::MAX), 10_500, 10_000),
            (u128::MAX, u128::MAX, 1),
            (u128::MAX, 3, 1),
            (u128::MAX, 7, 2),
            (1u128 << 127, 5, 1),
            (12_345, 678, 9),
            (0, 5, 7),
        ];
        for (a, b, d) in cases {
            let want = U256::from_product(a, b).div_rem_u128(d).unwrap().0.isqrt();
            assert_eq!(isqrt_ratio(a, b, d), Ok(want), "{a} {b} {d}");
        }
        assert_eq!(isqrt_ratio(1, 1, 0), Err(MathError::DivByZero));
        let x = u64::MAX;
        let q = max_q_position(x, 1_000).unwrap();
        let root = U256::from_product(u128::from(x), u128::from(x))
            .checked_mul(U256::from_u128(11_000))
            .unwrap()
            .div_rem_u128(10_000)
            .unwrap()
            .0
            .isqrt();
        assert_eq!(u128::from(q), root - u128::from(x));
        let xx = u128::from(x) * u128::from(x);
        let root = U256::from_product(xx, 10_000)
            .div_rem_u128(15_000)
            .unwrap()
            .0
            .isqrt();
        assert_eq!(
            u128::from(max_total_q(x, 5_000).unwrap()),
            u128::from(x) - root
        );
    }

    #[test]
    fn predicates() {
        assert_eq!(util_ok(20, 70, 11, 9_000), Ok(true));
        assert_eq!(util_ok(20, 70, 12, 9_000), Ok(false));
        assert_eq!(util_ok(0, 0, 1, 10_000), Ok(false));
        assert_eq!(util_ok(0, 0, 0, 9_000), Ok(true));
        assert_eq!(curve_gamma_ok(0, 100, 100, 5_000), Ok(true));
        assert_eq!(curve_gamma_ok(0, 101, 100, 5_000), Ok(false));
        assert_eq!(curve_gamma_max_q(0, 100, 5_000), Ok(100));
        assert_eq!(curve_gamma_max_q(60, 100, 5_000), Ok(0));
        assert_eq!(
            curve_gamma_max_q(0, 100, 10_000),
            Err(MathError::InvalidInput)
        );
        assert_eq!(wallet_ok(20, 5, 100, 2_500), Ok(true));
        assert_eq!(wallet_ok(20, 6, 100, 2_500), Ok(false));
        assert!(notional_ok(100, 0, 100, 0));
        assert!(!notional_ok(99, 0, 100, 0));
        assert!(notional_ok(100, 900, 100, 1_000));
        assert!(!notional_ok(100, 901, 100, 1_000));
    }

    /// "Switch only if all fit": the per-position caps on the bound of every open `q`, then the
    /// per-market caps on `ΣQ` at `X₁ = X₀`, each exactly at its boundary.
    #[test]
    fn carried_positions_fit_the_candidates_opening_caps() {
        let x0 = 150_000_000_000_000u64;
        // 1 % fee (10,000 ppm): the fee-scaled caps are 3 % / 7.5 % of X₀, so the impact cap
        // binds per position (2.470 % < 3 %) and the fee-scaled cap per market (7.5 % < 8.713 %).
        let c = CarriedCaps {
            x0,
            impact_pos_bps: 500,
            impact_mkt_bps: 2_000,
            phi_ppm: 10_000,
            fee_size_cap_x100: 300,
            fee_oi_cap_x100: 750,
        };
        let pos = max_q_position(x0, 500).unwrap();
        let oi = oi_cap(x0, 2_000, 10_000, 750).unwrap();
        assert_eq!(position_cap(x0, 500, 10_000, 300), Ok(pos));
        assert_eq!(oi, max_total_q(x0, 2_000).unwrap().min(x0 * 75 / 1_000));
        assert_eq!(carried_fit(pos, pos, &c), Ok(CarriedFit::Fits));
        assert_eq!(carried_fit(pos + 1, pos + 1, &c), Ok(CarriedFit::Position));
        assert_eq!(carried_fit(pos, oi, &c), Ok(CarriedFit::Fits));
        assert_eq!(carried_fit(pos, oi + 1, &c), Ok(CarriedFit::Market));
        // The position check comes first.
        assert_eq!(carried_fit(pos + 1, oi + 1, &c), Ok(CarriedFit::Position));
        // A 25 bps pool: the fee-scaled caps bind (0.75 % of X₀ per position, 1.875 % OI).
        let thin = CarriedCaps {
            phi_ppm: 2_500,
            ..c
        };
        let f = x0 * 75 / 10_000;
        assert_eq!(position_cap(x0, 500, 2_500, 300), Ok(f));
        assert_eq!(carried_fit(f, f, &thin), Ok(CarriedFit::Fits));
        assert_eq!(carried_fit(f + 1, f + 1, &thin), Ok(CarriedFit::Position));
        let foi = x0 * 1_875 / 100_000;
        assert_eq!(carried_fit(f, foi, &thin), Ok(CarriedFit::Fits));
        assert_eq!(carried_fit(f, foi + 1, &thin), Ok(CarriedFit::Market));
        // The fee-scaled caps off: the impact caps alone.
        let off = CarriedCaps {
            fee_size_cap_x100: 0,
            fee_oi_cap_x100: 0,
            ..thin
        };
        assert_eq!(position_cap(x0, 500, 2_500, 0), Ok(pos));
        assert_eq!(
            carried_fit(pos, max_total_q(x0, 2_000).unwrap(), &off),
            Ok(CarriedFit::Fits)
        );
        // Tier D's 3 % / 10 %.
        let d = CarriedCaps {
            impact_pos_bps: 300,
            impact_mkt_bps: 1_000,
            ..off
        };
        let pd = max_q_position(x0, 300).unwrap();
        assert_eq!(carried_fit(pd + 1, pd + 1, &d), Ok(CarriedFit::Position));
        assert_eq!(
            carried_fit(pd, max_total_q(x0, 1_000).unwrap() + 1, &d),
            Ok(CarriedFit::Market)
        );
        // No position: nothing to carry, always fits (even on an empty reserve).
        let empty = CarriedCaps { x0: 0, ..c };
        assert_eq!(carried_fit(0, 0, &empty), Ok(CarriedFit::Fits));
        assert_eq!(carried_fit(1, 1, &empty), Ok(CarriedFit::Position));
        assert!(CarriedFit::Fits.fits() && !CarriedFit::Market.fits());
    }

    /// The position bound is `min(max_q_hwm, ΣQ)`; a zero mark with positions open bounds nothing.
    #[test]
    fn the_carried_bound_is_the_mark_capped_at_the_sum() {
        assert_eq!(carried_q_bound(0, 0), 0);
        assert_eq!(carried_q_bound(0, 700), 700, "no mark: ΣQ");
        assert_eq!(carried_q_bound(500, 700), 500);
        assert_eq!(carried_q_bound(900, 700), 700, "the large position closed");
        assert_eq!(carried_q_bound(u64::MAX, u64::MAX), u64::MAX);
    }

    /// FIT-2 / FITS-1: the open-fee mark is the lowest fee an open paid since the market emptied
    /// (the first open sets it); the carried fit's fee-scaled caps take `min(mark, φ_cand,
    /// CARRIED_FEE_CAP_PPM)`, `min(φ_cand, CARRIED_FEE_CAP_PPM)` for a zero mark. At `X₀` =
    /// 150·10¹² a position margined on a 25 bps pool and carried onto a 1 % candidate keeps the
    /// 0.75 % size class (not the impact cap's 2.47 %), and so does one margined at 1 % (FITS-1).
    #[test]
    fn the_carried_fee_is_the_lower_of_the_open_fee_mark_and_the_candidates() {
        assert_eq!(open_fee_mark(0, 2_500), 2_500, "the first open sets it");
        assert_eq!(open_fee_mark(2_500, 10_000), 2_500);
        assert_eq!(open_fee_mark(10_000, 2_500), 2_500);
        assert_eq!(open_fee_mark(2_500, 0), 0);
        assert_eq!(carried_phi(2_500, 10_000), 2_500, "a higher-fee candidate");
        assert_eq!(carried_phi(10_000, 2_500), 2_500, "a lower-fee candidate");
        assert_eq!(
            carried_phi(1_000, 10_000),
            1_000,
            "a mark under the ceiling"
        );
        assert_eq!(
            carried_phi(10_000, 1_000),
            1_000,
            "a candidate under the ceiling"
        );
        // FITS-1: the ceiling binds whatever both fees are (a 1 % → 1 % switch).
        assert_eq!(
            carried_phi(10_000, 10_000),
            CARRIED_FEE_CAP_PPM,
            "1 % → 1 %"
        );
        assert_eq!(
            carried_phi(0, 10_000),
            CARRIED_FEE_CAP_PPM,
            "no mark: min(φ_cand, ceiling)"
        );
        assert_eq!(
            carried_phi(0, 1_000),
            1_000,
            "no mark: the candidate's under the ceiling"
        );
        assert_eq!(carried_phi(u32::MAX, u32::MAX), CARRIED_FEE_CAP_PPM);
        let x0 = 150_000_000_000_000u64;
        let c = CarriedCaps {
            x0,
            impact_pos_bps: 500,
            impact_mkt_bps: 2_000,
            phi_ppm: carried_phi(2_500, 10_000),
            fee_size_cap_x100: 300,
            fee_oi_cap_x100: 750,
        };
        let f = x0 * 75 / 10_000;
        let impact = max_q_position(x0, 500).unwrap();
        assert!(f < impact);
        assert_eq!(carried_fit(f, f, &c), Ok(CarriedFit::Fits));
        assert_eq!(carried_fit(f + 1, f + 1, &c), Ok(CarriedFit::Position));
        let at_cand = CarriedCaps {
            phi_ppm: 10_000,
            ..c
        };
        assert_eq!(carried_fit(impact, impact, &at_cand), Ok(CarriedFit::Fits));
        assert_eq!(carried_fit(impact, impact, &c), Ok(CarriedFit::Position));
        // FITS-1: margined at 1 % and carried onto a 1 % candidate, the same 0.75 % class.
        let equal = CarriedCaps {
            phi_ppm: carried_phi(10_000, 10_000),
            ..c
        };
        assert_eq!(carried_fit(f, f, &equal), Ok(CarriedFit::Fits));
        assert_eq!(carried_fit(f + 1, f + 1, &equal), Ok(CarriedFit::Position));
        assert_eq!(
            carried_fit(impact, impact, &equal),
            Ok(CarriedFit::Position)
        );
        // The OI cap follows too: 1.875 % of X₀ at 25 bps, not 7.5 % at 1 %.
        let foi = x0 * 1_875 / 100_000;
        assert_eq!(carried_fit(f, foi, &c), Ok(CarriedFit::Fits));
        assert_eq!(carried_fit(f, foi + 1, &c), Ok(CarriedFit::Market));
    }

    #[test]
    fn conservative_reserve() {
        let wad = crate::num::WAD;
        // L = 10¹², P = 1 WAD → X = 10¹².
        assert_eq!(
            conservative_x0(u64::MAX, 1_000_000_000_000, wad, None),
            Ok(1_000_000_000_000)
        );
        assert_eq!(conservative_x0(5, 1_000_000_000_000, wad, None), Ok(5));
        // Locked depth 4·10¹¹ over λ_min 50 % → 8·10¹¹.
        assert_eq!(
            conservative_x0(
                u64::MAX,
                1_000_000_000_000,
                wad,
                Some((400_000_000_000, 5_000))
            ),
            Ok(800_000_000_000)
        );
        assert_eq!(conservative_x0(1, 1, 0, None), Err(MathError::DivByZero));
        assert_eq!(
            conservative_x0(1, 1, wad, Some((1, 0))),
            Err(MathError::DivByZero)
        );
        // isqrt⁺ in both denominators (ECON §2.1): P = WAD + 1 → sp = 10⁹ + 1.
        let sp = 1_000_000_001u128;
        assert_eq!(
            conservative_x0(u64::MAX, 1_000_000_000_000, wad + 1, None),
            Ok(u64::try_from(1_000_000_000_000u128 * 1_000_000_000 / sp).unwrap())
        );
        assert_eq!(
            conservative_x0(u64::MAX, u128::MAX, wad + 1, Some((400_000_000_000, 5_000))),
            Ok(u64::try_from(400_000_000_000u128 * 10_000_000_000_000 / (5_000 * sp)).unwrap())
        );
        // Bounds beyond u128 do not bind.
        assert_eq!(
            conservative_x0(7, u128::MAX, 1, Some((u128::MAX, 1))),
            Ok(7)
        );
    }
}
