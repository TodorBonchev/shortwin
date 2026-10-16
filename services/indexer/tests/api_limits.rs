//! Rate and abuse limits (INDEXER.md §10; §11.2 "the rate limiter is tested with a tightened
//! config"). Owner: API.
//!
//! On an embedded PostgreSQL seeded from `tests/data/api_seed.sql`: the GCRA bucket and its
//! headers, route costs, API-key tiers, `X-Forwarded-For` behind trusted proxies, the statement
//! timeout, the request timeout, the query semaphore, the read-only database role, CORS.

mod api_support;

use std::net::IpAddr;
use std::time::Duration;

use api_support::*;
use axum::http::{Method, StatusCode};
use shortwin_indexer::api::ApiConfig;

fn ip(s: &str) -> IpAddr {
    s.parse().expect("ip")
}

fn tight(burst: u32) -> ApiConfig {
    ApiConfig {
        anon_rps: 1,
        anon_burst: burst,
        key_rps: 1,
        key_burst: burst * 2,
        ..ApiConfig::default()
    }
}

/// GCRA: the burst, then `429` with `Retry-After`; the `RateLimit-*` headers count down; heavy
/// routes cost more; the probes are not limited.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_are_rate_limited_per_client() {
    let f = Fixture::seeded().await;
    let c = Contract::load();
    let app = f.router(tight(3));
    let a = ip("192.0.2.1");
    for left in [2, 1, 0] {
        let r = send(&app, Method::GET, "/v1/stats", a, &[]).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.header_i64("ratelimit-limit"), Some(3));
        assert_eq!(r.header_i64("ratelimit-remaining"), Some(left));
        assert!(r.header_i64("ratelimit-reset").unwrap_or(0) >= 1);
    }
    let r = send(&app, Method::GET, "/v1/stats", a, &[]).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.header_i64("retry-after"), Some(1));
    c.check("getGlobalStats", "429", &r);
    // Another address has its own bucket; an IPv6 /64 is one client.
    assert_eq!(
        send(&app, Method::GET, "/v1/stats", ip("192.0.2.2"), &[])
            .await
            .status,
        StatusCode::OK
    );
    for (n, peer) in ["2001:db8::1", "2001:db8::2", "2001:db8::3"]
        .iter()
        .enumerate()
    {
        let r = send(&app, Method::GET, "/v1/stats", ip(peer), &[]).await;
        assert_eq!(
            r.header_i64("ratelimit-remaining"),
            Some(2 - i64::try_from(n).expect("n"))
        );
    }
    // The bucket refills at the rate.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert_eq!(
        send(&app, Method::GET, "/v1/stats", a, &[]).await.status,
        StatusCode::OK
    );

    // Heavy routes cost 5 (§10.1): a burst of 6 admits one search, then refuses the next.
    let app = f.router(tight(6));
    let b = ip("192.0.2.3");
    let r = send(&app, Method::GET, "/v1/search?q=bonk", b, &[]).await;
    assert_eq!(
        (r.status, r.header_i64("ratelimit-remaining")),
        (StatusCode::OK, Some(1))
    );
    let r = send(
        &app,
        Method::GET,
        &format!("/v1/markets/{TA}/candles?interval=1h"),
        b,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    let r = send(&app, Method::GET, "/v1/markets", b, &[]).await;
    assert_eq!(
        (r.status, r.header_i64("ratelimit-remaining")),
        (StatusCode::OK, Some(0))
    );
    let d = ip("192.0.2.4");
    let r = send(&app, Method::GET, "/v1/markets?q=wif", d, &[]).await;
    assert_eq!(
        r.header_i64("ratelimit-remaining"),
        Some(1),
        "`q=` makes /v1/markets heavy"
    );
    let r = send(
        &app,
        Method::GET,
        "/v1/stats/timeseries?metric=tvl",
        ip("192.0.2.5"),
        &[],
    )
    .await;
    assert_eq!(r.header_i64("ratelimit-remaining"), Some(1));

    // The probes are never limited.
    let app = f.router(tight(1));
    for _ in 0..10 {
        assert_eq!(
            send(&app, Method::GET, "/v1/health", a, &[]).await.status,
            StatusCode::OK
        );
        assert_eq!(
            send(&app, Method::GET, "/v1/status", a, &[]).await.status,
            StatusCode::OK
        );
    }
    // Unknown routes are limited like any request (no free probing).
    assert_eq!(
        send(&app, Method::GET, "/v1/nope", a, &[]).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&app, Method::GET, "/v1/nope", a, &[]).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    f.stop().await;
}

