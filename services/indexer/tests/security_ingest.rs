//! Regressions of the M8 security review, ingest side (M8-SEC-1, -2, -5, -6, -7, -11, -12, -13,
//! -14). Each test is the review's reproducer turned around: it fails on the code the review
//! found and passes on the fix. A mock JSON-RPC node (axum) serves the committed
//! `mock_lifecycle` session for the RPC paths; the finality paths use stub clusters; the rest
//! write synthetic transactions through the real writer.

mod ingest_common;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anchor_lang::{AnchorSerialize, Discriminator, InstructionData};
use axum::extract::State;
use axum::http::Request;
use axum::{Json, Router};
use http_body_util::BodyExt;
use ingest_common::pg;
use serde_json::{Value, json};
use shortwin_indexer::IndexerConfig;
use shortwin_indexer::api::{self, ApiConfig, AppState};
use shortwin_indexer::ingest::decode::venue;
use shortwin_indexer::ingest::finality::{self, BlockCheck, StatusSource, TxStatus};
use shortwin_indexer::ingest::raw::{Commitment, RawIx, RawTopIx, RawTx};
use shortwin_indexer::ingest::writer::{CursorAdvance, Writer};
use shortwin_indexer::ingest::{self, ProgramIds};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use sqlx::PgPool;
use tower::ServiceExt;

fn k(b: u8) -> Pubkey {
    Pubkey::new_from_array([b; 32])
}

fn ix(program: Pubkey, h: u32, data: Vec<u8>, accounts: Vec<Pubkey>) -> RawIx {
    RawIx {
        program_id: program,
        accounts,
        data,
        stack_height: h,
    }
}

fn tx(sig: u8, slot: u64, instructions: Vec<RawTopIx>, logs: Vec<String>) -> RawTx {
    RawTx {
        signature: Signature::from([sig; 64]),
        slot,
        tx_index: 0,
        block_time: Some(1_790_000_000 + i64::try_from(slot).expect("slot") * 10),
        commitment: Commitment::Finalized,
        fee_payer: k(50),
        fee: 5_000,
        compute_units: Some(10),
        err: None,
        account_keys: vec![k(50), shortwin::ID],
        instructions,
        logs,
    }
}

fn ev<T: AnchorSerialize + Discriminator>(e: &T) -> Vec<u8> {
    let mut v = T::DISCRIMINATOR.to_vec();
    e.serialize(&mut v).expect("borsh");
    v
}

fn sw_event(payload: &[u8]) -> RawIx {
    let mut data = anchor_lang::event::EVENT_IX_TAG_LE.to_vec();
    data.extend_from_slice(payload);
    ix(shortwin::ID, 2, data, vec![k(200)])
}

fn sw_top(data: Vec<u8>, inner: Vec<RawIx>) -> RawTopIx {
    RawTopIx {
        ix: ix(shortwin::ID, 1, data, vec![k(60), k(61), k(62)]),
        inner,
    }
}

async fn api_get(pool: &PgPool, uri: &str) -> Value {
    let cfg = ApiConfig {
        anon_rps: 10_000,
        anon_burst: 10_000,
        ..ApiConfig::default()
    };
    let app = api::router(AppState::new(api::api_pool(pool, &cfg), cfg));
    let resp = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(axum::body::Body::empty())
                .expect("req"),
        )
        .await
        .expect("infallible");
    let raw = resp.into_body().collect().await.expect("body").to_bytes();
    serde_json::from_slice(&raw).unwrap_or(Value::Null)
}

async fn scalar_i64(pool: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.expect(sql)
}

fn market_created(
    market: Pubkey,
    mint: Pubkey,
    slot: u64,
    venue: u8,
) -> shortwin::events::MarketCreated {
    shortwin::events::MarketCreated {
        market,
        slot,
        ts: 1,
        mint,
        token_program: k(5),
        venue,
        creator: k(6),
        bonding_curve: Pubkey::default(),
        canonical_pool: k(7),
        ray_pool: Pubkey::default(),
        tier: 3,
        creation_fee: 1_000,
    }
}

