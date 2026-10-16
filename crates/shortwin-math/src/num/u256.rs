//! Minimal 256-bit unsigned integer (ARCHITECTURE §2.2, A30; ECONOMICS §2).
//!
//! Four little-endian 64-bit limbs. The API mirrors the primitive integers:
//! `checked_*` return `None` on overflow or a zero divisor. Limb arithmetic uses
//! explicit `wrapping_*` / `overflowing_*` operations; each one states why it
//! cannot wrap, or that the wrap is the carry being computed.

use core::cmp::Ordering;
use core::fmt;
use core::num::NonZeroU128;
use core::str::FromStr;

use crate::error::MathError;

const LIMBS: usize = 4;
/// Longest numerator the divider accepts: a 512-bit product (`mul_div` on U256).
const MAX_NUM: usize = 2 * LIMBS;

/// 256-bit unsigned integer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct U256([u64; LIMBS]);

#[inline(always)]
const fn widen(x: u64) -> u128 {
    x as u128
}

#[inline(always)]
#[allow(clippy::cast_possible_truncation)] // keeping only the low 64 bits is the point
const fn lo64(x: u128) -> u64 {
    x as u64
}

#[inline(always)]
const fn hi64(x: u128) -> u64 {
    lo64(x.wrapping_shr(64))
}

#[inline(always)]
const fn join(hi: u64, lo: u64) -> u128 {
    widen(hi).wrapping_shl(64) | widen(lo)
}

/// Number of limbs up to and including the most significant non-zero one.
fn significant(limbs: &[u64]) -> usize {
    limbs
        .iter()
        .rposition(|&l| l != 0)
        .map_or(0, |i| i.wrapping_add(1))
}

/// `out = x << s` for `s < 64` and `out.len() == x.len()`; returns the limb shifted out.
fn shl_small(x: &[u64], s: u32, out: &mut [u64]) -> u64 {
    if s == 0 {
        out.copy_from_slice(x);
        return 0;
    }
    let back = u64::BITS.wrapping_sub(s);
    let mut carry = 0;
    for (o, &l) in out.iter_mut().zip(x) {
        *o = l.wrapping_shl(s) | carry;
        carry = l.wrapping_shr(back);
    }
    carry
}

/// `out = x >> s` for `s < 64` and `x.len() == out.len() + 1`.
fn shr_small(x: &[u64], s: u32, out: &mut [u64]) {
    if s == 0 {
        for (o, &l) in out.iter_mut().zip(x) {
            *o = l;
        }
        return;
    }
    let back = u64::BITS.wrapping_sub(s);
    for (o, pair) in out.iter_mut().zip(x.windows(2)) {
        *o = pair[0].wrapping_shr(s) | pair[1].wrapping_shl(back);
    }
}

/// Schoolbook product. `out` starts zeroed and has `a.len() + b.len()` limbs.
fn mul_limbs(a: &[u64], b: &[u64], out: &mut [u64]) {
    for (i, &ai) in a.iter().enumerate() {
        if ai == 0 {
            continue;
        }
        let mut carry: u64 = 0;
        for (j, &bj) in b.iter().enumerate() {
            let k = i.wrapping_add(j);
            // ai·bj + out[k] + carry ≤ (2^64 − 1)² + 2·(2^64 − 1) = 2^128 − 1: no wrap.
            let t = widen(ai)
                .wrapping_mul(widen(bj))
                .wrapping_add(widen(out[k]))
                .wrapping_add(widen(carry));
            out[k] = lo64(t);
            carry = hi64(t);
        }
        // Row i accumulated into out[i..i + b.len()]; the next limb is still untouched.
        out[i.wrapping_add(b.len())] = carry;
    }
}

