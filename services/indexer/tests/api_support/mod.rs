//! Shared by the API tests (`tests/api_*.rs`; INDEXER.md §11.2). Owner: API.
//!
//! - [`Fixture`]: an embedded PostgreSQL with a migrated database seeded from
//!   `tests/data/api_seed.sql`, the read-only API pool on it ([`api::api_pool`]) and routers;
//! - [`Contract`]: `openapi.yaml` parsed with `serde-saphyr`; a response is checked against its
//!   operation — the status is documented, the `Content-Type` is the documented one, the body
//!   validates against the documented schema (JSON Schema 2020-12 with `jsonschema`, every
//!   `#/components/...` reference resolved inside the document, no network), and the headers of
//!   §9.1 / §10.1 are present;
//! - the seed's keys and signatures.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shortwin_indexer::api::{self, ApiConfig, AppState};
use shortwin_indexer::testing::pg::TestPg;
use sqlx::PgPool;
use tower::ServiceExt;

// ---- the seed's keys (tests/data/api_seed.sql header) ------------------------------------------

pub const MA: &str = "29d2S7vB453rNYFdR5Ycwt7y9haRT5fwVwL9zTmBhfV2";
pub const TA: &str = "2DYKaRPBeNM5WdW8rNsYEktjPrnd89Mm4Lzp3qonSzoj";
pub const MB: &str = "3EKkiwNLWqoUbzFkPrmKbtUB4EweE6f4STzevYUmezeL";
pub const TB: &str = "3JF3sEqM796hk5WFqA6EtmEwJQ9quALszsfJyvXNQKy3";
pub const MJ: &str = "4K2V1kpVycZ6qSFsNdz2FtpNxnJs17eBNzf9rdCMcKoe";
pub const TJ: &str = "4NwnA4HWZurKyXWNowJwYmb9CwX4gBKzwQKov1ExMf8M";
pub const L1: &str = "5PjDJaGfSPJj4tFzMRCiuuAasKg5n8dJKXKenhuwZexx";
pub const L2: &str = "5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf";
pub const COIN_CREATOR_A: &str = "2MNus2KCpxwXnp19iyXNpWSFtBD2UGjQBAL8AbtywfT9";
pub const B1: &str = "6URwbPipuA4MJLG7LCRRZuWnms3JZ9cRG3z9indXWz8G";
pub const B2: &str = "6YMEjhBqVTMaSRWcmVkLrnHZ22FWEDJEpTeonAg8GKSy";
pub const B3: &str = "6cGXszer5keoaWm8Co5G9f4KGBThuGz4NsKTqYij1emg";
pub const K1: &str = "7Z8ftDAzMvoyXnGEJye8DurzgQQXLAbYCaeeesM7UKHa";
pub const K2: &str = "7d3y2WdzxE7CfsWjkGy3WndkvZcj1EHMkzKJiFPiDecH";
pub const GOV: &str = "8dqQB2d9phZbmEGMHkrpsvDCawmk7Baf97K9ax4hReSt";
pub const COVAUTH: &str = "8mfzTdZB1JA43QmNAMWfTfkj5GC9TJxJFveThi9tvK6J";
pub const POOL_A: &str = "2RJD1KnDRGEkvuFfAGrJ7PD28LRE9LRDjZznDywagzmr";
pub const POOL_X: &str = "2VDW9dFE1ZXz4zWAbaBDQFynNVdRpQ73HyfSHMzBSL6Z";
pub const X1: &str = "Bswb3UyeD1pUTaGiE6WvqwFpJZsQSEY1xhJePCDTHdvp";
pub const X2: &str = "BwrtBnSeoK7hbfXDfPqr8p2aYj5c7JDqX6yJSaG42yFX";
pub const P1: &str = "9iY8Tr5KHUKDzgGUGY5XXvZQVV8xtCZn5dyeX2nHNycC";
pub const P2: &str = "9nTRc9YKsmcT8mWyhqQSpoLAjeMAZGFbe3eJaQpt8Jvu";
pub const P3: &str = "9rNikT1LU4ugGrmV98jN7g6vyoZNEKwRCTJxdnsUseFc";
pub const P4: &str = "9vJ1tkUM4NCuQx1zaS4HQYshDxmZuPdEkrychAv5cyaK";
pub const P5: &str = "9zDK33wMefW8Z3GW1jPChReTU7ymaTK4KGeGkYxgNJu2";
pub const P6: &str = "A48cBMQNExoMh8X1T2i7zJRDiHByFWzssgJvow1H7eDj";
pub const P7: &str = "A83uKesNqG6aqDmWtL33HBByxSQAvaghS5yasK3sryYS";
pub const P8: &str = "AByCTxLPRZPoyK22KdMxa3xkCbcNbeNWzVeEvh6UcJs9";
pub const P9: &str = "AFtVcFoQ1rh37QGXkvgsrvjWSkpaGi4LYuJtz595MeBr";
/// `market_created` of market A (slot 1001, finalized).
pub const SIG_INIT: &str =
    "2AXDGYSE4f2sz7tvMMzyHvUfcoJmxudvdhBcmiUSo6ijwfYmfZYsKRxboQMPh3R4kUhXRVdtSXFXMheka4Rc4P2";
