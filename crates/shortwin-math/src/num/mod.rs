//! Owner: scaffold. Fixed-point and wide-integer primitives (ARCHITECTURE §2.2; ECONOMICS §2).
//!
//! Every `mul_div` uses a 256-bit intermediate (512-bit for [`mul_div_u256`]) and takes
//! an explicit [`Rounding`]; nothing here picks a direction on the caller's behalf.

mod u256;

pub use u256::U256;

use crate::error::MathError;

/// 10¹⁸: scale of prices (lamports per raw token unit), indices and accumulators (ECON §2).
pub const WAD: u128 = 1_000_000_000_000_000_000;
/// 10⁹ = √WAD, used by `reprice` (ECON §2).
pub const SQRT_WAD: u128 = 1_000_000_000;
/// Basis-point denominator: 1 bp = 0.01 %.
pub const BPS: u64 = 10_000;
/// Parts-per-million denominator (Raydium CPMM fee rates, ECON §3.1).
pub const PPM: u64 = 1_000_000;

/// Rounding direction of a division.
///
/// ECON §2 fixes the direction per quantity, always in the pool's and lenders' favour:
/// quotes `B̂`, fees inside quotes, rent index increments, rent due, penalty, expiry fee,
/// open fee, margin, FV, locks and utilisation round **up** ([`Rounding::Ceil`]); shares
/// minted, tokens paid out, SOL credits, claims and keeper/protocol cuts round **down**
/// ([`Rounding::Floor`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rounding {
    /// Toward −∞ (for unsigned values: down).
    Floor,
    /// Toward +∞ (for unsigned values: up).
    Ceil,
}

impl Rounding {
    /// The opposite direction.
    pub const fn flip(self) -> Self {
        match self {
            Self::Floor => Self::Ceil,
            Self::Ceil => Self::Floor,
        }
    }
}

/// Adds one to a floored quotient when rounding up an inexact division.
fn bump(q: u128, inexact: bool, r: Rounding) -> Result<u128, MathError> {
    match (r, inexact) {
        (Rounding::Ceil, true) => q.checked_add(1).ok_or(MathError::Overflow),
        _ => Ok(q),
    }
}

/// `a / d` rounded as requested (ECON §2).
pub fn div(a: u128, d: u128, r: Rounding) -> Result<u128, MathError> {
    let q = a.checked_div(d).ok_or(MathError::DivByZero)?;
    let rem = a.checked_rem(d).ok_or(MathError::DivByZero)?;
    bump(q, rem != 0, r)
}

/// `⌊a / d⌋`.
pub fn div_floor(a: u128, d: u128) -> Result<u128, MathError> {
    div(a, d, Rounding::Floor)
}

/// `⌈a / d⌉`.
pub fn div_ceil(a: u128, d: u128) -> Result<u128, MathError> {
    div(a, d, Rounding::Ceil)
}

/// `a / d` on `u64`, rounded as requested.
pub fn div_u64(a: u64, d: u64, r: Rounding) -> Result<u64, MathError> {
    to_u64(div(u128::from(a), u128::from(d), r)?)
}

/// Signed `a / d`: `Floor` rounds toward −∞ and `Ceil` toward +∞, matching Python's
/// `//` and `cdiv` in the normative models (`pool_model.py`: the signed accumulators).
pub fn div_i128(a: i128, d: u128, r: Rounding) -> Result<i128, MathError> {
    mul_div_i128(a, 1, d, r)
}

/// `a·b/d` with a 256-bit intermediate, rounded as requested (ARCH §2.2, ECON §2).
///
/// Errors: `DivByZero` if `d == 0`; `Overflow` if the result exceeds `u128`.
pub fn mul_div(a: u128, b: u128, d: u128, r: Rounding) -> Result<u128, MathError> {
    if d == 0 {
        return Err(MathError::DivByZero);
    }
    if let Some(p) = a.checked_mul(b) {
        return div(p, d, r);
    }
    let (q, rem) = U256::from_product(a, b)
        .div_rem_u128(d)
        .ok_or(MathError::DivByZero)?;
    bump(q.to_u128().ok_or(MathError::Overflow)?, rem != 0, r)
}