#[cfg(test)]
pub(super) static ADD_BACKS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Knuth's Algorithm D (TAOCP vol. 2, §4.3.1) on 64-bit digits.
///
/// `u` has at most `MAX_NUM` limbs; `v` has at most `LIMBS` limbs and must not be zero
/// (a zero divisor yields zero quotient and remainder; callers check first).
/// Returns the quotient (`MAX_NUM` limbs) and the remainder (`LIMBS` limbs).
fn div_rem_limbs(u: &[u64], v: &[u64]) -> ([u64; MAX_NUM], [u64; LIMBS]) {
    let mut q = [0u64; MAX_NUM];
    let mut r = [0u64; LIMBS];
    let n = significant(v);
    let m = significant(u);
    if n == 0 {
        return (q, r);
    }
    if m < n {
        r[..m].copy_from_slice(&u[..m]);
        return (q, r);
    }
    if n == 1 {
        let Some(d) = NonZeroU128::new(widen(v[0])) else {
            return (q, r);
        };
        let mut rem: u128 = 0;
        for i in (0..m).rev() {
            // rem < d < 2^64, so cur / d < 2^64.
            let cur = join(lo64(rem), u[i]);
            q[i] = lo64(cur / d);
            rem = cur % d;
        }
        r[0] = lo64(rem);
        return (q, r);
    }

    // D1: normalise so that the divisor's top bit is set.
    let s = v[n.wrapping_sub(1)].leading_zeros();
    let mut vn = [0u64; LIMBS];
    shl_small(&v[..n], s, &mut vn[..n]);
    let mut un = [0u64; MAX_NUM + 1];
    let spill = shl_small(&u[..m], s, &mut un[..m]);
    un[m] = spill;
    // Normalised, so the top limb is at least 2^63.
    let Some(v_top) = NonZeroU128::new(widen(vn[n.wrapping_sub(1)])) else {
        return (q, r);
    };
    let v_next = widen(vn[n.wrapping_sub(2)]);

    for j in (0..=m.wrapping_sub(n)).rev() {
        let top = j.wrapping_add(n);
        // D3: estimate the quotient digit from the top two digits, refine with the third.
        let num = join(un[top], un[top.wrapping_sub(1)]);
        let mut qhat = num / v_top;
        let mut rhat = num % v_top;
        // The product is evaluated only once qhat < 2^64, and rhat < 2^64 inside the
        // condition (the loop breaks as soon as it is not), so neither side wraps.
        while qhat > widen(u64::MAX)
            || qhat.wrapping_mul(v_next) > join(lo64(rhat), un[top.wrapping_sub(2)])
        {
            qhat = qhat.wrapping_sub(1);
            rhat = rhat.wrapping_add(v_top.get());
            if rhat > widen(u64::MAX) {
                break;
            }
        }

        // D4: un[j..=top] -= qhat · vn. Here qhat < 2^64, so p < 2^128.
        let mut mul_carry: u64 = 0;
        let mut borrow = false;
        for (i, &vi) in vn[..n].iter().enumerate() {
            let k = j.wrapping_add(i);
            let p = qhat.wrapping_mul(widen(vi)).wrapping_add(widen(mul_carry));
            mul_carry = hi64(p);
            let (t, b1) = un[k].overflowing_sub(lo64(p));
            let (t, b2) = t.overflowing_sub(u64::from(borrow));
            un[k] = t;
            borrow = b1 | b2;
        }
        let (t, b1) = un[top].overflowing_sub(mul_carry);
        let (t, b2) = t.overflowing_sub(u64::from(borrow));
        un[top] = t;

        let mut digit = lo64(qhat);
        // D5–D6: qhat was one too large (probability about 2/2^64); add the divisor back.
        // The final carry cancels the borrow taken above, hence the wrapping add.
        if b1 | b2 {
            #[cfg(test)]
            ADD_BACKS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            digit = digit.wrapping_sub(1);
            let mut carry = false;
            for (i, &vi) in vn[..n].iter().enumerate() {
                let k = j.wrapping_add(i);
                let (t, c1) = un[k].overflowing_add(vi);
                let (t, c2) = t.overflowing_add(u64::from(carry));
                un[k] = t;
                carry = c1 | c2;
            }
            un[top] = un[top].wrapping_add(u64::from(carry));
        }
        q[j] = digit;
    }

    // D8: the remainder is un[..n] >> s (un[n] is zero at this point).
    shr_small(&un[..=n], s, &mut r[..n]);
    (q, r)
}

