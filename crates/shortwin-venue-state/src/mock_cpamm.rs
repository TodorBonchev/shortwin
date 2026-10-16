//! TEST-ONLY: the mock constant-product venue `mock_cpamm` (`tests/mock_cpamm`, ARCH §8.2, §9
//! M2, A49). Compiled only with this crate's `test-venue` feature, which short.win's own
//! `test-venue` feature enables and release builds never do.
//!
//! Reader contract (`tests/mock_cpamm/INTERFACE.md` "Reader contract"): owner `BPAZFPkm…`,
//! discriminator `3ac4d0f11fd8f330`, length-tolerant reads (ARCH §4.1), and
//! `layout_version == 0`, else the pool does not parse (a simulated venue-state layout change
//! is a reader failure, which the route logic reads as "not executable", A63, V1-8). The
//! program may read `disabled`, `fee_ppm`, the two version-0 reserve counters, both vaults,
//! both mints and the base token program; it must not read `over_delivery` or
//! `required_extras` (venue behaviour a real venue does not expose).
//!
//! Pricing reserves (as Raydium CPMM's `vault − counters`): `X = base_vault.amount −
//! protocol_fee_base`, `Y = quote_vault.amount − protocol_fee_quote`; the venue predicate is
//! "parses ∧ ¬disabled ∧ both vaults ≥ their counters ∧ X > 0 ∧ Y > 0" ([`reserves`]).

use crate::bytes::{has_discriminator, key_at, u8_at, u32_at, u64_at};
use crate::{Key, ReadError};

/// Program id `BPAZFPkmVtYEWEZHCzFUns7oybh1VWfAvmu3ofM2mQi7`.
pub const PROGRAM: Key = [
    154, 66, 115, 186, 213, 162, 91, 162, 241, 190, 88, 111, 112, 26, 147, 48, 162, 210, 107, 161,
    250, 61, 191, 12, 176, 240, 204, 109, 86, 111, 118, 124,
];

/// `sha256("account:MockPool")[..8]`.
pub const POOL_DISCRIMINATOR: [u8; 8] = [0x3a, 0xc4, 0xd0, 0xf1, 0x1f, 0xd8, 0xf3, 0x30];

/// `u8` layout version (only 0 parses).
pub const OFF_LAYOUT_VERSION: usize = 8;
/// `u8` bool: swaps fail while set.
pub const OFF_DISABLED: usize = 9;
/// `u32` fee on the input, ppm.
pub const OFF_FEE_PPM: usize = 10;
/// `u64` base-side reserve counter (layout 0).
pub const OFF_PROTOCOL_FEE_BASE: usize = 22;
/// `u64` quote-side reserve counter (layout 0).
pub const OFF_PROTOCOL_FEE_QUOTE: usize = 30;
/// Base vault key.
pub const OFF_BASE_VAULT: usize = 167;
/// Quote (WSOL) vault key.
pub const OFF_QUOTE_VAULT: usize = 199;
/// Base mint key.
pub const OFF_BASE_MINT: usize = 264;
/// Quote mint key (always WSOL).
pub const OFF_QUOTE_MINT: usize = 296;
/// Token program of the base mint.
pub const OFF_BASE_TOKEN_PROGRAM: usize = 328;
/// Smallest account the reader accepts: every field it reads.
pub const MIN_LEN: usize = OFF_BASE_TOKEN_PROGRAM + 32;

/// Protocol share of the trade fee the mock moves into the input side's counter, ppm
/// (INTERFACE.md "Swap arithmetic"; short.win's `CpmmFees.protocol`).
pub const PROTOCOL_FEE_RATE: u64 = 120_000;

/// The fields of a mock pool short.win may read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MockPool {
    pub disabled: bool,
    pub fee_ppm: u32,
    pub protocol_fee_base: u64,
    pub protocol_fee_quote: u64,
    pub base_vault: Key,
    pub quote_vault: Key,
    pub base_mint: Key,
    pub quote_mint: Key,
    pub base_token_program: Key,
}

/// Reads a mock pool (the reader contract above).
pub fn read(owner: &Key, data: &[u8]) -> Result<MockPool, ReadError> {
    if *owner != PROGRAM {
        return Err(ReadError::WrongOwner);
    }
    if !has_discriminator(data, &POOL_DISCRIMINATOR) {
        return Err(ReadError::WrongTag);
    }
    if data.len() < MIN_LEN {
        return Err(ReadError::TooShort);
    }
    if u8_at(data, OFF_LAYOUT_VERSION) != 0 {
        return Err(ReadError::Malformed);
    }
    let disabled = match u8_at(data, OFF_DISABLED) {
        0 => false,
        1 => true,
        _ => return Err(ReadError::Malformed),
    };
    Ok(MockPool {
        disabled,
        fee_ppm: u32_at(data, OFF_FEE_PPM),
        protocol_fee_base: u64_at(data, OFF_PROTOCOL_FEE_BASE),
        protocol_fee_quote: u64_at(data, OFF_PROTOCOL_FEE_QUOTE),
        base_vault: key_at(data, OFF_BASE_VAULT),
        quote_vault: key_at(data, OFF_QUOTE_VAULT),
        base_mint: key_at(data, OFF_BASE_MINT),
        quote_mint: key_at(data, OFF_QUOTE_MINT),
        base_token_program: key_at(data, OFF_BASE_TOKEN_PROGRAM),
    })
}

