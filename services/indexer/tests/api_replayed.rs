//! The API contract on replayed sessions (INDEXER.md §11.2, §13.5 join 2; ARCH §9 M8 exit "API
//! contract tests"). Integrator-owned; it uses the API track's `api_support` checker.
//!
//! Every committed LiteSVM session (`tests/data/sessions/`, REPLAY's, and `tests/data/ingest/`,
//! INGEST's) is ingested into a fresh embedded-Postgres database through the real pipeline
//! (`ingest::ingest_session`, then `analytics::rollups::run_once`), and the API is called on it:
//! - every REST operation answers `200` and its body conforms to `openapi.yaml`, for every
//!   market, position, borrower, lender and keeper the session has, every enumerated filter and
//!   both series intervals; across the sessions every operation is exercised;
//! - every list pages without gaps or repeats (small pages = one big page);
//! - the served state equals the chain's at the session's end: each `Market` (T, ΣB_Q, S, the
//!   share epoch, open positions, notional) and its `MarketVault` (`lender_sol`,
//!   `protocol_fees`), each live `ShortPosition` (`q`, `s`, `v`, `m` = open margin + top-ups)
//!   and each `LenderPosition` (shares, epoch, lock), decoded from the final checkpoint; a
//!   closed position is served with an exit status, and each market's open-positions list has
//!   `open_positions` rows;
//! - the transactions are served by signature (`/v1/tx`), failed ones with their error.

mod api_support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use api_support::*;
use base64::Engine as _;
use serde_json::Value;
use shortwin_indexer::analytics::rollups;
use shortwin_indexer::ingest::decode::idl;
use shortwin_indexer::ingest::raw::Commitment;
use shortwin_indexer::ingest::rpc;
use shortwin_indexer::ingest::session::{self, Session};
use shortwin_indexer::testing::pg::TestPg;

