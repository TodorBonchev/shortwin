//! `num` helpers against num-bigint (ARCHITECTURE §2.2, §8.1; ECONOMICS §2): mul_div
//! on u64/u128/U256/i128, divisions, WAD/BPS/PPM, square roots and casts, both
//! rounding directions, at least 2,000 cases each.

mod common;

use num_bigint::{BigInt, BigUint};
use proptest::prelude::*;
use shortwin_math::MathError;
use shortwin_math::num::{
    self, BPS, PPM, Rounding, U256, WAD, cast, div, div_ceil, div_floor, div_i128, div_u64,
    div_wad, i128_to_u128, mul_bps, mul_bps_u64, mul_div, mul_div_i128, mul_div_u64, mul_div_u256,
    mul_ppm, mul_ppm_u64, mul_wad, sqrt_u128, sqrt_u256, to_i128, to_u64,
};

fn big(x: U256) -> BigUint {
    BigUint::from_bytes_le(&x.to_le_bytes())
}

fn rounding() -> impl Strategy<Value = Rounding> {
    prop_oneof![Just(Rounding::Floor), Just(Rounding::Ceil)]
}

/// Unsigned reference: `n / d` rounded, `DivByZero` or `Overflow` beyond `max`.
fn reference(n: BigUint, d: BigUint, r: Rounding, max: &BigUint) -> Result<BigUint, MathError> {
    if d == BigUint::ZERO {
        return Err(MathError::DivByZero);
    }
    let (q, rem) = (&n / &d, &n % &d);
    let q = if r == Rounding::Ceil && rem != BigUint::ZERO {
        q + 1u8
    } else {
        q
    };
    if &q > max {
        Err(MathError::Overflow)
    } else {
        Ok(q)
    }
}

/// Signed reference: floor toward −∞, ceil toward +∞ (d > 0).
fn reference_signed(n: BigInt, d: BigInt, r: Rounding) -> BigInt {
    let q = &n / &d; // truncates toward zero
    let rem = &n - &q * &d; // sign of n
    match r {
        Rounding::Floor if rem < BigInt::ZERO => q - 1,
        Rounding::Ceil if rem > BigInt::ZERO => q + 1,
        _ => q,
    }
}

fn limb() -> impl Strategy<Value = u64> {
    prop_oneof![
        4 => any::<u64>(),
        1 => Just(0u64),
        1 => Just(1u64),
        1 => Just(u64::MAX),
        1 => Just(1u64 << 63),
        1 => 0u64..1024,
    ]
}

fn u128_edge() -> impl Strategy<Value = u128> {
    prop_oneof![
        3 => (limb(), limb()).prop_map(|(hi, lo)| (u128::from(hi) << 64) | u128::from(lo)),
        2 => limb().prop_map(u128::from),
        1 => Just(u128::MAX),
        1 => Just(WAD),
        1 => 0u128..16,
    ]
}

fn i128_edge() -> impl Strategy<Value = i128> {
    prop_oneof![
        4 => u128_edge().prop_map(|x| x as i128),
        1 => Just(i128::MIN),
        1 => Just(i128::MAX),
        1 => -16i128..16,
    ]
}

fn u256() -> impl Strategy<Value = U256> {
    (prop::array::uniform4(limb()), 0usize..=4).prop_map(|(mut l, n)| {
        l[n..].iter_mut().for_each(|x| *x = 0);
        U256::from_limbs(l)
    })
}

fn max_u128() -> BigUint {
    BigUint::from(u128::MAX)
}

