//! Regressions of the M8 security review, API side (M8-SEC-3, -4, -8, -9, -10): the review's
//! reproducers turned around, on the API seed (`tests/data/api_seed.sql`) plus bulk rows.
//!
//! `cost_one_pages_stay_fast_at_thirty_days_of_keeper_observations` is the CI load test the
//! review asked for: 30 days × 86,400 keeper observations of one market (2,592,000
//! `invocations` rows), attacker-made failed transactions and trades on pools that are no
//! market's, then every page that used to aggregate them.

mod api_support;

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use api_support::*;
use axum::http::{Method, StatusCode};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use shortwin_indexer::api::{self, ApiConfig, AppState};
use tokio_tungstenite::tungstenite::Message;

fn ip(s: &str) -> IpAddr {
    s.parse().expect("ip")
}

async fn serve(app: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });
    addr
}

fn rand_key(i: u64) -> String {
    let mut b = [7u8; 32];
    b[..8].copy_from_slice(&i.to_le_bytes());
    solana_pubkey::Pubkey::new_from_array(b).to_string()
}

/// The text frames of `ws` until (and with) the first of type `until`, or a 5 s silence.
async fn frames_until(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    until: &str,
) -> Vec<Value> {
    let mut got = Vec::new();
    while let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
        if let Message::Text(t) = m {
            let v: Value = serde_json::from_str(t.as_str()).expect("json");
            let done = v["type"] == until;
            got.push(v);
            if done {
                break;
            }
        }
    }
    got
}

async fn subscribe(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    channels: &[String],
    since: Option<i64>,
) {
    let mut m = json!({"op": "subscribe", "channels": channels});
    if let Some(s) = since {
        m["since"] = json!(s);
    }
    ws.send(Message::Text(m.to_string().into()))
        .await
        .expect("send");
    ws.send(Message::Text(json!({"op": "ping"}).to_string().into()))
        .await
        .expect("ping");
}