const fn first4(q: &[u64; MAX_NUM]) -> [u64; LIMBS] {
    [q[0], q[1], q[2], q[3]]
}

/// 10^38, the largest power of ten below 2^128 (used by `Display`).
const TEN_POW_38: u128 = 100_000_000_000_000_000_000_000_000_000_000_000_000;

impl U256 {
    /// 0.
    pub const ZERO: Self = Self([0; LIMBS]);
    /// 1.
    pub const ONE: Self = Self([1, 0, 0, 0]);
    /// 2^256 − 1.
    pub const MAX: Self = Self([u64::MAX; LIMBS]);
    /// Width in bits.
    pub const BITS: u32 = 256;

    /// From little-endian 64-bit limbs.
    pub const fn from_limbs(limbs: [u64; 4]) -> Self {
        Self(limbs)
    }

    /// Little-endian 64-bit limbs.
    pub const fn to_limbs(self) -> [u64; 4] {
        self.0
    }

    /// From a `u64`.
    pub const fn from_u64(x: u64) -> Self {
        Self([x, 0, 0, 0])
    }

    /// From a `u128`.
    pub const fn from_u128(x: u128) -> Self {
        Self([lo64(x), hi64(x), 0, 0])
    }

    /// `hi · 2^128 + lo`.
    pub const fn from_parts(hi: u128, lo: u128) -> Self {
        Self([lo64(lo), hi64(lo), lo64(hi), hi64(hi)])
    }

    /// Low 128 bits (truncating).
    pub const fn low_u128(self) -> u128 {
        join(self.0[1], self.0[0])
    }

    /// High 128 bits.
    pub const fn high_u128(self) -> u128 {
        join(self.0[3], self.0[2])
    }

    /// The value as a `u128`, if it fits.
    pub const fn to_u128(self) -> Option<u128> {
        if self.high_u128() == 0 {
            Some(self.low_u128())
        } else {
            None
        }
    }

    /// The value as a `u64`, if it fits.
    pub const fn to_u64(self) -> Option<u64> {
        if self.0[1] == 0 && self.0[2] == 0 && self.0[3] == 0 {
            Some(self.0[0])
        } else {
            None
        }
    }

    /// Full 256-bit product of two `u128` values; never overflows.
    pub fn from_product(a: u128, b: u128) -> Self {
        let mut out = [0u64; LIMBS];
        mul_limbs(&[lo64(a), hi64(a)], &[lo64(b), hi64(b)], &mut out);
        Self(out)
    }

    /// Little-endian bytes.
    pub fn to_le_bytes(self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (chunk, limb) in out.as_chunks_mut::<8>().0.iter_mut().zip(self.0) {
            *chunk = limb.to_le_bytes();
        }
        out
    }

    /// From little-endian bytes.
    pub fn from_le_bytes(bytes: [u8; 32]) -> Self {
        let mut limbs = [0u64; LIMBS];
        for (limb, chunk) in limbs.iter_mut().zip(bytes.as_chunks::<8>().0) {
            *limb = u64::from_le_bytes(*chunk);
        }
        Self(limbs)
    }

    /// `true` if the value is 0.
    pub const fn is_zero(self) -> bool {
        self.0[0] == 0 && self.0[1] == 0 && self.0[2] == 0 && self.0[3] == 0
    }

