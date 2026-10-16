//! `PodU128` / `PodI128`: 128-bit values in zero-copy accounts (ARCH §2.3 zero-copy layout
//! rule, A67; R3B-4).
//!
//! `align_of::<u128>()` is 8 on sbpfv3 but 16 on x86_64 and wasm32 [V20], so a raw 128-bit
//! field would give the program, the keeper and the frontend different offsets. These are
//! `#[repr(transparent)]` newtypes over `[u8; 16]` (little-endian, alignment 1), so the IDL —
//! and hence Codama and `declare_program!` — describes them as 16-byte arrays. Zero-copy
//! structs never hold a raw `u128`/`i128` field (the `raw_128_bit_fields` lint test in
//! `layout_asserts.rs`); the program converts to the math crate's native types at load and
//! store and never casts account bytes to a math type (ARCH §2.2).

use anchor_lang::prelude::*;

// `#[zero_copy]` needs named fields (its accessor derive), so the two newtypes spell the
// derives out: `bytemuck::Pod` makes the IDL say `serialization: bytemuck`, `AnchorSerialize`
// emits the IDL type (a tuple struct of `[u8; 16]`) under `idl-build`.

/// Unsigned 128-bit value, 16 little-endian bytes, alignment 1.
#[derive(
    AnchorSerialize,
    AnchorDeserialize,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    bytemuck::Pod,
    bytemuck::Zeroable,
)]
#[repr(transparent)]
pub struct PodU128(pub [u8; 16]);

/// Signed 128-bit value (two's complement), 16 little-endian bytes, alignment 1.
#[derive(
    AnchorSerialize,
    AnchorDeserialize,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    bytemuck::Pod,
    bytemuck::Zeroable,
)]
#[repr(transparent)]
pub struct PodI128(pub [u8; 16]);

impl PodU128 {
    /// Zero.
    pub const ZERO: Self = Self([0; 16]);

    /// Wraps a native value.
    pub const fn new(v: u128) -> Self {
        Self(v.to_le_bytes())
    }

    /// The native value.
    pub const fn get(&self) -> u128 {
        u128::from_le_bytes(self.0)
    }

    /// Stores a native value.
    pub fn set(&mut self, v: u128) {
        self.0 = v.to_le_bytes();
    }
}

impl PodI128 {
    /// Zero.
    pub const ZERO: Self = Self([0; 16]);

    /// Wraps a native value.
    pub const fn new(v: i128) -> Self {
        Self(v.to_le_bytes())
    }

    /// The native value.
    pub const fn get(&self) -> i128 {
        i128::from_le_bytes(self.0)
    }

    /// Stores a native value.
    pub fn set(&mut self, v: i128) {
        self.0 = v.to_le_bytes();
    }
}

impl From<u128> for PodU128 {
    fn from(v: u128) -> Self {
        Self::new(v)
    }
}

impl From<PodU128> for u128 {
    fn from(v: PodU128) -> Self {
        v.get()
    }
}

impl From<i128> for PodI128 {
    fn from(v: i128) -> Self {
        Self::new(v)
    }
}

impl From<PodI128> for i128 {
    fn from(v: PodI128) -> Self {
        v.get()
    }
}

impl core::fmt::Debug for PodU128 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.get())
    }
}

impl core::fmt::Debug for PodI128 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.get())
    }
}

/// Converts a `[PodU128; N]` to native values.
pub fn get_all<const N: usize>(a: &[PodU128; N]) -> [u128; N] {
    let mut out = [0u128; N];
    for (o, v) in out.iter_mut().zip(a.iter()) {
        *o = v.get();
    }
    out
}

/// Stores native values into a `[PodU128; N]`.
pub fn set_all<const N: usize>(a: &mut [PodU128; N], v: &[u128; N]) {
    for (o, x) in a.iter_mut().zip(v.iter()) {
        o.set(*x);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_alignment() {
        for v in [0u128, 1, u64::MAX as u128 + 1, u128::MAX] {
            assert_eq!(PodU128::new(v).get(), v);
        }
        for v in [0i128, -1, i128::MIN, i128::MAX] {
            assert_eq!(PodI128::new(v).get(), v);
        }
        assert_eq!(core::mem::align_of::<PodU128>(), 1);
        assert_eq!(core::mem::align_of::<PodI128>(), 1);
        assert_eq!(core::mem::size_of::<PodU128>(), 16);
        assert_eq!(PodU128::new(0x0102).0[..2], [2, 1]);
    }
}