/// The failed `liquidate` at slot 1047 (confirmed).
pub const SIG_FAILED: &str =
    "z3XN8ZK368b5Fps5CjyZJBNdF81zEUCJzwyDn3BnwqGHBZqjaVvWa5yQCjEAq3LAjQEhhCATZZGeVLtovtncrbr";
/// The close of P2 (slot 1020, finalized).
pub const SIG_CLOSE: &str =
    "RSwSdP8jKmgTgVoKNbzP8N1yxmSHF4NRHYqxC1wLh57YnxWAxnYDg38boTPDVsiMk2g1sNMdExFzVifmyEuhDyN";
/// A valid key / signature nobody used.
pub fn unknown_key() -> String {
    solana_pubkey::Pubkey::new_from_array([0xEE; 32]).to_string()
}
pub fn unknown_sig() -> String {
    solana_signature::Signature::from([0xEE; 64]).to_string()
}

/// Slot / time of the seed (one block every 600 s).
pub const T0: i64 = 1_790_000_000;
pub fn ts(slot: i64) -> i64 {
    T0 + (slot - 1000) * 600
}
pub const WAD: u128 = 1_000_000_000_000_000_000;
/// The seed's EMA prices (markets A and B).
pub const P_A: u128 = 10_000_000_000_000_000;
pub const P_B: u128 = 20_000_000_000_000_000;

// ---- the fixture -------------------------------------------------------------------------------

pub const SEED: &str = include_str!("../data/api_seed.sql");

/// A running PostgreSQL with one seeded database.
pub struct Fixture {
    pub pg: TestPg,
    /// Read-write (the seed, test inserts, `NOTIFY`).
    pub admin: PgPool,
}

impl Fixture {
    /// Starts a server, migrates and seeds a database.
    pub async fn seeded() -> Self {
        let f = Self::empty().await;
        sqlx::raw_sql(SEED)
            .execute(&f.admin)
            .await
            .expect("the API seed applies");
        f
    }

    /// Starts a server with a migrated, empty database.
    pub async fn empty() -> Self {
        let pg = TestPg::start_in(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")))
            .await
            .expect("the embedded PostgreSQL starts");
        let admin = pg.fresh_db().await.expect("a migrated database");
        Self { pg, admin }
    }

    /// The API's read-only pool for `cfg` (statement timeout, read-only transactions).
    pub fn api_pool(&self, cfg: &ApiConfig) -> PgPool {
        api::api_pool(&self.admin, cfg)
    }

    /// A router on the read-only pool.
    pub fn router(&self, cfg: ApiConfig) -> Router {
        api::router(AppState::new(self.api_pool(&cfg), cfg))
    }

    pub async fn stop(self) {
        self.admin.close().await;
        self.pg.stop().await.expect("stop");
    }
}

/// The default configuration with generous limits (contract tests are not about rate limits).
pub fn relaxed() -> ApiConfig {
    ApiConfig {
        anon_rps: 100_000,
        anon_burst: 100_000,
        ..ApiConfig::default()
    }
}

// ---- requests ----------------------------------------------------------------------------------

/// A response, read whole.
#[derive(Debug)]
pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
    pub raw: Vec<u8>,
}

impl Resp {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn header_i64(&self, name: &str) -> Option<i64> {
        self.header(name).and_then(|v| v.parse().ok())
    }

    pub fn data(&self) -> &Vec<Value> {
        self.body["data"].as_array().expect("a `data` array")
    }

    pub fn code(&self) -> &str {
        self.body["code"].as_str().unwrap_or("")
    }
}

