//! SPEC §4 size caps at open (ARCH §2.1 `engine/caps.rs`, §3.4 step 6, §4.6; ECON §6.1): a thin
//! wrapper that applies `shortwin_math::caps` in ARCH's order and maps each refusal to its
//! error. Caps are checked in `open_short` only and never force a close.
//!
//! **The curve's reserves (M5-SEC-3; a deviation from ARCH §4.6, flagged).** ARCH §4.6 sizes a
//! curve open on the live `X₀ = vT` and guards Tier D with the live `rT`, which the opener moves in
//! the same transaction: `[sell_v2(δ), open_short(q), buy_v2(δ)]` raises both by `δ` for the open
//! only (the open's 10 % spot–EMA guard lets `δ` reach ≈ 5 % of `vT`, the whole `rT` near
//! completion), so the graduation guard `ΣQ + q ≤ γ·(rT + q)` stopped nothing and every position
//! could be made a straddle at once. [`curve_reserves`] reads the curve as it stands at the EMA
//! price instead whenever that is the smaller one: `x_ema = ⌊L·10⁹/isqrt⁺(P_ema)⌋` with the
//! curve's own `L = isqrt(vT·vQ)` (which a curve trade does not move, ECON §2.3's
//! `curve_reprice`), `X₀ = min(vT, x_ema)` and `rT_guard = min(rT, max(0, x_ema − (vT − rT)))` —
//! the PumpSwap / Raydium rule (`X₀ = min(X_now, reprice(L_slow, P_ema).X)`, ARCH §4.6) applied
//! to the curve. While `P_ema ≤ P_spot` the EMA curve holds at least the live reserves, so the
//! live ones are used as they are (no rounding of the reprojection can then tighten a cap: the
//! ECON §11.2 opens at their exact caps are unchanged).

use anchor_lang::prelude::*;
use shortwin_math::caps;
use shortwin_math::quote::{liquidity, reprice_token};

use crate::errors::{CheckedExt, MathResultExt, ShortwinError};
use crate::state::{Params, TierParams};

/// The live curve an open on it is sized against (this instruction's ReadSet, before the sale).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveCap {
    /// `vT`, `vQ`, `rT` of the live curve.
    pub vt: u64,
    pub vq: u64,
    pub rt: u64,
    /// The live pre-trade spot `⌊vQ·WAD/vT⌋`.
    pub p_spot: u128,
}

/// The curve's cap reserves (module docs, M5-SEC-3): `(X₀, rT_guard)`, the live `(vT, rT)`
/// while `P_ema ≤ P_spot`, else `(min(vT, x_ema), min(rT, max(0, x_ema − (vT − rT))))` with
/// `x_ema = ⌊isqrt(vT·vQ)·10⁹/isqrt⁺(P_ema)⌋`.
pub fn curve_reserves(c: &CurveCap, p_ema: u128) -> Result<(u64, u64)> {
    if p_ema <= c.p_spot {
        return Ok((c.vt, c.rt));
    }
    let l = liquidity(u128::from(c.vt), u128::from(c.vq)).math()?;
    let x_ema = reprice_token(l, p_ema).math()?;
    let vt = u128::from(c.vt);
    let offset = vt.checked_sub(u128::from(c.rt)).or_math()?;
    let x0 = vt.min(x_ema);
    let rt = u128::from(c.rt).min(x_ema.saturating_sub(offset));
    Ok((
        u64::try_from(x0).map_err(|_| error!(ShortwinError::MathOverflow))?,
        u64::try_from(rt).map_err(|_| error!(ShortwinError::MathOverflow))?,
    ))
}

