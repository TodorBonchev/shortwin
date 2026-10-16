//! Request parameters, parsed and validated before any SQL (INDEXER.md §10.2). Owner: API.
//!
//! Keys and signatures by base58 decoding, enums by allowlist, integers by range, strings by
//! length; a repeated parameter is a `bad_request`. Unknown parameters are ignored (cache
//! busters). Every failure is a problem+json `400` with the code of its class.

use std::str::FromStr;

use axum::extract::rejection::PathRejection;
use axum::extract::{FromRequestParts, Path, Query};
use axum::http::request::Parts;
use solana_pubkey::Pubkey;
use solana_signature::Signature;

use super::error::ApiError;
use super::pagination::MAX_CURSOR_LEN;

/// The query string as decoded pairs.
#[derive(Clone, Debug, Default)]
pub struct Params(Vec<(String, String)>);

impl<S: Send + Sync> FromRequestParts<S> for Params {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let Query(pairs) = Query::<Vec<(String, String)>>::try_from_uri(&parts.uri)
            .map_err(|_| ApiError::bad_request("the query string is not valid"))?;
        Ok(Self(pairs))
    }
}

/// The canonical base58 form of a public key, if `s` is one.
pub fn canonical_key(s: &str) -> Option<String> {
    if !(32..=44).contains(&s.len()) {
        return None;
    }
    Pubkey::from_str(s).ok().map(|k| k.to_string())
}

/// The canonical base58 form of a transaction signature, if `s` is one.
pub fn canonical_signature(s: &str) -> Option<String> {
    if !(64..=88).contains(&s.len()) {
        return None;
    }
    Signature::from_str(s).ok().map(|k| k.to_string())
}

/// A path parameter that must be a public key.
pub fn path_key(p: Result<Path<String>, PathRejection>, name: &str) -> Result<String, ApiError> {
    let Path(raw) = p.map_err(|_| ApiError::invalid_pubkey(name))?;
    canonical_key(&raw).ok_or_else(|| ApiError::invalid_pubkey(name))
}

/// A path parameter that must be a transaction signature.
pub fn path_signature(
    p: Result<Path<String>, PathRejection>,
    name: &str,
) -> Result<String, ApiError> {
    let Path(raw) = p.map_err(|_| ApiError::invalid_signature(name))?;
    canonical_signature(&raw).ok_or_else(|| ApiError::invalid_signature(name))
}

/// `%`, `_` and `\` escaped for `LIKE … ESCAPE '\'`, then the prefix wildcard.
pub fn like_prefix(q: &str) -> String {
    let mut s = String::with_capacity(q.len() + 2);
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('%');
    s
}