/// API keys (§10.1): `partner` gets its tier's bucket (by key, whatever the address), `internal`
/// is unlimited, a disabled or unknown key is anonymous.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_keys_select_the_tier() {
    let f = Fixture::seeded().await;
    let app = f.router(tight(2));
    // A cost-1 route (`/v1/keepers` costs HEAVY_COST since the M8 security review).
    let partner = [("x-api-key", "test-partner-key")];
    for (i, left) in [3, 2, 1, 0].into_iter().enumerate() {
        // The key's bucket follows the key across addresses.
        let peer = IpAddr::from([198, 51, 100, u8::try_from(i).expect("i")]);
        let r = send(&app, Method::GET, "/v1/settlements", peer, &partner).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.header_i64("ratelimit-limit"), Some(4));
        assert_eq!(r.header_i64("ratelimit-remaining"), Some(left));
    }
    let r = send(
        &app,
        Method::GET,
        "/v1/settlements",
        ip("198.51.100.9"),
        &partner,
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // The address itself is still anonymous: its bucket paid only for the first lookup.
    let r = send(
        &app,
        Method::GET,
        "/v1/settlements",
        ip("198.51.100.0"),
        &[],
    )
    .await;
    assert_eq!(
        (r.status, r.header_i64("ratelimit-remaining")),
        (StatusCode::OK, Some(0))
    );
    // Random keys cannot buy free lookups: each miss costs the address an anonymous request.
    let spray = ip("198.51.100.40");
    let r = send(
        &app,
        Method::GET,
        "/v1/settlements",
        spray,
        &[("x-api-key", "random-1")],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(
        &app,
        Method::GET,
        "/v1/settlements",
        spray,
        &[("x-api-key", "random-2")],
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);

    let internal = [("x-api-key", "test-internal-key")];
    for _ in 0..30 {
        let r = send(
            &app,
            Method::GET,
            "/v1/settlements",
            ip("198.51.100.20"),
            &internal,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
    }
    let long = "k".repeat(300);
    for (i, key) in ["test-disabled-key", "no-such-key", long.as_str()]
        .into_iter()
        .enumerate()
    {
        let peer = IpAddr::from([198, 51, 100, 30 + u8::try_from(i).expect("i")]);
        let h = [("x-api-key", key)];
        let r = send(&app, Method::GET, "/v1/settlements", peer, &h).await;
        assert_eq!(
            r.header_i64("ratelimit-limit"),
            Some(2),
            "{key}: anonymous limits"
        );
        let _ = send(&app, Method::GET, "/v1/settlements", peer, &h).await;
        let r = send(&app, Method::GET, "/v1/settlements", peer, &h).await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{key}");
    }
    f.stop().await;
}

/// `X-Forwarded-For` names the client only behind a trusted proxy (§10.1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forwarded_for_is_trusted_only_from_trusted_proxies() {
    let f = Fixture::seeded().await;
    let app = f.router(ApiConfig {
        trusted_proxies: vec!["10.0.0.0/8".to_string()],
        ..tight(1)
    });
    let proxy = ip("10.0.0.7");
    let xff = |v: &'static str| [("x-forwarded-for", v)];
    assert_eq!(
        send(&app, Method::GET, "/v1/stats", proxy, &xff("203.0.113.1"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        send(&app, Method::GET, "/v1/stats", proxy, &xff("203.0.113.1"))
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Another client behind the same proxy; a spoofed left-most hop does not help.
    assert_eq!(
        send(
            &app,
            Method::GET,
            "/v1/stats",
            proxy,
            &xff("1.2.3.4, 203.0.113.2")
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            Method::GET,
            "/v1/stats",
            proxy,
            &xff("9.9.9.9, 203.0.113.2")
        )
        .await
        .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // From an untrusted peer the header is ignored: the peer is the client.
    let peer = ip("192.0.2.77");
    assert_eq!(
        send(&app, Method::GET, "/v1/stats", peer, &xff("203.0.113.3"))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        send(&app, Method::GET, "/v1/stats", peer, &xff("203.0.113.4"))
            .await
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    f.stop().await;
}

/// Holds `ACCESS EXCLUSIVE` on `table` until the returned transaction is dropped.
async fn lock(f: &Fixture, table: &'static str) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = f.admin.begin().await.expect("begin");
    let sql = match table {
        "markets" => "LOCK TABLE markets IN ACCESS EXCLUSIVE MODE",
        _ => "LOCK TABLE fees IN ACCESS EXCLUSIVE MODE",
    };
    sqlx::query(sql).execute(&mut *tx).await.expect("lock");
    tx
}

/// The statement timeout, the request timeout and the query semaphore answer `503` (§10.2).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_queries_time_out_and_saturation_is_unavailable() {
    let f = Fixture::seeded().await;
    let c = Contract::load();
    let held = lock(&f, "markets").await;

    // Statement timeout (the API pool's `statement_timeout`): 503 timeout.
    let app = f.router(ApiConfig {
        statement_timeout_ms: 300,
        ..relaxed()
    });
    let r = get(&app, "/v1/markets").await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "timeout")
    );
    assert_eq!(r.header_i64("retry-after"), Some(1));
    c.validate(&c.component("Problem"), &r.body, "statement timeout");

    // Request timeout (the middleware): 503 timeout.
    let app = f.router(ApiConfig {
        statement_timeout_ms: 10_000,
        request_timeout_ms: 300,
        ..relaxed()
    });
    let t = std::time::Instant::now();
    let r = get(&app, &format!("/v1/markets/{TA}")).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "timeout")
    );
    assert!(t.elapsed() < Duration::from_secs(3));

    // Saturation: one permit, held by a blocked request; the next waits 1 s, then 503.
    let app = f.router(ApiConfig {
        statement_timeout_ms: 10_000,
        request_timeout_ms: 8_000,
        max_concurrent_queries: 1,
        ..relaxed()
    });
    let blocked = {
        let app = app.clone();
        tokio::spawn(async move { get(&app, "/v1/markets").await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = get(&app, "/v1/keepers").await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
    );
    // The probe needs no permit.
    assert_eq!(get(&app, "/v1/health").await.status, StatusCode::OK);
    drop(held);
    let r = blocked.await.expect("join");
    assert_eq!(
        r.status,
        StatusCode::OK,
        "the blocked request completes once the lock is gone"
    );
    assert_eq!(get(&app, "/v1/keepers").await.status, StatusCode::OK);
    f.stop().await;
}

/// The API's connections are read-only (§10.2): even a handler bug could not write.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_api_pool_is_read_only() {
    let f = Fixture::seeded().await;
    let pool = f.api_pool(&ApiConfig::default());
    let err = sqlx::query("DELETE FROM api_keys")
        .execute(&pool)
        .await
        .expect_err("read-only");
    assert!(err.to_string().contains("read-only"), "{err}");
    let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&pool)
        .await
        .expect("show");
    assert_eq!(timeout, "2s");
    pool.close().await;
    f.stop().await;
}

/// CORS (§9.1): any origin by default, the API's headers exposed; an allowlist otherwise.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cors_allows_the_configured_origins() {
    let f = Fixture::seeded().await;
    let app = f.router(relaxed());
    let r = send(
        &app,
        Method::OPTIONS,
        "/v1/markets",
        localhost(),
        &[
            ("origin", "https://app.example"),
            ("access-control-request-method", "GET"),
            ("access-control-request-headers", "x-api-key"),
        ],
    )
    .await;
    assert!(r.status.is_success());
    assert_eq!(r.header("access-control-allow-origin"), Some("*"));
    let r = send(
        &app,
        Method::GET,
        "/v1/stats",
        localhost(),
        &[("origin", "https://app.example")],
    )
    .await;
    assert_eq!(r.header("access-control-allow-origin"), Some("*"));
    let exposed = r
        .header("access-control-expose-headers")
        .unwrap_or("")
        .to_ascii_lowercase();
    for h in ["x-indexer-slot", "ratelimit-remaining", "retry-after"] {
        assert!(exposed.contains(h), "{h} exposed: {exposed}");
    }
    let app = f.router(ApiConfig {
        cors_origins: vec!["https://short.win".to_string()],
        ..relaxed()
    });
    let r = send(
        &app,
        Method::GET,
        "/v1/stats",
        localhost(),
        &[("origin", "https://short.win")],
    )
    .await;
    assert_eq!(
        r.header("access-control-allow-origin"),
        Some("https://short.win")
    );
    let r = send(
        &app,
        Method::GET,
        "/v1/stats",
        localhost(),
        &[("origin", "https://evil.example")],
    )
    .await;
    assert_eq!(r.header("access-control-allow-origin"), None);
    f.stop().await;
}
