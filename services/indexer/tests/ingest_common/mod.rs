//! Shared helpers of the INGEST integration tests (`tests/ingest_*.rs`; INDEXER.md §11.5): the
//! embedded Postgres, the committed LiteSVM-recorded sessions, deterministic table dumps and the
//! end-state comparison of the projections with a session's final account checkpoint.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::Value;
use shortwin_indexer::ingest::decode::idl;
use shortwin_indexer::ingest::session::{self, Session};
use shortwin_indexer::testing::pg::TestPg;
use sqlx::PgPool;

/// The committed sessions (recorded from the LiteSVM harness; INDEXER.md §11.3).
pub fn sessions_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/ingest"))
}

/// Every committed session, by file name; plus REPLAY's `tests/data/sessions/*.jsonl` when present.
pub fn all_sessions() -> Vec<(String, Session)> {
    let mut out = Vec::new();
    let mut dirs = vec![sessions_dir()];
    let replay = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sessions"));
    if replay.is_dir() {
        dirs.push(replay);
    }
    if let Some(extra) = std::env::var_os("SHORTWIN_INGEST_SESSIONS") {
        dirs = vec![PathBuf::from(extra)];
    }
    for dir in dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        files.sort();
        for f in files {
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push((
                name,
                session::read(&f).unwrap_or_else(|e| panic!("{}: {e:#}", f.display())),
            ));
        }
    }
    out
}

/// One committed session.
pub fn load(name: &str) -> Session {
    session::read(&sessions_dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e:#}"))
}

/// A server for one test binary.
pub async fn pg() -> TestPg {
    TestPg::start_in(Path::new(env!("CARGO_TARGET_TMPDIR")))
        .await
        .expect("the embedded PostgreSQL starts")
}

/// The tables a dump covers, with the wall-clock or ingestion-sequence columns left out.
const DUMP: &[(&str, &[&str])] = &[
    ("ingest_txs", &["ingested_at", "seq"]),
    ("ingest_quarantine", &["created_at"]),
    ("events", &["id"]),
    ("invocations", &[]),
    ("venue_trades", &[]),
    ("venue_events", &[]),
    ("venue_program_upgrades", &[]),
    ("markets", &["updated_at"]),
    ("market_state_log", &[]),
    ("positions", &[]),
    ("lender_positions", &[]),
    ("share_epochs", &[]),
    ("settlements", &[]),
    ("fees", &[]),
    ("protocol_sweeps", &[]),
    ("market_snapshots", &[]),
    ("coverage_events", &[]),
    ("pool_events", &[]),
    ("pools", &[]),
    ("config_changes", &[]),
    ("protocol_params_history", &[]),
    ("protocol_state", &[]),
    ("venue_guards", &[]),
    ("venue_extras", &[]),
    ("account_snapshots", &["fetched_at"]),
    ("oracle_prices", &["observed_at"]),
    ("venue_pools", &["updated_at"]),
];

/// The projection tables (what a rebuild recomputes).
pub const PROJECTIONS: &[&str] = &[
    "markets",
    "market_state_log",
    "positions",
    "lender_positions",
    "share_epochs",
    "settlements",
    "fees",
    "protocol_sweeps",
    "market_snapshots",
    "coverage_events",
    "pool_events",
    "pools",
    "config_changes",
    "protocol_params_history",
    "protocol_state",
    "venue_guards",
    "venue_extras",
];

/// A deterministic dump of `tables` (every table of [`DUMP`] when empty): table → rows as JSON,
/// ordered by their text.
pub async fn dump(pool: &PgPool, tables: &[&str]) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for (t, skip) in DUMP {
        if !tables.is_empty() && !tables.contains(t) {
            continue;
        }
        let skip: Vec<String> = skip.iter().map(|s| (*s).to_string()).collect();
        let sql = format!(
            "SELECT COALESCE(jsonb_agg(x ORDER BY x::text), '[]'::jsonb) FROM \
             (SELECT to_jsonb(t) - $1::text[] AS x FROM {t} t) s"
        );
        let v: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(skip)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("dumping {t}: {e}"));
        out.push(((*t).to_string(), v));
    }
    out
}

/// Asserts two dumps are equal, naming the first table that differs.
pub fn assert_same(a: &[(String, Value)], b: &[(String, Value)], what: &str) {
    assert_eq!(a.len(), b.len());
    for ((ta, va), (tb, vb)) in a.iter().zip(b) {
        assert_eq!(ta, tb);
        if va != vb {
            let ra = va.as_array().map_or(0, Vec::len);
            let rb = vb.as_array().map_or(0, Vec::len);
            let first = va
                .as_array()
                .zip(vb.as_array())
                .and_then(|(x, y)| x.iter().zip(y).find(|(p, q)| p != q))
                .map(|(p, q)| format!("\n left: {p}\nright: {q}"))
                .unwrap_or_default();
            panic!("{what}: table {ta} differs ({ra} vs {rb} rows){first}");
        }
    }
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        other => other.to_string(),
    }
}