impl Params {
    /// The parameters as given (tests).
    pub fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        Self(pairs)
    }

    /// The single value of `name`.
    pub fn raw(&self, name: &str) -> Result<Option<&str>, ApiError> {
        let mut it = self.0.iter().filter(|(k, _)| k == name);
        let first = it.next().map(|(_, v)| v.as_str());
        if it.next().is_some() {
            return Err(ApiError::bad_request(format!(
                "`{name}` is given more than once"
            )));
        }
        Ok(first)
    }

    /// Whether the query has `name` at all.
    pub fn has(&self, name: &str) -> bool {
        self.0.iter().any(|(k, _)| k == name)
    }

    /// A string of `min..=max` characters.
    pub fn string(&self, name: &str, min: usize, max: usize) -> Result<Option<&str>, ApiError> {
        match self.raw(name)? {
            None => Ok(None),
            Some(v) => {
                let n = v.chars().count();
                if n < min || n > max {
                    return Err(ApiError::bad_request(format!(
                        "`{name}` must be {min} to {max} characters"
                    )));
                }
                Ok(Some(v))
            }
        }
    }

    /// A required parameter.
    pub fn required<T>(name: &str, v: Option<T>) -> Result<T, ApiError> {
        v.ok_or_else(|| ApiError::bad_request(format!("`{name}` is required")))
    }

    /// `true` / `false`.
    pub fn boolean(&self, name: &str) -> Result<Option<bool>, ApiError> {
        match self.raw(name)? {
            None => Ok(None),
            Some("true") => Ok(Some(true)),
            Some("false") => Ok(Some(false)),
            Some(_) => Err(ApiError::bad_request(format!(
                "`{name}` must be `true` or `false`"
            ))),
        }
    }

    /// An integer within `[min, max]`.
    pub fn int(&self, name: &str, min: i64, max: i64) -> Result<Option<i64>, ApiError> {
        match self.raw(name)? {
            None => Ok(None),
            Some(v) => v
                .parse::<i64>()
                .ok()
                .filter(|x| (min..=max).contains(x))
                .map(Some)
                .ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "`{name}` must be an integer from {min} to {max}"
                    ))
                }),
        }
    }

    /// One of `allowed` (the allowlisted value is returned, never the request's text).
    pub fn choice(
        &self,
        name: &str,
        allowed: &[&'static str],
    ) -> Result<Option<&'static str>, ApiError> {
        match self.raw(name)? {
            None => Ok(None),
            Some(v) => allowed
                .iter()
                .find(|a| **a == v)
                .copied()
                .map(Some)
                .ok_or_else(|| {
                    ApiError::bad_request(format!("`{name}` must be one of {}", allowed.join(", ")))
                }),
        }
    }

    /// A public key.
    pub fn pubkey(&self, name: &str) -> Result<Option<String>, ApiError> {
        match self.raw(name)? {
            None => Ok(None),
            Some(v) => canonical_key(v)
                .map(Some)
                .ok_or_else(|| ApiError::invalid_pubkey(name)),
        }
    }

    /// `limit` (§9.1): 1..=`max`, default `default`.
    pub fn limit(&self, default: u32, max: u32) -> Result<i64, ApiError> {
        match self.raw("limit")? {
            None => Ok(i64::from(default.clamp(1, max.max(1)))),
            Some(v) => v
                .parse::<i64>()
                .ok()
                .filter(|x| (1..=i64::from(max)).contains(x))
                .ok_or_else(|| ApiError::limit_out_of_range(max)),
        }
    }

    /// `cursor` (≤ 256 characters; decoded by the endpoint).
    pub fn cursor(&self) -> Result<Option<&str>, ApiError> {
        match self.raw("cursor")? {
            Some(c) if c.is_empty() || c.len() > MAX_CURSOR_LEN => Err(ApiError::invalid_cursor()),
            c => Ok(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pairs: &[(&str, &str)]) -> Params {
        Params::from_pairs(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn values_are_validated() {
        let q = p(&[("limit", "0"), ("b", "yes"), ("n", "12"), ("e", "asc")]);
        assert_eq!(
            q.limit(100, 500).unwrap_err().code,
            super::super::error::Code::LimitOutOfRange
        );
        assert!(q.boolean("b").is_err());
        assert_eq!(q.int("n", 0, 20).unwrap(), Some(12));
        assert!(q.int("n", 0, 5).is_err());
        assert_eq!(q.choice("e", &["asc", "desc"]).unwrap(), Some("asc"));
        assert!(q.choice("e", &["desc"]).is_err());
        assert_eq!(p(&[]).limit(100, 500).unwrap(), 100);
        assert!(p(&[("x", "1"), ("x", "2")]).raw("x").is_err());
        assert!(p(&[("cursor", &"a".repeat(257))]).cursor().is_err());
    }

    #[test]
    fn keys_and_signatures_are_canonical_base58() {
        let k = Pubkey::new_from_array([3; 32]).to_string();
        assert_eq!(canonical_key(&k), Some(k.clone()));
        assert_eq!(canonical_key("0OIl"), None);
        assert_eq!(canonical_key(&format!("1{k}")), None);
        let s = Signature::from([4u8; 64]).to_string();
        assert_eq!(canonical_signature(&s), Some(s.clone()));
        assert_eq!(canonical_signature(&k), None);
        assert_eq!(like_prefix("a_b%c\\"), "a\\_b\\%c\\\\%");
    }
}
