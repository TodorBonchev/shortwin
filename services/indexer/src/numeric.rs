//! Exact integers and keys across the Rust ↔ Postgres ↔ JSON boundaries (INDEXER.md §4.1, §4.2).
//! FOUNDATION.
//!
//! - Postgres: u64 / u128 / i128 columns are NUMERIC domains; bind `to_text(v)` as
//!   `$n::numeric` and read `col::text` back through `parse_*`.
//! - JSON (`events.data`, the API): integers wider than 32 bits are decimal strings, so no
//!   JavaScript client loses precision (2^53).
//! - `Pubkey::default()` is stored and served as `null` ([`key_or_null`]).

use anyhow::{Context, anyhow};
use shortwin_math::Rounding;
use solana_pubkey::Pubkey;

/// The program's WAD (prices are lamports per raw token unit × 10^18).
pub const WAD: u128 = 1_000_000_000_000_000_000;

/// Decimal text of an integer (the bind form of a NUMERIC domain).
pub fn to_text<T: ToString>(v: T) -> String {
    v.to_string()
}

/// Parses a NUMERIC domain read as `col::text`.
pub fn parse_u64(s: &str) -> anyhow::Result<u64> {
    s.parse().with_context(|| format!("not a u64: {s}"))
}

/// Parses a NUMERIC domain read as `col::text`.
pub fn parse_u128(s: &str) -> anyhow::Result<u128> {
    s.parse().with_context(|| format!("not a u128: {s}"))
}

/// Parses a NUMERIC domain read as `col::text`.
pub fn parse_i128(s: &str) -> anyhow::Result<i128> {
    s.parse().with_context(|| format!("not an i128: {s}"))
}

/// `⌊a·p / WAD⌋` on a 256-bit intermediate: the mark value of `a` raw token units at the WAD
/// price `p` (OI, TVL, the settlement's token value; §8). `None` if the result exceeds u128.
pub fn mul_wad_floor(a: u128, p: u128) -> Option<u128> {
    shortwin_math::num::mul_div(a, p, WAD, Rounding::Floor).ok()
}

/// Utilisation in basis points exactly as `shortwin_math::rent::utilization_bps` (and the SQL
/// `sw_util_bps`): `⌈borrowed·10⁴ / (free + borrowed)⌉`, 0 for an empty pool.
pub fn util_bps(t_free: u64, borrowed: u64) -> u32 {
    shortwin_math::rent::utilization_bps(t_free, borrowed).unwrap_or(0)
}

/// Base58 of `key`, or `None` for `Pubkey::default()` (the program's "none").
pub fn key_or_null(key: &Pubkey) -> Option<String> {
    (*key != Pubkey::default()).then(|| key.to_string())
}

/// Parses a base58 key.
pub fn parse_key(s: &str) -> anyhow::Result<Pubkey> {
    s.parse().map_err(|e| anyhow!("not a pubkey: {s} ({e:?})"))
}

/// Lowercase hex of bytes (`[u8; 32]` hashes and feed ids in JSON and TEXT columns).
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_value_is_floored_and_wide() {
        assert_eq!(mul_wad_floor(3, WAD / 2), Some(1));
        // q·P beyond u128 (a pumped price) still divides exactly.
        let q = u128::from(u64::MAX);
        let p = 1_000 * WAD;
        assert_eq!(mul_wad_floor(q, p), Some(q * 1_000));
    }

    #[test]
    fn utilisation_matches_the_program() {
        assert_eq!(util_bps(0, 0), 0);
        assert_eq!(util_bps(2, 1), 3_334);
        assert_eq!(util_bps(0, 5), 10_000);
    }

    #[test]
    fn default_key_is_null_and_hex_is_lowercase() {
        assert_eq!(key_or_null(&Pubkey::default()), None);
        assert_eq!(hex(&[0xab, 0x01]), "ab01");
        assert_eq!(parse_u128(&to_text(u128::MAX)).unwrap(), u128::MAX);
        assert_eq!(parse_i128(&to_text(i128::MIN)).unwrap(), i128::MIN);
    }
}
