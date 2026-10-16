//! WebSocket tests (INDEXER.md §9.3, §10.1, §11.2). Owner: API.
//!
//! A real server on 127.0.0.1:0 (the upgrade needs a connection), clients from
//! `tokio-tungstenite`, the embedded PostgreSQL seeded from `tests/data/api_seed.sql`; new
//! events are inserted and announced with the writer's `NOTIFY` as the ingest would. Every
//! server message is validated against `WsServerMessage`, every well-formed client message
//! against `WsClientMessage`.

mod api_support;

use std::net::SocketAddr;
use std::time::Duration;

use api_support::*;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use shortwin_indexer::api::ApiConfig;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn serve(f: &Fixture, cfg: ApiConfig) -> SocketAddr {
    let app = f.router(cfg);
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

async fn connect(addr: SocketAddr) -> Ws {
    tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws"))
        .await
        .expect("the upgrade succeeds")
        .0
}

struct Client {
    ws: Ws,
    contract: Contract,
    server: jsonschema::Validator,
    client: jsonschema::Validator,
}

/// What the server sent: a JSON message or a close frame.
#[derive(Debug)]
enum Got {
    Msg(Value),
    Closed(Option<u16>),
}

impl Client {
    async fn new(addr: SocketAddr) -> Self {
        let contract = Contract::load();
        let server = contract.component("WsServerMessage");
        let client = contract.component("WsClientMessage");
        Self {
            ws: connect(addr).await,
            contract,
            server,
            client,
        }
    }

    /// Sends a message that conforms to `WsClientMessage`.
    async fn send(&mut self, v: Value) {
        self.contract.validate(&self.client, &v, "client message");
        self.send_raw(&v.to_string()).await;
    }

    async fn send_raw(&mut self, s: &str) {
        self.ws
            .send(Message::Text(s.to_string().into()))
            .await
            .expect("send");
    }

    /// The next server message within `secs`.
    async fn next(&mut self, secs: u64) -> Got {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(secs), self.ws.next())
                .await
                .expect("a server message in time");
            match m {
                Some(Ok(Message::Text(t))) => {
                    let v: Value = serde_json::from_str(t.as_str()).expect("JSON");
                    self.contract.validate(&self.server, &v, "server message");
                    return Got::Msg(v);
                }
                Some(Ok(Message::Close(f))) => return Got::Closed(f.map(|f| u16::from(f.code))),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(other)) => panic!("unexpected frame {other:?}"),
                Some(Err(_)) | None => return Got::Closed(None),
            }
        }
    }

    async fn msg(&mut self) -> Value {
        match self.next(5).await {
            Got::Msg(v) => v,
            Got::Closed(c) => panic!("closed ({c:?}) while a message was expected"),
        }
    }

    /// The next message other than heartbeats and stats.
    async fn msg_skipping(&mut self, skip: &[&str]) -> Value {
        loop {
            let v = self.msg().await;
            if !skip.iter().any(|t| v["type"] == *t) {
                return v;
            }
        }
    }

    /// Subscribes and returns the acknowledgement.
    async fn subscribe(&mut self, channels: &[&str], since: Option<i64>) -> Value {
        let mut m = json!({ "op": "subscribe", "channels": channels });
        if let Some(s) = since {
            m["since"] = json!(s);
        }
        self.send(m).await;
        self.msg_skipping(&["heartbeat"]).await
    }
}