// ---- M8-SEC-7: the Emergency's disable_venue survives later configuration changes ---------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_venue_stays_disabled_through_later_config_changes() {
    use shortwin::state::{ConfigChange, EmaParamsArgs, InitConfigArgs, Params, TierParams};
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let mut w = Writer::open(&pool, ProgramIds::default(), "rpc_backfill")
        .await
        .expect("writer");
    let all = 0b1111u32;
    let init = shortwin::instruction::InitializeConfig {
        args: InitConfigArgs {
            governance: k(70),
            pause_authority: k(71),
            coverage_authority: Pubkey::default(),
            fee_recipient: k(72),
            pyth_sol_usd: k(73),
            pyth_feed_id: [1; 32],
            timelock_s: 172_800,
            venue_enabled: all,
            params: Params::defaults(),
            tiers: TierParams::defaults(),
            ema: EmaParamsArgs::defaults(),
            util_kink_bps: 8_000,
            guard_slot: [1; 6],
        },
    }
    .data();
    w.write(&tx(1, 10, vec![sw_top(init, vec![])], vec![]))
        .await
        .expect("init");
    let enabled = || async {
        sqlx::query_scalar::<_, i64>("SELECT venue_enabled::bigint FROM protocol_state")
            .fetch_one(&pool)
            .await
            .expect("protocol_state")
    };
    // disable_venue(bit 2): the chain holds 0b1011.
    let disabled = all & !(1 << 2);
    let dv = shortwin::instruction::DisableVenue { bit: 2 }.data();
    let e = shortwin::events::VenueDisabled {
        market: Pubkey::default(),
        slot: 11,
        ts: 1,
        bit: 2,
        venue_enabled: disabled,
    };
    w.write(&tx(
        2,
        11,
        vec![sw_top(dv, vec![sw_event(&ev(&e))])],
        vec![],
    ))
    .await
    .expect("disable");
    assert_eq!(enabled().await, i64::from(disabled));
    // propose_config(change) then apply_config, as the chain does (`GlobalConfig::apply`).
    let mut sig = 3u8;
    let mut slot = 12u64;
    let mut govern = |change: ConfigChange| {
        let (kind, payload) = change.encode().expect("encode");
        let hash = shortwin::state::change_hash(kind, &payload);
        let pc = shortwin::instruction::ProposeConfig { change }.data();
        let proposed = shortwin::events::ConfigProposed {
            market: Pubkey::default(),
            slot,
            ts: 2,
            kind,
            hash,
            not_before: 3,
        };
        let a = tx(
            sig,
            slot,
            vec![sw_top(pc, vec![sw_event(&ev(&proposed))])],
            vec![],
        );
        let applied = shortwin::events::ConfigApplied {
            market: Pubkey::default(),
            slot: slot + 1,
            ts: 4,
            kind,
            hash,
        };
        let ac = shortwin::instruction::ApplyConfig {}.data();
        let b = tx(
            sig + 1,
            slot + 1,
            vec![sw_top(ac, vec![sw_event(&ev(&applied))])],
            vec![],
        );
        sig += 2;
        slot += 2;
        (a, b)
    };
    // A Kink change leaves venue_enabled alone on chain.
    let (a, b) = govern(ConfigChange::Kink {
        util_kink_bps: 7_000,
    });
    w.write(&a).await.expect("propose");
    w.write(&b).await.expect("apply");
    assert_eq!(
        enabled().await,
        i64::from(disabled),
        "Kink re-enabled the disabled venue"
    );
    // EnableVenues ORs into the live value: bit 0 (already on) changes nothing, bit 2 stays off.
    let (a, b) = govern(ConfigChange::EnableVenues { bits: 0b0001 });
    w.write(&a).await.expect("propose");
    w.write(&b).await.expect("apply");
    assert_eq!(enabled().await, i64::from(disabled));
    w.close().await.expect("close");
    for uri in ["/v1/venues", "/v1/admin/config"] {
        let v = api_get(&pool, uri).await;
        assert_eq!(v["venue_enabled"], json!(disabled), "{uri}");
    }
    // A rebuild of the global projection gives the same value.
    ingest::rebuild(&pool, None).await.expect("rebuild");
    assert_eq!(enabled().await, i64::from(disabled), "after a rebuild");
    // The parameters history records the disable (the next ConfigApplied starts from it).
    let sources: Vec<String> = sqlx::query_scalar(
        "SELECT source FROM protocol_params_history ORDER BY slot, tx_index, ix_index",
    )
    .fetch_all(&pool)
    .await
    .expect("history");
    assert_eq!(
        sources,
        [
            "initialize_config",
            "disable_venue",
            "apply_config",
            "apply_config"
        ]
    );
    pool.close().await;
}

// ---- M8-SEC-6: venue trades on pools that are not the market's ------------------------------------

fn pamm_buy_event(base_out: u64, user_quote_in: u64, pool: Pubkey, user: Pubkey) -> Vec<u8> {
    let mut p = venue::anchor_event_discriminator("BuyEvent").to_vec();
    let u = |p: &mut Vec<u8>, v: u64| p.extend(v.to_le_bytes());
    u(&mut p, 1_790_000_000);
    u(&mut p, base_out);
    u(&mut p, u64::MAX);
    u(&mut p, 0);
    u(&mut p, user_quote_in);
    u(&mut p, 10);
    u(&mut p, 10);
    u(&mut p, user_quote_in);
    u(&mut p, 0);
    u(&mut p, 0);
    u(&mut p, 0);
    u(&mut p, 0);
    u(&mut p, user_quote_in);
    u(&mut p, user_quote_in);
    p.extend(pool.to_bytes());
    p.extend(user.to_bytes());
    for _ in 0..5 {
        p.extend(k(99).to_bytes());
    }
    u(&mut p, 0);
    u(&mut p, 0);
    p
}

fn cpmm_swap_event(pool: Pubkey, sol_in: u64, tokens_out: u64, mint: Pubkey) -> Vec<u8> {
    let mut p = venue::CPMM_SWAP_EVENT.to_vec();
    p.extend(pool.to_bytes());
    for v in [1u64, 1, sol_in, tokens_out, 0, 0] {
        p.extend(v.to_le_bytes());
    }
    p.push(1);
    p.extend(venue::WSOL.to_bytes());
    p.extend(mint.to_bytes());
    p.extend(0u64.to_le_bytes());
    p.extend(0u64.to_le_bytes());
    p.push(1);
    p
}

