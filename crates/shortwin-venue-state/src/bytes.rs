//! Little-endian field reads with zero padding (ARCH §4.1).
//!
//! A read at `offset` of `N` bytes returns the bytes that exist and zeros for the rest, so a
//! venue account that is shorter than the newest layout (an old PumpSwap `Pool` below 300
//! bytes, R-VEN §1.2) reads its appended fields as zero instead of failing. Callers check the
//! minimum length a field needs before they rely on it being present.

use crate::Key;

/// `N` bytes at `offset`, zero-padded past the end of `data`.
pub fn array_at<const N: usize>(data: &[u8], offset: usize) -> [u8; N] {
    let mut out = [0u8; N];
    if let Some(src) = data.get(offset..) {
        let n = src.len().min(N);
        if let (Some(dst), Some(src)) = (out.get_mut(..n), src.get(..n)) {
            dst.copy_from_slice(src);
        }
    }
    out
}

/// `u8` at `offset` (0 past the end).
pub fn u8_at(data: &[u8], offset: usize) -> u8 {
    data.get(offset).copied().unwrap_or(0)
}

/// Little-endian `u16` at `offset`.
pub fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(array_at(data, offset))
}

/// Little-endian `u32` at `offset`.
pub fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(array_at(data, offset))
}

/// Little-endian `u64` at `offset`.
pub fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(array_at(data, offset))
}

/// Little-endian `i32` at `offset`.
pub fn i32_at(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(array_at(data, offset))
}

/// Little-endian `i64` at `offset`.
pub fn i64_at(data: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(array_at(data, offset))
}

/// Little-endian `u128` at `offset`.
pub fn u128_at(data: &[u8], offset: usize) -> u128 {
    u128::from_le_bytes(array_at(data, offset))
}

/// Little-endian `i128` at `offset`.
pub fn i128_at(data: &[u8], offset: usize) -> i128 {
    i128::from_le_bytes(array_at(data, offset))
}

/// 32-byte key at `offset`.
pub fn key_at(data: &[u8], offset: usize) -> Key {
    array_at(data, offset)
}

/// `true` if the first 8 bytes of `data` equal `disc` (never true for a shorter account).
pub fn has_discriminator(data: &[u8], disc: &[u8; 8]) -> bool {
    data.get(..8) == Some(disc.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_little_endian() {
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!(u8_at(&data, 0), 1);
        assert_eq!(u16_at(&data, 0), 0x0201);
        assert_eq!(u32_at(&data, 1), 0x0504_0302);
        assert_eq!(u64_at(&data, 1), 0x0908_0706_0504_0302);
        assert_eq!(i64_at(&data, 0), 0x0807_0605_0403_0201);
    }

    #[test]
    fn reads_past_the_end_are_zero_padded() {
        let data = [0xffu8, 0xff, 0xff];
        assert_eq!(u32_at(&data, 1), 0x0000_ffff);
        assert_eq!(u64_at(&data, 3), 0);
        assert_eq!(u64_at(&data, 100), 0);
        assert_eq!(u8_at(&data, 3), 0);
        assert_eq!(key_at(&data, 2)[0], 0xff);
        assert_eq!(key_at(&data, 2)[1..], [0u8; 31]);
        assert_eq!(u128_at(&[], 0), 0);
        assert_eq!(i128_at(&[0xff; 16], 0), -1);
        assert_eq!(i32_at(&[0xff; 4], 0), -1);
    }

    #[test]
    fn discriminator() {
        let d = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert!(has_discriminator(&[1, 2, 3, 4, 5, 6, 7, 8, 9], &d));
        assert!(!has_discriminator(&[1, 2, 3, 4, 5, 6, 7], &d));
        assert!(!has_discriminator(&[0, 2, 3, 4, 5, 6, 7, 8], &d));
    }
}