/// `a·b/d` on `u64` inputs and output (the product always fits `u128`). When `a·b` fits
/// `u64` the division is native u64 (≈ 3× cheaper on SBF, T-CU-MATH; ARCH §9 M0b), with
/// results and errors identical to the u128 path.
///
/// Errors: `DivByZero` if `d == 0`; `Overflow` if the result exceeds `u64`.
pub fn mul_div_u64(a: u64, b: u64, d: u64, r: Rounding) -> Result<u64, MathError> {
    if let Some(p) = a.checked_mul(b) {
        let q = p.checked_div(d).ok_or(MathError::DivByZero)?;
        let inexact = p.checked_rem(d).ok_or(MathError::DivByZero)? != 0;
        return match (r, inexact) {
            (Rounding::Ceil, true) => q.checked_add(1).ok_or(MathError::Overflow),
            _ => Ok(q),
        };
    }
    to_u64(mul_div(u128::from(a), u128::from(b), u128::from(d), r)?)
}

/// `a·b/d` on [`U256`] with a 512-bit intermediate, rounded as requested.
///
/// Errors: `DivByZero` if `d` is 0; `Overflow` if the result exceeds 256 bits.
pub fn mul_div_u256(a: U256, b: U256, d: U256, r: Rounding) -> Result<U256, MathError> {
    if d.is_zero() {
        return Err(MathError::DivByZero);
    }
    let (q, inexact) = U256::mul_div_floor(a, b, d).ok_or(MathError::Overflow)?;
    match (r, inexact) {
        (Rounding::Ceil, true) => q.checked_add(U256::ONE).ok_or(MathError::Overflow),
        _ => Ok(q),
    }
}

/// Signed `a·b/d` with a 256-bit intermediate: `Floor` rounds toward −∞, `Ceil`
/// toward +∞ (ECON §9: the signed SOL accumulator `acc` and pending balances).
///
/// Errors: `DivByZero` if `d == 0`; `Overflow` if the result is outside `i128`.
pub fn mul_div_i128(a: i128, b: u128, d: u128, r: Rounding) -> Result<i128, MathError> {
    if a >= 0 {
        return to_i128(mul_div(a.unsigned_abs(), b, d, r)?);
    }
    // Rounding a negative value toward −∞ rounds its magnitude up, and vice versa.
    let mag = mul_div(a.unsigned_abs(), b, d, r.flip())?;
    0i128.checked_sub_unsigned(mag).ok_or(MathError::Overflow)
}

/// `a·b/WAD`: product of two WAD-scaled values, or a raw amount times a WAD price.
pub fn mul_wad(a: u128, b: u128, r: Rounding) -> Result<u128, MathError> {
    mul_div(a, b, WAD, r)
}

/// `a·WAD/b`: WAD-scaled quotient (e.g. a price `Y/X`).
pub fn div_wad(a: u128, b: u128, r: Rounding) -> Result<u128, MathError> {
    mul_div(a, WAD, b, r)
}

/// `x·bps/10⁴` (fee, penalty and haircut amounts; ECON §3.1 `⌈n·φ/10⁴⌉`).
pub fn mul_bps(x: u128, bps: u128, r: Rounding) -> Result<u128, MathError> {
    mul_div(x, bps, u128::from(BPS), r)
}

/// `x·bps/10⁴` on `u64`.
pub fn mul_bps_u64(x: u64, bps: u64, r: Rounding) -> Result<u64, MathError> {
    mul_div_u64(x, bps, BPS, r)
}

/// `x·ppm/10⁶` (Raydium CPMM rates; ECON §3.1).
pub fn mul_ppm(x: u128, ppm: u128, r: Rounding) -> Result<u128, MathError> {
    mul_div(x, ppm, u128::from(PPM), r)
}

/// `x·ppm/10⁶` on `u64`.
pub fn mul_ppm_u64(x: u64, ppm: u64, r: Rounding) -> Result<u64, MathError> {
    mul_div_u64(x, ppm, PPM, r)
}

/// `√x` on `u128`, rounded as requested (`L = √(X·Y)`, `reprice`; ECON §2).
pub fn sqrt_u128(x: u128, r: Rounding) -> u128 {
    let s = x.isqrt();
    match r {
        // s ≤ 2^64 − 1, so s·s fits and s + 1 cannot overflow.
        Rounding::Ceil if s.checked_mul(s) != Some(x) => s.saturating_add(1),
        _ => s,
    }
}