/// The inputs of the open caps.
#[derive(Clone, Copy, Debug)]
pub struct CapInputs {
    /// The position's `q`.
    pub q: u64,
    /// `X_now`, the live pre-trade token reserve.
    pub x_now: u64,
    /// `L_slow` and `P_ema` after this instruction's roll.
    pub l_slow: u128,
    pub p_ema: u128,
    /// Raydium and — since M6R-1 (owner decision of 2026-09-30) — PumpSwap: `(D, λ_min)`, the
    /// locked depth over `VenueKind::lock_min_bps` (V2-8): Raydium's `pool_lock_min_bps`,
    /// PumpSwap's burned-LP depth over `PUMPSWAP_LOCK_MIN_BPS` = 100 % (OWNER-M6D-1, ECON #106);
    /// `None` elsewhere. (The name predates PumpSwap's lock.)
    pub raydium_lock: Option<(u128, u32)>,
    /// The live one-way venue fee φ, ppm.
    pub phi_ppm: u32,
    /// `T` and `ΣB_Q` before the open.
    pub tokens_free: u64,
    pub borrowed_q: u64,
    /// The borrower's open quantity in this market (`WalletExposure.q_open`).
    pub wallet_q: u64,
    /// On the curve: the live curve, from which [`curve_reserves`] takes `X₀` and the `rT` of
    /// Tier D's guard `ΣQ + q ≤ γ·(rT + q)` (`curve_gamma_ok`) at `P_ema` (M5-SEC-3; ARCH §4.6
    /// said the live `vT` and `rT`); `None` off the curve.
    pub curve: Option<CurveCap>,
}

/// What the caps computed (reported and reused by the open).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapOutputs {
    /// `X₀`, the conservative pre-trade token reserve (`caps::conservative_x0`).
    pub x0: u64,
    /// `X₁ = X₀ + q`.
    pub x1: u64,
    /// The binding per-market cap `OI_cap = min(SPEC impact cap, fee-scaled cap)` on `X₁`.
    pub oi_cap: u64,
}

