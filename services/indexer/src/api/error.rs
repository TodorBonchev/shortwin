//! Errors as RFC 9457 `application/problem+json` (INDEXER.md §9.1; `openapi.yaml`
//! `components.schemas.Problem`). Owner: API.
//!
//! Every error body is `{type, title, status, detail, code}` with `code` one of the
//! `openapi.yaml` `Problem.code` values. 429 and 503 carry `Retry-After` (§10.1). Database
//! failures map to `503 unavailable` (unreachable, pool exhausted), `503 timeout` (the statement
//! timeout, SQLSTATE 57014) or `500 internal` (anything else; logged, never echoed).

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// `Problem.code` (`openapi.yaml`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    BadRequest,
    InvalidPubkey,
    InvalidSignature,
    InvalidCursor,
    LimitOutOfRange,
    RangeTooLarge,
    NotFound,
    RateLimited,
    Timeout,
    Unavailable,
    Internal,
}

impl Code {
    /// The wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::InvalidPubkey => "invalid_pubkey",
            Self::InvalidSignature => "invalid_signature",
            Self::InvalidCursor => "invalid_cursor",
            Self::LimitOutOfRange => "limit_out_of_range",
            Self::RangeTooLarge => "range_too_large",
            Self::NotFound => "not_found",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::BadRequest => "Bad request",
            Self::InvalidPubkey => "Invalid public key",
            Self::InvalidSignature => "Invalid signature",
            Self::InvalidCursor => "Invalid cursor",
            Self::LimitOutOfRange => "Limit out of range",
            Self::RangeTooLarge => "Range too large",
            Self::NotFound => "Not found",
            Self::RateLimited => "Too many requests",
            Self::Timeout => "Query timed out",
            Self::Unavailable => "Service unavailable",
            Self::Internal => "Internal error",
        }
    }
}

/// An API error: rendered as `application/problem+json`.
#[derive(Clone, Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: Code,
    pub detail: String,
    /// `Retry-After` seconds (429, 503).
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: Code, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
            retry_after: None,
        }
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, Code::BadRequest, detail)
    }

    pub fn invalid_pubkey(param: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            Code::InvalidPubkey,
            format!("`{param}` is not a base58 public key"),
        )
    }

    pub fn invalid_signature(param: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            Code::InvalidSignature,
            format!("`{param}` is not a base58 transaction signature"),
        )
    }

    pub fn invalid_cursor() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            Code::InvalidCursor,
            "`cursor` is not a cursor this endpoint issued for these parameters",
        )
    }

    pub fn limit_out_of_range(max: u32) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            Code::LimitOutOfRange,
            format!("`limit` must be an integer from 1 to {max}"),
        )
    }

    pub fn range_too_large(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, Code::RangeTooLarge, detail)
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, Code::NotFound, detail)
    }

    pub fn rate_limited(retry_after: u64, detail: impl Into<String>) -> Self {
        Self {
            retry_after: Some(retry_after.max(1)),
            ..Self::new(StatusCode::TOO_MANY_REQUESTS, Code::RateLimited, detail)
        }
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            retry_after: Some(1),
            ..Self::new(StatusCode::SERVICE_UNAVAILABLE, Code::Unavailable, detail)
        }
    }

    pub fn timeout(detail: impl Into<String>) -> Self {
        Self {
            retry_after: Some(1),
            ..Self::new(StatusCode::SERVICE_UNAVAILABLE, Code::Timeout, detail)
        }
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            Code::Internal,
            "internal error",
        )
    }

    /// The problem document.
    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({
            "type": format!("https://short.win/problems/{}", self.code.as_str()),
            "title": self.code.title(),
            "status": self.status.as_u16(),
            "detail": self.detail,
            "code": self.code.as_str(),
        })
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.status,
            self.code.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = self.body().to_string();
        let mut resp = (self.status, body).into_response();
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Some(s) = self.retry_after {
            h.insert(header::RETRY_AFTER, HeaderValue::from(s));
        }
        resp
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            // query_canceled: the per-statement timeout (§10.2).
            sqlx::Error::Database(db) if db.code().as_deref() == Some("57014") => {
                Self::timeout("the query exceeded the statement timeout")
            }
            sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed
            | sqlx::Error::Io(_)
            | sqlx::Error::Tls(_) => {
                tracing::warn!(error = %e, "database unavailable");
                Self::unavailable("the database is unavailable")
            }
            // SQLSTATE classes 08 (connection exception) and 57 (operator intervention: admin
            // shutdown, cannot connect now).
            sqlx::Error::Database(db)
                if db
                    .code()
                    .is_some_and(|c| c.starts_with("08") || c.starts_with("57")) =>
            {
                tracing::warn!(error = %e, "database unavailable");
                Self::unavailable("the database is unavailable")
            }
            _ => {
                tracing::error!(error = %e, "database error");
                Self::internal()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_bodies_carry_the_contract_fields() {
        let e = ApiError::rate_limited(0, "slow down");
        let b = e.body();
        assert_eq!(b["status"], 429);
        assert_eq!(b["code"], "rate_limited");
        assert_eq!(b["type"], "https://short.win/problems/rate_limited");
        assert_eq!(e.retry_after, Some(1));
        let r = ApiError::invalid_pubkey("mint").into_response();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
    }

    #[test]
    fn pool_failures_are_unavailable() {
        let e = ApiError::from(sqlx::Error::PoolClosed);
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(e.code, Code::Unavailable);
        assert_eq!(e.retry_after, Some(1));
    }
}