    /// Number of leading zero bits (256 for 0).
    pub fn leading_zeros(self) -> u32 {
        let mut zeros = 0u32;
        for &l in self.0.iter().rev() {
            if l != 0 {
                return zeros.wrapping_add(l.leading_zeros());
            }
            zeros = zeros.wrapping_add(u64::BITS);
        }
        zeros
    }

    /// Bit length: the position of the highest set bit plus one (0 for 0).
    pub fn bits(self) -> u32 {
        Self::BITS.wrapping_sub(self.leading_zeros())
    }

    /// `self + rhs` and whether it wrapped.
    pub fn overflowing_add(self, rhs: Self) -> (Self, bool) {
        let mut out = [0u64; LIMBS];
        let mut carry = false;
        for ((o, &a), &b) in out.iter_mut().zip(&self.0).zip(&rhs.0) {
            let (s, c1) = a.overflowing_add(b);
            let (s, c2) = s.overflowing_add(u64::from(carry));
            *o = s;
            carry = c1 | c2;
        }
        (Self(out), carry)
    }

    /// `self − rhs` and whether it wrapped.
    pub fn overflowing_sub(self, rhs: Self) -> (Self, bool) {
        let mut out = [0u64; LIMBS];
        let mut borrow = false;
        for ((o, &a), &b) in out.iter_mut().zip(&self.0).zip(&rhs.0) {
            let (d, b1) = a.overflowing_sub(b);
            let (d, b2) = d.overflowing_sub(u64::from(borrow));
            *o = d;
            borrow = b1 | b2;
        }
        (Self(out), borrow)
    }