/// Makes every client backend of the test database report its pending statistics now.
///
/// A backend reports only when it goes idle, and then at most once a second unless
/// `pg_stat_force_next_flush()` asked (the report is sent before the query's `ReadyForQuery`), so
/// a scan can reach `pg_stat_user_tables` seconds late, on either side of a baseline. Each idle
/// connection of `pools` is asked in turn. A connection still on its way back to its pool (sqlx
/// returns a dropped connection from a spawned task, late on a loaded machine) cannot be asked
/// yet, so this repeats until every client backend of the database in `pg_stat_activity` has been
/// asked or has exited (an exiting backend reports).
async fn flush_stats(pools: &[&sqlx::PgPool]) {
    async fn ask(c: &mut sqlx::PgConnection, asked: &mut BTreeSet<i32>) {
        sqlx::query("SELECT pg_stat_force_next_flush()")
            .execute(&mut *c)
            .await
            .expect("flush the statistics");
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *c)
            .await
            .expect("pid");
        asked.insert(pid);
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut asked = BTreeSet::new();
    loop {
        let mut held = Vec::new();
        for p in pools {
            while let Some(mut c) = p.try_acquire() {
                ask(&mut c, &mut asked).await;
                held.push(c);
            }
        }
        if held.is_empty() {
            let mut c = pools[0].acquire().await.expect("a connection");
            ask(&mut c, &mut asked).await;
            held.push(c);
        }
        let backends: Vec<i32> = sqlx::query_scalar(
            "SELECT pid FROM pg_stat_activity \
             WHERE datname = current_database() AND backend_type = 'client backend'",
        )
        .fetch_all(&mut *held[0])
        .await
        .expect("backends");
        if backends.iter().all(|pid| asked.contains(pid)) {
            return;
        }
        drop(held);
        assert!(
            Instant::now() < deadline,
            "backends {backends:?} never reported (asked {asked:?})"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `api_keys`'s scans so far, once every backend has reported its own.
async fn api_keys_scans(admin: &sqlx::PgPool, api: &sqlx::PgPool) -> i64 {
    flush_stats(&[admin, api]).await;
    sqlx::query_scalar(
        "SELECT (COALESCE(seq_scan, 0) + COALESCE(idx_scan, 0))::bigint FROM pg_stat_user_tables \
         WHERE relname = 'api_keys'",
    )
    .fetch_one(admin)
    .await
    .expect("stats")
}

/// M8-SEC-9: on the rate-limit-exempt probes an uncached `X-Api-Key` is not looked up at all; on
/// the other routes a lookup needs the anonymous bucket's request.
///
/// Two timing effects made this flaky under load (M10 integration: `left: 1, right: 0`):
/// - the fixture's migrations build `api_keys`'s primary key, which counts as a sequential scan
///   of it, and the admin backend reported that up to a second or more after the seed, so it
///   could land between the two reads; every backend now reports before each read
///   ([`flush_stats`]);
/// - the anonymous bucket (`anon_rps` 1, `anon_burst` 1) refills one request a second, and a
///   refilled request buys one lookup by design. The probes are counted on their own and allow
///   none; the `/v1/stats` leg allows one lookup per whole second it ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_api_keys_on_probes_buy_no_database_lookups() {
    let f = Fixture::seeded().await;
    let cfg = ApiConfig {
        anon_rps: 1,
        anon_burst: 1,
        ..ApiConfig::default()
    };
    let pool = f.api_pool(&cfg);
    let app = api::router(AppState::new(pool.clone(), cfg));
    let a = ip("198.51.100.7");

    // The probes: not one lookup, however long they take.
    let before = api_keys_scans(&f.admin, &pool).await;
    for i in 0..300 {
        let key = format!("random-key-{i}");
        for path in ["/v1/health", "/v1/status"] {
            let r = send(&app, Method::GET, path, a, &[("x-api-key", key.as_str())]).await;
            assert_eq!(r.status, StatusCode::OK, "{path}");
        }
    }
    let probes = api_keys_scans(&f.admin, &pool).await - before;
    assert_eq!(probes, 0, "no random key on a probe reached the database");

    // `/v1/stats`: the first request empties the bucket, then each lookup needs a refill.
    let started = Instant::now();
    assert_eq!(
        send(&app, Method::GET, "/v1/stats", a, &[]).await.status,
        StatusCode::OK
    );
    let before = api_keys_scans(&f.admin, &pool).await;
    for i in 300..600 {
        let key = format!("random-key-{i}");
        let r = send(
            &app,
            Method::GET,
            "/v1/stats",
            a,
            &[("x-api-key", key.as_str())],
        )
        .await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    }
    let refills = i64::try_from(started.elapsed().as_secs()).expect("seconds");
    let lookups = api_keys_scans(&f.admin, &pool).await - before;
    assert!(
        lookups <= refills,
        "{lookups} random keys reached the database in {refills} s of refills"
    );
    pool.close().await;
    f.stop().await;
}

/// M8-SEC-10: `/v1/markets?%71=…` is a search (`%71` = `q`) and costs the heavy price.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_percent_encoded_search_costs_the_heavy_price() {
    let f = Fixture::seeded().await;
    let app = f.router(ApiConfig {
        anon_rps: 1,
        anon_burst: 6,
        ..ApiConfig::default()
    });
    let r1 = send(&app, Method::GET, "/v1/markets?q=a", ip("192.0.2.10"), &[]).await;
    let r2 = send(
        &app,
        Method::GET,
        "/v1/markets?%71=a",
        ip("192.0.2.11"),
        &[],
    )
    .await;
    assert_eq!(r1.status, StatusCode::OK);
    assert_eq!(r2.status, StatusCode::OK);
    assert_eq!(r1.body["data"], r2.body["data"]);
    assert_eq!(r1.header_i64("ratelimit-remaining"), Some(1));
    assert_eq!(r2.header_i64("ratelimit-remaining"), Some(1));
    f.stop().await;
}

/// M8-SEC-8: behind the reference deployment's balancer (its gateway address trusted, as
/// deploy/indexer.toml ships) each user behind it has its own bucket; the process caps
/// WebSocket connections across clients (503 at the upgrade).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reference_deployment_tells_clients_apart_and_caps_websockets() {
    let f = Fixture::seeded().await;
    let deploy: shortwin_indexer::IndexerConfig =
        toml::from_str(include_str!("../deploy/indexer.toml")).expect("deploy config");
    let proxy = ip(&deploy.api.trusted_proxies[0]);
    let app = f.router(deploy.api.clone());
    for i in 0..80 {
        let xff = format!("198.51.100.{}", i % 2);
        let _ = send(
            &app,
            Method::GET,
            "/v1/stats",
            proxy,
            &[("x-forwarded-for", xff.as_str())],
        )
        .await;
    }
    let other = send(
        &app,
        Method::GET,
        "/v1/stats",
        proxy,
        &[("x-forwarded-for", "203.0.113.99")],
    )
    .await;
    assert_eq!(other.status, StatusCode::OK, "a third user is not limited");
    // One WebSocket per process.
    let addr = serve(f.router(ApiConfig {
        ws_max_conns_total: 1,
        ..relaxed()
    }))
    .await;
    let (_first, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws"))
        .await
        .expect("the first connection");
    match tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws")).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        other => panic!("the second connection: {other:?}"),
    }
    f.stop().await;
}