/// ARCH §3.4 step 6 (ECON §6.1), in order: the SPEC per-position impact cap and the
/// fee-scaled per-position cap (`CapPositionImpact`); the SPEC per-market impact cap and the
/// fee-scaled per-market cap on `ΣB_Q + q` (`CapMarketOi`); utilisation after the open ≤
/// `util_cap_bps` (`CapUtilization`); Tier D's `ΣQ + q ≤ γ·(rT + q)` (`CapCurveGamma`); the
/// wallet cap `exposure + q ≤ wallet_cap_bps·OI_cap/10⁴` (`CapWallet`); `q ≤ T`
/// (`InsufficientInventory`).
pub fn check_open(inp: &CapInputs, tier: &TierParams, params: &Params) -> Result<CapOutputs> {
    let (x0, curve_rt) = match inp.curve.as_ref() {
        Some(c) => {
            let (x0, rt) = curve_reserves(c, inp.p_ema)?;
            (x0, Some(rt))
        }
        None => (
            caps::conservative_x0(inp.x_now, inp.l_slow, inp.p_ema, inp.raydium_lock).math()?,
            None,
        ),
    };
    let q = inp.q;
    require_gte!(
        caps::max_q_position(x0, tier.impact_pos_bps).math()?,
        q,
        ShortwinError::CapPositionImpact
    );
    if let Some(cap) = caps::fee_size_cap(x0, inp.phi_ppm, params.fee_size_cap_x100).math()? {
        require_gte!(cap, q, ShortwinError::CapPositionImpact);
    }
    let x1 = x0.checked_add(q).or_math()?;
    let oi_cap =
        caps::oi_cap(x1, tier.impact_mkt_bps, inp.phi_ppm, params.fee_oi_cap_x100).math()?;
    let total = inp.borrowed_q.checked_add(q).or_math()?;
    require_gte!(oi_cap, total, ShortwinError::CapMarketOi);
    require!(
        caps::util_ok(inp.tokens_free, inp.borrowed_q, q, params.util_cap_bps).math()?,
        ShortwinError::CapUtilization
    );
    if let Some(rt) = curve_rt {
        require!(
            caps::curve_gamma_ok(inp.borrowed_q, q, rt, params.curve_oi_gamma_bps).math()?,
            ShortwinError::CapCurveGamma
        );
    }
    require!(
        caps::wallet_ok(inp.wallet_q, q, oi_cap, params.wallet_cap_bps).math()?,
        ShortwinError::CapWallet
    );
    require_gte!(inp.tokens_free, q, ShortwinError::InsufficientInventory);
    Ok(CapOutputs { x0, x1, oi_cap })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::constants::PUMPSWAP_LOCK_MIN_BPS;
    use crate::state::VenueKind;
    use shortwin_math::num::WAD;

    fn cap(vt: u64, vq: u64, rt: u64) -> CurveCap {
        CurveCap {
            vt,
            vq,
            rt,
            p_spot: u128::from(vq) * WAD / u128::from(vt),
        }
    }

    /// M5-SEC-3: at or below the spot the live reserves are used exactly; above it the curve at
    /// `P_ema` bounds both, so a sale of `δ` in the same transaction (which lowers the spot and
    /// raises `vT`, `rT` by `δ`, leaving `vT·vQ`) cannot lift them above the pre-sale curve.
    #[test]
    fn a_same_transaction_sale_does_not_lift_the_curve_caps() {
        // ECON's D@90 % curve near completion: vT 291.9e12, rT 12e12, vQ ≈ 110.3 SOL.
        let (vt, rt) = (291_900_000_000_000u64, 12_000_000_000_000u64);
        let k = 1_073_000_000_000_000u128 * 30_000_000_000;
        let vq = u64::try_from(k / u128::from(vt)).unwrap();
        let before = cap(vt, vq, rt);
        let p_ema = before.p_spot;
        assert_eq!(curve_reserves(&before, p_ema).unwrap(), (vt, rt));
        assert_eq!(curve_reserves(&before, p_ema - 1).unwrap(), (vt, rt));
        // The attacker sells δ = 12e12 (the whole rT) just before the open.
        let d = 12_000_000_000_000u64;
        let vq_after = u64::try_from(k / u128::from(vt + d)).unwrap();
        let after = cap(vt + d, vq_after, rt + d);
        assert!(after.p_spot < p_ema);
        let (x0, rt_guard) = curve_reserves(&after, p_ema).unwrap();
        // Within the reprojection's rounding of the pre-sale curve (`isqrt⁺(P)` is exact to
        // ≈ 1/√P, 5·10⁻⁸ here), never above it.
        assert!(x0 <= vt && vt - x0 < vt / 10_000_000, "{x0} vs {vt}");
        assert!(
            rt_guard <= rt && rt - rt_guard < vt / 10_000_000,
            "{rt_guard} vs {rt}"
        );
        // The live values the guard read before the fix.
        assert_eq!((after.vt, after.rt), (vt + d, rt + d));
        // An EMA price above the completion price leaves no real tokens to count.
        let (_, rt_hi) = curve_reserves(&before, p_ema * 2).unwrap();
        assert_eq!(rt_hi, 0);
    }

    /// OWNER-M6D-1 (ARCH row 57, ECON #106): an open's `(D, λ_min)` on PumpSwap is the burned LP
    /// at `λ_min` = 100 % whatever the governed `pool_lock_min_bps` is (`VenueKind::lock_min_bps`,
    /// as `open_short` passes it), so `X₀`'s `D` term is the burned LP's own reserve at `P_ema`;
    /// on Raydium it is `D/λ_min`.
    #[test]
    fn pumpswap_sizes_on_the_burned_lp_raydium_on_lambda_min() {
        let mut params = Params::defaults();
        let (d, p) = (4_204_936_085_629u128, 49_000_000_000_000u128);
        for lam in [2_000u32, 5_000, 10_000] {
            params.pool_lock_min_bps = lam;
            let x0 = |k: VenueKind| {
                let lock = k.lock_min_bps(params.pool_lock_min_bps).map(|l| (d, l));
                caps::conservative_x0(u64::MAX, u128::MAX, p, lock).map(u128::from)
            };
            assert_eq!(x0(VenueKind::PumpSwap), reprice_token(d, p));
            assert_eq!(
                x0(VenueKind::PumpSwap),
                caps::conservative_x0(u64::MAX, u128::MAX, p, Some((d, PUMPSWAP_LOCK_MIN_BPS)))
                    .map(u128::from)
            );
            for k in [VenueKind::RaydiumV4, VenueKind::RaydiumCpmm] {
                assert_eq!(
                    x0(k),
                    caps::conservative_x0(u64::MAX, u128::MAX, p, Some((d, lam))).map(u128::from)
                );
            }
        }
    }
}