/// `GET uri` from `peer`, with extra headers.
pub async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    peer: IpAddr,
    headers: &[(&str, &str)],
) -> Resp {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .extension(ConnectInfo(SocketAddr::new(peer, 40_000)));
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let resp = app
        .clone()
        .oneshot(b.body(Body::empty()).expect("request"))
        .await
        .expect("infallible");
    let status = resp.status();
    let headers = resp.headers().clone();
    let raw = resp
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    let body = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    Resp {
        status,
        headers,
        body,
        raw,
    }
}

pub fn localhost() -> IpAddr {
    IpAddr::from([127, 0, 0, 1])
}

/// `GET uri` from 127.0.0.1.
pub async fn get(app: &Router, uri: &str) -> Resp {
    send(app, Method::GET, uri, localhost(), &[]).await
}

// ---- the contract ------------------------------------------------------------------------------

/// `openapi.yaml`.
pub struct Contract {
    pub doc: Value,
}

/// One documented operation.
#[derive(Clone, Debug)]
pub struct Operation {
    pub path: String,
    pub id: String,
    pub op: Value,
}

fn pointer_unescape(s: &str) -> String {
    s.replace("~1", "/").replace("~0", "~")
}

impl Contract {
    pub fn load() -> Self {
        let text = include_str!("../../openapi.yaml");
        // YAML 1.2 (OpenAPI 3.1's): only `true` / `false` are booleans.
        let mut opts = serde_saphyr::Options::default();
        opts.strict_booleans = true;
        let doc: Value =
            serde_saphyr::from_str_with_options(text, opts).expect("openapi.yaml parses");
        assert_eq!(doc["openapi"], "3.1.0");
        // A YAML 1.1 reader (serde-saphyr's default, many code generators) must read the same
        // document: no bare `y` / `n` / `on` / `off` scalars (FeeLine's `'n'` is quoted for this).
        let lenient: Value = serde_saphyr::from_str(text).expect("openapi.yaml parses (YAML 1.1)");
        assert!(
            lenient == doc,
            "openapi.yaml reads differently under YAML 1.1 (quote the ambiguous scalars)"
        );
        Self { doc }
    }

    /// Resolves a local `$ref` (`#/a/b`), recursively.
    pub fn resolve<'a>(&'a self, v: &'a Value) -> &'a Value {
        match v.get("$ref").and_then(Value::as_str) {
            Some(r) => {
                let ptr = r.strip_prefix('#').expect("local refs only");
                let target = self
                    .doc
                    .pointer(
                        &ptr.split('/')
                            .map(pointer_unescape)
                            .collect::<Vec<_>>()
                            .join("/"),
                    )
                    .unwrap_or_else(|| panic!("dangling $ref {r}"));
                self.resolve(target)
            }
            None => v,
        }
    }

    /// Every operation (path, operationId, operation object); asserts GET-only.
    pub fn operations(&self) -> Vec<Operation> {
        let paths = self.doc["paths"].as_object().expect("paths");
        let mut out = Vec::new();
        for (path, item) in paths {
            let methods: Vec<&String> = item.as_object().expect("path item").keys().collect();
            assert_eq!(methods, vec!["get"], "{path}: the API is GET-only");
            let op = &item["get"];
            out.push(Operation {
                path: path.clone(),
                id: op["operationId"].as_str().expect("operationId").to_string(),
                op: op.clone(),
            });
        }
        out
    }

    pub fn operation(&self, id: &str) -> Operation {
        self.operations()
            .into_iter()
            .find(|o| o.id == id)
            .unwrap_or_else(|| panic!("no operation {id}"))
    }

    /// The documented statuses of an operation.
    pub fn statuses(&self, id: &str) -> BTreeSet<u16> {
        self.operation(id).op["responses"]
            .as_object()
            .expect("responses")
            .keys()
            .map(|k| k.parse().expect("numeric status"))
            .collect()
    }

    /// The operation's parameters, resolved.
    pub fn parameters(&self, id: &str) -> Vec<Value> {
        self.operation(id).op["parameters"]
            .as_array()
            .map(|ps| ps.iter().map(|p| self.resolve(p).clone()).collect())
            .unwrap_or_default()
    }

