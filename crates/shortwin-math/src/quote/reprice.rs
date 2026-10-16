//! Reprojection of a constant-product state to a price (ECON §2.1 `reprice`, §2.3, §5.3;
//! ARCH §2.2, §4.3 EMA leg, §4.4).
//!
//! Square roots are `u128` only (ARCH §2.2): `L = isqrt(X·Y)` needs `X·Y < 2¹²⁸`, which
//! ECON §2.2's bounds guarantee (`X ≤ 2⁶⁴`, `Y ≤ 6·10¹⁷`), and `reprice` takes the
//! ceiling root of the price.

use super::CurveState;
use crate::error::MathError;
use crate::num::{Rounding, SQRT_WAD, mul_div, sqrt_u128};

/// Liquidity `L = isqrt(X·Y)` of a constant-product state (ECON §2.1): the `L` an
/// observation records. The curve's is `isqrt(vT·vQ)`. `u128` only (ARCH §2.2).
///
/// Errors: `Overflow` if `X·Y ≥ 2¹²⁸`, outside ECON §2.2's bounds (`X ≤ 2⁶⁴`, `Y` at most
/// the SOL supply).
pub fn liquidity(x: u128, y: u128) -> Result<u128, MathError> {
    let k = x.checked_mul(y).ok_or(MathError::Overflow)?;
    Ok(sqrt_u128(k, Rounding::Floor))
}

/// `reprice(L, P)`: the constant-product state with liquidity `L` at price `P` (WAD),
/// ECON §2.1 (R3B-7 c): `sp = isqrt⁺(P)`, `X = ⌊L·10⁹/sp⌋`, `Y = ⌈L·sp/10⁹⌉`. Rounding
/// `X` down and `Y` up with a ceiling root gives `Y/X ≥ P`, so every quote on the
/// reprojected state is an upper bound (ECON §2.3). Returns `(X, Y)` = (token, SOL);
/// `sim/shortwin_sim.py` `reprice` exactly.
///
/// `None` iff `L = 0`, `P = 0`, `X = 0` (`L·10⁹ < sp`, as the sim), or `X` or `Y` does not
/// fit `u128`. Both products take `mul_div`'s `u128` fast path within ECON §2.2's bounds
/// (`L·10⁹ ≤ 3.3·10²⁷`, `L·sp ≤ 2.5·10³⁶`).
pub fn reprice(l: u128, p: u128) -> Option<(u128, u128)> {
    if l == 0 || p == 0 {
        return None;
    }
    let sp = sqrt_u128(p, Rounding::Ceil);
    let x = mul_div(l, SQRT_WAD, sp, Rounding::Floor).ok()?;
    if x == 0 {
        return None;
    }
    let y = mul_div(l, sp, SQRT_WAD, Rounding::Ceil).ok()?;
    Some((x, y))
}

/// The token side of [`reprice`] alone, `X = ⌊L·10⁹/isqrt⁺(P)⌋` (ECON §2.1), for bounds
/// that need only `X` (the conservative cap reserve `⌊L_slow·10⁹/isqrt⁺(P_ema)⌋`,
/// ECON §6.1, ARCH §4.6). Unlike [`reprice`] it returns 0 when `L·10⁹ < isqrt⁺(P)`.
///
/// Errors: `DivByZero` if `P = 0`; `Overflow` if `X` exceeds `u128`.
pub fn reprice_token(l: u128, p: u128) -> Result<u128, MathError> {
    mul_div(l, SQRT_WAD, sqrt_u128(p, Rounding::Ceil), Rounding::Floor)
}

/// The constant offsets `vT − rT` and `vQ − rQ` of a bonding curve (ECON §1.1: 279.9 M
/// tokens and 30 SOL on standard SOL curves).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CurveOffsets {
    /// `vT − rT`.
    pub token: u128,
    /// `vQ − rQ`.
    pub quote: u128,
}