/// A transaction that only lists short.win (an extra account of a System transfer), with a
/// PumpSwap buy on `pamm_pool` (base `mint`, quote `quote`) and a CPMM swap on `cpmm_pool`.
fn third_party_trades(
    sig: u8,
    slot: u64,
    mint: Pubkey,
    pamm_pool: Pubkey,
    quote: Pubkey,
    cpmm_pool: Pubkey,
) -> RawTx {
    use base64::Engine;
    let system = solana_pubkey::pubkey!("11111111111111111111111111111111");
    let mut transfer = 2u32.to_le_bytes().to_vec();
    transfer.extend(1u64.to_le_bytes());
    let trader = k(82);
    let mut event = anchor_lang::event::EVENT_IX_TAG_LE.to_vec();
    event.extend(pamm_buy_event(1, 1_000_000_000_000, pamm_pool, trader));
    let buy = RawTopIx {
        ix: ix(
            venue::PUMP_AMM,
            1,
            vec![0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea],
            vec![pamm_pool, trader, k(83), mint, quote],
        ),
        inner: vec![ix(venue::PUMP_AMM, 2, event, vec![k(84)])],
    };
    let cpmm = RawTopIx {
        ix: ix(
            venue::RAYDIUM_CPMM,
            1,
            vec![1; 8],
            vec![trader, k(86), cpmm_pool],
        ),
        inner: vec![],
    };
    let logs = vec![
        "Program 11111111111111111111111111111111 invoke [1]".to_string(),
        "Program 11111111111111111111111111111111 success".to_string(),
        format!("Program {} invoke [1]", venue::PUMP_AMM),
        format!("Program {} invoke [2]", venue::PUMP_AMM),
        format!("Program {} success", venue::PUMP_AMM),
        format!("Program {} success", venue::PUMP_AMM),
        format!("Program {} invoke [1]", venue::RAYDIUM_CPMM),
        format!(
            "Program data: {}",
            base64::engine::general_purpose::STANDARD.encode(cpmm_swap_event(
                cpmm_pool,
                5_000_000_000,
                1,
                mint
            ))
        ),
        format!("Program {} success", venue::RAYDIUM_CPMM),
    ];
    tx(
        sig,
        slot,
        vec![
            RawTopIx {
                ix: ix(system, 1, transfer, vec![trader, trader, shortwin::ID]),
                inner: vec![],
            },
            buy,
            cpmm,
        ],
        logs,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trades_on_pools_that_are_not_the_markets_never_reach_its_candles() {
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let mut w = Writer::open(&pool, ProgramIds::default(), "rpc_backfill")
        .await
        .expect("writer");
    let market = k(1);
    let mint = k(4);
    let canonical = k(7);
    // A trade on the canonical pool before the listing (a gap-fill delivers it first): it takes
    // the market at MarketCreated.
    let early = third_party_trades(9, 19, mint, canonical, venue::WSOL, k(87));
    w.write(&early).await.expect("early");
    let mc = market_created(market, mint, 20, 3);
    let cm = shortwin::instruction::CreateMarket::DISCRIMINATOR.to_vec();
    w.write(&tx(
        10,
        20,
        vec![sw_top(cm, vec![sw_event(&ev(&mc))])],
        vec![],
    ))
    .await
    .expect("market");
    // The attack: a PumpSwap pool <mint, FAKE> (not a SOL pool) and a dust CPMM pool <WSOL, mint>.
    let attack = third_party_trades(11, 21, mint, k(80), k(81), k(85));
    w.write(&attack).await.expect("attack");
    // Someone else's trade on the market's own (canonical) pool, after the listing.
    let organic = third_party_trades(12, 22, mint, canonical, venue::WSOL, k(88));
    w.write(&organic).await.expect("organic");
    w.close().await.expect("close");
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT signature::text, pool::text, market::text FROM venue_trades ORDER BY slot, ix_index",
    )
    .fetch_all(&pool)
    .await
    .expect("trades");
    let sig = |b: u8| Signature::from([b; 64]).to_string();
    let m = Some(market.to_string());
    // The non-SOL PumpSwap pool is not decoded at all; the dust CPMM pools are stored without a
    // market; the canonical pool's trades (before and after the listing) are the market's.
    assert_eq!(
        rows,
        vec![
            (sig(9), canonical.to_string(), m.clone()),
            (sig(9), k(87).to_string(), None),
            (sig(11), k(85).to_string(), None),
            (sig(12), canonical.to_string(), m.clone()),
            (sig(12), k(88).to_string(), None),
        ]
    );
    let t0 = 1_790_000_000 + 19 * 10;
    let candles = api_get(
        &pool,
        &format!(
            "/v1/markets/{mint}/candles?interval=1m&from={}&to={}",
            t0 - t0 % 60,
            t0 - t0 % 60 + 120
        ),
    )
    .await;
    let data = candles["data"].as_array().expect("candles");
    // Only the canonical pool's two trades (1 token for 10^12 lamports each, one per minute):
    // no 5e27 dust print from the attacker's CPMM pool, no 10^12-token "SOL" from the pool
    // pairing the mint with a worthless token.
    let trades_n: i64 = data.iter().map(|c| c["trades"].as_i64().unwrap_or(0)).sum();
    assert_eq!(trades_n, 2, "{candles}");
    for c in data {
        assert_eq!(c["low"], "1000000000000000000000000000000", "{candles}");
        assert_eq!(c["volume_sol"], "1000000000000", "{candles}");
    }
    let trades = api_get(&pool, &format!("/v1/markets/{mint}/trades")).await;
    let pools: HashSet<String> = trades["data"]
        .as_array()
        .expect("trades")
        .iter()
        .map(|t| t["pool"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(pools, HashSet::from([canonical.to_string()]));
    // A rebuild attributes exactly the same rows (incremental = rebuild).
    let before: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT pool::text, market::text FROM venue_trades ORDER BY 1, 2")
            .fetch_all(&pool)
            .await
            .expect("rows");
    ingest::rebuild(&pool, None).await.expect("rebuild");
    let after: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT pool::text, market::text FROM venue_trades ORDER BY 1, 2")
            .fetch_all(&pool)
            .await
            .expect("rows");
    assert_eq!(before, after);
    pool.close().await;
}

// ---- a mock JSON-RPC node serving a recorded session (M8-SEC-2, -5, -11) ------------------------------

/// Per address: (signature, slot, block time), newest first.
type Listing = HashMap<String, Vec<(String, u64, Option<i64>)>>;

#[derive(Clone)]
struct Mock {
    by_addr: Arc<Listing>,
    txs: Arc<HashMap<String, Value>>,
    /// `getTransaction` answers `null`.
    hide: Arc<Mutex<HashSet<String>>>,
    /// `getTransaction` answers a JSON-RPC error with this text.
    error_for: Arc<Mutex<HashMap<String, (i64, String)>>>,
    page_cap: usize,
    fetched: Arc<Mutex<HashSet<String>>>,
}

async fn rpc(State(m): State<Mock>, Json(req): Json<Value>) -> Json<Value> {
    let id = req["id"].clone();
    let params = &req["params"];
    let ok = |result: Value| Json(json!({"jsonrpc": "2.0", "result": result, "id": id}));
    let err = |code: i64, message: String| {
        Json(json!({"jsonrpc": "2.0", "error": {"code": code, "message": message}, "id": id}))
    };
    match req["method"].as_str().unwrap_or("") {
        "getSignaturesForAddress" => {
            let addr = params[0].as_str().unwrap_or("");
            let cfg = &params[1];
            let limit = usize::try_from(cfg["limit"].as_u64().unwrap_or(1000))
                .unwrap_or(1000)
                .min(m.page_cap);
            let list = m.by_addr.get(addr).cloned().unwrap_or_default();
            // A bound the node does not know: -32020, as a real node answers.
            for bound in ["before", "until"] {
                if let Some(b) = cfg[bound].as_str()
                    && !list.iter().any(|x| x.0 == b)
                {
                    return err(-32_020, format!("Transaction {b} not found"));
                }
            }
            let start = match cfg["before"].as_str() {
                Some(b) => list
                    .iter()
                    .position(|x| x.0 == b)
                    .map_or(list.len(), |p| p + 1),
                None => 0,
            };
            let until = cfg["until"].as_str();
            let mut out = Vec::new();
            for x in &list[start..] {
                if Some(x.0.as_str()) == until {
                    break;
                }
                out.push(
                    json!({"signature": x.0, "slot": x.1, "err": null, "memo": null,
                    "blockTime": x.2, "confirmationStatus": "finalized"}),
                );
                if out.len() == limit {
                    break;
                }
            }
            ok(Value::Array(out))
        }
        "getTransaction" => {
            let sig = params[0].as_str().unwrap_or("").to_string();
            m.fetched.lock().expect("lock").insert(sig.clone());
            if let Some((code, msg)) = m.error_for.lock().expect("lock").get(&sig).cloned() {
                return err(code, msg);
            }
            if m.hide.lock().expect("lock").contains(&sig) {
                ok(Value::Null)
            } else {
                ok(m.txs.get(&sig).cloned().unwrap_or(Value::Null))
            }
        }
        _ => ok(Value::Null),
    }
}

async fn serve_mock(m: Mock) -> SocketAddr {
    let app = Router::new()
        .route("/", axum::routing::post(rpc))
        .with_state(m);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = l.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(l, app).await.expect("serve") });
    addr
}

fn mock_of(list: Listing, txs: HashMap<String, Value>, page_cap: usize) -> Mock {
    Mock {
        by_addr: Arc::new(list),
        txs: Arc::new(txs),
        hide: Arc::new(Mutex::new(HashSet::new())),
        error_for: Arc::new(Mutex::new(HashMap::new())),
        page_cap,
        fetched: Arc::new(Mutex::new(HashSet::new())),
    }
}

fn mock_of_session(s: &ingest::session::Session, page_cap: usize) -> Mock {
    let mut by_addr: Listing = HashMap::new();
    let mut txs = HashMap::new();
    type Ordered = (u64, u32, String, Vec<Pubkey>, Option<i64>);
    let mut order: Vec<Ordered> = Vec::new();
    for t in &s.txs {
        let raw = ingest::rpc::to_raw_tx(&t.transaction, t.tx_index, Commitment::Finalized)
            .expect("to_raw_tx");
        let sig = raw.signature.to_string();
        let mut v = serde_json::to_value(&t.transaction).expect("json");
        v["transactionIndex"] = json!(t.tx_index);
        txs.insert(sig.clone(), v);
        order.push((t.slot, t.tx_index, sig, raw.account_keys, raw.block_time));
    }
    order.sort_by_key(|o| std::cmp::Reverse((o.0, o.1)));
    for (slot, _, sig, keys, bt) in order {
        let mut seen = HashSet::new();
        for key in keys {
            if seen.insert(key) {
                by_addr
                    .entry(key.to_string())
                    .or_default()
                    .push((sig.clone(), slot, bt));
            }
        }
    }
    mock_of(by_addr, txs, page_cap)
}

fn cfg_for(s: &ingest::session::Session, addr: SocketAddr) -> IndexerConfig {
    let mut cfg = IndexerConfig::default();
    cfg.ingest.rpc_url = format!("http://{addr}/");
    cfg.ingest.program_id = s.header.program_id.clone();
    cfg.ingest.backfill.max_rps = 1_000_000;
    cfg
}

fn lifecycle() -> ingest::session::Session {
    ingest::session::read(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/sessions/mock_lifecycle.jsonl"
    )))
    .expect("session")
}