/// `n` events of market B (valid base58 signatures from md5), bulk.
async fn bulk_events(admin: &sqlx::PgPool, n: i64) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SET synchronous_commit = off;
         INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, fee_payer, fee)
         SELECT '5' || translate(md5(i::text) || md5((i * 7)::text), '0', 'x'), 5000000 + i, 0,
                1790000000 + i, 'finalized', 'replay', '{K1}', 5000
         FROM generate_series(1, {n}) i;
         INSERT INTO events (signature, ix_index, ev_source, ev_seq, slot, tx_index, block_time,
                program, invocation, kind, market, position, actor, ev_slot, ev_ts, data)
         SELECT '5' || translate(md5(i::text) || md5((i * 7)::text), '0', 'x'), 0, 0, 0, 5000000 + i, 0,
                1790000000 + i, '5yvyAddWb8RRGh3pPiFS5eV33eZYeHVmAAfwAFmqVpKK', 'crank_price',
                'PriceCranked', '{MB}', NULL, NULL, 5000000 + i, 1790000000 + i,
                '{{\"spot_wad\": \"1\", \"ema_price\": \"2\"}}'::jsonb
         FROM generate_series(1, {n}) i;
         ANALYZE events; ANALYZE ingest_txs;"
    )))
    .execute(admin)
    .await
    .expect("bulk events");
}

