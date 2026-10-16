//! The HTTP API (INDEXER.md §9, §10; `openapi.yaml`). Owner: API.
//!
//! axum 0.8: REST under `/v1` exactly as `openapi.yaml` specifies (paths, parameters, schemas,
//! problem+json errors, cursor pagination), the WebSocket at `/v1/ws` (§9.3), rate and abuse
//! limits (§10). Read-only: the API never writes chain-derived tables. The cross-track contract
//! is [`router`] and [`serve`] (called by `main.rs` and the contract tests).
//!
//! Layers, outermost first: request id (`x-request-id`), tracing, CORS, a 4 KiB body limit, then
//! the API's own edge middleware, which for every request (unknown paths included):
//! 1. identifies the client ([`ratelimit::Client`]: address behind `trusted_proxies`, `X-Api-Key`
//!    — an uncached key is looked up only when the client's anonymous bucket pays for it, under a
//!    query permit, and never on the exempt probes, M8-SEC-9; an `X-Forwarded-For` from a peer
//!    that is not a trusted proxy is logged once, M8-SEC-8),
//! 2. charges the GCRA bucket with the route's cost — `429 rate_limited` + `Retry-After` when it
//!    does not fit (`/v1/health` and `/v1/status`, the operational probes, are exempt),
//! 3. takes a query permit (`max_concurrent_queries`; `503 unavailable` after waiting 1 s) and
//!    bounds the handler by `request_timeout_ms` (`503 timeout`),
//! 4. adds `RateLimit-Limit/-Remaining/-Reset`, `X-Indexer-Slot`, `X-Indexer-Finalized-Slot`
//!    and a default `Cache-Control: no-store` (handlers set `public, max-age=2` on lists and
//!    `max-age=3600` on a finalized transaction).
//!
//! The pool [`serve`] gives the handlers is [`api_pool`]: every connection runs with
//! `statement_timeout` = `statement_timeout_ms` and `default_transaction_read_only = on`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::IndexerConfig;
use error::ApiError;
use ratelimit::{Client, Decision, Tier};

#[macro_use]
mod sql;
pub mod error;
pub mod pagination;
pub mod params;
pub mod ratelimit;
pub mod routes;
pub mod ws;

/// `[api]` (§10).
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    /// Listen address.
    pub listen: String,
    /// CORS origins allowed (`["*"]` for any).
    pub cors_origins: Vec<String>,
    /// Proxies whose `X-Forwarded-For` is trusted for the client address (CIDRs or addresses).
    pub trusted_proxies: Vec<String>,
    /// Anonymous clients: sustained requests per second and burst, per client address.
    pub anon_rps: u32,
    pub anon_burst: u32,
    /// `partner` API keys (`api_keys`, §10.1).
    pub key_rps: u32,
    pub key_burst: u32,
    /// IPv6 clients are bucketed by this prefix (64: one subscriber's allocation; 56 or 48 makes
    /// one ISP allocation one client, M8-SEC-8).
    pub ipv6_bucket_prefix: u8,
    /// WebSocket limits (§9.3, §10.1).
    pub ws_max_conns_per_ip: u32,
    /// WebSocket connections per process, all clients together (M8-SEC-8).
    pub ws_max_conns_total: u32,
    pub ws_max_subscriptions: u32,
    pub ws_outbound_buffer: usize,
    pub ws_max_inbound_per_s: u32,
    /// WebSocket `heartbeat` period (§9.3: 20 s) and `stats` channel period (10 s).
    pub ws_heartbeat_s: u64,
    pub ws_stats_interval_s: u64,
    /// Pagination (§9.1).
    pub default_page_limit: u32,
    pub max_page_limit: u32,
    /// Per-request database statement timeout and request timeout.
    pub statement_timeout_ms: u64,
    pub request_timeout_ms: u64,
    /// Database queries in flight per process.
    pub max_concurrent_queries: u32,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8080".to_string(),
            cors_origins: vec!["*".to_string()],
            trusted_proxies: Vec::new(),
            anon_rps: 20,
            anon_burst: 40,
            key_rps: 100,
            key_burst: 200,
            ipv6_bucket_prefix: 64,
            ws_max_conns_per_ip: 5,
            ws_max_conns_total: 10_000,
            ws_max_subscriptions: 50,
            ws_outbound_buffer: 1_000,
            ws_max_inbound_per_s: 10,
            ws_heartbeat_s: 20,
            ws_stats_interval_s: 10,
            default_page_limit: 100,
            max_page_limit: 500,
            statement_timeout_ms: 2_000,
            request_timeout_ms: 5_000,
            max_concurrent_queries: 32,
        }
    }
}

/// The web's largest page request (a borrower's positions; FRONTEND.md §3.6), which it does not
/// retry with less: the floor of `max_page_limit` (INDEXER.md §9.1; the owner's decision of
/// 2026-10-09, FE-30).
pub const WEB_MIN_PAGE_LIMIT: u32 = 200;