    /// A validator for `schema` with the document's components reachable.
    pub fn validator(&self, schema: &Value) -> jsonschema::Validator {
        let mut root = schema.clone();
        let obj = root.as_object_mut().expect("a schema object");
        obj.insert("components".to_string(), self.doc["components"].clone());
        jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&root)
            .unwrap_or_else(|e| panic!("schema does not compile: {e}"))
    }

    /// A validator for `#/components/schemas/<name>`.
    pub fn component(&self, name: &str) -> jsonschema::Validator {
        self.validator(&json!({ "$ref": format!("#/components/schemas/{name}") }))
    }

    /// Validates `instance` (panics with every error).
    pub fn validate(&self, v: &jsonschema::Validator, instance: &Value, what: &str) {
        let errors: Vec<String> = v
            .iter_errors(instance)
            .map(|e| format!("{} at {}", e, e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "{what}: the body does not match the schema:\n  {}\nbody: {}",
            errors.join("\n  "),
            serde_json::to_string_pretty(instance).unwrap_or_default()
        );
    }

    /// Checks one response of operation `id` against the document (status, content type, body
    /// schema, headers).
    pub fn check(&self, id: &str, what: &str, r: &Resp) {
        let op = self.operation(id);
        let status = r.status.as_u16().to_string();
        let resp = op.op["responses"].get(&status).unwrap_or_else(|| {
            panic!(
                "{what}: status {status} is not documented for {id}: {:?}",
                r.body
            )
        });
        let resp = self.resolve(resp);
        for h in [
            "x-indexer-slot",
            "x-indexer-finalized-slot",
            "ratelimit-limit",
            "ratelimit-remaining",
            "ratelimit-reset",
        ] {
            assert!(
                r.header_i64(h).is_some(),
                "{what}: header {h} missing or not an integer"
            );
        }
        if let Some(hs) = resp.get("headers").and_then(Value::as_object) {
            for h in hs.keys() {
                assert!(
                    r.header_i64(&h.to_ascii_lowercase()).is_some(),
                    "{what}: documented header {h} missing"
                );
            }
        }
        let Some(content) = resp.get("content").and_then(Value::as_object) else {
            return;
        };
        assert_eq!(content.len(), 1, "{id} {status}: one media type");
        let (media, body) = content.iter().next().expect("media type");
        let ct = r.header("content-type").unwrap_or("");
        assert_eq!(
            ct.split(';').next().unwrap_or("").trim(),
            media,
            "{what}: content type"
        );
        assert!(r.header("cache-control").is_some(), "{what}: Cache-Control");
        let v = self.validator(&body["schema"]);
        self.validate(&v, &r.body, what);
    }
}

/// Inserts a transaction and one event for `market` (or none), returning the event's id. The
/// writer's `NOTIFY` is left to the caller.
pub async fn insert_event(
    admin: &PgPool,
    n: u32,
    slot: i64,
    kind: &str,
    market: Option<&str>,
    position: Option<&str>,
    data: Value,
) -> i64 {
    let mut b = [0u8; 64];
    b[..4].copy_from_slice(&n.to_le_bytes());
    b[63] = 0x5A;
    let sig = solana_signature::Signature::from(b).to_string();
    sqlx::query(
        "INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, \
         fee_payer, fee) VALUES ($1, $2, $3, $4, 'confirmed', 'replay', $5, 5000) \
         ON CONFLICT DO NOTHING",
    )
    .bind(&sig)
    .bind(slot)
    .bind(i32::try_from(n % 1_000_000).expect("tx index") + 10)
    .bind(ts(slot))
    .bind(K1)
    .execute(admin)
    .await
    .expect("ingest_txs");
    sqlx::query_scalar(
        "INSERT INTO events (signature, ix_index, ev_source, ev_seq, slot, tx_index, block_time, \
         program, invocation, kind, market, position, actor, ev_slot, ev_ts, data) \
         VALUES ($1, 0, 0, 0, $2, $3, $4, '5yvyAddWb8RRGh3pPiFS5eV33eZYeHVmAAfwAFmqVpKK', \
         'test', $5, $6, $7, NULL, $2, $4, $8) RETURNING id",
    )
    .bind(&sig)
    .bind(slot)
    .bind(i32::try_from(n % 1_000_000).expect("tx index") + 10)
    .bind(ts(slot))
    .bind(kind)
    .bind(market)
    .bind(position)
    .bind(data)
    .fetch_one(admin)
    .await
    .expect("events")
}

/// The writer's notification (§9.3).
pub async fn notify_events(admin: &PgPool) {
    sqlx::query("SELECT pg_notify('shortwin_events', (SELECT max(id) FROM events)::text)")
        .execute(admin)
        .await
        .expect("notify");
}