    /// `self + rhs`, or `None` on overflow.
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        match self.overflowing_add(rhs) {
            (sum, false) => Some(sum),
            (_, true) => None,
        }
    }

    /// `self − rhs`, or `None` if `rhs > self`.
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        match self.overflowing_sub(rhs) {
            (diff, false) => Some(diff),
            (_, true) => None,
        }
    }

    /// Full 512-bit product as little-endian limbs.
    pub(crate) fn mul_wide(self, rhs: Self) -> [u64; MAX_NUM] {
        let mut out = [0u64; MAX_NUM];
        let (a, b) = (significant(&self.0), significant(&rhs.0));
        mul_limbs(&self.0[..a], &rhs.0[..b], &mut out[..a.wrapping_add(b)]);
        out
    }

    /// `self · rhs`, or `None` on overflow.
    pub fn checked_mul(self, rhs: Self) -> Option<Self> {
        let wide = self.mul_wide(rhs);
        if wide[LIMBS..].iter().any(|&l| l != 0) {
            None
        } else {
            Some(Self(first4(&wide)))
        }
    }

    /// `(self / rhs, self % rhs)`, or `None` if `rhs` is 0.
    pub fn div_rem(self, rhs: Self) -> Option<(Self, Self)> {
        if rhs.is_zero() {
            return None;
        }
        if self < rhs {
            return Some((Self::ZERO, self));
        }
        if let Some(a) = self.to_u128() {
            // rhs ≤ self, so it fits too.
            let b = rhs.low_u128();
            return Some((
                Self::from_u128(a.checked_div(b)?),
                Self::from_u128(a.checked_rem(b)?),
            ));
        }
        let (q, r) = div_rem_limbs(&self.0, &rhs.0);
        Some((Self(first4(&q)), Self(r)))
    }

    /// `self / rhs`, or `None` if `rhs` is 0.
    pub fn checked_div(self, rhs: Self) -> Option<Self> {
        self.div_rem(rhs).map(|(q, _)| q)
    }

    /// `self % rhs`, or `None` if `rhs` is 0.
    pub fn checked_rem(self, rhs: Self) -> Option<Self> {
        self.div_rem(rhs).map(|(_, r)| r)
    }

    /// `(self / rhs, self % rhs)` for a `u128` divisor, or `None` if `rhs` is 0.
    pub fn div_rem_u128(self, rhs: u128) -> Option<(Self, u128)> {
        if rhs == 0 {
            return None;
        }
        if let Some(a) = self.to_u128() {
            return Some((Self::from_u128(a.checked_div(rhs)?), a.checked_rem(rhs)?));
        }
        let (q, r) = div_rem_limbs(&self.0, &[lo64(rhs), hi64(rhs)]);
        Some((Self(first4(&q)), join(r[1], r[0])))
    }

    /// `self / rhs` for a `u128` divisor, or `None` if `rhs` is 0.
    pub fn checked_div_u128(self, rhs: u128) -> Option<Self> {
        self.div_rem_u128(rhs).map(|(q, _)| q)
    }

    /// `self % rhs` for a `u128` divisor, or `None` if `rhs` is 0.
    pub fn checked_rem_u128(self, rhs: u128) -> Option<u128> {
        self.div_rem_u128(rhs).map(|(_, r)| r)
    }

    /// `⌊a·b/d⌋` over the full 512-bit product and whether the division left a
    /// remainder; `None` if `d` is 0 or the quotient does not fit 256 bits.
    pub(crate) fn mul_div_floor(a: Self, b: Self, d: Self) -> Option<(Self, bool)> {
        if d.is_zero() {
            return None;
        }
        let wide = a.mul_wide(b);
        if wide[LIMBS..].iter().all(|&l| l == 0) {
            let (q, r) = Self(first4(&wide)).div_rem(d)?;
            return Some((q, !r.is_zero()));
        }
        let (q, r) = div_rem_limbs(&wide, &d.0);
        if q[LIMBS..].iter().any(|&l| l != 0) {
            return None;
        }
        Some((Self(first4(&q)), r.iter().any(|&l| l != 0)))
    }

    /// `self << k` for `k < 256`, discarding bits shifted out.
    fn shl_bits(self, k: u32) -> Self {
        let limb = usize::try_from(k / 64).unwrap_or(LIMBS);
        let bit = k % 64;
        let mut out = [0u64; LIMBS];
        for (i, o) in out.iter_mut().enumerate() {
            let Some(src) = i.checked_sub(limb) else {
                continue;
            };
            let mut v = self.0[src].wrapping_shl(bit);
            if bit != 0
                && let Some(lower) = src.checked_sub(1)
            {
                v |= self.0[lower].wrapping_shr(u64::BITS.wrapping_sub(bit));
            }
            *o = v;
        }
        Self(out)
    }

    /// `self >> k` for `k < 256`.
    fn shr_bits(self, k: u32) -> Self {
        let limb = usize::try_from(k / 64).unwrap_or(LIMBS);
        let bit = k % 64;
        let mut out = [0u64; LIMBS];
        for (i, o) in out.iter_mut().enumerate() {
            let src = i.wrapping_add(limb);
            let Some(&cur) = self.0.get(src) else {
                continue;
            };
            let mut v = cur.wrapping_shr(bit);
            if bit != 0
                && let Some(&upper) = self.0.get(src.wrapping_add(1))
            {
                v |= upper.wrapping_shl(u64::BITS.wrapping_sub(bit));
            }
            *o = v;
        }
        Self(out)
    }

    /// `self << k`, or `None` if `k ≥ 256`. As with the primitive integers, bits
    /// shifted out at the top are discarded; use [`U256::shl_exact`] to detect that.
    pub fn checked_shl(self, k: u32) -> Option<Self> {
        (k < Self::BITS).then(|| self.shl_bits(k))
    }

    /// `self >> k`, or `None` if `k ≥ 256`.
    pub fn checked_shr(self, k: u32) -> Option<Self> {
        (k < Self::BITS).then(|| self.shr_bits(k))
    }

    /// `self << k`, or `None` if `k ≥ 256` or any set bit would be shifted out.
    pub fn shl_exact(self, k: u32) -> Option<Self> {
        if self.is_zero() {
            return (k < Self::BITS).then_some(self);
        }
        (k <= self.leading_zeros()).then(|| self.shl_bits(k))
    }

    /// `⌊√self⌋`. The square root of a 256-bit value always fits 128 bits.
    pub fn isqrt(self) -> u128 {
        if let Some(x) = self.to_u128() {
            return x.isqrt();
        }
        // Newton's iteration from above: x₀ = 2^⌈bits/2⌉ ≥ √self, then x decreases
        // strictly until it reaches ⌊√self⌋. Here self ≥ 2^128, so every x ≥ 2^64 and
        // x + self/x < 2^130: the addition cannot wrap.
        let mut x = Self::ONE.shl_bits(self.bits().div_ceil(2));
        loop {
            let (q, _) = div_rem_limbs(&self.0, &x.0);
            let (sum, _) = x.overflowing_add(Self(first4(&q)));
            let y = sum.shr_bits(1);
            if y >= x {
                return x.low_u128();
            }
            x = y;
        }
    }
}