impl ApiConfig {
    /// Rejects a configuration the API cannot honour (`serve` checks it at start).
    pub fn check(&self) -> anyhow::Result<()> {
        for p in &self.trusted_proxies {
            if ratelimit::Cidr::parse(p).is_none() {
                bail!("api.trusted_proxies: {p:?} is not an address or CIDR");
            }
        }
        for o in &self.cors_origins {
            if o != "*" && HeaderValue::from_str(o).is_err() {
                bail!("api.cors_origins: {o:?} is not a valid origin");
            }
        }
        if !(1..=500).contains(&self.max_page_limit)
            || !(1..=self.max_page_limit).contains(&self.default_page_limit)
        {
            bail!("api: need 1 <= default_page_limit <= max_page_limit <= 500 (openapi.yaml)");
        }
        if self.max_page_limit < WEB_MIN_PAGE_LIMIT {
            bail!(
                "api.max_page_limit must be at least {WEB_MIN_PAGE_LIMIT}: the web asks for up to \
                 {WEB_MIN_PAGE_LIMIT} a page and does not retry with less (INDEXER.md §9.1)"
            );
        }
        if self.max_concurrent_queries == 0 || self.ws_outbound_buffer == 0 {
            bail!("api: max_concurrent_queries and ws_outbound_buffer must be positive");
        }
        if !(32..=128).contains(&self.ipv6_bucket_prefix) {
            bail!("api.ipv6_bucket_prefix must be 32 to 128");
        }
        if self.ws_max_conns_total == 0 {
            bail!("api.ws_max_conns_total must be positive");
        }
        Ok(())
    }
}

/// The newest ingested slot and the finalized watermark, cached for the response headers.
#[derive(Default)]
struct Heads {
    at: Option<Instant>,
    slot: i64,
    finalized: i64,
}

const HEADS_TTL: Duration = Duration::from_millis(500);

/// How long `/v1/stats` (whole-protocol aggregates) is served from memory: the response says
/// `max-age=2` anyway, and N clients then cost one query per period (M8-SEC-3).
const STATS_TTL: Duration = Duration::from_secs(2);

/// Process-wide API state behind [`AppState`].
struct Shared {
    limiter: ratelimit::Limiter,
    keys: ratelimit::KeyCache,
    trusted: Vec<ratelimit::Cidr>,
    queries: Arc<tokio::sync::Semaphore>,
    heads: tokio::sync::Mutex<Heads>,
    ws_conns: ratelimit::ConnCounter,
    hub: tokio::sync::OnceCell<ws::Hub>,
    /// `/v1/stats`, cached for [`STATS_TTL`].
    stats: tokio::sync::Mutex<Option<(Instant, serde_json::Value)>>,
    /// An `X-Forwarded-For` from an untrusted peer was logged (once per process).
    xff_warned: std::sync::atomic::AtomicBool,
    /// Dropped with the last [`AppState`]: the WebSocket hub's tasks stop.
    shutdown: tokio::sync::watch::Sender<()>,
}

/// What every handler receives.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub cfg: Arc<ApiConfig>,
    shared: Arc<Shared>,
}

impl AppState {
    pub fn new(pool: PgPool, cfg: ApiConfig) -> Self {
        let trusted = cfg
            .trusted_proxies
            .iter()
            .filter_map(|p| {
                let c = ratelimit::Cidr::parse(p);
                if c.is_none() {
                    tracing::warn!(proxy = %p, "ignoring an invalid api.trusted_proxies entry");
                }
                c
            })
            .collect();
        let permits = usize::try_from(cfg.max_concurrent_queries.max(1)).unwrap_or(1);
        Self {
            pool,
            shared: Arc::new(Shared {
                limiter: ratelimit::Limiter::new(),
                keys: ratelimit::KeyCache::default(),
                trusted,
                queries: Arc::new(tokio::sync::Semaphore::new(permits)),
                heads: tokio::sync::Mutex::new(Heads::default()),
                ws_conns: ratelimit::ConnCounter::default(),
                hub: tokio::sync::OnceCell::new(),
                stats: tokio::sync::Mutex::new(None),
                xff_warned: std::sync::atomic::AtomicBool::new(false),
                shutdown: tokio::sync::watch::channel(()).0,
            }),
            cfg: Arc::new(cfg),
        }
    }