proptest! {
    #![proptest_config(common::proptest_config())]

    #[test]
    fn mul_div_u128(a in u128_edge(), b in u128_edge(), d in u128_edge(), r in rounding()) {
        let expected = reference(BigUint::from(a) * BigUint::from(b), BigUint::from(d), r, &max_u128());
        prop_assert_eq!(mul_div(a, b, d, r).map(BigUint::from), expected);
    }

    #[test]
    fn mul_div_u64_matches(a in limb(), b in limb(), d in limb(), r in rounding()) {
        let expected = reference(
            BigUint::from(a) * BigUint::from(b), BigUint::from(d), r, &BigUint::from(u64::MAX),
        );
        prop_assert_eq!(mul_div_u64(a, b, d, r).map(BigUint::from), expected);
    }

    /// The native u64 path (`a·b` fits `u64`) equals the reference, at every width split.
    #[test]
    fn mul_div_u64_native_path(
        (a, b) in (1u32..=63).prop_flat_map(|k| (0..=u64::MAX >> k, 0..=u64::MAX >> (64 - k))),
        d in prop_oneof![limb(), 0u64..=16, 1u64..=u64::from(u32::MAX)],
        r in rounding(),
    ) {
        let expected = reference(
            BigUint::from(a) * BigUint::from(b), BigUint::from(d), r, &BigUint::from(u64::MAX),
        );
        prop_assert_eq!(mul_div_u64(a, b, d, r).map(BigUint::from), expected);
    }

    #[test]
    fn mul_div_u256_matches(a in u256(), b in u256(), d in u256(), r in rounding()) {
        let max = (BigUint::from(1u8) << 256) - 1u8;
        let expected = reference(big(a) * big(b), big(d), r, &max);
        prop_assert_eq!(mul_div_u256(a, b, d, r).map(big), expected);
    }

    #[test]
    fn mul_div_i128_matches(a in i128_edge(), b in u128_edge(), d in u128_edge(), r in rounding()) {
        let got = mul_div_i128(a, b, d, r);
        if d == 0 {
            prop_assert_eq!(got, Err(MathError::DivByZero));
        } else {
            let q = reference_signed(BigInt::from(a) * BigInt::from(b), BigInt::from(d), r);
            prop_assert_eq!(got, i128::try_from(&q).map_err(|_| MathError::Overflow));
        }
    }

    #[test]
    fn divisions(a in u128_edge(), d in u128_edge(), x in limb(), y in limb(), s in i128_edge(), r in rounding()) {
        let expected = reference(BigUint::from(a), BigUint::from(d), r, &max_u128());
        prop_assert_eq!(div(a, d, r).map(BigUint::from), expected);
        prop_assert_eq!(div_floor(a, d), div(a, d, Rounding::Floor));
        prop_assert_eq!(div_ceil(a, d), div(a, d, Rounding::Ceil));
        let expected = reference(BigUint::from(x), BigUint::from(y), r, &BigUint::from(u64::MAX));
        prop_assert_eq!(div_u64(x, y, r).map(BigUint::from), expected);
        if d == 0 {
            prop_assert_eq!(div_i128(s, d, r), Err(MathError::DivByZero));
        } else {
            let q = reference_signed(BigInt::from(s), BigInt::from(d), r);
            prop_assert_eq!(div_i128(s, d, r), i128::try_from(&q).map_err(|_| MathError::Overflow));
        }
    }

    #[test]
    fn fixed_point(a in u128_edge(), b in u128_edge(), x in limb(), f in 0u64..=20_000, r in rounding()) {
        let (ba, bb, wad) = (BigUint::from(a), BigUint::from(b), BigUint::from(WAD));
        prop_assert_eq!(mul_wad(a, b, r).map(BigUint::from), reference(&ba * &bb, wad.clone(), r, &max_u128()));
        prop_assert_eq!(div_wad(a, b, r).map(BigUint::from), reference(&ba * &wad, bb.clone(), r, &max_u128()));
        let (bf, bx) = (BigUint::from(f), BigUint::from(x));
        prop_assert_eq!(mul_bps(a, u128::from(f), r).map(BigUint::from), reference(&ba * &bf, BigUint::from(BPS), r, &max_u128()));
        prop_assert_eq!(mul_ppm(a, u128::from(f), r).map(BigUint::from), reference(&ba * &bf, BigUint::from(PPM), r, &max_u128()));
        let max64 = BigUint::from(u64::MAX);
        prop_assert_eq!(mul_bps_u64(x, f, r).map(BigUint::from), reference(&bx * &bf, BigUint::from(BPS), r, &max64));
        prop_assert_eq!(mul_ppm_u64(x, f, r).map(BigUint::from), reference(&bx * &bf, BigUint::from(PPM), r, &max64));
    }

    #[test]
    fn square_roots(x in u128_edge(), y in u256(), r in rounding()) {
        let bx = BigUint::from(x);
        let floor = bx.sqrt();
        let want = if r == Rounding::Ceil && &floor * &floor != bx { floor + 1u8 } else { floor };
        prop_assert_eq!(BigUint::from(sqrt_u128(x, r)), want);

        let by = big(y);
        let floor = by.sqrt();
        let want = if r == Rounding::Ceil && &floor * &floor != by { floor + 1u8 } else { floor };
        let want = if want > max_u128() { Err(MathError::Overflow) } else { Ok(want) };
        prop_assert_eq!(sqrt_u256(y, r).map(BigUint::from), want);
    }

    #[test]
    fn casts(x in u128_edge(), s in i128_edge(), y in u256()) {
        prop_assert_eq!(to_u64(x), u64::try_from(x).map_err(|_| MathError::Overflow));
        prop_assert_eq!(to_i128(x), i128::try_from(x).map_err(|_| MathError::Overflow));
        prop_assert_eq!(i128_to_u128(s), u128::try_from(s).map_err(|_| MathError::Underflow));
        prop_assert_eq!(num::to_u128(y), y.to_u128().ok_or(MathError::Overflow));
        prop_assert_eq!(cast::<i128, u64>(s), u64::try_from(s).map_err(|_| MathError::Overflow));
    }
}