/// Acknowledgements, ping/pong, heartbeats, and every `error` class of a bad request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_pings_heartbeats_and_errors() {
    let f = Fixture::seeded().await;
    let addr = serve(
        &f,
        ApiConfig {
            ws_heartbeat_s: 1,
            ws_max_subscriptions: 3,
            ws_max_inbound_per_s: 100,
            ..relaxed()
        },
    )
    .await;
    let mut c = Client::new(addr).await;
    c.send(json!({ "op": "ping" })).await;
    assert_eq!(
        c.msg_skipping(&["heartbeat"]).await,
        json!({ "type": "pong" })
    );

    let market = format!("market:{TA}");
    let ack = c.subscribe(&["markets", &market, &market], None).await;
    assert_eq!(
        ack,
        json!({ "type": "subscribed", "channels": ["markets", market] })
    );

    for (bad, why) in [
        (
            json!({ "op": "subscribe", "channels": ["market:nope"] }).to_string(),
            "a bad key",
        ),
        (
            json!({ "op": "subscribe", "channels": ["pools"] }).to_string(),
            "an unknown channel",
        ),
        (
            json!({ "op": "subscribe", "channels": [] }).to_string(),
            "no channel",
        ),
        (
            json!({ "op": "subscribe", "channels": vec!["stats"; 51] }).to_string(),
            "more than 50 entries",
        ),
        (
            json!({ "op": "subscribe", "channels": ["stats"], "since": -1 }).to_string(),
            "since < 0",
        ),
        (json!({ "op": "dance" }).to_string(), "an unknown op"),
        ("not json".to_string(), "not JSON"),
    ] {
        c.send_raw(&bad).await;
        let e = c.msg_skipping(&["heartbeat"]).await;
        assert_eq!(
            (e["type"].clone(), e["code"].clone()),
            (json!("error"), json!("bad_message")),
            "{why}"
        );
    }
    c.ws.send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .expect("binary");
    assert_eq!(c.msg_skipping(&["heartbeat"]).await["code"], "bad_message");

    // Three subscriptions at most (the tightened limit).
    let e = c.subscribe(&["admin", "coverage"], None).await;
    assert_eq!(e["code"], "too_many_subscriptions");
    let ack = c.subscribe(&["admin"], None).await;
    assert_eq!(ack["type"], "subscribed");

    // A heartbeat every second (the tightened period).
    loop {
        let v = c.msg().await;
        if v["type"] == "heartbeat" {
            break;
        }
    }
    c.send(json!({ "op": "unsubscribe", "channels": ["markets", "liquidations"] }))
        .await;
    let ack = c.msg_skipping(&["heartbeat"]).await;
    assert_eq!(
        ack,
        json!({ "type": "unsubscribed", "channels": ["markets", "liquidations"] })
    );
    // One slot is free again.
    assert_eq!(c.subscribe(&["coverage"], None).await["type"], "subscribed");
    f.stop().await;
}

async fn event_count(f: &Fixture, sql: &'static str) -> usize {
    let n: i64 = sqlx::query_scalar(sql)
        .fetch_one(&f.admin)
        .await
        .expect("count");
    usize::try_from(n).expect("count")
}

/// Reads `n` event messages of `channel`, in id order.
async fn events(c: &mut Client, channel: &str, n: usize) -> Vec<Value> {
    let mut out = Vec::new();
    while out.len() < n {
        let v = c.msg_skipping(&["heartbeat"]).await;
        assert_eq!(v["type"], "event", "{v}");
        assert_eq!(v["channel"], channel);
        assert_eq!(v["id"], v["data"]["id"]);
        if let Some(prev) = out.last() {
            let prev: &Value = prev;
            assert!(v["id"].as_i64() > prev["id"].as_i64(), "ids increase");
        }
        out.push(v);
    }
    out
}