    /// `(X-Indexer-Slot, X-Indexer-Finalized-Slot)`: refreshed at most every 500 ms; the last
    /// known values (0 at first) when the database does not answer within a second.
    async fn heads(&self) -> (i64, i64) {
        let mut h = self.shared.heads.lock().await;
        if h.at.is_none_or(|t| t.elapsed() >= HEADS_TTL) {
            let q = sqlx::query_as::<_, (i64, i64)>(
                "SELECT GREATEST(s.confirmed_slot, COALESCE((SELECT max(slot) FROM ingest_txs), 0)), \
                 s.finalized_slot FROM ingest_state s",
            )
            .fetch_one(&self.pool);
            if let Ok(Ok((slot, finalized))) = tokio::time::timeout(Duration::from_secs(1), q).await
            {
                h.slot = slot;
                h.finalized = finalized;
            }
            h.at = Some(Instant::now());
        }
        (h.slot, h.finalized)
    }

    /// A query permit (`max_concurrent_queries`), waiting at most `wait`.
    async fn permit(&self, wait: Duration) -> Option<tokio::sync::OwnedSemaphorePermit> {
        tokio::time::timeout(wait, Arc::clone(&self.shared.queries).acquire_owned())
            .await
            .ok()
            .and_then(Result::ok)
    }

    /// The WebSocket hub, started at the first connection.
    async fn hub(&self) -> Result<&ws::Hub, ApiError> {
        self.shared
            .hub
            .get_or_try_init(|| {
                ws::Hub::start(
                    self.pool.clone(),
                    Arc::clone(&self.cfg),
                    self.shared.shutdown.subscribe(),
                )
            })
            .await
    }
}

/// The pool the handlers use: `pool`'s connection options plus `statement_timeout` and
/// `default_transaction_read_only = on` on every connection (§10.2), lazily connected.
pub fn api_pool(pool: &PgPool, cfg: &ApiConfig) -> PgPool {
    let opts = (*pool.connect_options()).clone().options([
        ("statement_timeout", cfg.statement_timeout_ms.to_string()),
        ("default_transaction_read_only", "on".to_string()),
    ]);
    PgPoolOptions::new()
        .max_connections(pool.options().get_max_connections().max(2))
        .acquire_timeout(Duration::from_secs(2))
        .connect_lazy_with(opts)
}

const EXPOSED: [&str; 7] = [
    "x-indexer-slot",
    "x-indexer-finalized-slot",
    "ratelimit-limit",
    "ratelimit-remaining",
    "ratelimit-reset",
    "retry-after",
    "x-request-id",
];

fn cors(cfg: &ApiConfig) -> CorsLayer {
    let base = CorsLayer::new()
        .allow_methods([Method::GET, Method::OPTIONS])
        .allow_headers([HeaderName::from_static("x-api-key"), header::CONTENT_TYPE])
        .expose_headers(EXPOSED.map(HeaderName::from_static))
        .max_age(Duration::from_secs(600));
    if cfg.cors_origins.iter().any(|o| o == "*") {
        base.allow_origin(Any)
    } else {
        base.allow_origin(AllowOrigin::list(
            cfg.cors_origins
                .iter()
                .filter_map(|o| HeaderValue::from_str(o).ok()),
        ))
    }
}

/// The application: every route of `openapi.yaml` and `/v1/ws`, with the middleware of §10.
pub fn router(state: AppState) -> Router {
    let cors = cors(&state.cfg);
    routes::routes()
        .fallback(no_route)
        .method_not_allowed_fallback(no_method)
        .layer(middleware::from_fn_with_state(state.clone(), edge))
        .layer(RequestBodyLimitLayer::new(4 * 1024))
        .layer(cors)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
}

async fn no_route() -> ApiError {
    ApiError::not_found("no such route (openapi.yaml lists the API's paths)")
}

async fn no_method() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        error::Code::BadRequest,
        "the API is read-only: GET only",
    )
}

