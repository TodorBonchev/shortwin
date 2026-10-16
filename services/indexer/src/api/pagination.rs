//! Cursor pagination (INDEXER.md §9.1). Owner: API.
//!
//! `?limit=` (1..=`max_page_limit`, default `default_page_limit`) and `?cursor=` (opaque:
//! URL-safe base64 of the last row's sort key, e.g. `(slot, tx_index, ix_index, ev_source,
//! ev_seq)` for event lists); responses are `{data: [...], next_cursor: string | null}`. Keyset
//! pagination only (no OFFSET), so a page costs the same at any depth.
//!
//! A cursor is `base64url(JSON [tag, key…])` without padding. The tag names the endpoint family
//! (and, for `/v1/markets`, the sort and order), so a cursor is only accepted where it was issued;
//! every key part is type- and range-checked before it reaches SQL (`invalid_cursor` otherwise).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use super::error::ApiError;

/// `openapi.yaml` `components.parameters.Cursor.maxLength`.
pub const MAX_CURSOR_LEN: usize = 256;

/// A decoded cursor: its key parts (the tag stripped).
#[derive(Clone, Debug, PartialEq)]
pub struct Cursor(Vec<Value>);

impl Cursor {
    /// Encodes `[tag, parts…]`.
    pub fn encode(tag: &str, parts: Vec<Value>) -> String {
        let mut all = Vec::with_capacity(parts.len() + 1);
        all.push(Value::String(tag.to_string()));
        all.extend(parts);
        URL_SAFE_NO_PAD.encode(Value::Array(all).to_string())
    }

    /// Decodes a cursor issued with `tag` and `arity` key parts.
    pub fn decode(raw: &str, tag: &str, arity: usize) -> Result<Self, ApiError> {
        if raw.is_empty() || raw.len() > MAX_CURSOR_LEN {
            return Err(ApiError::invalid_cursor());
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| ApiError::invalid_cursor())?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| ApiError::invalid_cursor())?;
        let Value::Array(mut parts) = v else {
            return Err(ApiError::invalid_cursor());
        };
        if parts.len() != arity + 1 || parts[0].as_str() != Some(tag) {
            return Err(ApiError::invalid_cursor());
        }
        parts.remove(0);
        Ok(Self(parts))
    }

    fn get(&self, i: usize) -> Result<&Value, ApiError> {
        self.0.get(i).ok_or_else(ApiError::invalid_cursor)
    }

    /// An integer part within `[min, max]`.
    pub fn int(&self, i: usize, min: i64, max: i64) -> Result<i64, ApiError> {
        self.get(i)?
            .as_i64()
            .filter(|v| (min..=max).contains(v))
            .ok_or_else(ApiError::invalid_cursor)
    }

    /// A decimal-string part (a NUMERIC sort value: digits with an optional leading `-`).
    pub fn decimal(&self, i: usize) -> Result<String, ApiError> {
        let s = self.get(i)?.as_str().ok_or_else(ApiError::invalid_cursor)?;
        let digits = s.strip_prefix('-').unwrap_or(s);
        if digits.is_empty() || digits.len() > 40 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ApiError::invalid_cursor());
        }
        Ok(s.to_string())
    }

    /// A base58 public-key part.
    pub fn key(&self, i: usize) -> Result<String, ApiError> {
        let s = self.get(i)?.as_str().ok_or_else(ApiError::invalid_cursor)?;
        super::params::canonical_key(s).ok_or_else(ApiError::invalid_cursor)
    }
}

/// The chain-order key `(slot, tx_index, ix_index, ev_source, ev_seq)` (INDEXER.md §3.3) of
/// event-like rows: events, snapshots, trades, settlements, config changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainKey {
    pub slot: i64,
    pub tx_index: i32,
    pub ix_index: i16,
    pub ev_source: i16,
    pub ev_seq: i32,
}

impl ChainKey {
    /// The cursor of this key under `tag`.
    pub fn cursor(&self, tag: &str) -> String {
        Cursor::encode(
            tag,
            vec![
                json!(self.slot),
                json!(self.tx_index),
                json!(self.ix_index),
                json!(self.ev_source),
                json!(self.ev_seq),
            ],
        )
    }