/// `√x` on [`U256`], rounded as requested. Errors with `Overflow` only when rounding
/// up gives 2^128 (x > (2^128 − 1)²).
pub fn sqrt_u256(x: U256, r: Rounding) -> Result<u128, MathError> {
    let s = x.isqrt();
    match r {
        Rounding::Ceil if U256::from_product(s, s) != x => {
            s.checked_add(1).ok_or(MathError::Overflow)
        }
        _ => Ok(s),
    }
}

/// `a·b ≤ c·d`, exactly on 256-bit products: the cross-multiplied form of a cap or gate
/// such as `x ≤ bps·y/10⁴`, which then needs no rounding (ECON §6.1; ARCH §2.6, §4.5).
pub fn mul_le(a: u128, b: u128, c: u128, d: u128) -> bool {
    U256::from_product(a, b) <= U256::from_product(c, d)
}

/// `x` as `u64`, saturating at `u64::MAX`: for caps and bounds, which cannot bind
/// beyond the largest representable amount.
pub fn saturating_u64(x: u128) -> u64 {
    u64::try_from(x).unwrap_or(u64::MAX)
}

/// Maps an `Overflow` result to `u128::MAX`, keeping every other error: for upper
/// clamps and bounds, which cannot bind beyond `u128` (e.g. `P_ema·(1 + C)` in the EMA
/// clamp, `L·10⁹/√P` in the conservative cap reserve).
pub fn overflow_to_max(r: Result<u128, MathError>) -> Result<u128, MathError> {
    match r {
        Err(MathError::Overflow) => Ok(u128::MAX),
        r => r,
    }
}

/// Checked conversion between integer types; `Overflow` if `x` is not representable
/// in `U` (too large, or negative for an unsigned `U`).
pub fn cast<T, U: TryFrom<T>>(x: T) -> Result<U, MathError> {
    U::try_from(x).map_err(|_| MathError::Overflow)
}

/// `u128 → u64`, `Overflow` if it does not fit.
pub fn to_u64(x: u128) -> Result<u64, MathError> {
    cast(x)
}

/// `U256 → u128`, `Overflow` if it does not fit.
pub fn to_u128(x: U256) -> Result<u128, MathError> {
    x.to_u128().ok_or(MathError::Overflow)
}

/// `u128 → i128`, `Overflow` above `i128::MAX`.
pub fn to_i128(x: u128) -> Result<i128, MathError> {
    cast(x)
}