/// `since` replays the stored events of each event channel, then live events arrive exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn since_replays_then_live_events_arrive_once() {
    let f = Fixture::seeded().await;
    let addr = serve(
        &f,
        ApiConfig {
            ws_max_inbound_per_s: 100,
            ..relaxed()
        },
    )
    .await;
    let mut c = Client::new(addr).await;

    let market = format!("market:{TA}");
    assert_eq!(c.subscribe(&[&market], Some(0)).await["type"], "subscribed");
    let n = event_count(
        &f,
        "SELECT count(*) FROM events e JOIN markets m ON m.market = e.market \
         WHERE m.mint = '2DYKaRPBeNM5WdW8rNsYEktjPrnd89Mm4Lzp3qonSzoj'",
    )
    .await;
    assert!(n > 20);
    let replayed = events(&mut c, &market, n).await;
    assert!(replayed.iter().all(|e| e["data"]["market"] == MA));
    let last = replayed.last().expect("events")["id"].as_i64().expect("id");

    // A live event of the market, announced by the writer's NOTIFY.
    let id = insert_event(&f.admin, 1, 1048, "PriceCranked", Some(MA), None, json!({})).await;
    assert!(id > last);
    notify_events(&f.admin).await;
    let live = events(&mut c, &market, 1).await;
    assert_eq!(live[0]["id"], id);
    assert_eq!(live[0]["data"]["finalized"], false);
    // Nothing is sent twice: the next message answers our ping.
    c.send(json!({ "op": "ping" })).await;
    assert_eq!(c.msg_skipping(&["heartbeat"]).await["type"], "pong");

    // Each event channel replays its own events.
    let position = format!("position:{P4}");
    c.subscribe(&[&position], Some(0)).await;
    let kinds: Vec<Value> = events(&mut c, &position, 3)
        .await
        .iter()
        .map(|e| e["data"]["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        vec![json!("Open"), json!("SettleArmed"), json!("SolSettle")]
    );
    let lender = format!("lender:{L1}");
    c.subscribe(&[&lender], Some(0)).await;
    let kinds: Vec<Value> = events(&mut c, &lender, 3)
        .await
        .iter()
        .map(|e| e["data"]["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        vec![json!("Deposit"), json!("Deposit"), json!("ClaimSol")]
    );
    let expire_id: i64 = sqlx::query_scalar("SELECT id FROM events WHERE kind = 'Expire'")
        .fetch_one(&f.admin)
        .await
        .expect("expire");
    c.subscribe(&["liquidations"], Some(expire_id)).await;
    let kinds: Vec<Value> = events(&mut c, "liquidations", 3)
        .await
        .iter()
        .map(|e| e["data"]["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        vec![json!("SolSettle"), json!("Liquidate"), json!("SolSettle")]
    );
    c.subscribe(&["admin"], Some(0)).await;
    assert_eq!(events(&mut c, "admin", 15).await.len(), 15);
    c.subscribe(&["coverage"], Some(0)).await;
    let cov = events(&mut c, "coverage", 3).await;
    assert_eq!(cov[0]["data"]["source"], "log");

    // A live liquidation reaches the market and the liquidations channel (`position:P1` is not
    // subscribed), once each.
    let id = insert_event(
        &f.admin,
        2,
        1049,
        "Liquidate",
        Some(MA),
        Some(P1),
        json!({}),
    )
    .await;
    notify_events(&f.admin).await;
    let mut got = Vec::new();
    for _ in 0..2 {
        let v = c.msg_skipping(&["heartbeat"]).await;
        assert_eq!(v["id"], id);
        got.push(v["channel"].as_str().expect("channel").to_string());
    }
    got.sort();
    let mut want = vec![market.clone(), "liquidations".to_string()];
    want.sort();
    assert_eq!(got, want);
    c.send(json!({ "op": "ping" })).await;
    assert_eq!(c.msg_skipping(&["heartbeat"]).await["type"], "pong");
    f.stop().await;
}

/// `markets` summaries, `trades:<mint>`, `stats`, and `rollback`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn markets_trades_stats_and_rollbacks_are_pushed() {
    let f = Fixture::seeded().await;
    let addr = serve(
        &f,
        ApiConfig {
            ws_stats_interval_s: 1,
            ..relaxed()
        },
    )
    .await;
    let mut c = Client::new(addr).await;
    let trades = format!("trades:{TA}");
    assert_eq!(
        c.subscribe(&["markets", &trades, "stats"], None).await["type"],
        "subscribed"
    );
    // Stats every second (the tightened period); after it the hub follows the trades.
    let s = c.msg_skipping(&["heartbeat"]).await;
    assert_eq!(
        (s["type"].clone(), s["channel"].clone()),
        (json!("stats"), json!("stats"))
    );
    assert_eq!(s["data"]["markets"], 3);

    insert_event(&f.admin, 3, 1048, "PriceCranked", Some(MA), None, json!({})).await;
    notify_events(&f.admin).await;
    let m = c.msg_skipping(&["heartbeat", "stats"]).await;
    assert_eq!(
        (m["type"].clone(), m["channel"].clone()),
        (json!("market"), json!("markets"))
    );
    assert_eq!(m["data"]["mint"], TA);

    // A third-party trade of mint A.
    let sig = solana_signature::Signature::from([0x77; 64]).to_string();
    sqlx::query(
        "INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, \
         fee_payer, fee) VALUES ($1, 1050, 0, $2, 'confirmed', 'replay', $3, 5000)",
    )
    .bind(&sig)
    .bind(ts(1050))
    .bind(K1)
    .execute(&f.admin)
    .await
    .expect("tx");
    sqlx::query(
        "INSERT INTO venue_trades (signature, ix_index, ev_source, ev_seq, slot, tx_index, ts, \
         venue, program, pool, mint, market, side, token_amount, sol_amount, user_key, \
         is_shortwin, price_wad) VALUES ($1, 0, 1, 0, 1050, 0, $2, 'raydium_v4', \
         '675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8', $3, $4, $5, 'buy', 1000, 10, $6, FALSE, \
         10000000000000000)",
    )
    .bind(&sig)
    .bind(ts(1050))
    .bind(POOL_A)
    .bind(TA)
    .bind(MA)
    .bind(K1)
    .execute(&f.admin)
    .await
    .expect("trade");
    let t = c.msg_skipping(&["heartbeat", "stats", "market"]).await;
    assert_eq!(
        (t["type"].clone(), t["channel"].clone()),
        (json!("trade"), json!(trades))
    );
    assert_eq!(t["data"]["signature"], sig);

    // A rollback reaches every subscribed connection.
    sqlx::query("SELECT pg_notify('shortwin_rollback', '1040')")
        .execute(&f.admin)
        .await
        .expect("notify");
    let r = c.msg_skipping(&["heartbeat", "stats", "market"]).await;
    assert_eq!(r, json!({ "type": "rollback", "slot": 1040 }));
    f.stop().await;
}

/// A client that cannot keep up gets `error{lagged}` and close code 4008 (§9.3).
#[tokio::test]
async fn slow_consumers_are_closed_with_4008() {
    let f = Fixture::seeded().await;
    let addr = serve(
        &f,
        ApiConfig {
            ws_outbound_buffer: 1,
            ..relaxed()
        },
    )
    .await;
    let mut c = Client::new(addr).await;
    let market = format!("market:{TA}");
    c.subscribe(&[&market], None).await;
    let sig = solana_signature::Signature::from([0x78; 64]).to_string();
    sqlx::query(
        "INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, \
         fee_payer, fee) VALUES ($1, 1051, 0, $2, 'confirmed', 'replay', $3, 5000)",
    )
    .bind(&sig)
    .bind(ts(1051))
    .bind(K1)
    .execute(&f.admin)
    .await
    .expect("tx");
    sqlx::query(
        "INSERT INTO events (signature, ix_index, ev_source, ev_seq, slot, tx_index, block_time, \
         program, invocation, kind, market, ev_slot, ev_ts, data) \
         SELECT $1, 0, 0, g, 1051, 0, $2, '5yvyAddWb8RRGh3pPiFS5eV33eZYeHVmAAfwAFmqVpKK', \
         'crank_price', 'PriceCranked', $3, 1051, $2, jsonb_build_object('pad', repeat('x', 512)) \
         FROM generate_series(0, 299) g",
    )
    .bind(&sig)
    .bind(ts(1051))
    .bind(MA)
    .execute(&f.admin)
    .await
    .expect("events");
    notify_events(&f.admin).await;
    let mut lagged = false;
    let mut events = 0;
    loop {
        match c.next(10).await {
            Got::Msg(v) if v["type"] == "error" => {
                assert_eq!(v["code"], "lagged");
                lagged = true;
            }
            Got::Msg(v) => {
                assert!(!lagged, "nothing after the lagged error");
                if v["type"] == "event" {
                    events += 1;
                }
            }
            Got::Closed(code) => {
                assert_eq!(code, Some(4008));
                break;
            }
        }
    }
    assert!(lagged);
    assert!(
        events < 300,
        "the connection was cut before the backlog was delivered"
    );
    f.stop().await;
}

/// More than `ws_max_inbound_per_s` messages a second: an `error{rate_limited}`, the excess
/// ignored, then a close (1008) at twice the limit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_floods_are_refused_then_closed() {
    let f = Fixture::seeded().await;
    let addr = serve(
        &f,
        ApiConfig {
            ws_max_inbound_per_s: 5,
            ..relaxed()
        },
    )
    .await;
    let mut c = Client::new(addr).await;
    for _ in 0..12 {
        c.send(json!({ "op": "ping" })).await;
    }
    let mut pongs = 0;
    let mut errors = 0;
    loop {
        match c.next(5).await {
            Got::Msg(v) if v["type"] == "pong" => pongs += 1,
            Got::Msg(v) if v["type"] == "error" => {
                assert_eq!(v["code"], "rate_limited");
                errors += 1;
            }
            Got::Msg(v) => panic!("unexpected {v}"),
            Got::Closed(code) => {
                assert_eq!(code, Some(1008));
                break;
            }
        }
    }
    assert_eq!(pongs, 5);
    assert_eq!(
        errors, 2,
        "one when the limit is passed, one with the close"
    );
    f.stop().await;
}

/// WebSocket connections per client are capped (`429` at the upgrade) and released on close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connections_per_client_are_capped() {
    let f = Fixture::seeded().await;
    let contract = Contract::load();
    let addr = serve(
        &f,
        ApiConfig {
            ws_max_conns_per_ip: 2,
            ..relaxed()
        },
    )
    .await;
    let a = connect(addr).await;
    let _b = connect(addr).await;
    match tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws")).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status().as_u16(), 429);
            let body: Value =
                serde_json::from_slice(resp.body().as_deref().unwrap_or_default()).expect("JSON");
            assert_eq!(body["code"], "rate_limited");
            contract.validate(&contract.component("Problem"), &body, "429 at the upgrade");
            assert!(resp.headers().get("retry-after").is_some());
        }
        other => panic!("expected a 429, got {other:?}", other = other.map(|_| ())),
    }
    drop(a);
    let mut ok = false;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if tokio_tungstenite::connect_async(format!("ws://{addr}/v1/ws"))
            .await
            .is_ok()
        {
            ok = true;
            break;
        }
    }
    assert!(ok, "a closed connection frees its slot");
    f.stop().await;
}