/// The committed sessions, sorted by path.
fn sessions() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut out = Vec::new();
    for dir in ["sessions", "ingest"] {
        for e in std::fs::read_dir(root.join(dir)).expect("tests/data") {
            let p = e.expect("entry").path();
            if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    assert!(out.len() >= 2, "the committed sessions are missing");
    out
}

/// The end state's short.win accounts, decoded through the IDL: type → pubkey → JSON.
fn end_accounts(s: &Session) -> Option<BTreeMap<String, BTreeMap<String, Value>>> {
    let (k, cp) = s.checkpoints.last()?;
    if *k != s.txs.len() {
        return None;
    }
    let mut out: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for a in &cp.accounts {
        if a.owner != s.header.program_id {
            continue;
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(&a.data)
            .expect("base64");
        if let Ok((ty, json)) = idl::decode_account(&data) {
            out.entry(ty).or_default().insert(a.pubkey.clone(), json);
        }
    }
    Some(out)
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn int(v: &Value) -> u128 {
    text(v)
        .parse()
        .unwrap_or_else(|_| panic!("not an integer: {v}"))
}

/// Calls and checks one response against the contract; records the operation.
struct Run<'a> {
    app: &'a axum::Router,
    contract: &'a Contract,
    seen: &'a mut BTreeSet<String>,
    calls: usize,
}

impl Run<'_> {
    async fn ok(&mut self, id: &str, uri: &str) -> Resp {
        let r = get(self.app, uri).await;
        assert_eq!(r.status.as_u16(), 200, "{id} {uri}: {}", r.body);
        self.contract.check(id, uri, &r);
        self.seen.insert(id.to_string());
        self.calls += 1;
        r
    }

    /// Every page of a list (small pages), checked; equal to one big page.
    async fn list(&mut self, id: &str, uri: &str) -> Vec<Value> {
        let big = self.walk(id, uri, 500).await;
        assert_eq!(
            self.walk(id, uri, 7).await,
            big,
            "{uri}: paging with limit=7"
        );
        big
    }

    async fn walk(&mut self, id: &str, uri: &str, limit: u32) -> Vec<Value> {
        let sep = if uri.contains('?') { '&' } else { '?' };
        let mut out = Vec::new();
        let mut next: Option<String> = None;
        for _ in 0..1_000 {
            let mut u = format!("{uri}{sep}limit={limit}");
            if let Some(c) = &next {
                u = format!("{u}&cursor={c}");
            }
            let r = self.ok(id, &u).await;
            out.extend(r.data().iter().cloned());
            match r.body["next_cursor"].as_str() {
                Some(c) => next = Some(c.to_string()),
                None => return out,
            }
        }
        panic!("{uri}: no last page");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_operation_conforms_on_replayed_sessions_and_serves_the_chain_state() {
    let pg = TestPg::start_in(Path::new(env!("CARGO_TARGET_TMPDIR")))
        .await
        .expect("the embedded PostgreSQL starts");
    let contract = Contract::load();
    let mut seen = BTreeSet::new();
    let mut calls = 0usize;
    let mut compared = 0usize;
    for path in sessions() {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        let s = session::read(&path).expect("session");
        let pool = pg.fresh_db().await.expect("db");
        let rep = shortwin_indexer::ingest::ingest_session(&pool, &s, 0..s.txs.len())
            .await
            .unwrap_or_else(|e| panic!("{name}: ingest: {e:?}"));
        assert_eq!(rep.quarantined, 0, "{name}: quarantined rows");
        rollups::run_once(&pool, None).await.expect("rollups");
        let cfg = relaxed();
        let app = shortwin_indexer::api::router(shortwin_indexer::api::AppState::new(
            shortwin_indexer::api::api_pool(&pool, &cfg),
            cfg,
        ));
        let mut run = Run {
            app: &app,
            contract: &contract,
            seen: &mut seen,
            calls: 0,
        };

        // ---- protocol-wide
        run.ok("getHealth", "/v1/health").await;
        let st = run.ok("getStatus", "/v1/status").await;
        assert!(st.body.is_object());
        run.ok("getGlobalStats", "/v1/stats").await;
        for interval in ["1h", "1d"] {
            for metric in [
                "tvl",
                "oi",
                "fees_protocol",
                "realised_lender_loss",
                "lender_sol",
            ] {
                run.ok(
                    "getGlobalTimeseries",
                    &format!("/v1/stats/timeseries?metric={metric}&interval={interval}"),
                )
                .await;
            }
        }
        for sort in ["tvl", "oi", "util", "created", "open_positions"] {
            run.list("listMarkets", &format!("/v1/markets?sort={sort}"))
                .await;
        }
        let settlements = run.list("listSettlements", "/v1/settlements").await;
        run.list("listLiquidations", "/v1/liquidations").await;
        run.list("listKeepers", "/v1/keepers").await;
        run.ok("getProtocolConfig", "/v1/admin/config").await;
        run.list("listConfigChanges", "/v1/admin/changes").await;
        run.list("listAdminEvents", "/v1/admin/events").await;
        run.ok("getVenues", "/v1/venues").await;

        // ---- per market
        let markets: Vec<(String, String)> =
            sqlx::query_as("SELECT market::text, mint::text FROM markets ORDER BY market")
                .fetch_all(&pool)
                .await
                .expect("markets");
        let served: BTreeSet<String> = run
            .list("listMarkets", "/v1/markets")
            .await
            .iter()
            .map(|m| text(&m["mint"]))
            .collect();
        for (_, mint) in &markets {
            // The junk filter (§10.3) may hide an empty market from the plain list; a mint
            // query always finds it.
            if !served.contains(mint) {
                let r = run
                    .ok("listMarkets", &format!("/v1/markets?q={mint}"))
                    .await;
                assert_eq!(text(&r.data()[0]["mint"]), *mint, "{name}");
            }
            let r = run.ok("search", &format!("/v1/search?q={mint}")).await;
            assert_eq!(text(&r.data()[0]["mint"]), *mint, "{name}: search by mint");
            let detail = run.ok("getMarket", &format!("/v1/markets/{mint}")).await;
            run.list(
                "listMarketPositions",
                &format!("/v1/markets/{mint}/positions"),
            )
            .await;
            let share_epoch = detail.body["share_epoch"].as_i64().expect("share_epoch");
            for active in ["true", "false"] {
                let uri = format!("/v1/markets/{mint}/lenders?active={active}");
                let all = run.list("listMarketLenders", &uri).await;
                // FE-30: `epoch=behind` / `current` split the list by the lender's epoch
                // against the market's share epoch, in the same order.
                let behind = run
                    .list("listMarketLenders", &format!("{uri}&epoch=behind"))
                    .await;
                let current = run
                    .list("listMarketLenders", &format!("{uri}&epoch=current"))
                    .await;
                let is_behind = |l: &Value| l["epoch"].as_i64().expect("epoch") < share_epoch;
                let want_behind: Vec<Value> =
                    all.iter().filter(|l| is_behind(l)).cloned().collect();
                let want_current: Vec<Value> =
                    all.iter().filter(|l| !is_behind(l)).cloned().collect();
                assert_eq!(behind, want_behind, "{name}: {uri}&epoch=behind");
                assert_eq!(current, want_current, "{name}: {uri}&epoch=current");
            }
            run.ok("getMarketPools", &format!("/v1/markets/{mint}/pools"))
                .await;
            run.ok("getMarketCoverage", &format!("/v1/markets/{mint}/coverage"))
                .await;
            run.list(
                "listMarketLiquidations",
                &format!("/v1/markets/{mint}/liquidations"),
            )
            .await;
            let events = run
                .list("listMarketEvents", &format!("/v1/markets/{mint}/events"))
                .await;
            let in_db: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM events e JOIN markets m ON m.market = e.market \
                 WHERE m.mint = $1",
            )
            .bind(mint)
            .fetch_one(&pool)
            .await
            .expect("count");
            assert_eq!(
                events.len() as i64,
                in_db,
                "{name} {mint}: every event served"
            );
            run.list(
                "listMarketSnapshots",
                &format!("/v1/markets/{mint}/snapshots"),
            )
            .await;
            run.list("listMarketTrades", &format!("/v1/markets/{mint}/trades"))
                .await;
            for interval in ["1m", "5m", "15m", "1h", "4h", "1d"] {
                run.ok(
                    "getMarketCandles",
                    &format!("/v1/markets/{mint}/candles?interval={interval}"),
                )
                .await;
            }
            for interval in ["1h", "1d"] {
                run.ok(
                    "getMarketStats",
                    &format!("/v1/markets/{mint}/stats?interval={interval}"),
                )
                .await;
            }
            run.ok("getMarketFees", &format!("/v1/markets/{mint}/fees"))
                .await;
        }

        // ---- positions, borrowers, lenders
        let positions: Vec<(String, String)> =
            sqlx::query_as("SELECT position::text, borrower::text FROM positions ORDER BY 1")
                .fetch_all(&pool)
                .await
                .expect("positions");
        for (p, _) in &positions {
            let r = run.ok("getPosition", &format!("/v1/positions/{p}")).await;
            assert_eq!(text(&r.body["position"]["position"]), *p);
        }
        let borrowers: BTreeSet<&String> = positions.iter().map(|(_, b)| b).collect();
        for b in borrowers {
            run.list(
                "listBorrowerPositions",
                &format!("/v1/borrowers/{b}/positions"),
            )
            .await;
        }
        let owners: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT owner::text FROM lender_positions ORDER BY 1")
                .fetch_all(&pool)
                .await
                .expect("lenders");
        for o in &owners {
            run.ok("getLender", &format!("/v1/lenders/{o}")).await;
            run.list("listLenderEvents", &format!("/v1/lenders/{o}/events"))
                .await;
        }
        for st in &settlements {
            let p = text(&st["position"]);
            assert!(
                positions.iter().any(|(x, _)| *x == p),
                "{name}: settlement of an unknown position {p}"
            );
        }
        for c in ["S1", "S2", "S3"] {
            run.list("listSettlements", &format!("/v1/settlements?condition={c}"))
                .await;
        }

        // ---- transactions
        let sigs: Vec<(String, bool)> = sqlx::query_as(
            "SELECT signature::text, err IS NOT NULL FROM ingest_txs ORDER BY slot, tx_index",
        )
        .fetch_all(&pool)
        .await
        .expect("txs");
        // Every distinct transaction is stored once (a recording may hold a resent duplicate).
        let distinct: BTreeSet<String> = s
            .txs
            .iter()
            .map(|t| {
                rpc::to_raw_tx(&t.transaction, t.tx_index, Commitment::Finalized)
                    .expect("to_raw_tx")
                    .signature
                    .to_string()
            })
            .collect();
        assert_eq!(
            sigs.iter().map(|(x, _)| x.clone()).collect::<BTreeSet<_>>(),
            distinct,
            "{name}: every transaction stored"
        );
        for (i, (sig, failed)) in sigs.iter().enumerate() {
            if !*failed && i % 25 != 0 {
                continue;
            }
            let r = run.ok("getTransaction", &format!("/v1/tx/{sig}")).await;
            assert_eq!(r.body["ok"], Value::Bool(!*failed), "{name} {sig}");
        }

        // ---- the chain's end state, through the API
        if let Some(acc) = end_accounts(&s) {
            let none = BTreeMap::new();
            let vaults = acc.get("MarketVault").unwrap_or(&none);
            for (market, m) in acc.get("Market").unwrap_or(&none) {
                let mint = text(&m["mint"]);
                let r = run.ok("getMarket", &format!("/v1/markets/{mint}")).await;
                let b = &r.body;
                assert_eq!(text(&b["market"]), *market, "{name}");
                for (api, chain) in [
                    ("tokens_free", "tokens_free"),
                    ("borrowed_q", "borrowed_q"),
                    ("total_shares", "total_shares"),
                    ("share_epoch", "share_epoch"),
                    ("open_positions", "open_positions"),
                    ("total_notional_open", "total_notional_open"),
                ] {
                    assert_eq!(
                        int(&b[api]),
                        int(&m[chain]),
                        "{name} {mint}: {api} served vs the chain"
                    );
                }
                let vault = vaults
                    .get(&text(&m["market_vault"]))
                    .unwrap_or_else(|| panic!("{name}: the vault of {market}"));
                for f in ["lender_sol", "protocol_fees"] {
                    assert_eq!(int(&b[f]), int(&vault[f]), "{name} {mint}: {f}");
                }
                let open = run
                    .list(
                        "listMarketPositions",
                        &format!("/v1/markets/{mint}/positions?status=open"),
                    )
                    .await;
                assert_eq!(
                    open.len() as u128,
                    int(&m["open_positions"]),
                    "{name} {mint}: open positions listed"
                );
                compared += 1;
            }
            for (p, sp) in acc.get("ShortPosition").unwrap_or(&none) {
                let r = run.ok("getPosition", &format!("/v1/positions/{p}")).await;
                let pos = &r.body["position"];
                for f in ["q", "s", "v"] {
                    assert_eq!(int(&pos[f]), int(&sp[f]), "{name} {p}: {f}");
                }
                assert_eq!(
                    int(&pos["m"]) + int(&pos["margin_added"]),
                    int(&sp["m"]),
                    "{name} {p}: m = open margin + top-ups"
                );
                compared += 1;
            }
            let closed: BTreeSet<&String> = s
                .checkpoints
                .last()
                .map(|(_, c)| c.closed.iter().collect())
                .unwrap_or_default();
            for (p, _) in &positions {
                if closed.contains(p) {
                    let r = run.ok("getPosition", &format!("/v1/positions/{p}")).await;
                    assert_ne!(
                        r.body["position"]["status"], "open",
                        "{name} {p}: a closed account is an exited position"
                    );
                }
            }
            for lp in acc.get("LenderPosition").unwrap_or(&none).values() {
                let owner = text(&lp["owner"]);
                let r = run.ok("getLender", &format!("/v1/lenders/{owner}")).await;
                let row = r.body["positions"]
                    .as_array()
                    .expect("positions")
                    .iter()
                    .find(|x| x["market"] == lp["market"])
                    .unwrap_or_else(|| panic!("{name}: lender {owner} in {}", lp["market"]));
                for f in ["shares", "epoch", "lock", "lock_until"] {
                    assert_eq!(int(&row[f]), int(&lp[f]), "{name} lender {owner}: {f}");
                }
                compared += 1;
            }
        }
        calls += run.calls;
        pool.close().await;
        eprintln!(
            "{name}: {} transactions, {} markets",
            s.txs.len(),
            markets.len()
        );
    }
    // Every REST operation was exercised with a 200 on replayed data (the WebSocket is
    // tests/api_ws.rs's).
    let missing: Vec<String> = contract
        .operations()
        .into_iter()
        .map(|o| o.id)
        .filter(|id| id != "openWebSocket" && !seen.contains(id))
        .collect();
    assert!(missing.is_empty(), "operations never called: {missing:?}");
    assert!(compared > 0, "no end state compared");
    println!(
        "{calls} responses checked against openapi.yaml on replayed sessions; {compared} \
         accounts compared with the chain through the API"
    );
    pg.stop().await.expect("stop");
}
