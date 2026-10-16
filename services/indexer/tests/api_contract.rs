//! API contract tests (INDEXER.md §11.2; ARCH §9 M8 exit "API contract tests"). Owner: API.
//!
//! On an embedded PostgreSQL seeded from `tests/data/api_seed.sql`:
//! - every operation of `openapi.yaml` is called and every response is checked against the
//!   document (status documented, content type, body schema, headers) — the base request, every
//!   enum value of every enumerated parameter, every invalid-parameter class (`400` with its
//!   code), `404`, `429` (a tightened limiter) and `503` (`/v1/status` without a database); the
//!   test fails unless every documented (operation, status) pair was produced (the WebSocket's
//!   `101` is `tests/api_ws.rs`'s);
//! - the router serves exactly the documented paths;
//! - responses carry the seeded values (filters, ordering, the analytics);
//! - every list pages without gaps or repeats, and cursors are endpoint-bound.

mod api_support;

use std::collections::BTreeSet;
use std::net::IpAddr;

use api_support::*;
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use shortwin_indexer::api::routes::ROUTES;
use shortwin_indexer::api::{self, ApiConfig, AppState};

/// The base request of each operation: path parameters from the seed, required query
/// parameters given.
fn base(id: &str) -> String {
    match id {
        "getHealth" => "/v1/health".into(),
        "getStatus" => "/v1/status".into(),
        "listMarkets" => "/v1/markets".into(),
        "getMarket" => format!("/v1/markets/{TA}"),
        "listMarketPositions" => format!("/v1/markets/{TA}/positions"),
        "listMarketLenders" => format!("/v1/markets/{TA}/lenders"),
        "getMarketPools" => format!("/v1/markets/{TA}/pools"),
        "getMarketCoverage" => format!("/v1/markets/{TA}/coverage"),
        "listMarketLiquidations" => format!("/v1/markets/{TA}/liquidations"),
        "listMarketEvents" => format!("/v1/markets/{TA}/events"),
        "listMarketSnapshots" => format!("/v1/markets/{TA}/snapshots"),
        "listMarketTrades" => format!("/v1/markets/{TA}/trades"),
        "getMarketCandles" => format!("/v1/markets/{TA}/candles?interval=1h"),
        "getMarketStats" => format!("/v1/markets/{TA}/stats"),
        "getMarketFees" => format!("/v1/markets/{TA}/fees"),
        "getPosition" => format!("/v1/positions/{P4}"),
        "listBorrowerPositions" => format!("/v1/borrowers/{B3}/positions"),
        "getLender" => format!("/v1/lenders/{L1}"),
        "listLenderEvents" => format!("/v1/lenders/{L1}/events"),
        "getGlobalStats" => "/v1/stats".into(),
        "getGlobalTimeseries" => "/v1/stats/timeseries?metric=tvl".into(),
        "listSettlements" => "/v1/settlements".into(),
        "listLiquidations" => "/v1/liquidations".into(),
        "listKeepers" => "/v1/keepers".into(),
        "getProtocolConfig" => "/v1/admin/config".into(),
        "listConfigChanges" => "/v1/admin/changes".into(),
        "listAdminEvents" => "/v1/admin/events".into(),
        "getVenues" => "/v1/venues".into(),
        "search" => "/v1/search?q=bonk".into(),
        "getTransaction" => format!("/v1/tx/{SIG_INIT}"),
        "openWebSocket" => "/v1/ws".into(),
        other => panic!("no base request for {other}"),
    }
}

fn with(uri: &str, kv: &str) -> String {
    if uri.contains('?') {
        format!("{uri}&{kv}")
    } else {
        format!("{uri}?{kv}")
    }
}

/// `uri` without the query parameter `name`.
fn without(uri: &str, name: &str) -> String {
    let Some((path, q)) = uri.split_once('?') else {
        return uri.to_string();
    };
    let kept: Vec<&str> = q
        .split('&')
        .filter(|kv| kv.split('=').next() != Some(name))
        .collect();
    if kept.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", kept.join("&"))
    }
}

/// Calls operations, checks each response against the contract and records what was produced.
struct Run {
    contract: Contract,
    seen: BTreeSet<(String, u16)>,
    calls: usize,
}

impl Run {
    async fn call(&mut self, app: &axum::Router, id: &str, uri: &str, expect: u16) -> Resp {
        self.call_from(app, id, uri, expect, localhost()).await
    }

    async fn call_from(
        &mut self,
        app: &axum::Router,
        id: &str,
        uri: &str,
        expect: u16,
        peer: IpAddr,
    ) -> Resp {
        let r = send(app, Method::GET, uri, peer, &[]).await;
        assert_eq!(
            r.status.as_u16(),
            expect,
            "{id} {uri}: unexpected status; body {}",
            r.body
        );
        self.contract.check(id, uri, &r);
        self.seen.insert((id.to_string(), expect));
        self.calls += 1;
        r
    }

    /// A `400` with problem code `code`.
    async fn bad(&mut self, app: &axum::Router, id: &str, uri: &str, code: &str) {
        let r = self.call(app, id, uri, 400).await;
        assert_eq!(r.code(), code, "{id} {uri}");
    }
}