/// M8-SEC-4: a `since` replay reads each channel through an index (fast whatever `since`),
/// costs the client a heavy request, and runs under the query semaphore.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_replays_are_bounded_charged_and_under_the_query_semaphore() {
    let f = Fixture::seeded().await;
    bulk_events(&f.admin, 300_000).await;
    // 1. Fast: `since: 0` on 50 channels that match nothing scans nothing.
    let addr = serve(f.router(relaxed())).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws"))
        .await
        .expect("upgrade");
    let channels: Vec<String> = (0..50).map(|i| format!("market:{}", rand_key(i))).collect();
    let t = Instant::now();
    subscribe(&mut ws, &channels, Some(0)).await;
    let got = frames_until(&mut ws, "pong").await;
    let took = t.elapsed();
    assert!(got.iter().all(|v| v["type"] != "error"), "{got:?}");
    assert!(
        took < Duration::from_millis(500),
        "the replay took {took:?}"
    );
    // A busy channel replays its first 1,000 events after `since`, in id order.
    drop(ws);
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws"))
        .await
        .expect("upgrade");
    subscribe(&mut ws, &[format!("market:{TB}")], Some(0)).await;
    let got = frames_until(&mut ws, "pong").await;
    let events: Vec<i64> = got
        .iter()
        .filter(|v| v["type"] == "event")
        .map(|v| v["id"].as_i64().expect("id"))
        .collect();
    assert_eq!(
        events.len(),
        1_000,
        "{:?}",
        got.iter()
            .filter(|v| v["type"] != "event")
            .collect::<Vec<_>>()
    );
    assert!(events.windows(2).all(|w| w[0] < w[1]));
    drop(ws);
    // 2. Charged: a replay costs HEAVY_COST (5) of the client's bucket.
    let addr = serve(f.router(ApiConfig {
        anon_rps: 1,
        anon_burst: 6,
        ..ApiConfig::default()
    }))
    .await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws"))
        .await
        .expect("upgrade");
    subscribe(&mut ws, &[format!("market:{TA}")], Some(0)).await;
    let first = frames_until(&mut ws, "pong").await;
    assert!(first.iter().all(|v| v["type"] != "error"), "{first:?}");
    subscribe(&mut ws, &[format!("market:{TB}")], Some(0)).await;
    let second = frames_until(&mut ws, "pong").await;
    assert!(
        second
            .iter()
            .any(|v| v["type"] == "error" && v["code"] == "rate_limited"),
        "{second:?}"
    );
    drop(ws);
    // 3. Under the semaphore: with its only permit held by a REST request (blocked on a table
    // the replay does not read), a replay answers `unavailable` instead of running.
    let cfg = ApiConfig {
        max_concurrent_queries: 1,
        ..relaxed()
    };
    let app = f.router(cfg);
    let addr = serve(app.clone()).await;
    let mut lock = f.admin.begin().await.expect("tx");
    sqlx::query("LOCK TABLE settlements IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .expect("lock");
    let blocked = tokio::spawn({
        let app = app.clone();
        async move { get(&app, "/v1/settlements").await.status }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws"))
        .await
        .expect("upgrade");
    subscribe(&mut ws, &[format!("market:{TB}")], Some(0)).await;
    let got = frames_until(&mut ws, "pong").await;
    assert!(
        got.iter()
            .any(|v| v["type"] == "error" && v["code"] == "unavailable"),
        "{got:?}"
    );
    assert!(got.iter().all(|v| v["type"] != "event"), "no replay ran");
    lock.rollback().await.expect("unlock");
    let _ = blocked.await;
    f.stop().await;
}

/// A pool that is no market's (an attacker's pool naming market A's mint).
fn attacker_pool() -> String {
    rand_key(424_242)
}

/// Bulk rows for the load test: `days` × 86,400 successful `crank_price` observations of market
/// A (one per second up to the chain clock; 1,000 per transaction), `failed` failed
/// `crank_price` transactions of market A paid by 5,000 wallets, and `trades` venue trades of
/// the mint on a pool that is no market's (market NULL).
async fn bulk_load(admin: &sqlx::PgPool, days: i64, failed: i64, trades: i64) {
    let n_tx = days * 86_400 / 1_000;
    let pool_z = attacker_pool();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SET synchronous_commit = off;
         INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, fee_payer, fee)
         SELECT '7' || translate(md5('k' || i::text) || md5(('m' || i)::text), '0', 'x'), 9000000 + i, 0,
                (SELECT latest_block_time FROM ingest_state) - {days} * 86400 + i * 1000,
                'finalized', 'replay', '{K1}', 5000
         FROM generate_series(1, {n_tx}) i;
         INSERT INTO invocations (signature, ix_index, inner_index, stack_height, slot, tx_index,
                                  block_time, program, name, market, args, failed)
         SELECT '7' || translate(md5('k' || i::text) || md5(('m' || i)::text), '0', 'x'), j, -1, 1,
                9000000 + i, 0,
                (SELECT latest_block_time FROM ingest_state) - {days} * 86400 + (i - 1) * 1000 + j + 1,
                '5yvyAddWb8RRGh3pPiFS5eV33eZYeHVmAAfwAFmqVpKK', 'crank_price', '{MA}', NULL, FALSE
         FROM generate_series(1, {n_tx}) i, generate_series(0, 999) j;
         INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, fee_payer,
                                 fee, err, err_ix_index, err_code)
         SELECT '4' || translate(md5('f' || i::text) || md5(('g' || i)::text), '0', 'x'), 6000000 + i, 0,
                (SELECT latest_block_time FROM ingest_state) - (i % 80000), 'finalized', 'replay',
                '9' || translate(substr(md5('p' || (i % 5000)::text), 1, 32), '0', 'x') || 'AAAAAAAAA',
                5000, '{{\"InstructionError\": [0, {{\"Custom\": 6001}}]}}'::jsonb, 0, 6001
         FROM generate_series(1, {failed}) i;
         INSERT INTO invocations (signature, ix_index, inner_index, stack_height, slot, tx_index,
                                  block_time, program, name, market, args, failed)
         SELECT '4' || translate(md5('f' || i::text) || md5(('g' || i)::text), '0', 'x'), 0, -1, 1,
                6000000 + i, 0, (SELECT latest_block_time FROM ingest_state) - (i % 80000),
                '5yvyAddWb8RRGh3pPiFS5eV33eZYeHVmAAfwAFmqVpKK', 'crank_price', '{MA}', NULL, TRUE
         FROM generate_series(1, {failed}) i;
         INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, fee_payer, fee)
         SELECT '3' || translate(md5('t' || i::text) || md5(('u' || i)::text), '0', 'x'), 7000000 + i, 0,
                1790000000 + i, 'finalized', 'replay', '{K2}', 5000
         FROM generate_series(1, {trades}) i;
         INSERT INTO venue_trades (signature, ix_index, ev_source, ev_seq, slot, tx_index, ts, venue,
                program, pool, mint, market, side, token_amount, sol_amount, fee_creator_lamports,
                user_key, is_shortwin, price_wad)
         SELECT '3' || translate(md5('t' || i::text) || md5(('u' || i)::text), '0', 'x'), 1, 0, 0,
                7000000 + i, 0, 1790000000 + i, 'pump_swap', 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA',
                '{pool_z}', '{TA}', NULL, 'buy', 1, 1000000000000, 1, '{K2}', FALSE,
                1000000000000000000000000000000
         FROM generate_series(1, {trades}) i;
         ANALYZE;"
    )))
    .execute(admin)
    .await
    .expect("bulk load");
}