/// Compares the event-derived [E] projections with the session's final checkpoint (the chain):
/// every `Market`'s accounting, flags and venue, every `MarketVault`'s ledgers, every open
/// `ShortPosition` and every `LenderPosition`. Returns the mismatches.
pub async fn end_state_mismatches(pool: &PgPool, session: &Session) -> Vec<String> {
    let Some(cp) = session.end_state() else {
        return vec![];
    };
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut out = Vec::new();
    for a in &cp.accounts {
        if a.owner != session.header.program_id {
            continue;
        }
        let Ok(data) = b64.decode(&a.data) else {
            continue;
        };
        let Ok((ty, j)) = idl::decode_account(&data) else {
            continue;
        };
        match ty.as_str() {
            "Market" => {
                let row: Option<Value> = sqlx::query_scalar(
                    "SELECT to_jsonb(m) FROM (SELECT tokens_free::text, borrowed_q::text, \
                       borrowed_q_tier0::text, borrowed_q_tier1::text, borrowed_q_tier2::text, \
                       borrowed_q_tier3::text, total_shares::text, share_epoch::text, \
                       open_positions::text, total_notional_open::text, max_q_hwm::text, \
                       venue::text, paused_opens, paused_coverage, mint_unsafe, route_broken, \
                       wipe_live, COALESCE(coverage_cleared_at, 0)::text AS coverage_cleared_at \
                     FROM markets WHERE market = $1) m",
                )
                .bind(&a.pubkey)
                .fetch_optional(pool)
                .await
                .expect("markets");
                let Some(row) = row else {
                    out.push(format!("market {} not indexed", a.pubkey));
                    continue;
                };
                let flags = j["flags"].as_u64().unwrap_or(0);
                let tiers = j["borrowed_q_by_tier"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let checks: Vec<(&str, String, String)> = vec![
                    ("tokens_free", s(&row["tokens_free"]), s(&j["tokens_free"])),
                    ("borrowed_q", s(&row["borrowed_q"]), s(&j["borrowed_q"])),
                    (
                        "borrowed_q_tier0",
                        s(&row["borrowed_q_tier0"]),
                        s(&tiers[0]),
                    ),
                    (
                        "borrowed_q_tier1",
                        s(&row["borrowed_q_tier1"]),
                        s(&tiers[1]),
                    ),
                    (
                        "borrowed_q_tier2",
                        s(&row["borrowed_q_tier2"]),
                        s(&tiers[2]),
                    ),
                    (
                        "borrowed_q_tier3",
                        s(&row["borrowed_q_tier3"]),
                        s(&tiers[3]),
                    ),
                    (
                        "total_shares",
                        s(&row["total_shares"]),
                        s(&j["total_shares"]),
                    ),
                    ("share_epoch", s(&row["share_epoch"]), s(&j["share_epoch"])),
                    (
                        "open_positions",
                        s(&row["open_positions"]),
                        s(&j["open_positions"]),
                    ),
                    (
                        "total_notional_open",
                        s(&row["total_notional_open"]),
                        s(&j["total_notional_open"]),
                    ),
                    ("max_q_hwm", s(&row["max_q_hwm"]), s(&j["max_q_hwm"])),
                    ("venue", s(&row["venue"]), s(&j["venue"])),
                    (
                        "paused_opens",
                        s(&row["paused_opens"]),
                        (flags & 1 != 0).to_string(),
                    ),
                    (
                        "mint_unsafe",
                        s(&row["mint_unsafe"]),
                        (flags & 2 != 0).to_string(),
                    ),
                    (
                        "route_broken",
                        s(&row["route_broken"]),
                        (flags & 4 != 0).to_string(),
                    ),
                    (
                        "wipe_live",
                        s(&row["wipe_live"]),
                        (flags & 32 != 0).to_string(),
                    ),
                    (
                        "paused_coverage",
                        s(&row["paused_coverage"]),
                        (flags & 64 != 0).to_string(),
                    ),
                    (
                        "coverage_cleared_at",
                        s(&row["coverage_cleared_at"]),
                        s(&j["coverage_cleared_at"]),
                    ),
                ];
                for (name, got, want) in checks {
                    if got != want {
                        out.push(format!(
                            "market {} {name}: indexed {got}, chain {want}",
                            a.pubkey
                        ));
                    }
                }
            }
            "MarketVault" => {
                let market = market_of_vault(pool, &a.pubkey).await;
                let Some(market) = market else { continue };
                let row: Option<(String, String)> = sqlx::query_as(
                    "SELECT lender_sol::text, protocol_fees::text FROM markets WHERE market = $1",
                )
                .bind(&market)
                .fetch_optional(pool)
                .await
                .expect("markets");
                let Some((ls, pf)) = row else { continue };
                if ls != s(&j["lender_sol"]) {
                    out.push(format!(
                        "vault of {market} lender_sol: indexed {ls}, chain {}",
                        s(&j["lender_sol"])
                    ));
                }
                if pf != s(&j["protocol_fees"]) {
                    out.push(format!(
                        "vault of {market} protocol_fees: indexed {pf}, chain {}",
                        s(&j["protocol_fees"])
                    ));
                }
            }
            "ShortPosition" => {
                let row: Option<Value> = sqlx::query_scalar(
                    // The chain's `m` includes `add_margin` top-ups (`positions.m` is the margin
                    // at open, `margin_added` the top-ups; INDEXER.md §6.5).
                    "SELECT to_jsonb(p) FROM (SELECT status, q::text, s::text, \
                       (m + margin_added)::text AS m, v::text, \
                       n_open::text, fee_open::text, deadline::text, tier::text, armed, \
                       COALESCE(s_count, 0)::text AS s_count, COALESCE(s_since, 0)::text AS s_since, \
                       rent_protocol_bps::text, expiry_fee_bps::text \
                     FROM positions WHERE position = $1) p",
                )
                .bind(&a.pubkey)
                .fetch_optional(pool)
                .await
                .expect("positions");
                let Some(row) = row else {
                    out.push(format!("position {} not indexed", a.pubkey));
                    continue;
                };
                let checks: Vec<(&str, String, String)> = vec![
                    ("status", s(&row["status"]), "open".into()),
                    ("q", s(&row["q"]), s(&j["q"])),
                    ("s", s(&row["s"]), s(&j["s"])),
                    ("m", s(&row["m"]), s(&j["m"])),
                    ("v", s(&row["v"]), s(&j["v"])),
                    ("n_open", s(&row["n_open"]), s(&j["n_open"])),
                    ("fee_open", s(&row["fee_open"]), s(&j["fee_open"])),
                    ("deadline", s(&row["deadline"]), s(&j["deadline"])),
                    ("tier", s(&row["tier"]), s(&j["tier"])),
                    (
                        "armed",
                        s(&row["armed"]),
                        (j["s_armed"].as_u64().unwrap_or(0) != 0).to_string(),
                    ),
                    (
                        "rent_protocol_bps",
                        s(&row["rent_protocol_bps"]),
                        s(&j["rent_protocol_bps"]),
                    ),
                    (
                        "expiry_fee_bps",
                        s(&row["expiry_fee_bps"]),
                        s(&j["expiry_fee_bps"]),
                    ),
                ];
                for (name, got, want) in checks {
                    if got != want {
                        out.push(format!(
                            "position {} {name}: indexed {got}, chain {want}",
                            a.pubkey
                        ));
                    }
                }
                if j["s_armed"].as_u64().unwrap_or(0) != 0 {
                    for (name, f) in [("s_count", "s_count"), ("s_since", "s_since")] {
                        if s(&row[name]) != s(&j[f]) {
                            out.push(format!(
                                "position {} {name}: indexed {}, chain {}",
                                a.pubkey,
                                s(&row[name]),
                                s(&j[f])
                            ));
                        }
                    }
                }
            }
            "LenderPosition" => {
                let row: Option<Value> = sqlx::query_scalar(
                    "SELECT to_jsonb(l) FROM (SELECT shares::text, epoch::text, lock::text, \
                       lock_until::text FROM lender_positions WHERE market = $1 AND owner = $2) l",
                )
                .bind(s(&j["market"]))
                .bind(s(&j["owner"]))
                .fetch_optional(pool)
                .await
                .expect("lender_positions");
                let Some(row) = row else {
                    out.push(format!("lender {} not indexed", a.pubkey));
                    continue;
                };
                for (name, want) in [
                    ("shares", s(&j["shares"])),
                    ("epoch", s(&j["epoch"])),
                    ("lock", s(&j["lock"])),
                    ("lock_until", s(&j["lock_until"])),
                ] {
                    if s(&row[name]) != want {
                        out.push(format!(
                            "lender {} {name}: indexed {}, chain {want}",
                            a.pubkey,
                            s(&row[name])
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

async fn market_of_vault(pool: &PgPool, vault: &str) -> Option<String> {
    let markets: Vec<String> = sqlx::query_scalar("SELECT market::text FROM markets")
        .fetch_all(pool)
        .await
        .ok()?;
    let program = shortwin::ID;
    markets.into_iter().find(|m| {
        m.parse::<solana_pubkey::Pubkey>()
            .map(|mk| {
                solana_pubkey::Pubkey::find_program_address(
                    &[b"market_vault", mk.as_ref()],
                    &program,
                )
                .0
                .to_string()
                    == vault
            })
            .unwrap_or(false)
    })
}