/// §10: client identification, rate limit, query permit, timeout, common headers.
async fn edge(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let ip = ratelimit::client_ip(
        peer,
        req.headers()
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok()),
        &st.shared.trusted,
    );
    if peer.is_some_and(|p| !st.shared.trusted.iter().any(|c| c.contains(p)))
        && req.headers().contains_key("x-forwarded-for")
        && !st
            .shared
            .xff_warned
            .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        tracing::warn!(
            peer = ?peer,
            "X-Forwarded-For from a peer that is not in api.trusted_proxies: it is ignored and \
             every client behind that proxy shares its rate-limit bucket (add the load \
             balancer's address to [api] trusted_proxies)"
        );
    }
    let path = req.uri().path().to_string();
    // `/v1/health` and `/v1/status` are operational probes: not rate-limited (`openapi.yaml`
    // documents no 429 for them; they still take a query permit).
    let exempt = path == "/v1/health" || path == "/v1/status";
    let mut refused = None;
    let key = match req.headers().get("x-api-key").map(HeaderValue::to_str) {
        Some(Ok(raw)) => match st.shared.keys.peek(raw) {
            ratelimit::KeyLookup::Ignored => None,
            ratelimit::KeyLookup::Cached(hit) => hit,
            // The probes read cached keys only: a fresh random key on an unthrottled path
            // must not buy a database lookup (§10.1, M8-SEC-9).
            ratelimit::KeyLookup::Miss if exempt => None,
            ratelimit::KeyLookup::Miss => {
                // A lookup costs the address one anonymous request (§10.1) and a query permit.
                let anon = Client::new(ip, None, &st.cfg);
                let d = st
                    .shared
                    .limiter
                    .check(&anon.key, anon.rps, anon.burst, 1, Instant::now());
                if d.allowed {
                    match st.permit(Duration::from_secs(1)).await {
                        Some(permit) => {
                            let hit = st.shared.keys.lookup(&st.pool, raw).await;
                            drop(permit);
                            hit
                        }
                        None => None,
                    }
                } else {
                    refused = Some(d);
                    None
                }
            }
        },
        _ => None,
    };
    let client = Client::new(ip, key, &st.cfg);
    let cost = ratelimit::route_cost(&path, req.uri().query());
    let decision = if let Some(d) = refused {
        d
    } else if client.tier == Tier::Internal || exempt {
        Decision::unlimited(client.burst)
    } else {
        st.shared
            .limiter
            .check(&client.key, client.rps, client.burst, cost, Instant::now())
    };
    req.extensions_mut().insert(client);
    let mut resp = if !decision.allowed {
        ApiError::rate_limited(decision.retry_after_s, "rate limit exceeded").into_response()
    } else if path == "/v1/health" || path == "/v1/ws" || !path.starts_with("/v1/") {
        next.run(req).await
    } else {
        let permit = tokio::time::timeout(
            Duration::from_secs(1),
            Arc::clone(&st.shared.queries).acquire_owned(),
        )
        .await;
        match permit {
            Ok(Ok(permit)) => {
                let limit = Duration::from_millis(st.cfg.request_timeout_ms.max(1));
                let r = match tokio::time::timeout(limit, next.run(req)).await {
                    Ok(r) => r,
                    Err(_) => ApiError::timeout("the request timed out").into_response(),
                };
                drop(permit);
                r
            }
            _ => ApiError::unavailable("too many queries in flight").into_response(),
        }
    };
    let (slot, finalized) = st.heads().await;
    let h = resp.headers_mut();
    h.insert("ratelimit-limit", HeaderValue::from(decision.limit));
    h.insert("ratelimit-remaining", HeaderValue::from(decision.remaining));
    h.insert("ratelimit-reset", HeaderValue::from(decision.reset_s));
    h.insert("x-indexer-slot", HeaderValue::from(slot));
    h.insert("x-indexer-finalized-slot", HeaderValue::from(finalized));
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    resp
}

/// `shortwin-indexer serve`: binds `cfg.api.listen` and serves [`router`] on [`api_pool`] until
/// a signal.
pub async fn serve(cfg: &IndexerConfig, pool: PgPool) -> anyhow::Result<()> {
    cfg.api.check()?;
    if cfg.api.trusted_proxies.is_empty() {
        tracing::warn!(
            "api.trusted_proxies is empty: behind a load balancer every client is the balancer's \
             address — one rate-limit bucket and one WebSocket allowance for everyone. List the \
             balancer's address or network there (deploy/indexer.toml does)."
        );
    }
    let listener = tokio::net::TcpListener::bind(&cfg.api.listen)
        .await
        .with_context(|| format!("binding {}", cfg.api.listen))?;
    let app = router(AppState::new(api_pool(&pool, &cfg.api), cfg.api.clone()));
    tracing::info!(listen = %cfg.api.listen, "serving the API");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .context("serving the API")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_configuration_passes_its_own_check() {
        ApiConfig::default().check().expect("defaults");
        let bad = ApiConfig {
            trusted_proxies: vec!["10.0.0.0/99".to_string()],
            ..ApiConfig::default()
        };
        assert!(bad.check().is_err());
        let bad = ApiConfig {
            max_page_limit: 501,
            ..ApiConfig::default()
        };
        assert!(bad.check().is_err());
        // Below the web's largest page request (FE-30), and at it.
        let bad = ApiConfig {
            max_page_limit: WEB_MIN_PAGE_LIMIT - 1,
            ..ApiConfig::default()
        };
        assert!(bad.check().is_err());
        let ok = ApiConfig {
            max_page_limit: WEB_MIN_PAGE_LIMIT,
            ..ApiConfig::default()
        };
        ok.check().expect("max_page_limit at the web's floor");
        let bad = ApiConfig {
            ipv6_bucket_prefix: 129,
            ..ApiConfig::default()
        };
        assert!(bad.check().is_err());
        let bad = ApiConfig {
            ws_max_conns_total: 0,
            ..ApiConfig::default()
        };
        assert!(bad.check().is_err());
    }
}