/// Every operation, every documented status, every parameter class.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_operation_conforms_to_openapi() {
    let f = Fixture::seeded().await;
    let app = f.router(relaxed());
    let mut run = Run {
        contract: Contract::load(),
        seen: BTreeSet::new(),
        calls: 0,
    };
    let ops = run.contract.operations();
    assert_eq!(ops.len(), 31);
    for op in &ops {
        if op.id == "openWebSocket" {
            // Without the upgrade headers (a 101 needs a real connection: tests/api_ws.rs).
            run.bad(&app, &op.id, "/v1/ws", "bad_request").await;
            continue;
        }
        let uri = base(&op.id);
        run.call(&app, &op.id, &uri, 200).await;
        for p in run.contract.parameters(&op.id) {
            let name = p["name"].as_str().expect("name").to_string();
            let schema = run.contract.resolve(&p["schema"]).clone();
            let raw_schema = p["schema"].clone();
            match p["in"].as_str() {
                Some("path") => {
                    let template = op.path.replace(&format!("{{{name}}}"), "PLACEHOLDER");
                    let bad = template.replace("PLACEHOLDER", "0OIl");
                    let is_sig = raw_schema["$ref"] == "#/components/schemas/Signature";
                    let code = if is_sig {
                        "invalid_signature"
                    } else {
                        "invalid_pubkey"
                    };
                    run.bad(&app, &op.id, &bad, code).await;
                    let unknown = if is_sig { unknown_sig() } else { unknown_key() };
                    let path = template.replace("PLACEHOLDER", &unknown);
                    let query = uri
                        .split_once('?')
                        .map(|(_, q)| format!("?{q}"))
                        .unwrap_or_default();
                    let expect = if run.contract.statuses(&op.id).contains(&404) {
                        404
                    } else {
                        200
                    };
                    let r = run
                        .call(&app, &op.id, &format!("{path}{query}"), expect)
                        .await;
                    if expect == 404 {
                        assert_eq!(r.code(), "not_found");
                    }
                }
                Some("query") => {
                    let base_uri = without(&uri, &name);
                    if p["required"] == true {
                        run.bad(&app, &op.id, &base_uri, "bad_request").await;
                    }
                    if let Some(values) = schema["enum"].as_array() {
                        for v in values {
                            let v = v.as_str().expect("string enum");
                            let u = with(&base_uri, &format!("{name}={v}"));
                            run.call(&app, &op.id, &u, 200).await;
                        }
                        run.bad(
                            &app,
                            &op.id,
                            &with(&base_uri, &format!("{name}=zz_nope")),
                            "bad_request",
                        )
                        .await;
                    } else if schema["type"] == "boolean" {
                        for v in ["true", "false"] {
                            run.call(&app, &op.id, &with(&base_uri, &format!("{name}={v}")), 200)
                                .await;
                        }
                        run.bad(
                            &app,
                            &op.id,
                            &with(&base_uri, &format!("{name}=maybe")),
                            "bad_request",
                        )
                        .await;
                    } else if raw_schema["$ref"] == "#/components/schemas/Pubkey" {
                        let u = with(&base_uri, &format!("{name}={}", unknown_key()));
                        run.call(&app, &op.id, &u, 200).await;
                        run.bad(
                            &app,
                            &op.id,
                            &with(&base_uri, &format!("{name}=0OIl")),
                            "invalid_pubkey",
                        )
                        .await;
                    } else if name == "limit" {
                        let max = schema["maximum"].as_i64().expect("maximum");
                        for v in [1, max] {
                            run.call(&app, &op.id, &with(&base_uri, &format!("limit={v}")), 200)
                                .await;
                        }
                        // Out of range or not an integer: `limit_out_of_range` either way.
                        for v in ["0".to_string(), (max + 1).to_string(), "x".into()] {
                            let u = with(&base_uri, &format!("limit={v}"));
                            run.bad(&app, &op.id, &u, "limit_out_of_range").await;
                        }
                    } else if name == "from" || name == "to" {
                        // A valid one-hour window, then the invalid classes.
                        let (from, to) = (ts(1032), ts(1032) + 3600);
                        let other = if name == "from" { "to" } else { "from" };
                        let u = with(&without(&base_uri, other), &format!("from={from}&to={to}"));
                        run.call(&app, &op.id, &u, 200).await;
                        for v in ["-1", "x"] {
                            run.bad(
                                &app,
                                &op.id,
                                &with(&base_uri, &format!("{name}={v}")),
                                "bad_request",
                            )
                            .await;
                        }
                        let inverted =
                            with(&without(&base_uri, other), &format!("from={to}&to={from}"));
                        run.bad(&app, &op.id, &inverted, "bad_request").await;
                    } else if name == "cursor" {
                        run.bad(
                            &app,
                            &op.id,
                            &with(&base_uri, "cursor=garbage"),
                            "invalid_cursor",
                        )
                        .await;
                        let long = "a".repeat(257);
                        run.bad(
                            &app,
                            &op.id,
                            &with(&base_uri, &format!("cursor={long}")),
                            "invalid_cursor",
                        )
                        .await;
                    } else if let Some(max) = schema["maxLength"].as_u64() {
                        let long = "a".repeat(usize::try_from(max).expect("len") + 1);
                        run.bad(
                            &app,
                            &op.id,
                            &with(&base_uri, &format!("{name}={long}")),
                            "bad_request",
                        )
                        .await;
                        // A repeated parameter is ambiguous.
                        run.bad(
                            &app,
                            &op.id,
                            &with(&with(&base_uri, &format!("{name}=a")), &format!("{name}=b")),
                            "bad_request",
                        )
                        .await;
                    } else {
                        panic!("{}: parameter {name} has no test class", op.id);
                    }
                }
                Some("header") => assert_eq!(name, "Upgrade"),
                other => panic!("parameter location {other:?}"),
            }
        }
    }

    // Series limits (§10.2).
    run.bad(
        &app,
        "getMarketCandles",
        &format!(
            "/v1/markets/{TA}/candles?interval=1m&from=0&to={}",
            ts(1047)
        ),
        "range_too_large",
    )
    .await;
    run.bad(
        &app,
        "getMarketStats",
        &format!("/v1/markets/{TA}/stats?interval=1h&from=0&to={}", ts(1047)),
        "range_too_large",
    )
    .await;
    run.bad(
        &app,
        "getGlobalTimeseries",
        &format!(
            "/v1/stats/timeseries?metric=tvl&interval=1d&from=0&to={}",
            ts(1047)
        ),
        "range_too_large",
    )
    .await;
    // An unknown event kind.
    run.bad(
        &app,
        "listMarketEvents",
        &format!("/v1/markets/{TA}/events?kind=Open,Bogus"),
        "bad_request",
    )
    .await;

    // 429: a tightened limiter, one fresh client per operation (§10.1). `/v1/health` and
    // `/v1/status` are not rate-limited (the document lists no 429 for them).
    let tight = f.router(ApiConfig {
        anon_rps: 1,
        anon_burst: 1,
        ..ApiConfig::default()
    });
    for (i, op) in ops.iter().enumerate() {
        let peer = IpAddr::from([10, 1, 0, u8::try_from(i + 1).expect("ip")]);
        let uri = base(&op.id);
        let documented = run.contract.statuses(&op.id).contains(&429);
        let first = send(&tight, Method::GET, &uri, peer, &[]).await;
        let second = run
            .call_from(
                &tight,
                &op.id,
                &uri,
                if documented {
                    429
                } else {
                    first.status.as_u16()
                },
                peer,
            )
            .await;
        if documented {
            assert_eq!(second.code(), "rate_limited");
            assert!(second.header_i64("retry-after").unwrap_or(0) >= 1);
            assert_eq!(second.header_i64("ratelimit-remaining"), Some(0));
        }
    }

    // 503: the database is gone — every database-backed operation answers `unavailable` with
    // `Retry-After` (the document lists 503 on all of them; `/v1/health` does not touch it).
    let gone = f.api_pool(&relaxed());
    gone.close().await;
    let down = api::router(AppState::new(gone, relaxed()));
    for op in &ops {
        // The WebSocket's 503 is its process-wide connection cap (a real connection,
        // tests/security_api.rs); it touches no database before the upgrade.
        if op.id == "openWebSocket" || !run.contract.statuses(&op.id).contains(&503) {
            continue;
        }
        let uri = base(&op.id);
        let r = run.call(&down, &op.id, &uri, 503).await;
        assert_eq!(r.code(), "unavailable", "{} {uri}", op.id);
        assert!(r.header_i64("retry-after").is_some(), "{} {uri}", op.id);
    }
    let r = run.call(&down, "getHealth", "/v1/health", 200).await;
    assert_eq!(r.body["status"], "ok");

    // 404 of the configuration: nothing ingested yet.
    sqlx::query("DELETE FROM protocol_params_history")
        .execute(&f.admin)
        .await
        .expect("delete");
    let r = run
        .call(&app, "getProtocolConfig", "/v1/admin/config", 404)
        .await;
    assert_eq!(r.code(), "not_found");

    // Every documented (operation, status) was produced.
    let mut missing = Vec::new();
    for op in &ops {
        for s in run.contract.statuses(&op.id) {
            // A real connection produces these (tests/api_ws.rs, tests/security_api.rs: the
            // process-wide connection cap).
            if op.id == "openWebSocket" && (s == 101 || s == 503) {
                continue;
            }
            if !run.seen.contains(&(op.id.clone(), s)) {
                missing.push(format!("{} {s}", op.id));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "documented responses never exercised: {missing:?}"
    );
    println!(
        "{} responses checked against openapi.yaml; {} (operation, status) pairs",
        run.calls,
        run.seen.len()
    );
    f.stop().await;
}

/// The contract checks have teeth: bodies that break the schemas are rejected.
#[test]
fn the_validator_rejects_what_the_contract_forbids() {
    let c = Contract::load();
    let problem = c.component("Problem");
    assert!(problem.is_valid(&json!({
        "type": "about:blank", "title": "x", "status": 400, "code": "bad_request"
    })));
    assert!(!problem.is_valid(&json!({ "type": "about:blank", "title": "x", "status": 400 })));
    assert!(!problem.is_valid(&json!({
        "type": "about:blank", "title": "x", "status": 400, "code": "teapot"
    })));
    // Integers wider than 32 bits are strings (§4.2).
    let fee = c.component("FeeLine");
    let good = json!({
        "fee_type": "open_fee", "recipient": "protocol", "n": 1, "total": "5", "last_24h": "0"
    });
    assert!(fee.is_valid(&good));
    let mut bad = good.clone();
    bad["total"] = json!(5);
    assert!(!fee.is_valid(&bad));
    let mut bad = good.clone();
    bad.as_object_mut().expect("object").remove("n");
    assert!(!fee.is_valid(&bad), "`n` is required (YAML 1.2 reading)");
    // References resolve through allOf (ChainRef inside Event).
    let event = c.component("Event");
    let ev = json!({
        "signature": SIG_INIT, "slot": 1, "tx_index": 0, "ix_index": 0, "ts": 1,
        "finalized": true, "id": 1, "kind": "Open", "invocation": "open_short",
        "source": "cpi", "market": null, "data": {}
    });
    assert!(event.is_valid(&ev));
    let mut bad = ev.clone();
    bad["kind"] = json!("Teleport");
    assert!(!event.is_valid(&bad));
    let mut bad = ev;
    bad["signature"] = json!("0OIl");
    assert!(!event.is_valid(&bad));
    // WebSocket messages.
    let ws = c.component("WsServerMessage");
    assert!(ws.is_valid(&json!({ "type": "rollback", "slot": 5 })));
    assert!(ws.is_valid(&json!({ "type": "error", "code": "lagged", "message": "x" })));
    assert!(!ws.is_valid(&json!({ "type": "error", "code": "oops", "message": "x" })));
    assert!(!ws.is_valid(&json!({ "type": "subscribed", "channels": ["market:nope"] })));
}

/// The router serves exactly the paths of `openapi.yaml`, GET only.
#[tokio::test]
async fn router_serves_exactly_the_documented_paths() {
    let contract = Contract::load();
    let documented: BTreeSet<(String, String)> = contract
        .operations()
        .into_iter()
        .map(|o| (o.path, o.id))
        .collect();
    let served: BTreeSet<(String, String)> = ROUTES
        .iter()
        .map(|r| (r.path.to_string(), r.operation_id.to_string()))
        .collect();
    assert_eq!(served, documented);

    // No database is needed for routing (the slot headers fall back to 0).
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(200))
        .connect_lazy("postgres://nobody@127.0.0.1:9/none")
        .expect("lazy pool");
    let app = api::router(AppState::new(pool, relaxed()));
    for uri in [
        "/",
        "/v1",
        "/v1/nope",
        "/v2/markets",
        "/v1/markets/x/nope",
        "/v1/admin",
    ] {
        let r = get(&app, uri).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(r.header("content-type"), Some("application/problem+json"));
        assert_eq!(r.code(), "not_found");
        contract.validate(&contract.component("Problem"), &r.body, uri);
    }
    for m in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
        let r = send(&app, m.clone(), "/v1/health", localhost(), &[]).await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{m}");
        assert_eq!(r.header("content-type"), Some("application/problem+json"));
        contract.validate(&contract.component("Problem"), &r.body, "405");
    }
    let r = get(&app, "/v1/health").await;
    assert_eq!(r.body, json!({ "status": "ok" }));
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert!(r.header("x-request-id").is_some());
}