/// M8-SEC-3, the load test: every cost-1 page that aggregated `invocations`, failed
/// transactions or venue trades answers fast at 30 days of keeper observations of one market
/// plus attacker rows; the counters behind them stay exactly the rows' aggregates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cost_one_pages_stay_fast_at_thirty_days_of_keeper_observations() {
    let f = Fixture::seeded().await;
    let t0 = Instant::now();
    bulk_load(&f.admin, 30, 100_000, 100_000).await;
    eprintln!(
        "seeded 30 days of observations and the attacker rows in {:?}",
        t0.elapsed()
    );
    let app = f.router(relaxed());
    for uri in [
        format!("/v1/markets/{TA}"),
        format!("/v1/markets/{TA}/coverage"),
        format!("/v1/markets/{TB}"),
        format!("/v1/markets/{TB}/coverage"),
        "/v1/keepers".to_string(),
        "/v1/stats".to_string(),
        format!("/v1/markets/{TA}/trades"),
        "/v1/markets".to_string(),
    ] {
        // The first call may warm caches (and fills `/v1/stats`'s); the second is the steady
        // state. Both answer within the statement timeout.
        let first = get(&app, &uri).await;
        assert_eq!(first.status, StatusCode::OK, "{uri}: {}", first.body);
        let t = Instant::now();
        let r = get(&app, &uri).await;
        let took = t.elapsed();
        eprintln!("{uri}: {} in {took:?}", r.status);
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.body);
        assert!(took < Duration::from_millis(750), "{uri} took {took:?}");
    }
    // The page's 24 h counts equal the rows' (the counters plus the window's partial hour).
    let want: (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE NOT failed), count(*) FILTER (WHERE failed) \
         FROM invocations WHERE market = $1 AND sw_observing(name) \
           AND block_time >= sw_chain_now() - 86400",
    )
    .bind(MA)
    .fetch_one(&f.admin)
    .await
    .expect("rows");
    let page = get(&app, &format!("/v1/markets/{TA}/coverage")).await;
    assert_eq!(
        (
            page.body["stats"]["observations_24h"].as_i64(),
            page.body["stats"]["failed_observations_24h"].as_i64()
        ),
        (Some(want.0), Some(want.1)),
        "{}",
        page.body["stats"]
    );
    assert!(want.0 >= 86_000, "a day of 1/s observations: {want:?}");
    // Market B's coverage never reads market A's rows.
    let plan: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN SELECT * FROM sw_market_coverage('{MB}')"
    )))
    .fetch_all(&f.admin)
    .await
    .expect("explain");
    let plan = plan.join("\n");
    assert!(!plan.contains("Seq Scan on invocations"), "{plan}");
    // The attacker's pool is no market's: market A's trades and creator totals are unchanged.
    let trades = get(&app, &format!("/v1/markets/{TA}/trades?limit=500")).await;
    assert!(
        trades
            .data()
            .iter()
            .all(|t| t["pool"] != attacker_pool().as_str()),
        "the attacker's pool is not market A's"
    );
    counters_equal_the_rows(&f.admin).await;
    f.stop().await;
}