/// The second successful transaction of `s` with a `Deposit` (well before the end).
fn a_deposit(s: &ingest::session::Session) -> (String, u64) {
    let ids = ProgramIds {
        shortwin: s.header.program_id.parse().expect("id"),
    };
    s.txs
        .iter()
        .filter_map(|t| {
            let raw =
                ingest::rpc::to_raw_tx(&t.transaction, t.tx_index, Commitment::Finalized).ok()?;
            let d = ingest::decode::decode(&raw, &ids, &venue::PoolCtx::default(), &|_| None);
            d.events
                .iter()
                .any(|e| e.kind == "Deposit")
                .then(|| (raw.signature.to_string(), raw.slot))
        })
        .nth(1)
        .expect("a deposit")
}

/// M8-SEC-2: a listed transaction the node answers `null` for stops the backfill before it —
/// nothing at or after its slot is written, no cursor passes it, no history is marked complete —
/// and the next run (the node serves it again) ingests it: the end state equals the chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unserved_transaction_stops_the_backfill_before_it() {
    let pg = pg().await;
    let s = lifecycle();
    let m = mock_of_session(&s, 1_000);
    let addr = serve_mock(m.clone()).await;
    let cfg = cfg_for(&s, addr);
    let (victim, victim_slot) = a_deposit(&s);
    m.hide.lock().expect("lock").insert(victim.clone());
    let db = pg.fresh_db().await.expect("db");
    let e = ingest::run_backfill(&cfg, &db)
        .await
        .expect_err("the backfill stops at the unserved transaction");
    assert!(format!("{e:#}").contains(&victim), "{e:#}");
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM ingest_txs WHERE slot >= $1")
        .bind(i64::try_from(victim_slot).expect("slot"))
        .fetch_one(&db)
        .await
        .expect("count");
    assert_eq!(
        after, 0,
        "nothing at or after the victim's slot was written"
    );
    let cursors: Vec<(Option<i64>, bool)> =
        sqlx::query_as("SELECT newest_slot, backfill_complete FROM ingest_cursors")
            .fetch_all(&db)
            .await
            .expect("cursors");
    assert!(
        cursors
            .iter()
            .all(|(n, c)| !c && n.is_none_or(|n| n < i64::try_from(victim_slot).expect("slot"))),
        "{cursors:?}"
    );
    // The node serves it again: the next run fetches it and completes the history.
    m.hide.lock().expect("lock").clear();
    ingest::run_backfill(&cfg, &db).await.expect("backfill 2");
    assert!(m.fetched.lock().expect("lock").contains(&victim));
    let mism = ingest_common::end_state_mismatches(&db, &s).await;
    assert!(mism.is_empty(), "{mism:?}");
    let complete: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ingest_cursors WHERE NOT backfill_complete")
            .fetch_one(&db)
            .await
            .expect("cursors");
    assert_eq!(complete, 0);
    db.close().await;
}