/// Pricing reserves `(X, Y)` = `(base_vault − protocol_fee_base, quote_vault −
/// protocol_fee_quote)` of a pool whose vaults hold `base_amount` and `quote_amount`, or
/// `None` when the venue cannot execute: disabled, a vault below its counter (the mock then
/// refuses every swap with `InsufficientVault`), or an empty side (INTERFACE.md "Pricing
/// reserves").
pub fn reserves(pool: &MockPool, base_amount: u64, quote_amount: u64) -> Option<(u64, u64)> {
    if pool.disabled {
        return None;
    }
    let x = base_amount.checked_sub(pool.protocol_fee_base)?;
    let y = quote_amount.checked_sub(pool.protocol_fee_quote)?;
    (x > 0 && y > 0).then_some((x, y))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    fn pool() -> [u8; 384] {
        let mut d = [0u8; 384];
        d[..8].copy_from_slice(&POOL_DISCRIMINATOR);
        d[OFF_FEE_PPM..OFF_FEE_PPM + 4].copy_from_slice(&2_500u32.to_le_bytes());
        d[OFF_PROTOCOL_FEE_BASE..OFF_PROTOCOL_FEE_BASE + 8].copy_from_slice(&7u64.to_le_bytes());
        d[OFF_PROTOCOL_FEE_QUOTE..OFF_PROTOCOL_FEE_QUOTE + 8].copy_from_slice(&9u64.to_le_bytes());
        d[OFF_BASE_VAULT..OFF_BASE_VAULT + 32].copy_from_slice(&[1; 32]);
        d[OFF_QUOTE_VAULT..OFF_QUOTE_VAULT + 32].copy_from_slice(&[2; 32]);
        d[OFF_BASE_MINT..OFF_BASE_MINT + 32].copy_from_slice(&[3; 32]);
        d[OFF_QUOTE_MINT..OFF_QUOTE_MINT + 32].copy_from_slice(&[4; 32]);
        d[OFF_BASE_TOKEN_PROGRAM..OFF_BASE_TOKEN_PROGRAM + 32].copy_from_slice(&[5; 32]);
        d
    }

    #[test]
    fn reads_the_contract_fields() {
        let p = read(&PROGRAM, &pool()).unwrap();
        assert!(!p.disabled);
        assert_eq!(p.fee_ppm, 2_500);
        assert_eq!((p.protocol_fee_base, p.protocol_fee_quote), (7, 9));
        assert_eq!((p.base_vault, p.quote_vault), ([1; 32], [2; 32]));
        assert_eq!((p.base_mint, p.quote_mint), ([3; 32], [4; 32]));
        assert_eq!(p.base_token_program, [5; 32]);
        assert_eq!(reserves(&p, 107, 1_009), Some((100, 1_000)));
        assert_eq!(reserves(&p, 7, 1_009), None);
        assert_eq!(reserves(&p, 6, 1_009), None);
        assert_eq!(reserves(&p, 107, 8), None);
    }

    #[test]
    fn a_layout_change_or_a_foreign_account_does_not_parse() {
        let d = pool();
        assert_eq!(read(&[0; 32], &d), Err(ReadError::WrongOwner));
        let mut v1 = d;
        v1[OFF_LAYOUT_VERSION] = 1;
        assert_eq!(read(&PROGRAM, &v1), Err(ReadError::Malformed));
        let mut disc = d;
        disc[0] ^= 1;
        assert_eq!(read(&PROGRAM, &disc), Err(ReadError::WrongTag));
        assert_eq!(read(&PROGRAM, &d[..MIN_LEN - 1]), Err(ReadError::TooShort));
        assert!(read(&PROGRAM, &d[..MIN_LEN]).is_ok());
        let mut dis = d;
        dis[OFF_DISABLED] = 1;
        let p = read(&PROGRAM, &dis).unwrap();
        assert!(p.disabled);
        assert_eq!(reserves(&p, 1_000, 1_000), None);
        dis[OFF_DISABLED] = 2;
        assert_eq!(read(&PROGRAM, &dis), Err(ReadError::Malformed));
    }
}