    /// Decodes a cursor issued by [`ChainKey::cursor`] under `tag`.
    pub fn decode(raw: &str, tag: &str) -> Result<Self, ApiError> {
        let c = Cursor::decode(raw, tag, 5)?;
        Ok(Self {
            slot: c.int(0, 0, i64::MAX)?,
            tx_index: i32::try_from(c.int(1, 0, i64::from(i32::MAX))?)
                .map_err(|_| ApiError::invalid_cursor())?,
            ix_index: i16::try_from(c.int(2, i64::from(i16::MIN), i64::from(i16::MAX))?)
                .map_err(|_| ApiError::invalid_cursor())?,
            ev_source: i16::try_from(c.int(3, 0, 2)?).map_err(|_| ApiError::invalid_cursor())?,
            ev_seq: i32::try_from(c.int(4, i64::from(i32::MIN), i64::from(i32::MAX))?)
                .map_err(|_| ApiError::invalid_cursor())?,
        })
    }

    /// Decodes an optional cursor.
    pub fn decode_opt(raw: Option<&str>, tag: &str) -> Result<Option<Self>, ApiError> {
        raw.map(|r| Self::decode(r, tag)).transpose()
    }
}

/// A chain-key tuple as read from a row.
pub type ChainRow = (i64, i32, i16, i16, i32);

impl From<ChainRow> for ChainKey {
    fn from((slot, tx_index, ix_index, ev_source, ev_seq): ChainRow) -> Self {
        Self {
            slot,
            tx_index,
            ix_index,
            ev_source,
            ev_seq,
        }
    }
}

/// `{data, next_cursor}` from up to `limit + 1` fetched rows: the extra row, when present, says
/// there is a next page, whose cursor is the last returned row's key.
pub fn page<K>(mut rows: Vec<(Value, K)>, limit: i64, cursor_of: impl Fn(&K) -> String) -> Value {
    let limit = usize::try_from(limit).unwrap_or(0);
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        rows.last().map(|(_, k)| cursor_of(k))
    } else {
        None
    };
    let data: Vec<Value> = rows.into_iter().map(|(v, _)| v).collect();
    json!({ "data": data, "next_cursor": next })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_cursors_round_trip_and_reject_other_tags() {
        let k = ChainKey {
            slot: 1_234,
            tx_index: 7,
            ix_index: 2,
            ev_source: 1,
            ev_seq: 40,
        };
        let c = k.cursor("ev");
        assert!(c.len() <= MAX_CURSOR_LEN);
        assert!(!c.contains('=') && !c.contains('+') && !c.contains('/'));
        assert_eq!(ChainKey::decode(&c, "ev").unwrap(), k);
        assert!(ChainKey::decode(&c, "tr").is_err());
        assert!(ChainKey::decode("not base64!", "ev").is_err());
        assert!(ChainKey::decode(&URL_SAFE_NO_PAD.encode("{}"), "ev").is_err());
        // Out-of-range parts never reach SQL.
        let bad = Cursor::encode(
            "ev",
            vec![json!(-1), json!(0), json!(0), json!(0), json!(0)],
        );
        assert!(ChainKey::decode(&bad, "ev").is_err());
        let bad = Cursor::encode("ev", vec![json!(1), json!(0), json!(0), json!(9), json!(0)]);
        assert!(ChainKey::decode(&bad, "ev").is_err());
    }

    #[test]
    fn decimal_and_key_parts_are_validated() {
        let key = solana_pubkey::Pubkey::new_from_array([7; 32]).to_string();
        let c = Cursor::encode("lp", vec![json!("123"), json!(key)]);
        let d = Cursor::decode(&c, "lp", 2).unwrap();
        assert_eq!(d.decimal(0).unwrap(), "123");
        assert_eq!(d.key(1).unwrap(), key);
        let c = Cursor::encode("lp", vec![json!("1e9"), json!("0OIl")]);
        let d = Cursor::decode(&c, "lp", 2).unwrap();
        assert!(d.decimal(0).is_err());
        assert!(d.key(1).is_err());
    }

    #[test]
    fn pages_cut_at_the_limit() {
        let rows: Vec<(Value, i64)> = (0..3).map(|i| (json!(i), i)).collect();
        let p = page(rows.clone(), 2, |k| format!("c{k}"));
        assert_eq!(p["data"], json!([0, 1]));
        assert_eq!(p["next_cursor"], "c1");
        let p = page(rows, 3, |k| format!("c{k}"));
        assert_eq!(p["next_cursor"], Value::Null);
    }
}