/// M8-SEC-2: an error whose text says "not found" (here JSON-RPC `-32601 Method not found`) is
/// an error, not a missing transaction: the backfill fails instead of skipping it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_rpc_error_is_never_read_as_a_missing_transaction() {
    let pg = pg().await;
    let s = lifecycle();
    let m = mock_of_session(&s, 1_000);
    let addr = serve_mock(m.clone()).await;
    let cfg = cfg_for(&s, addr);
    let (victim, _) = a_deposit(&s);
    m.error_for
        .lock()
        .expect("lock")
        .insert(victim.clone(), (-32_601, "Method not found".to_string()));
    let db = pg.fresh_db().await.expect("db");
    let e = ingest::run_backfill(&cfg, &db)
        .await
        .expect_err("an RPC error stops the backfill");
    assert!(format!("{e:#}").contains("Method not found"), "{e:#}");
    let present: i64 = scalar_i64(
        &db,
        "SELECT count(*) FROM ingest_cursors WHERE backfill_complete",
    )
    .await;
    assert_eq!(present, 0);
    db.close().await;
}

fn synthetic_history(program: Pubkey, total: u64) -> Listing {
    let list: Vec<(String, u64, Option<i64>)> = (0..total)
        .rev()
        .map(|i| {
            let mut b = [3u8; 64];
            b[..8].copy_from_slice(&i.to_le_bytes());
            (
                Signature::from(b).to_string(),
                1_000 + i,
                Some(1_790_000_000),
            )
        })
        .collect();
    HashMap::from([(program.to_string(), list)])
}

/// M8-SEC-11: `page_limit` above the RPC's 1,000 is refused at start, and a provider that caps
/// pages below `page_limit` does not end the history early (paging runs to an empty page).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn short_pages_never_end_the_history_early() {
    let pg = pg().await;
    let program = Pubkey::new_unique();
    let total = 2_500u64;
    // The provider returns at most 400 per page whatever the limit; the transactions themselves
    // are unknown to it (`null`): only the listing matters here, so the run stops at the first
    // fetch — after the listing, which must have reached all 2,500.
    let m = mock_of(synthetic_history(program, total), HashMap::new(), 400);
    let addr = serve_mock(m.clone()).await;
    let mut cfg = IndexerConfig::default();
    cfg.ingest.rpc_url = format!("http://{addr}/");
    cfg.ingest.program_id = program.to_string();
    cfg.ingest.backfill.max_rps = 1_000_000;
    cfg.ingest.backfill.page_limit = 2_000;
    let db = pg.fresh_db().await.expect("db");
    let e = ingest::run_backfill(&cfg, &db).await.expect_err("refused");
    assert!(format!("{e:#}").contains("page_limit"), "{e:#}");
    // Within the RPC's range, a 400-entry page under a 1,000 limit is not the end.
    let rpc = ingest::rpc::Rpc::new(&cfg.ingest.rpc_url, 1_000_000);
    let (listed, start) =
        ingest::rpc::list_signatures(&rpc, &program, None, None, 1_000, Commitment::Finalized)
            .await
            .expect("list");
    assert_eq!(listed.len(), 2_500);
    assert!(start);
    assert_eq!(
        listed.last().map(|l| l.slot),
        Some(1_000),
        "the true genesis"
    );
    db.close().await;
}