fn ids(r: &Resp, field: &str) -> Vec<String> {
    r.data()
        .iter()
        .map(|v| v[field].as_str().unwrap_or("").to_string())
        .collect()
}

fn u(v: &Value) -> u128 {
    v.as_str().expect("a decimal string").parse().expect("u128")
}

/// Responses carry the seeded values: filters, ordering and the analytics of §8.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_carry_the_seeded_values() {
    let f = Fixture::seeded().await;
    let app = f.router(relaxed());
    let c = Contract::load();

    // ---- meta and headers
    let r = get(&app, "/v1/status").await;
    assert_eq!(r.body["confirmed_slot"], 1047);
    assert_eq!(r.body["finalized_slot"], 1030);
    assert_eq!(r.body["latest_block_time"], ts(1047));
    assert_eq!(r.body["streams"].as_array().map(Vec::len), Some(2));
    assert_eq!(r.header_i64("x-indexer-slot"), Some(1047));
    assert_eq!(r.header_i64("x-indexer-finalized-slot"), Some(1030));

    // ---- markets: junk filtered, filters, sorts
    let r = get(&app, "/v1/markets").await;
    assert_eq!(ids(&r, "mint"), vec![TA, TB], "by TVL, junk hidden");
    assert_eq!(r.header("cache-control"), Some("public, max-age=2"));
    let r = get(&app, "/v1/markets?order=asc").await;
    assert_eq!(ids(&r, "mint"), vec![TB, TA]);
    let r = get(&app, "/v1/markets?q=junk").await;
    assert_eq!(ids(&r, "mint"), vec![TJ], "a query shows junk it matches");
    let r = get(&app, &format!("/v1/markets?q={TJ}")).await;
    assert_eq!(ids(&r, "mint"), vec![TJ]);
    let r = get(&app, "/v1/markets?venue=pump_swap").await;
    assert_eq!(ids(&r, "mint"), vec![TB]);
    let r = get(&app, "/v1/markets?tier=B").await;
    assert_eq!(ids(&r, "mint"), vec![TA]);
    let r = get(&app, "/v1/markets?has_positions=false").await;
    assert!(r.data().is_empty());
    let r = get(&app, "/v1/markets?sort=created").await;
    assert_eq!(ids(&r, "mint"), vec![TB, TA]);
    let r = get(&app, "/v1/markets?sort=open_positions&order=asc").await;
    assert_eq!(r.data()[0]["open_positions"], 1);
    let r = get(&app, "/v1/markets?q=_").await;
    assert!(r.data().is_empty(), "LIKE wildcards are escaped");

    // ---- one market (§8.1–§8.5)
    let r = get(&app, &format!("/v1/markets/{TA}")).await;
    let m = &r.body;
    let (t, b) = (u(&m["tokens_free"]), u(&m["borrowed_q"]));
    assert_eq!((t, b), (1_450_000_000, 100_000_000));
    let tvl = shortwin_indexer::analytics::formulas::tvl_lamports(
        u64::try_from(t).expect("u64"),
        P_A,
        u64::try_from(u(&m["lender_sol"])).expect("u64"),
        u64::try_from(u(&m["open_vault_lamports"])).expect("u64"),
    );
    assert_eq!(u(&m["tvl_lamports"]), tvl);
    assert_eq!(
        u(&m["oi_lamports"]),
        shortwin_indexer::analytics::formulas::oi_lamports(100_000_000, P_A)
    );
    assert_eq!(
        m["util_bps"],
        i64::from(shortwin_indexer::numeric::util_bps(
            1_450_000_000,
            100_000_000
        ))
    );
    let usd = m["tvl_usd"].as_f64().expect("tvl_usd");
    assert!(
        (usd - tvl as f64 * 150.0 / 1e9).abs() < 1e-6,
        "USD at the latest oracle price"
    );
    assert_eq!(m["venue"], "raydium_v4");
    assert_eq!(m["tier"], "B");
    assert_eq!(m["symbol"], "BONK2");
    assert_eq!(m["coin_creator"], COIN_CREATOR_A);
    assert_eq!(m["ray_pool"], POOL_A);
    assert_eq!(m["exits"]["positions"], 5);
    assert_eq!(m["exits"]["open"], 1);
    assert_eq!(m["exits"]["closes"], 1);
    assert_eq!(m["exits"]["liquidations_cr"], 1);
    assert_eq!(m["exits"]["settlements_s1"], 1);
    assert_eq!(m["exits"]["settlements_s3"], 1);
    assert_eq!(m["exits"]["exits"], 4);
    // S1: q 60e6 at P_A (box point) is worth 600,000; lenders got 500,000. S3: q 70e6 at the
    // last P_ema (clamped into the box) is worth 700,000; lenders got 600,000.
    assert_eq!(m["lender_loss"]["settlements"], 2);
    assert_eq!(m["lender_loss"]["realised_lender_loss"], "200000");
    assert_eq!(m["lender_loss"]["lender_pnl"], "-200000");
    assert_eq!(m["lender_loss"]["loss_events"], 2);
    assert_eq!(m["lender_loss"]["loss_bps_of_value"], 1538);
    // ANALYTICS' additive columns (v_market_exits, v_market_lender_loss, v_market_overview).
    assert_eq!(m["exits"]["close_rate_bps"], 2500);
    assert_eq!(m["exits"]["liquidation_rate_bps"], 2500);
    assert_eq!(m["exits"]["expiry_rate_bps"], 0);
    assert_eq!(m["exits"]["settlement_rate_bps"], 5000);
    assert!(m["exits"]["borrower_pnl"].is_string());
    assert_eq!(m["lender_loss"]["realised_lender_loss_s1"], "100000");
    assert_eq!(m["lender_loss"]["realised_lender_loss_s2"], "0");
    assert_eq!(m["lender_loss"]["realised_lender_loss_s3"], "100000");
    assert!(u(&m["inventory_value_lamports"]) >= u(&m["oi_lamports"]));
    assert_eq!(m["creator_exposure"]["coin_creator_share_bps"], 1515);
    assert_eq!(
        m["creator_exposure"]["coin_creator_deposited_tokens"],
        "250000000"
    );
    assert_eq!(m["coverage"]["gaps"], 1);
    assert_eq!(m["coverage"]["gap_s_total"], 55);
    assert_eq!(m["coverage"]["coverage_pauses"], 1);
    assert_eq!(m["coverage"]["coverage_paused_s"], 600);
    assert_eq!(m["tier_terms"]["l_bps"], 11_500);
    assert!(
        m["fees"]
            .as_array()
            .is_some_and(|f| f.iter().any(|l| l["fee_type"] == "creation_fee"))
    );
    let r = get(&app, &format!("/v1/markets/{TJ}")).await;
    assert_eq!(
        r.body["name"].as_str().map(|n| n.chars().count()),
        Some(64),
        "names are capped"
    );
    assert_eq!(r.body["ema_price"], Value::Null);
    assert_eq!(r.body["tier_terms"]["l_bps"], 12_500);

    // ---- positions
    let r = get(&app, &format!("/v1/markets/{TA}/positions")).await;
    assert_eq!(
        ids(&r, "position"),
        vec![P8, P4, P3, P2, P1],
        "newest first"
    );
    let r = get(&app, &format!("/v1/markets/{TA}/positions?status=open")).await;
    assert_eq!(ids(&r, "position"), vec![P1]);
    assert_eq!(r.data()[0]["exit"], Value::Null);
    assert_eq!(r.data()[0]["margin_added"], "100000");
    let r = get(&app, &format!("/v1/markets/{TA}/positions?status=exited")).await;
    assert_eq!(r.data().len(), 4);
    let r = get(&app, &format!("/v1/markets/{TA}/positions?borrower={B1}")).await;
    assert_eq!(ids(&r, "position"), vec![P2, P1]);
    let r = get(&app, &format!("/v1/borrowers/{B3}/positions")).await;
    assert_eq!(ids(&r, "position"), vec![P9, P8, P7, P6, P5]);
    let r = get(
        &app,
        &format!("/v1/borrowers/{B3}/positions?status=settled"),
    )
    .await;
    assert_eq!(ids(&r, "position"), vec![P8, P6]);
    assert_eq!(r.data()[0]["exit"]["settle_condition"], "S3");
    let r = get(&app, &format!("/v1/positions/{P4}")).await;
    assert_eq!(r.body["position"]["status"], "settled");
    let kinds: Vec<&str> = r.body["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|e| e["kind"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(kinds, vec!["Open", "SettleArmed", "SolSettle"]);
    assert_eq!(r.body["settlement"]["p_ref_source"], "box_point");
    assert_eq!(r.body["settlement"]["realised_lender_loss"], "100000");
    let r = get(&app, &format!("/v1/positions/{P1}")).await;
    assert_eq!(r.body["settlement"], Value::Null);
    assert_eq!(r.body["events"].as_array().map(Vec::len), Some(2));

    // ---- lenders (share_bps at the current epoch, §6.3)
    let r = get(&app, &format!("/v1/markets/{TA}/lenders")).await;
    assert_eq!(ids(&r, "owner"), vec![L1, L2, COIN_CREATOR_A]);
    let bps: Vec<i64> = r
        .data()
        .iter()
        .map(|l| l["share_bps"].as_i64().expect("bps"))
        .collect();
    assert_eq!(bps, vec![6060, 2424, 1515]);
    assert_eq!(r.data()[2]["is_coin_creator"], true);
    let r = get(&app, &format!("/v1/markets/{TB}/lenders")).await;
    assert_eq!(
        r.data()[0]["share_bps"],
        10_000,
        "converted across the rebase"
    );
    // FE-30: `epoch` splits by the lender's epoch against the market's share epoch (TB rolled
    // to 1 with L1 still at 0; TA never rolled).
    let r = get(&app, &format!("/v1/markets/{TB}/lenders?epoch=behind")).await;
    assert_eq!(ids(&r, "owner"), vec![L1]);
    let r = get(&app, &format!("/v1/markets/{TB}/lenders?epoch=current")).await;
    assert!(r.data().is_empty());
    let r = get(&app, &format!("/v1/markets/{TA}/lenders?epoch=behind")).await;
    assert!(r.data().is_empty());
    let r = get(&app, &format!("/v1/markets/{TA}/lenders?epoch=current")).await;
    assert_eq!(ids(&r, "owner"), vec![L1, L2, COIN_CREATOR_A]);
    let r = get(&app, &format!("/v1/markets/{TA}/lenders?epoch=late")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = get(&app, &format!("/v1/lenders/{L1}")).await;
    assert_eq!(r.body["totals"]["markets"], 2);
    assert_eq!(r.body["totals"]["sol_paid"], "15000");
    let r = get(&app, &format!("/v1/lenders/{}", unknown_key())).await;
    assert_eq!(r.body["positions"], json!([]));
    assert_eq!(r.body["totals"]["sol_paid"], "0");
    let r = get(&app, &format!("/v1/lenders/{L1}/events")).await;
    let kinds: Vec<String> = ids(&r, "kind");
    assert_eq!(kinds, vec!["ClaimSol", "Deposit", "Deposit"]);

    // ---- events
    let r = get(
        &app,
        &format!("/v1/markets/{TA}/events?kind=Open,Close,Open"),
    )
    .await;
    assert_eq!(r.data().len(), 6);
    let r = get(&app, &format!("/v1/markets/{TA}/events?kind=CoverageGap")).await;
    assert_eq!(r.data()[0]["source"], "log");
    assert_eq!(r.data()[0]["finalized"], true);
    let r = get(&app, "/v1/admin/events").await;
    assert_eq!(r.data().len(), 15);
    assert_eq!(r.data()[0]["kind"], "VenueRevalidated");
    assert_eq!(r.data()[0]["finalized"], false);

    // ---- exits and settlements (§8.3, §8.4)
    let r = get(&app, "/v1/liquidations").await;
    assert_eq!(ids(&r, "position"), vec![P8, P7, P6, P5, P4, P3]);
    let kinds = ids(&r, "kind");
    assert_eq!(
        kinds,
        vec![
            "settlement",
            "liquidation",
            "settlement",
            "expiry",
            "settlement",
            "liquidation"
        ]
    );
    assert_eq!(r.data()[1]["expired"], true);
    assert_eq!(r.data()[3]["expired"], true);
    assert_eq!(r.data()[4]["lender_amount"], "500000");
    assert_eq!(r.data()[4]["buyback_cost"], Value::Null);
    assert_eq!(r.data()[5]["buyback_cost"], "930000");
    assert_eq!(r.data()[5]["keeper_paid"], "5000");
    let r = get(&app, &format!("/v1/markets/{TA}/liquidations")).await;
    assert_eq!(ids(&r, "position"), vec![P8, P4, P3]);
    let r = get(&app, "/v1/settlements").await;
    assert_eq!(ids(&r, "position"), vec![P8, P6, P4]);
    let r = get(&app, "/v1/settlements?condition=S2").await;
    assert_eq!(ids(&r, "position"), vec![P6]);
    assert_eq!(r.data()[0]["p_ref_source"], "crank_clamped");
    let r = get(&app, &format!("/v1/settlements?mint={TA}")).await;
    assert_eq!(ids(&r, "position"), vec![P8, P4]);
    let r = get(&app, "/v1/keepers").await;
    assert_eq!(ids(&r, "keeper"), vec![K1, K2]);
    assert_eq!(r.data()[0]["rewards_lamports"], "9600");
    assert_eq!(r.data()[0]["markets"], 2);
    assert_eq!(r.data()[1]["rewards_lamports"], "3500");
    // K1 paid the failed `liquidate` at slot 1047 (v_keeper_failures).
    assert_eq!(r.data()[0]["failures"], 1);
    assert_eq!(r.data()[1]["failures"], 0);

    // ---- market detail lists
    let r = get(&app, &format!("/v1/markets/{TA}/pools")).await;
    assert_eq!(r.body["current"], POOL_A);
    assert_eq!(r.body["window"]["candidate"], POOL_X);
    assert_eq!(r.body["pools"].as_array().map(Vec::len), Some(2));
    assert_eq!(r.body["events"].as_array().map(Vec::len), Some(1));
    let r = get(&app, &format!("/v1/markets/{TB}/pools")).await;
    assert_eq!(r.body["window"], Value::Null);
    let r = get(&app, &format!("/v1/markets/{TA}/coverage")).await;
    assert_eq!(r.body["recent_gaps"][0]["gap_s"], 55);
    assert_eq!(r.body["pauses"][0]["paused_s"], 600);
    assert_eq!(r.body["pauses"][0]["ongoing"], false);
    assert_eq!(r.body["pauses"][0]["by"], COVAUTH);
    let r = get(&app, &format!("/v1/markets/{TA}/snapshots")).await;
    assert_eq!(r.data().len(), 1);
    assert_eq!(r.data()[0]["ema_price"], P_A.to_string());
    let r = get(
        &app,
        &format!("/v1/markets/{TA}/snapshots?from={}", ts(1009)),
    )
    .await;
    assert!(r.data().is_empty());
    let r = get(&app, &format!("/v1/markets/{TA}/trades")).await;
    assert_eq!(r.data().len(), 10);
    let r = get(&app, &format!("/v1/markets/{TA}/trades?shortwin_only=true")).await;
    assert_eq!(r.data().len(), 5);
    assert!(
        r.data()
            .iter()
            .all(|t| t["is_shortwin"] == true && t["side"] == "sell")
    );
    let r = get(&app, &format!("/v1/markets/{TA}/fees")).await;
    let lines = r.data();
    let open = lines
        .iter()
        .find(|l| l["fee_type"] == "open_fee")
        .expect("open_fee");
    assert_eq!(open["n"], 5);
    assert_eq!(open["total"], "18000");

    // Candles (§9.2): the hour of slots 1032–1033 holds three third-party trades.
    let hour = ts(1032) / 3600 * 3600;
    let r = get(
        &app,
        &format!(
            "/v1/markets/{TA}/candles?interval=1h&from={hour}&to={}",
            hour + 3600
        ),
    )
    .await;
    assert_eq!(r.body["interval"], "1h");
    let k = &r.data()[0];
    assert_eq!(k["t"], hour);
    assert_eq!(k["trades"], 3);
    assert_eq!(u(&k["open"]), 100_000 * WAD / 10_000_000);
    assert_eq!(u(&k["close"]), 124_000 * WAD / 12_000_000);
    assert_eq!(u(&k["high"]), 124_000 * WAD / 12_000_000);
    assert_eq!(u(&k["low"]), 100_000 * WAD / 10_000_000);
    assert_eq!(k["volume_sol"], "336000");
    assert_eq!(k["volume_tokens"], "33000000");
    let r = get(&app, &format!("/v1/markets/{TA}/candles?interval=1d")).await;
    assert_eq!(r.data().len(), 1);
    assert_eq!(r.data()[0]["trades"], 10);

    // Time series (§8.6).
    let r = get(&app, &format!("/v1/markets/{TA}/stats")).await;
    assert_eq!(r.data().len(), 8);
    let r = get(&app, &format!("/v1/markets/{TA}/stats?interval=1d")).await;
    assert_eq!(r.body["interval"], "1d");
    assert_eq!(r.data().len(), 1);
    assert_eq!(r.data()[0]["coverage_gap_s"], 55);
    let r = get(&app, "/v1/stats/timeseries?metric=coverage_gap_s").await;
    assert_eq!(r.body["points"].as_array().map(Vec::len), Some(8));
    let r = get(&app, "/v1/stats/timeseries?metric=tvl&interval=1d").await;
    assert_eq!(r.body["points"].as_array().map(Vec::len), Some(1));
    for metric in ["inventory_value", "lender_sol", "deposits", "observations"] {
        for interval in ["1h", "1d"] {
            let r = get(
                &app,
                &format!("/v1/stats/timeseries?metric={metric}&interval={interval}"),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{metric} {interval}");
            assert!(
                !r.body["points"].as_array().expect("points").is_empty(),
                "{metric} {interval}"
            );
        }
    }

    // ---- global (§8.7)
    let r = get(&app, "/v1/stats").await;
    let g = &r.body;
    assert_eq!(g["markets"], 3);
    assert_eq!(g["markets_with_positions"], 2);
    assert_eq!(g["open_positions"], 2);
    assert_eq!(
        (
            g["closes"].clone(),
            g["liquidations"].clone(),
            g["expiries"].clone()
        ),
        (json!(1), json!(2), json!(1))
    );
    assert_eq!(
        (
            g["settlements_s1"].clone(),
            g["settlements_s2"].clone(),
            g["settlements_s3"].clone()
        ),
        (json!(1), json!(1), json!(1))
    );
    assert_eq!(g["unique_lenders"], 3);
    assert_eq!(g["unique_borrowers"], 3);
    assert_eq!(g["repeat_borrowers"], 3);
    assert_eq!(g["repeat_lenders"], 1);
    assert_eq!(g["sol_usd"], 150.0);

    // ---- admin (§6.6)
    let r = get(&app, "/v1/admin/config").await;
    assert_eq!(r.body["source"], "apply_config");
    assert_eq!(r.body["params"]["rent_protocol_bps"], 2000);
    assert_eq!(r.body["venue_enabled"], 59);
    assert_eq!(r.body["paused_opens"], true);
    assert_eq!(r.body["route_broken_bits"], 2);
    let r = get(&app, "/v1/admin/changes").await;
    assert_eq!(
        ids(&r, "kind"),
        vec!["Kink", "Timelock", "Params", "EmaParams"]
    );
    assert_eq!(
        ids(&r, "status"),
        vec!["cancelled", "pending", "applied", "superseded"]
    );
    assert_eq!(r.data()[0]["cancelled"]["by"], GOV);
    assert_eq!(r.data()[2]["applied"]["slot"], 1034);
    let r = get(&app, "/v1/admin/changes?status=pending").await;
    assert_eq!(ids(&r, "kind"), vec!["Timelock"]);
    let r = get(&app, "/v1/venues").await;
    let names: Vec<&str> = r.body["guards"]
        .as_array()
        .expect("guards")
        .iter()
        .map(|g| g["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(names, vec!["pump_amm", "raydium_v4"]);
    assert_eq!(r.body["guards"][0]["upgraded"], true);
    assert_eq!(r.body["extras"][0]["keys"], json!([X1, X2]));
    assert_eq!(r.body["admin_changes"][0]["observed_slot"], 1035);

    // ---- search and transactions
    let r = get(&app, &format!("/v1/search?q={TA}")).await;
    assert_eq!(r.data()[0]["match"], "mint");
    let r = get(&app, "/v1/search?q=bOnK").await;
    assert_eq!(ids(&r, "mint"), vec![TA]);
    assert_eq!(r.data()[0]["match"], "symbol");
    let r = get(&app, "/v1/search?q=dog").await;
    assert_eq!(r.data()[0]["match"], "name");
    let r = get(&app, "/v1/search?q=junk_").await;
    assert_eq!(ids(&r, "mint"), vec![TJ]);
    let r = get(&app, "/v1/search?q=%25").await;
    assert!(r.data().is_empty());
    let r = get(&app, &format!("/v1/tx/{SIG_FAILED}")).await;
    assert_eq!(r.body["ok"], false);
    assert_eq!(r.body["err_code"], 6012);
    assert_eq!(r.body["commitment"], "confirmed");
    assert_eq!(r.body["invocations"][0]["name"], "liquidate");
    assert_eq!(r.header("cache-control"), Some("public, max-age=2"));
    let r = get(&app, &format!("/v1/tx/{SIG_CLOSE}")).await;
    assert_eq!(r.body["events"][0]["kind"], "Close");
    assert_eq!(
        r.header("cache-control"),
        Some("public, max-age=3600"),
        "finalized"
    );
    assert_eq!(r.body["venue_trades"], json!([]));

    // Everything above conforms too.
    c.validate(
        &c.component("GlobalStats"),
        &get(&app, "/v1/stats").await.body,
        "stats",
    );
    f.stop().await;
}

/// Walks a list with small pages and compares it with one big page.
async fn walk(app: &axum::Router, uri: &str, limit: u32) -> Vec<Value> {
    let mut out = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..100 {
        let mut u = with(uri, &format!("limit={limit}"));
        if let Some(c) = &next {
            u = with(&u, &format!("cursor={c}"));
        }
        let r = get(app, &u).await;
        assert_eq!(r.status, StatusCode::OK, "{u}: {}", r.body);
        assert!(r.data().len() <= usize::try_from(limit).expect("limit"));
        out.extend(r.data().iter().cloned());
        match r.body["next_cursor"].as_str() {
            Some(c) => next = Some(c.to_string()),
            None => return out,
        }
    }
    panic!("{uri}: no last page");
}

/// Keyset pagination returns every row exactly once, at every page size; cursors are bound to
/// their endpoint (and, for markets, to the sort).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_page_without_gaps_or_repeats() {
    let f = Fixture::seeded().await;
    let app = f.router(relaxed());
    let lists = [
        "/v1/markets".to_string(),
        "/v1/markets?sort=oi&order=asc".to_string(),
        "/v1/markets?sort=util".to_string(),
        "/v1/markets?q=j".to_string(),
        format!("/v1/markets/{TA}/positions"),
        format!("/v1/markets/{TA}/lenders"),
        format!("/v1/markets/{TA}/lenders?epoch=current"),
        format!("/v1/markets/{TB}/lenders?epoch=behind"),
        format!("/v1/markets/{TA}/liquidations"),
        format!("/v1/markets/{TA}/events"),
        format!("/v1/markets/{TA}/snapshots"),
        format!("/v1/markets/{TA}/trades"),
        format!("/v1/borrowers/{B3}/positions"),
        format!("/v1/lenders/{L1}/events"),
        "/v1/settlements".to_string(),
        "/v1/liquidations".to_string(),
        "/v1/keepers".to_string(),
        "/v1/admin/changes".to_string(),
        "/v1/admin/events".to_string(),
    ];
    for uri in &lists {
        let all = walk(&app, uri, 500).await;
        assert!(!all.is_empty(), "{uri}: the seed has rows");
        for limit in [1, 2, 3] {
            assert_eq!(
                walk(&app, uri, limit).await,
                all,
                "{uri} with limit={limit}"
            );
        }
    }
    // A cursor from one list is refused by another, and by the same list sorted differently.
    let r = get(&app, &format!("/v1/markets/{TA}/events?limit=1")).await;
    let c = r.body["next_cursor"].as_str().expect("cursor").to_string();
    for other in [
        format!("/v1/markets/{TA}/trades?cursor={c}"),
        format!("/v1/markets/{TA}/positions?cursor={c}"),
        format!("/v1/settlements?cursor={c}"),
    ] {
        let r = get(&app, &other).await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (400, "invalid_cursor"),
            "{other}"
        );
    }
    let r = get(&app, "/v1/markets?limit=1").await;
    let c = r.body["next_cursor"].as_str().expect("cursor").to_string();
    let r = get(&app, &format!("/v1/markets?sort=oi&cursor={c}")).await;
    assert_eq!(r.code(), "invalid_cursor");
    // The same cursor with the same sort works.
    let r = get(&app, &format!("/v1/markets?cursor={c}")).await;
    assert_eq!(r.status, StatusCode::OK);
    f.stop().await;
}

/// The SQL names of `0007_api.sql` are `model`'s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sql_names_match_the_model() {
    let f = Fixture::empty().await;
    let rows: Vec<(i16, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT x.v, sw_api_venue(x.v), sw_api_tier(x.v), sw_api_guard(x.v), \
         sw_api_condition(x.v) FROM (SELECT g::smallint AS v FROM generate_series(0, 255) g) x",
    )
    .fetch_all(&f.admin)
    .await
    .expect("names");
    assert_eq!(rows.len(), 256);
    for (v, venue, tier, guard, cond) in rows {
        let b = u8::try_from(v).expect("u8");
        assert_eq!(venue, shortwin_indexer::model::venue_name(b));
        assert_eq!(tier, shortwin_indexer::model::tier_name(b));
        assert_eq!(guard, shortwin_indexer::model::guard_name(b));
        let expect = match b {
            1..=3 => Some(format!("S{b}")),
            _ => None,
        };
        assert_eq!(cond, expect);
    }
    let null: Option<String> = sqlx::query_scalar("SELECT sw_api_venue(NULL)")
        .fetch_one(&f.admin)
        .await
        .expect("null");
    assert_eq!(null, None);
    f.stop().await;
}