impl CurveOffsets {
    /// The offsets of a live curve state; `None` if a real reserve exceeds its virtual one.
    pub fn of(s: &CurveState) -> Option<Self> {
        Some(Self {
            token: s.vt.checked_sub(s.rt)?,
            quote: s.vq.checked_sub(s.rq)?,
        })
    }
}

/// `curve_reprice(L, P, offset)`: the curve at price `P` with the same `√(vT·vQ) = L` and
/// the same offsets (ECON §2.3; ARCH §4.3 EMA leg, §4.4): `(vT, vQ) = reprice(L, P)`,
/// `rT = max(vT − offset.token, 0)`, `rQ = max(vQ − offset.quote, 0)`. A state whose
/// `rT` is 0 is past completion; [`curve_straddle`](super::curve_straddle) then prices
/// all of `q` on the migrated pool. The result is never `complete` (a hypothetical state).
///
/// `None` iff [`reprice`] is `None`.
pub fn curve_reprice(l: u128, p: u128, offset: CurveOffsets) -> Option<CurveState> {
    let (vt, vq) = reprice(l, p)?;
    Some(CurveState {
        vt,
        vq,
        rt: vt.saturating_sub(offset.token),
        rq: vq.saturating_sub(offset.quote),
        complete: false,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::num::WAD;

    #[test]
    fn reprice_round_trips_the_spot_state() {
        let (x, y) = (1_073_000_000_000_000u128, 30_000_000_000u128);
        let l = liquidity(x, y).unwrap();
        let p = y * WAD / x;
        let (x2, y2) = reprice(l, p).unwrap();
        // Within the rounding of isqrt on both L and P.
        assert!(x2.abs_diff(x) * 1_000_000 < x && y2.abs_diff(y) * 1_000_000 < y);
        // R3B-7 c: Y/X ≥ P, so the reprojected price never understates P.
        assert!(y2 * WAD / x2 >= p);
        assert_eq!(reprice(0, p), None);
        assert_eq!(reprice(l, 0), None);
        assert_eq!(reprice(u128::MAX, 1), None);
        // P = 1 (10⁻¹⁸ lamports per raw unit): sp = 1, X = 10⁹, Y = ⌈1/10⁹⌉ = 1.
        assert_eq!(reprice(1, 1), Some((1_000_000_000, 1)));
        // sp = isqrt⁺(2) = 2: X = ⌊10⁹/2⌋, Y = ⌈2/10⁹⌉.
        assert_eq!(reprice(1, 2), Some((500_000_000, 1)));
        // L·10⁹ < sp: X = 0, undefined (the sim returns None).
        assert_eq!(reprice(1, 10u128.pow(37)), None);
        assert_eq!(reprice_token(1, 10u128.pow(37)), Ok(0));
        assert_eq!(reprice_token(l, p), Ok(x2));
        assert_eq!(reprice_token(0, p), Ok(0));
        assert_eq!(reprice_token(1, 0), Err(MathError::DivByZero));
        assert_eq!(reprice_token(u128::MAX, 1), Err(MathError::Overflow));
        assert_eq!(liquidity(0, 5), Ok(0));
        let m = u128::from(u64::MAX);
        assert_eq!(liquidity(m, m), Ok(m));
        assert_eq!(liquidity(u128::MAX, 2), Err(MathError::Overflow));
    }

    #[test]
    fn curve_offsets_carry_over() {
        let live = CurveState {
            vt: 824_130_602_106_172,
            vq: 39_059_343_535,
            rt: 544_230_602_106_172,
            rq: 9_059_343_535,
            complete: false,
        };
        let off = CurveOffsets::of(&live).unwrap();
        assert_eq!(off.token, 279_900_000_000_000);
        assert_eq!(off.quote, 30_000_000_000);
        let l = liquidity(live.vt, live.vq).unwrap();
        // Far above the completion price: rT saturates at 0.
        let s = curve_reprice(l, 10u128.pow(18), off).unwrap();
        assert_eq!(s.rt, 0);
        assert!(!s.complete);
        assert_eq!(s.rq, s.vq - off.quote);
    }
}