impl Ord for U256 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.iter().rev().cmp(other.0.iter().rev())
    }
}

impl PartialOrd for U256 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

macro_rules! from_unsigned {
    ($($t:ty),*) => {$(
        impl From<$t> for U256 {
            fn from(x: $t) -> Self {
                Self::from_u128(u128::from(x))
            }
        }
    )*};
}
from_unsigned!(u8, u16, u32, u64, u128);

impl TryFrom<U256> for u128 {
    type Error = MathError;
    fn try_from(x: U256) -> Result<Self, MathError> {
        x.to_u128().ok_or(MathError::Overflow)
    }
}

impl TryFrom<U256> for u64 {
    type Error = MathError;
    fn try_from(x: U256) -> Result<Self, MathError> {
        x.to_u64().ok_or(MathError::Overflow)
    }
}

/// Fixed-capacity sink for `Display` (2^256 − 1 has 78 decimal digits).
struct DigitBuf {
    bytes: [u8; 78],
    len: usize,
}

impl fmt::Write for DigitBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len.checked_add(s.len()).ok_or(fmt::Error)?;
        self.bytes
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Decimal.
impl fmt::Display for U256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use fmt::Write as _;
        if let Some(x) = self.to_u128() {
            return fmt::Display::fmt(&x, f);
        }
        // self = a·10^76 + b·10^38 + c with b, c < 10^38 and a < 12.
        let (rest, c) = self.div_rem_u128(TEN_POW_38).ok_or(fmt::Error)?;
        let (a, b) = rest.div_rem_u128(TEN_POW_38).ok_or(fmt::Error)?;
        let a = a.to_u128().ok_or(fmt::Error)?;
        let mut buf = DigitBuf {
            bytes: [0; 78],
            len: 0,
        };
        if a == 0 {
            write!(buf, "{b}{c:038}")?;
        } else {
            write!(buf, "{a}{b:038}{c:038}")?;
        }
        let digits = buf.bytes.get(..buf.len).ok_or(fmt::Error)?;
        f.pad_integral(
            true,
            "",
            core::str::from_utf8(digits).map_err(|_| fmt::Error)?,
        )
    }
}

impl fmt::Debug for U256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Parses decimal, or hexadecimal with a `0x` prefix. No sign, no separators.
impl FromStr for U256 {
    type Err = MathError;

    fn from_str(s: &str) -> Result<Self, MathError> {
        let (digits, radix) = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(hex) => (hex, 16),
            None => (s, 10),
        };
        if digits.is_empty() {
            return Err(MathError::InvalidInput);
        }
        let base = Self::from(radix);
        let mut acc = Self::ZERO;
        for c in digits.chars() {
            let d = c.to_digit(radix).ok_or(MathError::InvalidInput)?;
            acc = acc
                .checked_mul(base)
                .and_then(|x| x.checked_add(Self::from(d)))
                .ok_or(MathError::Overflow)?;
        }
        Ok(acc)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use core::sync::atomic::Ordering::Relaxed;
    use std::string::ToString;

