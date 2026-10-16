//! Small helpers for the ingest side of the Postgres boundary (INDEXER.md §4.1). Owner: INGEST.
//!
//! u64 / u128 / i128 columns are NUMERIC domains: bound as decimal text (`$n::numeric`) and read
//! as `col::text`; keys are base58 text, `Pubkey::default()` is NULL.

use anyhow::Context;
use solana_pubkey::Pubkey;
use sqlx::Row;
use sqlx::postgres::PgRow;

/// The canonical chain order of an event (§3.3): `(slot, tx_index, ix_index, ev_source,
/// ev_seq)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChainKey {
    pub slot: i64,
    pub tx_index: i32,
    pub ix_index: i16,
    pub ev_source: i16,
    pub ev_seq: i32,
}

/// Decimal text of an optional integer (the bind form of a nullable NUMERIC column).
pub fn opt_text<T: ToString>(v: Option<T>) -> Option<String> {
    v.map(|v| v.to_string())
}

/// Base58 of a key, NULL for `Pubkey::default()`.
pub fn key_text(k: &Pubkey) -> Option<String> {
    crate::numeric::key_or_null(k)
}

/// Base58 of an optional key.
pub fn opt_key(k: Option<Pubkey>) -> Option<String> {
    k.as_ref().and_then(key_text)
}

/// `i64` of a u64 slot (slots fit).
pub fn slot_i64(slot: u64) -> i64 {
    i64::try_from(slot).unwrap_or(i64::MAX)
}

/// A `col::text` NUMERIC column as u64 (NULL → 0).
pub fn get_u64(row: &PgRow, col: &str) -> anyhow::Result<u64> {
    let s: Option<String> = row.try_get(col).with_context(|| col.to_string())?;
    s.map_or(Ok(0), |s| crate::numeric::parse_u64(&s))
}

/// A nullable `col::text` NUMERIC column as u64.
pub fn get_opt_u64(row: &PgRow, col: &str) -> anyhow::Result<Option<u64>> {
    let s: Option<String> = row.try_get(col).with_context(|| col.to_string())?;
    s.map(|s| crate::numeric::parse_u64(&s)).transpose()
}

/// A `col::text` NUMERIC column as u128 (NULL → 0).
pub fn get_u128(row: &PgRow, col: &str) -> anyhow::Result<u128> {
    let s: Option<String> = row.try_get(col).with_context(|| col.to_string())?;
    s.map_or(Ok(0), |s| crate::numeric::parse_u128(&s))
}

/// A nullable `col::text` NUMERIC column as u128.
pub fn get_opt_u128(row: &PgRow, col: &str) -> anyhow::Result<Option<u128>> {
    let s: Option<String> = row.try_get(col).with_context(|| col.to_string())?;
    s.map(|s| crate::numeric::parse_u128(&s)).transpose()
}

/// A nullable pubkey column.
pub fn get_opt_key(row: &PgRow, col: &str) -> anyhow::Result<Option<Pubkey>> {
    let s: Option<String> = row.try_get(col).with_context(|| col.to_string())?;
    s.map(|s| crate::numeric::parse_key(&s)).transpose()
}

/// A pubkey column (NULL → `Pubkey::default()`).
pub fn get_key(row: &PgRow, col: &str) -> anyhow::Result<Pubkey> {
    Ok(get_opt_key(row, col)?.unwrap_or_default())
}

/// A SMALLINT column holding a u8.
pub fn get_u8(row: &PgRow, col: &str) -> anyhow::Result<u8> {
    let v: i16 = row.try_get(col).with_context(|| col.to_string())?;
    Ok(u8::try_from(v)?)
}

/// A BIGINT column holding a u32.
pub fn get_u32(row: &PgRow, col: &str) -> anyhow::Result<u32> {
    let v: i64 = row.try_get(col).with_context(|| col.to_string())?;
    Ok(u32::try_from(v)?)
}

/// u8 → the `u8` domain's SMALLINT.
pub fn i16_of(v: u8) -> i16 {
    i16::from(v)
}

/// u32 → the `u32` domain's BIGINT.
pub fn i64_of(v: u32) -> i64 {
    i64::from(v)
}

/// usize → INTEGER (instruction and line indexes).
pub fn i32_of(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// usize → SMALLINT (top-level instruction indexes).
pub fn i16_idx(v: usize) -> i16 {
    i16::try_from(v).unwrap_or(i16::MAX)
}