/// M8-SEC-5: a listing bound the node does not know (a rolled-back or pruned signature: -32020)
/// falls back to its slot instead of failing forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_listing_bound_falls_back_to_its_slot() {
    let program = Pubkey::new_unique();
    let m = mock_of(synthetic_history(program, 50), HashMap::new(), 1_000);
    let addr = serve_mock(m).await;
    let rpc = ingest::rpc::Rpc::new(&format!("http://{addr}/"), 1_000_000);
    let unknown = Signature::from([0x11; 64]);
    // `until` unknown, at slot 1,040: everything at or above slot 1,040 is listed.
    let (listed, start) = ingest::rpc::list_signatures(
        &rpc,
        &program,
        None,
        Some(ingest::rpc::Bound {
            signature: unknown,
            slot: Some(1_040),
        }),
        7,
        Commitment::Finalized,
    )
    .await
    .expect("falls back to the slot");
    assert_eq!(listed.len(), 10);
    assert!(listed.iter().all(|l| l.slot >= 1_040));
    assert!(!start, "a slot bound is not the start of history");
    // `before` unknown, at slot 1,009: everything at or below it.
    let (listed, start) = ingest::rpc::list_signatures(
        &rpc,
        &program,
        Some(ingest::rpc::Bound {
            signature: unknown,
            slot: Some(1_009),
        }),
        None,
        7,
        Commitment::Finalized,
    )
    .await
    .expect("falls back to the slot");
    assert_eq!(listed.len(), 10);
    assert!(listed.iter().all(|l| l.slot <= 1_009));
    assert!(start);
    // Without a slot to fall back to, the error stands.
    assert!(
        ingest::rpc::list_signatures(
            &rpc,
            &program,
            None,
            Some(ingest::rpc::Bound {
                signature: unknown,
                slot: None,
            }),
            7,
            Commitment::Finalized,
        )
        .await
        .is_err()
    );
}

// ---- the finality tracker (M8-SEC-1, -5, -12) ------------------------------------------------------

/// A cluster that knows one transaction: its status (in `slot`), what `getTransaction` at
/// finalized serves, and what the finalized block at its stored slot says.
struct Cluster {
    finalized: u64,
    status: Option<TxStatus>,
    served: Option<RawTx>,
    block: BlockCheck,
}

impl StatusSource for Cluster {
    async fn finalized_slot(&self) -> anyhow::Result<u64> {
        Ok(self.finalized)
    }
    async fn statuses(&self, sigs: &[Signature]) -> anyhow::Result<Vec<Option<TxStatus>>> {
        Ok(sigs.iter().map(|_| self.status).collect())
    }
    async fn fetch_finalized(&self, _: &Signature) -> anyhow::Result<Option<RawTx>> {
        Ok(self.served.clone())
    }
    async fn block_check(&self, _: u64, _: &Signature) -> anyhow::Result<BlockCheck> {
        Ok(self.block)
    }
}

fn crank(sig: u8, slot: u64) -> RawTx {
    let mut t = tx(
        sig,
        slot,
        vec![sw_top(
            shortwin::instruction::CrankPrice::DISCRIMINATOR.to_vec(),
            vec![],
        )],
        vec![],
    );
    t.commitment = Commitment::Confirmed;
    t
}

async fn row_of(pool: &PgPool, t: &RawTx) -> Option<(i64, String)> {
    sqlx::query_as("SELECT slot, commitment FROM ingest_txs WHERE signature = $1")
        .bind(t.signature.to_string())
        .fetch_optional(pool)
        .await
        .expect("row")
}

/// M8-SEC-1 / -12: the tracker deletes history only on proof. "No status" with the transaction
/// served at finalized in its slot → promoted; with the block unavailable → kept confirmed (and
/// the finalized watermark held below it); with the finalized block lacking it → rolled back. A
/// transaction finalized in another slot is moved there (rolled back, re-ingested with its true
/// slot), never promoted in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_finality_tracker_deletes_history_only_on_proof() {
    let pg = pg().await;
    /// (what, the cluster, the row after the pass, (promoted, rolled back, re-ingested,
    /// unresolved)).
    type Case = (
        &'static str,
        Box<dyn Fn(&RawTx) -> Cluster>,
        Option<(i64, &'static str)>,
        (u64, u64, u64, u64),
    );
    let cases: Vec<Case> = vec![
        (
            "no status, served at finalized in its slot",
            Box::new(|t: &RawTx| Cluster {
                finalized: 200,
                status: None,
                served: Some(RawTx {
                    commitment: Commitment::Finalized,
                    ..t.clone()
                }),
                block: BlockCheck::Unknown,
            }),
            Some((100, "finalized")),
            (1, 0, 0, 0),
        ),
        (
            "no status, block unavailable",
            Box::new(|_: &RawTx| Cluster {
                finalized: 200,
                status: None,
                served: None,
                block: BlockCheck::Unknown,
            }),
            Some((100, "confirmed")),
            (0, 0, 0, 1),
        ),
        (
            "no status, the finalized block lacks it",
            Box::new(|_: &RawTx| Cluster {
                finalized: 200,
                status: None,
                served: None,
                block: BlockCheck::Lacks,
            }),
            None,
            (0, 1, 0, 0),
        ),
        (
            "finalized in slot 150",
            Box::new(|t: &RawTx| Cluster {
                finalized: 200,
                status: Some(TxStatus {
                    slot: 150,
                    finalized: true,
                }),
                served: Some(RawTx {
                    slot: 150,
                    tx_index: 3,
                    block_time: Some(1_790_001_500),
                    commitment: Commitment::Finalized,
                    ..t.clone()
                }),
                block: BlockCheck::Unknown,
            }),
            Some((150, "finalized")),
            (0, 1, 1, 0),
        ),
        (
            "finalized in its slot",
            Box::new(|_: &RawTx| Cluster {
                finalized: 200,
                status: Some(TxStatus {
                    slot: 100,
                    finalized: true,
                }),
                served: None,
                block: BlockCheck::Unknown,
            }),
            Some((100, "finalized")),
            (1, 0, 0, 0),
        ),
    ];
    for (what, cluster, want_row, (promoted, rolled_back, reingested, unresolved)) in cases {
        let pool = pg.fresh_db().await.expect("db");
        let mut w = Writer::open(&pool, ProgramIds::default(), "ws_live")
            .await
            .expect("writer");
        let t = crank(30, 100);
        w.write(&t).await.expect("write");
        let r = finality::run_once(&mut w, &cluster(&t), 256)
            .await
            .expect("finality");
        assert_eq!(
            (r.promoted, r.rolled_back, r.reingested, r.unresolved),
            (promoted, rolled_back, reingested, unresolved),
            "{what}: {r:?}"
        );
        let row = row_of(&pool, &t).await;
        assert_eq!(row, want_row.map(|(s, c)| (s, c.to_string())), "{what}");
        let fin = scalar_i64(&pool, "SELECT finalized_slot FROM ingest_state").await;
        if unresolved > 0 {
            assert!(
                fin < 100,
                "{what}: the watermark stays below the unresolved one ({fin})"
            );
        }
        w.close().await.expect("close");
        pool.close().await;
    }
}