/// The trigger-kept counters equal the aggregates of their rows.
async fn counters_equal_the_rows(admin: &sqlx::PgPool) {
    let checks = [
        (
            "keeper failures",
            "SELECT COALESCE(jsonb_agg(x ORDER BY x), '[]') FROM (SELECT keeper::text || '/' || \
               instruction || '/' || err_code || '=' || failures AS x FROM keeper_failure_counts) s",
            "SELECT COALESCE(jsonb_agg(x ORDER BY x), '[]') FROM (SELECT keeper::text || '/' || \
               instruction || '/' || COALESCE(err_code, -1) || '=' || failures AS x \
               FROM v_keeper_failures) s",
        ),
        (
            "observations by hour",
            "SELECT COALESCE(jsonb_agg(x ORDER BY x), '[]') FROM (SELECT market::text || '/' || \
               bucket_start || '=' || observations || '/' || failed AS x \
               FROM market_observations_hourly) s",
            "SELECT COALESCE(jsonb_agg(x ORDER BY x), '[]') FROM (SELECT market::text || '/' || \
               (block_time / 3600) * 3600 || '=' || count(*) FILTER (WHERE NOT failed) || '/' || \
               count(*) FILTER (WHERE failed) AS x FROM invocations \
               WHERE market IS NOT NULL AND block_time IS NOT NULL AND sw_observing(name) \
               GROUP BY market, (block_time / 3600) * 3600) s",
        ),
        (
            "trade totals",
            "SELECT COALESCE(jsonb_agg(x ORDER BY x), '[]') FROM (SELECT market::text || '=' || \
               trades || '/' || shortwin_volume || '/' || shortwin_creator_fees || '/' || \
               all_creator_fees AS x FROM market_trade_totals) s",
            "SELECT COALESCE(jsonb_agg(x ORDER BY x), '[]') FROM (SELECT market::text || '=' || \
               count(*) || '/' || COALESCE(sum(sol_amount) FILTER (WHERE is_shortwin), 0) || '/' || \
               COALESCE(sum(fee_creator_lamports) FILTER (WHERE is_shortwin), 0) || '/' || \
               COALESCE(sum(fee_creator_lamports), 0) AS x FROM venue_trades \
               WHERE market IS NOT NULL GROUP BY market) s",
        ),
    ];
    for (what, counters, rows) in checks {
        let a: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(counters))
            .fetch_one(admin)
            .await
            .expect(what);
        let b: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(rows))
            .fetch_one(admin)
            .await
            .expect(what);
        assert_eq!(a, b, "{what}");
    }
}

/// The counters follow every path that removes or moves their rows: a rollback (the
/// transaction's rows cascade), the retention prune (rows deleted, transaction kept), a
/// rebuild's re-attribution of venue trades.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn counters_follow_rollbacks_prunes_and_reattribution() {
    let f = Fixture::seeded().await;
    bulk_load(&f.admin, 1, 2_000, 500).await;
    counters_equal_the_rows(&f.admin).await;
    let failures = || async {
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT sum(failures)::bigint FROM keeper_failure_counts",
        )
        .fetch_one(&f.admin)
        .await
        .expect("sum")
        .unwrap_or(0)
    };
    let before = failures().await;
    // A rollback: failed transactions deleted (their invocations cascade).
    let n = sqlx::query(
        "DELETE FROM ingest_txs WHERE signature IN (SELECT signature FROM ingest_txs \
         WHERE err IS NOT NULL AND slot > 6000000 ORDER BY slot LIMIT 300)",
    )
    .execute(&f.admin)
    .await
    .expect("rollback")
    .rows_affected();
    assert_eq!(n, 300);
    counters_equal_the_rows(&f.admin).await;
    assert_eq!(failures().await, before - 300);
    // The prune: invocations deleted, their transactions kept.
    sqlx::query("DELETE FROM invocations WHERE slot > 6000000 AND slot <= 6000400")
        .execute(&f.admin)
        .await
        .expect("prune");
    sqlx::query("DELETE FROM invocations WHERE slot > 9000000 AND ix_index < 500")
        .execute(&f.admin)
        .await
        .expect("prune");
    counters_equal_the_rows(&f.admin).await;
    // Re-attribution (a rebuild clears and refills a market's third-party trades).
    let pool_z = attacker_pool();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE venue_trades SET market = '{MA}' WHERE pool = '{pool_z}'"
    )))
    .execute(&f.admin)
    .await
    .expect("attribute");
    counters_equal_the_rows(&f.admin).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE venue_trades SET market = NULL WHERE market = '{MA}' AND NOT is_shortwin"
    )))
    .execute(&f.admin)
    .await
    .expect("clear");
    counters_equal_the_rows(&f.admin).await;
    f.stop().await;
}
