//! U256 against num-bigint (ARCHITECTURE §2.2 A30, §8.1): every operation, at least
//! 2,000 cases each, on an edge-heavy distribution (0, 1, 2^63, 2^64 − 1, short values).

mod common;

use num_bigint::BigUint;
use proptest::prelude::*;
use shortwin_math::U256;

fn big(x: U256) -> BigUint {
    BigUint::from_bytes_le(&x.to_le_bytes())
}

fn from_big(b: &BigUint) -> U256 {
    let bytes = b.to_bytes_le();
    assert!(bytes.len() <= 32, "value exceeds 256 bits");
    let mut out = [0u8; 32];
    out[..bytes.len()].copy_from_slice(&bytes);
    U256::from_le_bytes(out)
}

fn modulus() -> BigUint {
    BigUint::from(1u8) << 256
}

fn limb() -> impl Strategy<Value = u64> {
    prop_oneof![
        4 => any::<u64>(),
        1 => Just(0u64),
        1 => Just(1u64),
        1 => Just(u64::MAX),
        1 => Just(u64::MAX - 1),
        1 => Just(1u64 << 63),
        1 => Just((1u64 << 63) - 1),
        1 => any::<u64>().prop_map(|x| x | (1 << 63)),
        1 => 0u64..1024,
    ]
}

/// Random limbs, truncated to 0–4 significant limbs.
fn u256() -> impl Strategy<Value = U256> {
    (prop::array::uniform4(limb()), 0usize..=4).prop_map(|(mut l, n)| {
        l[n..].iter_mut().for_each(|x| *x = 0);
        U256::from_limbs(l)
    })
}

fn u128_edge() -> impl Strategy<Value = u128> {
    (limb(), limb(), 0usize..=2).prop_map(|(lo, hi, n)| match n {
        0 => 0,
        1 => u128::from(lo),
        _ => (u128::from(hi) << 64) | u128::from(lo),
    })
}