/// M8-SEC-5: a rollback never leaves a cursor naming the rolled-back transaction: the stream's
/// cursor falls back to its newest finalized transaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rollback_resets_the_cursors_that_named_it() {
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let mut w = Writer::open(&pool, ProgramIds::default(), "ws_live")
        .await
        .expect("writer");
    let stream = CursorAdvance {
        stream: format!("program:{}", shortwin::ID),
        address: shortwin::ID,
    };
    let mut finalized = crank(40, 90);
    finalized.commitment = Commitment::Finalized;
    w.write_with(&finalized, std::slice::from_ref(&stream))
        .await
        .expect("finalized");
    let confirmed = crank(41, 100);
    w.write_with(&confirmed, std::slice::from_ref(&stream))
        .await
        .expect("confirmed");
    let cursor = || async {
        sqlx::query_as::<_, (Option<String>, Option<i64>)>(
            "SELECT newest_signature::text, newest_slot FROM ingest_cursors WHERE stream = $1",
        )
        .bind(&stream.stream)
        .fetch_one(&pool)
        .await
        .expect("cursor")
    };
    assert_eq!(
        cursor().await,
        (Some(confirmed.signature.to_string()), Some(100))
    );
    let cluster = Cluster {
        finalized: 200,
        status: None,
        served: None,
        block: BlockCheck::Lacks,
    };
    let r = finality::run_once(&mut w, &cluster, 256)
        .await
        .expect("finality");
    assert_eq!(r.rolled_back, 1);
    assert_eq!(
        cursor().await,
        (Some(finalized.signature.to_string()), Some(90))
    );
    w.close().await.expect("close");
    pool.close().await;
}

// ---- M8-SEC-13: event layouts and the logs ---------------------------------------------------------

/// A `Deposit` with appended bytes (a newer program version) is applied on its known prefix and
/// reported; an undecodable one is reported, per market too; a response without `logMessages`
/// is a cut log stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_longer_event_layout_is_applied_and_reported() {
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let mut w = Writer::open(&pool, ProgramIds::default(), "rpc_backfill")
        .await
        .expect("writer");
    let market = k(1);
    let mint = k(4);
    let mc = market_created(market, mint, 20, 6);
    let cm = shortwin::instruction::CreateMarket::DISCRIMINATOR.to_vec();
    w.write(&tx(
        40,
        20,
        vec![sw_top(cm, vec![sw_event(&ev(&mc))])],
        vec![],
    ))
    .await
    .expect("market");
    let dep = shortwin::events::Deposit {
        market,
        slot: 21,
        ts: 2,
        owner: k(9),
        amount: 1_000,
        shares: 1_000_000,
        t_after: 1_000,
        s_after: 1_000_000,
        sol_paid: 0,
        pot_draw: 0,
        lock: 0,
        lock_until: 0,
        epoch: 0,
    };
    let mut payload = ev(&dep);
    payload.extend(0u64.to_le_bytes()); // a field appended by a later program version
    let d = shortwin::instruction::Deposit::DISCRIMINATOR.to_vec();
    w.write(&tx(
        41,
        21,
        vec![sw_top(d.clone(), vec![sw_event(&payload)])],
        vec![],
    ))
    .await
    .expect("deposit");
    let lenders = scalar_i64(&pool, "SELECT count(*) FROM lender_positions").await;
    assert_eq!(lenders, 1);
    let t: String = sqlx::query_scalar("SELECT tokens_free::text FROM markets")
        .fetch_one(&pool)
        .await
        .expect("market");
    assert_eq!(t, "1000");
    // A truncated payload is not applied, and the market says so.
    let short = ev(&dep)[..60].to_vec();
    w.write(&tx(42, 22, vec![sw_top(d, vec![sw_event(&short)])], vec![]))
        .await
        .expect("broken");
    w.close().await.expect("close");
    let status = api_get(&pool, "/v1/status").await;
    assert_eq!(status["layout_warnings"], 1, "{status}");
    assert_eq!(status["quarantined"], 1, "{status}");
    assert_eq!(status["missing"], 0, "{status}");
    let detail = api_get(&pool, &format!("/v1/markets/{mint}")).await;
    assert_eq!(detail["quarantined_events"], 1, "{detail}");
    // logMessages absent → the transaction's logs count as cut.
    let s = lifecycle();
    let mut t = s.txs[0].transaction.clone();
    t.transaction.meta.as_mut().expect("meta").log_messages =
        solana_transaction_status_client_types::option_serializer::OptionSerializer::None;
    let raw = ingest::rpc::to_raw_tx(&t, 0, Commitment::Finalized).expect("converts");
    assert!(raw.logs_truncated());
    pool.close().await;
}