/// `i128 → u128`, `Underflow` if negative.
pub fn i128_to_u128(x: i128) -> Result<u128, MathError> {
    u128::try_from(x).map_err(|_| MathError::Underflow)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use Rounding::{Ceil, Floor};

    #[test]
    fn rounding_basics() {
        assert_eq!(div(7, 2, Floor), Ok(3));
        assert_eq!(div(7, 2, Ceil), Ok(4));
        assert_eq!(div(8, 2, Ceil), Ok(4));
        assert_eq!(div(0, 5, Ceil), Ok(0));
        assert_eq!(div(1, 0, Floor), Err(MathError::DivByZero));
        assert_eq!(div_u64(u64::MAX, 1, Ceil), Ok(u64::MAX));
        assert_eq!(div_i128(-7, 2, Floor), Ok(-4));
        assert_eq!(div_i128(-7, 2, Ceil), Ok(-3));
        assert_eq!(div_i128(7, 2, Floor), Ok(3));
        assert_eq!(div_i128(i128::MIN, 1, Floor), Ok(i128::MIN));
    }

    #[test]
    fn mul_div_edges() {
        assert_eq!(
            mul_div(u128::MAX, u128::MAX, u128::MAX, Floor),
            Ok(u128::MAX)
        );
        assert_eq!(
            mul_div(u128::MAX, u128::MAX, u128::MAX, Ceil),
            Ok(u128::MAX)
        );
        assert_eq!(mul_div(u128::MAX, 2, 1, Floor), Err(MathError::Overflow));
        assert_eq!(mul_div(u128::MAX, 3, 2, Floor), Err(MathError::Overflow));
        assert_eq!(mul_div(1, 1, 0, Floor), Err(MathError::DivByZero));
        assert_eq!(
            mul_div(u128::MAX, u128::MAX, u128::MAX - 1, Floor),
            Err(MathError::Overflow)
        );
        assert_eq!(
            mul_div_u64(u64::MAX, u64::MAX, u64::MAX, Ceil),
            Ok(u64::MAX)
        );
        assert_eq!(mul_div_u64(u64::MAX, 2, 1, Floor), Err(MathError::Overflow));
        // i128::MIN magnitude is 2^127: representable only as a negative result.
        assert_eq!(mul_div_i128(i128::MIN, 1, 1, Ceil), Ok(i128::MIN));
        assert_eq!(mul_div_i128(i128::MIN, 1, 1, Floor), Ok(i128::MIN));
        assert_eq!(mul_div_i128(i128::MIN, 2, 2, Floor), Ok(i128::MIN));
        assert_eq!(
            mul_div_i128(i128::MAX, 2, 1, Floor),
            Err(MathError::Overflow)
        );
        assert_eq!(mul_div_i128(-1, 1, 3, Floor), Ok(-1));
        assert_eq!(mul_div_i128(-1, 1, 3, Ceil), Ok(0));
        assert_eq!(
            mul_div_u256(U256::MAX, U256::MAX, U256::MAX, Ceil),
            Ok(U256::MAX)
        );
        assert_eq!(
            mul_div_u256(U256::MAX, U256::from(2u8), U256::ONE, Floor),
            Err(MathError::Overflow)
        );
        assert_eq!(
            mul_div_u256(U256::ONE, U256::ONE, U256::ZERO, Floor),
            Err(MathError::DivByZero)
        );
    }

    #[test]
    fn fixed_point_helpers() {
        assert_eq!(mul_wad(3 * WAD, WAD / 2, Floor), Ok(3 * WAD / 2));
        assert_eq!(div_wad(1, 3, Floor), Ok(333_333_333_333_333_333));
        assert_eq!(div_wad(1, 3, Ceil), Ok(333_333_333_333_333_334));
        // ECON §3.1: fees are ⌈n·φ/10⁴⌉.
        assert_eq!(mul_bps(1_001, 95, Ceil), Ok(10));
        assert_eq!(mul_bps(1_001, 95, Floor), Ok(9));
        assert_eq!(mul_bps_u64(u64::MAX, BPS, Floor), Ok(u64::MAX));
        assert_eq!(mul_ppm(1_000_001, 2_500, Ceil), Ok(2_501));
        assert_eq!(mul_ppm_u64(1_000_000, 2_500, Floor), Ok(2_500));
    }

    #[test]
    fn comparisons_and_saturation() {
        assert!(mul_le(u128::MAX, u128::MAX, u128::MAX, u128::MAX));
        assert!(!mul_le(u128::MAX, u128::MAX, u128::MAX, u128::MAX - 1));
        assert!(mul_le(3, 10_000, 1, 30_000));
        assert!(!mul_le(3, 10_001, 1, 30_000));
        assert_eq!(saturating_u64(u128::from(u64::MAX) + 1), u64::MAX);
        assert_eq!(saturating_u64(7), 7);
        assert_eq!(
            overflow_to_max(mul_div(u128::MAX, 2, 1, Floor)),
            Ok(u128::MAX)
        );
        assert_eq!(
            overflow_to_max(mul_div(1, 1, 0, Floor)),
            Err(MathError::DivByZero)
        );
        assert_eq!(overflow_to_max(mul_div(6, 1, 2, Floor)), Ok(3));
    }

    #[test]
    fn sqrt_and_casts() {
        assert_eq!(sqrt_u128(15, Floor), 3);
        assert_eq!(sqrt_u128(15, Ceil), 4);
        assert_eq!(sqrt_u128(16, Ceil), 4);
        assert_eq!(sqrt_u128(u128::MAX, Ceil), 1u128 << 64);
        assert_eq!(sqrt_u256(U256::MAX, Floor), Ok(u128::MAX));
        assert_eq!(sqrt_u256(U256::MAX, Ceil), Err(MathError::Overflow));
        assert_eq!(
            sqrt_u256(U256::from_product(u128::MAX, u128::MAX), Ceil),
            Ok(u128::MAX)
        );
        assert_eq!(to_u64(u128::from(u64::MAX) + 1), Err(MathError::Overflow));
        assert_eq!(to_u128(U256::from_parts(1, 0)), Err(MathError::Overflow));
        assert_eq!(to_i128(u128::MAX), Err(MathError::Overflow));
        assert_eq!(i128_to_u128(-1), Err(MathError::Underflow));
        assert_eq!(cast::<i64, u64>(-1), Err(MathError::Overflow));
        assert_eq!(cast::<u64, u32>(7), Ok(7u32));
    }
}