    const H: u64 = 1 << 63;

    fn u(limbs: &[u64]) -> U256 {
        let mut l = [0u64; 4];
        l[..limbs.len()].copy_from_slice(limbs);
        U256::from_limbs(l)
    }

    /// Scaled from the Hacker's Delight `divmnu` vectors; each needs step D6.
    #[test]
    fn knuth_add_back_is_exercised() {
        let cases: [(&[u64], &[u64]); 6] = [
            (&[0, 0, H, H - 1], &[1, 0, H]),
            (&[3, 0, H], &[1, 0, 1 << 61]),
            (&[3, 0, 1 << 15], &[1, 0, 1 << 13]),
            (&[0, 0, 1 << 15, (1 << 15) - 1], &[1, 0, 1 << 15]),
            (&[0, u64::MAX - 1, 0, H], &[u64::MAX, 0, H]),
            (&[3, 0, H, 0], &[1, 0, 1 << 61]),
        ];
        let before = ADD_BACKS.load(Relaxed);
        for (num, den) in cases {
            let (a, b) = (u(num), u(den));
            let (q, r) = a.div_rem(b).unwrap();
            assert!(r < b);
            assert_eq!(q.checked_mul(b).unwrap().checked_add(r).unwrap(), a);
        }
        assert!(ADD_BACKS.load(Relaxed) >= before + cases.len());
    }

    #[test]
    fn edges() {
        assert_eq!(U256::MAX.checked_add(U256::ONE), None);
        assert_eq!(U256::ZERO.checked_sub(U256::ONE), None);
        assert_eq!(U256::MAX.checked_mul(U256::from(2u8)), None);
        assert_eq!(U256::ONE.div_rem(U256::ZERO), None);
        assert_eq!(U256::ONE.div_rem_u128(0), None);
        assert_eq!(
            U256::MAX.div_rem(U256::MAX).unwrap(),
            (U256::ONE, U256::ZERO)
        );
        assert_eq!(U256::MAX.isqrt(), u128::MAX);
        assert_eq!(U256::from_parts(1, 0).isqrt(), 1u128 << 64);
        assert_eq!(U256::ZERO.bits(), 0);
        assert_eq!(U256::MAX.bits(), 256);
        assert_eq!(U256::ONE.checked_shl(255).unwrap().bits(), 256);
        assert_eq!(U256::ONE.checked_shl(256), None);
        assert_eq!(U256::ONE.shl_exact(255), U256::ONE.checked_shl(255));
        assert_eq!(U256::from(2u8).shl_exact(255), None);
        assert_eq!(U256::ZERO.shl_exact(255), Some(U256::ZERO));
        assert_eq!(
            U256::from_product(u128::MAX, u128::MAX),
            U256::from_parts(u128::MAX - 1, 1)
        );
    }

    #[test]
    fn text_round_trip() {
        let max = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        assert_eq!(U256::MAX.to_string(), max);
        assert_eq!(max.parse::<U256>().unwrap(), U256::MAX);
        assert_eq!(
            U256::from_parts(1, 0).to_string(),
            "340282366920938463463374607431768211456"
        );
        assert_eq!(U256::from_parts(0, 7).to_string(), "7");
        assert_eq!("0xff".parse::<U256>().unwrap(), U256::from(255u8));
        assert_eq!(std::format!("{:>5}", U256::from(42u8)), "   42");
        assert_eq!("".parse::<U256>(), Err(MathError::InvalidInput));
        assert_eq!("0x".parse::<U256>(), Err(MathError::InvalidInput));
        assert_eq!("-1".parse::<U256>(), Err(MathError::InvalidInput));
        let too_big =
            "115792089237316195423570985008687907853269984665640564039457584007913129639936";
        assert_eq!(too_big.parse::<U256>(), Err(MathError::Overflow));
    }
}