// ---- M8-SEC-14: late rows without events reach completed rollup hours -----------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_rows_without_events_recompute_their_completed_hour() {
    let pg = pg().await;
    let pool = pg.fresh_db().await.expect("db");
    let mut w = Writer::open(&pool, ProgramIds::default(), "rpc_backfill")
        .await
        .expect("writer");
    let market = k(1);
    let mut mc = market_created(market, k(4), 20, 6);
    mc.ts = 1_790_000_200;
    let cm = shortwin::instruction::CreateMarket::DISCRIMINATOR.to_vec();
    let mut t = tx(50, 20, vec![sw_top(cm, vec![sw_event(&ev(&mc))])], vec![]);
    t.block_time = Some(1_790_000_200);
    w.write(&t).await.expect("market");
    let crank_at = |sig: u8, slot: u64, ts: i64, with_event: bool| {
        let e = shortwin::events::PriceCranked {
            market,
            slot,
            ts,
            spot_wad: 1,
            ema_price: 1,
            ema_liq: 1,
            liq_slow: 1,
            ema_count: 1,
            lb_lo: 1,
            lb_hi: 1,
            tier: 3,
            util_bps: 0,
            rent_basis: [0; 3],
            vest_bal: 0,
        };
        let inner = if with_event {
            vec![sw_event(&ev(&e))]
        } else {
            vec![]
        };
        let mut t = tx(sig, slot, vec![], vec![]);
        t.block_time = Some(ts);
        t.instructions = vec![RawTopIx {
            ix: ix(
                shortwin::ID,
                1,
                shortwin::instruction::CrankPrice::DISCRIMINATOR.to_vec(),
                vec![k(9), market, k(10), k(11), k(200), shortwin::ID],
            ),
            inner,
        }];
        t
    };
    w.write(&crank_at(51, 2_000, 1_790_000_000 + 3 * 3_600 + 10, true))
        .await
        .expect("crank");
    shortwin_indexer::analytics::rollups::pass(&pool, None)
        .await
        .expect("pass 1");
    // Late (a gap-fill): a successful no-op observation two hours back, without an event.
    w.write(&crank_at(52, 1_000, 1_790_000_000 + 3_600 + 5, false))
        .await
        .expect("late observation");
    w.close().await.expect("close");
    let p2 = shortwin_indexer::analytics::rollups::pass(&pool, None)
        .await
        .expect("pass 2");
    assert!(p2.late_reset, "{p2:?}");
    let hour = 1_790_000_000 / 3_600 * 3_600 + 3_600;
    let got: i64 = sqlx::query_scalar(
        "SELECT observations::bigint FROM market_stats_hourly WHERE bucket_start = $1",
    )
    .bind(hour)
    .fetch_one(&pool)
    .await
    .expect("hour");
    assert_eq!(got, 1, "the late observation is in its hour");
    pool.close().await;
}

/// M8-SEC-2 (live): a notified signature the node did not serve waits in `ingest_missing` and is
/// ingested once served; one whose finalized block lacks it (an abandoned fork) leaves the table;
/// one the cluster cannot place yet stays (and backs off).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_notified_signatures_are_ingested_or_proven_abandoned() {
    let pg = pg().await;
    let s = lifecycle();
    let m = mock_of_session(&s, 1_000);
    let addr = serve_mock(m).await;
    let rpc = ingest::rpc::Rpc::new(&format!("http://{addr}/"), 1_000_000);
    let ids = ProgramIds {
        shortwin: s.header.program_id.parse().expect("id"),
    };
    let streams = vec![ingest::rpc::Stream {
        name: format!("program:{}", ids.shortwin),
        address: ids.shortwin,
    }];
    let served = ingest::rpc::to_raw_tx(
        &s.txs[0].transaction,
        s.txs[0].tx_index,
        Commitment::Finalized,
    )
    .expect("converts");
    let db = pg.fresh_db().await.expect("db");
    let gone = Signature::from([0x22; 64]);
    let unknown = Signature::from([0x33; 64]);
    for (sig, slot) in [(served.signature, served.slot), (gone, 10), (unknown, 11)] {
        sqlx::query(
            "INSERT INTO ingest_missing (signature, slot, reason, last_attempt_at) \
             VALUES ($1, $2, 'test', now() - interval '1 hour')",
        )
        .bind(sig.to_string())
        .bind(i64::try_from(slot).expect("slot"))
        .execute(&db)
        .await
        .expect("missing");
    }
    struct Blocks {
        gone: Signature,
    }
    impl StatusSource for Blocks {
        async fn finalized_slot(&self) -> anyhow::Result<u64> {
            Ok(1_000_000)
        }
        async fn statuses(&self, sigs: &[Signature]) -> anyhow::Result<Vec<Option<TxStatus>>> {
            Ok(sigs.iter().map(|_| None).collect())
        }
        async fn fetch_finalized(&self, _: &Signature) -> anyhow::Result<Option<RawTx>> {
            Ok(None)
        }
        async fn block_check(&self, _: u64, sig: &Signature) -> anyhow::Result<BlockCheck> {
            Ok(if *sig == self.gone {
                BlockCheck::Lacks
            } else {
                BlockCheck::Unknown
            })
        }
    }
    let mut w = Writer::open(&db, ids, "ws_live").await.expect("writer");
    let r = ingest::live::retry_missing(&mut w, &rpc, &Blocks { gone }, &streams, 16)
        .await
        .expect("retry");
    assert_eq!((r.ingested, r.abandoned, r.pending), (1, 1, 1), "{r:?}");
    let left: Vec<(String, i32)> =
        sqlx::query_as("SELECT signature::text, attempts FROM ingest_missing")
            .fetch_all(&db)
            .await
            .expect("left");
    assert_eq!(left, vec![(unknown.to_string(), 1)]);
    let written = scalar_i64(&db, "SELECT count(*) FROM ingest_txs").await;
    assert_eq!(written, 1);
    // Backing off: asked again at once, nothing is due.
    let r = ingest::live::retry_missing(&mut w, &rpc, &Blocks { gone }, &streams, 16)
        .await
        .expect("retry");
    assert_eq!((r.ingested, r.abandoned, r.pending), (0, 0, 0), "{r:?}");
    let status = api_get(&db, "/v1/status").await;
    assert_eq!(status["missing"], 1, "{status}");
    w.close().await.expect("close");
    db.close().await;
}