proptest! {
    #![proptest_config(common::proptest_config())]

    #[test]
    fn add(a in u256(), b in u256()) {
        let sum = big(a) + big(b);
        let fits = sum < modulus();
        prop_assert_eq!(a.checked_add(b).map(big), fits.then(|| sum.clone()));
        let (wrapped, overflow) = a.overflowing_add(b);
        prop_assert_eq!(big(wrapped), &sum % modulus());
        prop_assert_eq!(overflow, !fits);
    }

    #[test]
    fn sub(a in u256(), b in u256()) {
        let (ba, bb) = (big(a), big(b));
        let expected = (ba >= bb).then(|| &ba - &bb);
        prop_assert_eq!(a.checked_sub(b).map(big), expected);
        let (wrapped, borrow) = a.overflowing_sub(b);
        prop_assert_eq!(big(wrapped), (&ba + modulus() - &bb) % modulus());
        prop_assert_eq!(borrow, ba < bb);
    }

    #[test]
    fn mul(a in u256(), b in u256()) {
        let product = big(a) * big(b);
        let expected = (product < modulus()).then_some(product);
        prop_assert_eq!(a.checked_mul(b).map(big), expected);
    }

    #[test]
    fn from_product(a in u128_edge(), b in u128_edge()) {
        prop_assert_eq!(big(U256::from_product(a, b)), BigUint::from(a) * BigUint::from(b));
    }

    #[test]
    fn div_rem(a in u256(), b in u256()) {
        let got = a.div_rem(b);
        if b.is_zero() {
            prop_assert_eq!(got, None);
            prop_assert_eq!(a.checked_div(b), None);
            prop_assert_eq!(a.checked_rem(b), None);
        } else {
            let (q, r) = got.expect("non-zero divisor");
            prop_assert_eq!(big(q), big(a) / big(b));
            prop_assert_eq!(big(r), big(a) % big(b));
            prop_assert_eq!(a.checked_div(b), Some(q));
            prop_assert_eq!(a.checked_rem(b), Some(r));
        }
    }

    /// u = q·v + r with r < v, built so that it fits: exercises every quotient digit
    /// pattern (including Knuth's add-back step) with a known answer.
    #[test]
    fn div_rem_constructed(v in u256(), q in u256(), r in u256()) {
        let v = if v.is_zero() { U256::ONE } else { v };
        let (bv, br) = (big(v), big(r) % big(v));
        let q_max = (modulus() - 1u8 - &br) / &bv;
        let bq = big(q) % (q_max + 1u8);
        let u = from_big(&(&bq * &bv + &br));
        let (gq, gr) = u.div_rem(v).expect("non-zero divisor");
        prop_assert_eq!(big(gq), bq);
        prop_assert_eq!(big(gr), br);
    }

    #[test]
    fn div_rem_u128(a in u256(), d in u128_edge()) {
        let got = a.div_rem_u128(d);
        if d == 0 {
            prop_assert_eq!(got, None);
            prop_assert_eq!(a.checked_div_u128(d), None);
            prop_assert_eq!(a.checked_rem_u128(d), None);
        } else {
            let (q, r) = got.expect("non-zero divisor");
            prop_assert_eq!(big(q), big(a) / BigUint::from(d));
            prop_assert_eq!(BigUint::from(r), big(a) % BigUint::from(d));
            prop_assert_eq!(a.checked_div_u128(d), Some(q));
            prop_assert_eq!(a.checked_rem_u128(d), Some(r));
        }
    }

    #[test]
    fn shifts(a in u256(), k in 0u32..300) {
        let (shl, shr, exact) = (a.checked_shl(k), a.checked_shr(k), a.shl_exact(k));
        if k >= 256 {
            prop_assert_eq!(shl, None);
            prop_assert_eq!(shr, None);
            prop_assert_eq!(exact, None);
        } else {
            let wide = big(a) << k;
            prop_assert_eq!(shl.map(big), Some(&wide % modulus()));
            prop_assert_eq!(shr.map(big), Some(big(a) >> k));
            prop_assert_eq!(exact.map(big), (wide < modulus()).then_some(wide));
        }
    }

    #[test]
    fn compare(a in u256(), b in u256()) {
        prop_assert_eq!(a.cmp(&b), big(a).cmp(&big(b)));
        prop_assert_eq!(a == b, big(a) == big(b));
    }

    #[test]
    fn isqrt(a in u256()) {
        prop_assert_eq!(BigUint::from(a.isqrt()), big(a).sqrt());
    }

    #[test]
    fn bits(a in u256()) {
        let expected = u32::try_from(big(a).bits()).expect("at most 256");
        prop_assert_eq!(a.bits(), expected);
        prop_assert_eq!(a.leading_zeros(), 256 - expected);
        prop_assert_eq!(a.is_zero(), expected == 0);
    }

    #[test]
    fn conversions(a in u256(), x in u128_edge()) {
        prop_assert_eq!(U256::from_le_bytes(a.to_le_bytes()), a);
        prop_assert_eq!(U256::from_limbs(a.to_limbs()), a);
        prop_assert_eq!(U256::from_parts(a.high_u128(), a.low_u128()), a);
        prop_assert_eq!(a.to_u128(), u128::try_from(&big(a)).ok());
        prop_assert_eq!(a.to_u64(), u64::try_from(&big(a)).ok());
        prop_assert_eq!(u128::try_from(a).ok(), a.to_u128());
        prop_assert_eq!(U256::from(x).to_u128(), Some(x));
        prop_assert_eq!(big(U256::from_u128(x)), BigUint::from(x));
    }

    #[test]
    fn text(a in u256()) {
        let s = a.to_string();
        prop_assert_eq!(&s, &big(a).to_string());
        prop_assert_eq!(s.parse::<U256>(), Ok(a));
        prop_assert_eq!(format!("0x{}", big(a).to_str_radix(16)).parse::<U256>(), Ok(a));
    }
}
