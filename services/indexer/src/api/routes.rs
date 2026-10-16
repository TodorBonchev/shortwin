//! The REST routes of `openapi.yaml` (INDEXER.md §9.2). Owner: API.
//!
//! [`ROUTES`] is the route table: every path of `openapi.yaml` with its `operationId` (axum 0.8
//! writes path parameters as `{name}`, exactly as OpenAPI does). [`routes`] mounts each one from
//! the table, so the contract test that compares the table with the document also proves the
//! router serves exactly the documented paths.
//!
//! Handlers parse every parameter first ([`super::params`]), then run one or two static queries
//! that build the response objects in SQL ([`super::sql`]); lists are keyset-paginated
//! ([`super::pagination`]). The only dynamic SQL splices allowlisted identifiers (the market sort
//! column and order, the timeseries metric column).

use std::collections::HashMap;

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get};
use axum::{Json, Router};
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool};

use super::AppState;
use super::error::ApiError;
use super::pagination::{ChainKey, ChainRow, Cursor, page};
use super::params::{Params, canonical_key, like_prefix, path_key, path_signature};
use crate::analytics::formulas::convert_shares;

/// One documented path.
#[derive(Clone, Copy, Debug)]
pub struct Route {
    /// The OpenAPI path template (also axum's).
    pub path: &'static str,
    /// Its `get` operation's `operationId`.
    pub operation_id: &'static str,
}

const fn r(path: &'static str, operation_id: &'static str) -> Route {
    Route { path, operation_id }
}

/// Every path of `openapi.yaml` (all `GET`).
pub const ROUTES: &[Route] = &[
    r("/v1/health", "getHealth"),
    r("/v1/status", "getStatus"),
    r("/v1/markets", "listMarkets"),
    r("/v1/markets/{mint}", "getMarket"),
    r("/v1/markets/{mint}/positions", "listMarketPositions"),
    r("/v1/markets/{mint}/lenders", "listMarketLenders"),
    r("/v1/markets/{mint}/pools", "getMarketPools"),
    r("/v1/markets/{mint}/coverage", "getMarketCoverage"),
    r("/v1/markets/{mint}/liquidations", "listMarketLiquidations"),
    r("/v1/markets/{mint}/events", "listMarketEvents"),
    r("/v1/markets/{mint}/snapshots", "listMarketSnapshots"),
    r("/v1/markets/{mint}/trades", "listMarketTrades"),
    r("/v1/markets/{mint}/candles", "getMarketCandles"),
    r("/v1/markets/{mint}/stats", "getMarketStats"),
    r("/v1/markets/{mint}/fees", "getMarketFees"),
    r("/v1/positions/{pubkey}", "getPosition"),
    r("/v1/borrowers/{owner}/positions", "listBorrowerPositions"),
    r("/v1/lenders/{owner}", "getLender"),
    r("/v1/lenders/{owner}/events", "listLenderEvents"),
    r("/v1/stats", "getGlobalStats"),
    r("/v1/stats/timeseries", "getGlobalTimeseries"),
    r("/v1/settlements", "listSettlements"),
    r("/v1/liquidations", "listLiquidations"),
    r("/v1/keepers", "listKeepers"),
    r("/v1/admin/config", "getProtocolConfig"),
    r("/v1/admin/changes", "listConfigChanges"),
    r("/v1/admin/events", "listAdminEvents"),
    r("/v1/venues", "getVenues"),
    r("/v1/search", "search"),
    r("/v1/tx/{signature}", "getTransaction"),
    r("/v1/ws", "openWebSocket"),
];

fn handler(operation_id: &str) -> MethodRouter<AppState> {
    match operation_id {
        "getHealth" => get(health),
        "getStatus" => get(status),
        "listMarkets" => get(list_markets),
        "getMarket" => get(get_market),
        "listMarketPositions" => get(list_market_positions),
        "listMarketLenders" => get(list_market_lenders),
        "getMarketPools" => get(get_market_pools),
        "getMarketCoverage" => get(get_market_coverage),
        "listMarketLiquidations" => get(list_market_liquidations),
        "listMarketEvents" => get(list_market_events),
        "listMarketSnapshots" => get(list_market_snapshots),
        "listMarketTrades" => get(list_market_trades),
        "getMarketCandles" => get(get_market_candles),
        "getMarketStats" => get(get_market_stats),
        "getMarketFees" => get(get_market_fees),
        "getPosition" => get(get_position),
        "listBorrowerPositions" => get(list_borrower_positions),
        "getLender" => get(get_lender),
        "listLenderEvents" => get(list_lender_events),
        "getGlobalStats" => get(global_stats),
        "getGlobalTimeseries" => get(global_timeseries),
        "listSettlements" => get(list_settlements),
        "listLiquidations" => get(list_liquidations),
        "listKeepers" => get(list_keepers),
        "getProtocolConfig" => get(protocol_config),
        "listConfigChanges" => get(list_config_changes),
        "listAdminEvents" => get(list_admin_events),
        "getVenues" => get(venues),
        "search" => get(search),
        "getTransaction" => get(get_transaction),
        "openWebSocket" => get(super::ws::upgrade),
        other => unreachable!("no handler for operation {other} (api::routes::ROUTES)"),
    }
}

/// Every route of [`ROUTES`].
pub fn routes() -> Router<AppState> {
    ROUTES.iter().fold(Router::new(), |r, route| {
        r.route(route.path, handler(route.operation_id))
    })
}

// ---- vocabulary (openapi.yaml enums) ---------------------------------------------------------

/// `VenueName`.
pub const VENUE_NAMES: [&str; 8] = [
    "none",
    "pump_curve",
    "pump_migrating",
    "pump_swap",
    "raydium_v4",
    "raydium_cpmm",
    "mock",
    "unknown",
];
/// `TierName`.
pub const TIER_NAMES: [&str; 4] = ["A", "B", "C", "D"];
/// `PositionStatusFilter`.
const POSITION_FILTERS: [&str; 6] = [
    "open",
    "closed",
    "liquidated",
    "expired",
    "settled",
    "exited",
];
/// The lender events of `/v1/lenders/{owner}/events` and the `lender:` channel.
pub const LENDER_KINDS: [&str; 5] = [
    "Deposit",
    "Withdraw",
    "ClaimSol",
    "DeficitCovered",
    "LenderSynced",
];
/// Admin, emergency and latch events (§6.6, §7.3): `/v1/admin/events` and the `admin` channel.
pub const ADMIN_KINDS: [&str; 11] = [
    "ConfigProposed",
    "ConfigApplied",
    "ConfigCancelled",
    "PauseSet",
    "VenueDisabled",
    "RouteBrokenSet",
    "VenueExtrasSet",
    "VenueUpgradeFlagged",
    "VenueRevalidated",
    "CoveragePauseSet",
    "MintSafetyTripped",
];
/// The exits of `/v1/liquidations` and the `liquidations` channel.
pub const EXIT_KINDS: [&str; 3] = ["Liquidate", "Expire", "SolSettle"];
/// The `coverage` channel.
pub const COVERAGE_KINDS: [&str; 2] = ["CoverageGap", "CoveragePauseSet"];
/// At most this many buckets per series response (§10.2).
pub const MAX_BUCKETS: i64 = 1_000;

/// Every `EventKind`: the program's events (the retired straddle helper's two are no longer
/// ingested, design pump-final §9).
pub fn event_kinds() -> impl Iterator<Item = &'static str> {
    crate::model::SHORTWIN_EVENTS.iter().map(|s| s.name)
}

// ---- responses -------------------------------------------------------------------------------

/// A JSON body with `Cache-Control: public, max-age=2` (lists and current values, §9.1).
fn ok(v: Value) -> Response {
    with_cache(v, "public, max-age=2")
}

fn with_cache(v: Value, cache: &'static str) -> Response {
    let mut r = Json(v).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    r
}

type ApiResult = Result<Response, ApiError>;

/// The market of `mint` (404 when it is not listed).
async fn market_of(pool: &PgPool, mint: &str) -> Result<String, ApiError> {
    sqlx::query_scalar::<_, String>("SELECT market::text FROM markets WHERE mint = $1")
        .bind(mint)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no market for mint {mint}")))
}

/// The analytics clock (`sw_chain_now()`, §8).
async fn chain_now(pool: &PgPool) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT sw_chain_now()")
        .fetch_one(pool)
        .await?)
}

/// `(slot, tx_index, ix_index, ev_source, ev_seq)` binds of an optional keyset position.
type KeyBinds = (
    Option<i64>,
    Option<i32>,
    Option<i16>,
    Option<i16>,
    Option<i32>,
);

fn key_binds(k: Option<ChainKey>) -> KeyBinds {
    match k {
        Some(k) => (
            Some(k.slot),
            Some(k.tx_index),
            Some(k.ix_index),
            Some(k.ev_source),
            Some(k.ev_seq),
        ),
        None => (None, None, None, None, None),
    }
}

fn chain_page(rows: Vec<(Value, i64, i32, i16, i16, i32)>, limit: i64, tag: &str) -> Value {
    page(
        rows.into_iter()
            .map(|(j, a, b, c, d, e)| (j, ChainKey::from((a, b, c, d, e) as ChainRow)))
            .collect(),
        limit,
        |k| k.cursor(tag),
    )
}

/// `[from, to)` of a series request in chain seconds, at most [`MAX_BUCKETS`] buckets of `iv`
/// seconds (`range_too_large`); defaults: `to` = the end of the current bucket of chain time,
/// `from` = [`MAX_BUCKETS`] buckets before it.
async fn series_range(pool: &PgPool, q: &Params, iv: i64) -> Result<(i64, i64), ApiError> {
    let from = q.int("from", 0, i64::MAX)?;
    let to = match q.int("to", 0, i64::MAX)? {
        Some(t) => t,
        None => {
            let now = chain_now(pool).await?.max(0);
            (now / iv).saturating_add(1).saturating_mul(iv)
        }
    };
    let ceil_buckets = to / iv + i64::from(to % iv != 0);
    let from = from.unwrap_or_else(|| (ceil_buckets - MAX_BUCKETS).max(0) * iv);
    if from >= to {
        return Err(ApiError::bad_request("`from` must be before `to`"));
    }
    if ceil_buckets - from / iv > MAX_BUCKETS {
        return Err(ApiError::range_too_large(format!(
            "at most {MAX_BUCKETS} buckets of {iv} s per request"
        )));
    }
    Ok((from, to))
}

// ---- meta ------------------------------------------------------------------------------------

async fn health() -> Response {
    with_cache(json!({ "status": "ok" }), "no-store")
}

/// `Status`, with the data-quality counts an operator alerts on (M8-SEC-13): short.win / curve
/// event payloads not applied (`quarantined`), events applied on a known prefix of a longer
/// layout (`layout_warnings`), notified signatures the node has not served yet (`missing`), and
/// transactions of the last 24 h whose log stream was cut or absent (`logs_truncated`: their
/// `emit!` log events, `CoverageGap` and exit-path `MintSafetyTripped`, may be missing). Each
/// count reads a partial index of rows nobody else can mass-produce (events are attributed by
/// the invocation stack) or a bounded window.
const Q_STATUS: &str = "SELECT jsonb_build_object(\
    'confirmed_slot', s.confirmed_slot, 'finalized_slot', s.finalized_slot, \
    'latest_block_time', s.latest_block_time, \
    'lag_s', extract(epoch FROM now())::bigint - s.latest_block_time, \
    'streams', COALESCE((SELECT jsonb_agg(jsonb_build_object('stream', c.stream, \
        'newest_slot', c.newest_slot, 'backfill_complete', c.backfill_complete) \
        ORDER BY c.stream) FROM ingest_cursors c), '[]'::jsonb), \
    'rollups_complete_before', COALESCE((SELECT min(complete_before) FROM rollup_state), 0), \
    'quarantined', (SELECT count(*) FROM ingest_quarantine WHERE ev_source < 2 AND NOT applied), \
    'layout_warnings', (SELECT count(*) FROM ingest_quarantine WHERE ev_source < 2 AND applied), \
    'missing', (SELECT count(*) FROM ingest_missing), \
    'logs_truncated', (SELECT count(*) FROM ingest_txs WHERE logs_truncated \
        AND block_time >= sw_chain_now() - 86400)) \
    FROM ingest_state s";

async fn status(State(st): State<AppState>) -> ApiResult {
    let v: Value = sqlx::query_scalar(Q_STATUS).fetch_one(&st.pool).await?;
    Ok(with_cache(v, "no-store"))
}

// ---- markets ---------------------------------------------------------------------------------

const MARKET_SORTS: [&str; 5] = ["tvl", "oi", "util", "created", "open_positions"];

async fn list_markets(State(st): State<AppState>, q: Params) -> ApiResult {
    let text = q.string("q", 1, 64)?;
    let venue = q.choice("venue", &VENUE_NAMES)?;
    let tier = q.choice("tier", &TIER_NAMES)?;
    let has_positions = q.boolean("has_positions")?;
    let sort = q.choice("sort", &MARKET_SORTS)?.unwrap_or("tvl");
    let order = q.choice("order", &["asc", "desc"])?.unwrap_or("desc");
    let limit = q.limit(st.cfg.default_page_limit, st.cfg.max_page_limit)?;
    let tag = format!("mk.{sort}.{order}");
    let after = match q.cursor()? {
        Some(raw) => {
            let c = Cursor::decode(raw, &tag, 2)?;
            Some((c.decimal(0)?, c.key(1)?))
        }
        None => None,
    };
    let (after_v, after_m) = after.unzip();
    // Allowlisted identifiers only (§10.2).
    let col = match sort {
        "tvl" => "o.tvl_lamports",
        "oi" => "o.oi_lamports",
        "util" => "o.util_bps",
        "created" => "o.created_ts",
        _ => "o.open_positions",
    };
    let (dir, cmp) = if order == "desc" {
        ("DESC", "<")
    } else {
        ("ASC", ">")
    };
    let sql = format!(
        "SELECT {json}, ({col})::numeric::text, o.market::text \
         FROM v_market_overview o LEFT JOIN v_sol_usd u ON TRUE \
         WHERE ($1::text IS NULL OR sw_api_venue(o.venue) = $1) \
           AND ($2::text IS NULL OR sw_api_tier(o.tier) = $2) \
           AND ($3::bool IS NULL OR (o.open_positions > 0) = $3) \
           AND (CASE WHEN $4::text IS NULL AND $5::text IS NULL \
                THEN (o.open_positions > 0 OR o.lender_count > 0 OR o.total_shares > 0 \
                      OR EXISTS (SELECT 1 FROM positions p WHERE p.market = o.market)) \
                ELSE (o.mint = $4 OR lower(o.symbol) LIKE lower($5) ESCAPE '\\' \
                      OR lower(o.name) LIKE lower($5) ESCAPE '\\') END) \
           AND ($6::text IS NULL OR (({col})::numeric, o.market::text COLLATE \"C\") \
                {cmp} ($6::numeric, $7::text COLLATE \"C\")) \
         ORDER BY ({col})::numeric {dir}, o.market::text COLLATE \"C\" {dir} \
         LIMIT $8",
        json = market_summary_json!(),
    );
    let rows: Vec<(Value, String, String)> = sqlx::query_as(AssertSqlSafe(sql))
        .bind(venue)
        .bind(tier)
        .bind(has_positions)
        .bind(text.and_then(canonical_key))
        .bind(text.map(like_prefix))
        .bind(after_v)
        .bind(after_m)
        .bind(limit + 1)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(page(
        rows.into_iter().map(|(j, v, m)| (j, (v, m))).collect(),
        limit,
        |(v, m)| Cursor::encode(&tag, vec![json!(v), json!(m)]),
    )))
}

const Q_MARKET: &str = concat!(
    "SELECT ",
    market_summary_json!(),
    " || jsonb_build_object(\
     'creator', m.creator::text, 'coin_creator', m.coin_creator::text, \
     'token_program', m.token_program::text, 'bonding_curve', m.bonding_curve::text, \
     'canonical_pool', m.canonical_pool::text, 'ray_pool', m.ray_pool::text, \
     'graduated_at', m.graduated_at, 'total_shares', m.total_shares::text, \
     'share_epoch', m.share_epoch::bigint, 'total_notional_open', m.total_notional_open::text, \
     'open_vault_lamports', m.open_vault_lamports::text, 'lender_sol', m.lender_sol::text, \
     'protocol_fees', m.protocol_fees::text, 'lender_count', m.lender_count::bigint, \
     'lender_value_lamports', o.lender_value_lamports::text, \
     'inventory_value_lamports', o.inventory_value_lamports::text, 'spot_wad', m.spot_wad::text, \
     'route_chains', jsonb_build_object('no_route_since', m.no_route_since, \
         'pool_fail_since', m.pool_fail_since), \
     'fees', COALESCE((SELECT jsonb_agg(",
    fee_line_json!(),
    " ORDER BY f.fee_type) FROM v_market_fees f WHERE f.market = m.market), '[]'::jsonb), \
     'exits', (SELECT jsonb_build_object('positions', COALESCE(x.positions, 0), \
         'open', COALESCE(x.open, 0), 'closes', COALESCE(x.closes, 0), \
         'liquidations_cr', COALESCE(x.liquidations_cr, 0), \
         'liquidations_expired', COALESCE(x.liquidations_expired, 0), \
         'expiries', COALESCE(x.expiries, 0), 'settlements_s1', COALESCE(x.settlements_s1, 0), \
         'settlements_s2', COALESCE(x.settlements_s2, 0), \
         'settlements_s3', COALESCE(x.settlements_s3, 0), 'exits', COALESCE(x.exits, 0), \
         'avg_duration_s', round(x.avg_duration_s, 3)::float8, \
         'close_rate_bps', x.close_rate_bps, 'liquidation_rate_bps', x.liquidation_rate_bps, \
         'expiry_rate_bps', x.expiry_rate_bps, 'settlement_rate_bps', x.settlement_rate_bps, \
         'avg_open_impact_bps', round(x.avg_open_impact_bps, 3)::float8, \
         'avg_exit_slippage_bps', round(x.avg_exit_slippage_bps, 3)::float8, \
         'borrower_pnl', COALESCE(x.borrower_pnl, 0)::text) \
         FROM (SELECT 1) one LEFT JOIN v_market_exits x ON x.market = m.market), \
     'lender_loss', (SELECT jsonb_build_object('settlements', COALESCE(l.settlements, 0), \
         'token_value', COALESCE(l.token_value, 0)::text, \
         'lender_amount', COALESCE(l.lender_amount, 0)::text, \
         'lender_pnl', COALESCE(l.lender_pnl, 0)::text, \
         'realised_lender_loss', COALESCE(l.realised_lender_loss, 0)::text, \
         'loss_events', COALESCE(l.loss_events, 0), \
         'loss_bps_of_value', l.loss_bps_of_value::bigint, \
         'loss_bps_of_notional', l.loss_bps_of_notional::bigint, \
         'realised_lender_loss_s1', COALESCE(l.realised_lender_loss_s1, 0)::text, \
         'realised_lender_loss_s2', COALESCE(l.realised_lender_loss_s2, 0)::text, \
         'realised_lender_loss_s3', COALESCE(l.realised_lender_loss_s3, 0)::text) \
         FROM (SELECT 1) one LEFT JOIN v_market_lender_loss l ON l.market = m.market), \
     'creator_exposure', (SELECT jsonb_build_object('coin_creator', c.coin_creator::text, \
         'coin_creator_shares', c.coin_creator_shares::text, \
         'coin_creator_share_bps', c.coin_creator_share_bps::bigint, \
         'coin_creator_deposited_tokens', c.coin_creator_deposited_tokens::text, \
         'creator_fee_income_lamports', c.creator_fee_income_lamports::text, \
         'shortwin_volume_lamports', c.shortwin_volume_lamports::text) \
         FROM v_market_creator c WHERE c.market = m.market), \
     'coverage', (SELECT ",
    coverage_stats_json!(),
    " FROM sw_market_coverage(m.market) v), \
     'quarantined_events', (SELECT count(*) FROM ingest_quarantine q \
         WHERE q.market = m.market AND q.ev_source < 2 AND NOT q.applied), \
     'tier_terms', (SELECT h.tiers -> (m.tier::int) FROM protocol_params_history h \
         ORDER BY h.slot DESC, h.tx_index DESC, h.ix_index DESC LIMIT 1)) \
     FROM markets m JOIN v_market_overview o ON o.market = m.market \
     LEFT JOIN v_sol_usd u ON TRUE WHERE m.mint = $1"
);

async fn get_market(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let v: Option<Value> = sqlx::query_scalar(Q_MARKET)
        .bind(&mint)
        .fetch_optional(&st.pool)
        .await?;
    v.map(ok)
        .ok_or_else(|| ApiError::not_found(format!("no market for mint {mint}")))
}

// ---- positions -------------------------------------------------------------------------------

const Q_POSITIONS: &str = concat!(
    "SELECT ",
    position_json!(),
    ", p.opened_slot, p.opened_tx_index, p.position::text \
     FROM positions p JOIN markets m ON m.market = p.market \
     WHERE ($1::text IS NULL OR p.market = $1) AND ($2::text IS NULL OR p.borrower = $2) \
       AND ($3::text IS NULL OR p.status = $3 OR ($3 = 'exited' AND p.status <> 'open')) \
       AND ($4::bigint IS NULL OR (p.opened_slot, p.opened_tx_index, p.position::text COLLATE \"C\") \
            < ($4, $5::int, $6::text COLLATE \"C\")) \
     ORDER BY p.opened_slot DESC, p.opened_tx_index DESC, p.position::text COLLATE \"C\" DESC \
     LIMIT $7"
);

async fn positions_page(
    st: &AppState,
    q: &Params,
    market: Option<&str>,
    borrower: Option<&str>,
) -> ApiResult {
    let status = q.choice("status", &POSITION_FILTERS)?;
    let limit = q.limit(st.cfg.default_page_limit, st.cfg.max_page_limit)?;
    let after = match q.cursor()? {
        Some(raw) => {
            let c = Cursor::decode(raw, "pos", 3)?;
            Some((
                c.int(0, 0, i64::MAX)?,
                i32::try_from(c.int(1, 0, i64::from(i32::MAX))?)
                    .map_err(|_| ApiError::invalid_cursor())?,
                c.key(2)?,
            ))
        }
        None => None,
    };
    let rows: Vec<(Value, i64, i32, String)> = sqlx::query_as(Q_POSITIONS)
        .bind(market)
        .bind(borrower)
        .bind(status)
        .bind(after.as_ref().map(|a| a.0))
        .bind(after.as_ref().map(|a| a.1))
        .bind(after.as_ref().map(|a| a.2.clone()))
        .bind(limit + 1)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(page(
        rows.into_iter()
            .map(|(j, s, t, p)| (j, (s, t, p)))
            .collect(),
        limit,
        |(s, t, p)| Cursor::encode("pos", vec![json!(s), json!(t), json!(p)]),
    )))
}

async fn list_market_positions(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
    q: Params,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let borrower = q.pubkey("borrower")?;
    let market = market_of(&st.pool, &mint).await?;
    positions_page(&st, &q, Some(&market), borrower.as_deref()).await
}

async fn list_borrower_positions(
    State(st): State<AppState>,
    owner: Result<Path<String>, PathRejection>,
    q: Params,
) -> ApiResult {
    let owner = path_key(owner, "owner")?;
    positions_page(&st, &q, None, Some(&owner)).await
}

const Q_POSITION: &str = concat!(
    "SELECT ",
    position_json!(),
    ", COALESCE((SELECT jsonb_agg(x.j ORDER BY x.slot, x.tx_index, x.ix_index, x.ev_source, \
         x.ev_seq) FROM (SELECT ",
    event_json!(),
    " AS j, e.slot, e.tx_index, e.ix_index, e.ev_source, e.ev_seq FROM events e \
         WHERE e.position = p.position \
         ORDER BY e.slot, e.tx_index, e.ix_index, e.ev_source, e.ev_seq LIMIT 1000) x), \
         '[]'::jsonb), \
     (SELECT ",
    settlement_json!(),
    " FROM settlements s JOIN markets sm ON sm.market = s.market \
         WHERE s.position = p.position) \
     FROM positions p JOIN markets m ON m.market = p.market CROSS JOIN ingest_state st \
     WHERE p.position = $1"
);

async fn get_position(
    State(st): State<AppState>,
    key: Result<Path<String>, PathRejection>,
) -> ApiResult {
    let key = path_key(key, "pubkey")?;
    let row: Option<(Value, Value, Option<Value>)> = sqlx::query_as(Q_POSITION)
        .bind(&key)
        .fetch_optional(&st.pool)
        .await?;
    let (position, events, settlement) =
        row.ok_or_else(|| ApiError::not_found(format!("no position {key}")))?;
    Ok(ok(json!({
        "position": position,
        "events": events,
        "settlement": settlement,
    })))
}

// ---- lenders ---------------------------------------------------------------------------------

/// What a lender row's `share_bps` needs: `(market, shares, epoch, market's share_epoch,
/// market's total_shares)`.
type ShareCols = (String, String, i64, i64, String);

/// Fills `share_bps` (`⌊shares·10⁴/S⌋` at the market's current epoch, INDEXER.md §6.3) into each
/// lender object: a lender behind the epoch is converted through the `share_epochs` records it
/// has not applied yet (`null` when one is missing or `S = 0`).
async fn fill_share_bps(
    pool: &PgPool,
    objs: &mut [Value],
    cols: &[ShareCols],
) -> Result<(), ApiError> {
    let behind: Vec<String> = cols
        .iter()
        .filter(|(_, _, e, me, _)| e < me)
        .map(|(m, ..)| m.clone())
        .collect();
    let mut epochs: HashMap<(String, i64), (u8, u128)> = HashMap::new();
    if !behind.is_empty() {
        let recs: Vec<(String, i64, i16, String)> = sqlx::query_as(
            "SELECT market::text, epoch::bigint, num::smallint, den::text FROM share_epochs \
             WHERE market = ANY($1::text[])",
        )
        .bind(&behind)
        .fetch_all(pool)
        .await?;
        for (m, e, num, den) in recs {
            if let (Ok(num), Ok(den)) = (u8::try_from(num), den.parse::<u128>()) {
                epochs.insert((m, e), (num, den));
            }
        }
    }
    for (v, (market, shares, epoch, market_epoch, total)) in objs.iter_mut().zip(cols) {
        let bps = (|| {
            let total: u128 = total.parse().ok().filter(|t| *t > 0)?;
            let mut s: u128 = shares.parse().ok()?;
            for e in *epoch..*market_epoch {
                let (num, den) = epochs.get(&(market.clone(), e))?;
                s = convert_shares(s, *num, *den);
            }
            shortwin_math::num::mul_div(s, 10_000, total, shortwin_math::Rounding::Floor)
                .ok()
                .and_then(|b| i64::try_from(b).ok())
        })();
        if let Some(o) = v.as_object_mut() {
            o.insert("share_bps".to_string(), json!(bps));
        }
    }
    Ok(())
}

const Q_MARKET_LENDERS: &str = concat!(
    "SELECT ",
    lender_json!(),
    ", ",
    lender_share_cols!(),
    ", lp.owner::text \
     FROM lender_positions lp JOIN markets m ON m.market = lp.market \
     WHERE lp.market = $1 AND (NOT $2 OR lp.shares > 0) \
       AND ($6::text IS NULL OR ($6::text = 'behind') = (lp.epoch < m.share_epoch)) \
       AND ($3::text IS NULL OR (lp.shares::numeric, lp.owner::text COLLATE \"C\") \
            < ($3::numeric, $4::text COLLATE \"C\")) \
     ORDER BY lp.shares::numeric DESC, lp.owner::text COLLATE \"C\" DESC LIMIT $5"
);

type LenderRow = (Value, String, String, i64, i64, String, String);

/// `epoch` of `listMarketLenders`: lenders behind the market's share epoch (a rebase or full-wipe
/// record they have not converted through, parked lenders included) or at it. The web's claim
/// reads the behind lenders first for its pot-only syncs (FRONTEND.md §3.6, FE-30).
const LENDER_EPOCHS: [&str; 2] = ["behind", "current"];

async fn list_market_lenders(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
    q: Params,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let active = q.boolean("active")?.unwrap_or(true);
    let epoch = q.choice("epoch", &LENDER_EPOCHS)?;
    let limit = q.limit(st.cfg.default_page_limit, st.cfg.max_page_limit)?;
    let after = match q.cursor()? {
        Some(raw) => {
            let c = Cursor::decode(raw, "lp", 2)?;
            Some((c.decimal(0)?, c.key(1)?))
        }
        None => None,
    };
    let market = market_of(&st.pool, &mint).await?;
    let (after_s, after_o) = after.unzip();
    let rows: Vec<LenderRow> = sqlx::query_as(Q_MARKET_LENDERS)
        .bind(&market)
        .bind(active)
        .bind(after_s)
        .bind(after_o)
        .bind(limit + 1)
        .bind(epoch)
        .fetch_all(&st.pool)
        .await?;
    let mut objs = Vec::with_capacity(rows.len());
    let mut cols = Vec::with_capacity(rows.len());
    let mut keys = Vec::with_capacity(rows.len());
    for (j, m, s, e, me, t, owner) in rows {
        objs.push(j);
        keys.push((s.clone(), owner));
        cols.push((m, s, e, me, t));
    }
    fill_share_bps(&st.pool, &mut objs, &cols).await?;
    Ok(ok(page(
        objs.into_iter().zip(keys).collect(),
        limit,
        |(s, o)| Cursor::encode("lp", vec![json!(s), json!(o)]),
    )))
}

const Q_LENDER: &str = concat!(
    "SELECT ",
    lender_json!(),
    ", ",
    lender_share_cols!(),
    " FROM lender_positions lp JOIN markets m ON m.market = lp.market \
     WHERE lp.owner = $1 ORDER BY lp.first_slot, lp.market::text COLLATE \"C\" LIMIT 1000"
);

async fn get_lender(
    State(st): State<AppState>,
    owner: Result<Path<String>, PathRejection>,
) -> ApiResult {
    let owner = path_key(owner, "owner")?;
    let rows: Vec<(Value, String, String, i64, i64, String)> = sqlx::query_as(Q_LENDER)
        .bind(&owner)
        .fetch_all(&st.pool)
        .await?;
    let (mut positions, cols): (Vec<Value>, Vec<ShareCols>) = rows
        .into_iter()
        .map(|(j, m, s, e, me, t)| (j, (m, s, e, me, t)))
        .unzip();
    fill_share_bps(&st.pool, &mut positions, &cols).await?;
    let (markets, sol_paid): (i64, String) = sqlx::query_as(
        "SELECT count(*), COALESCE(sum(sol_paid), 0)::text FROM lender_positions \
         WHERE owner = $1",
    )
    .bind(&owner)
    .fetch_one(&st.pool)
    .await?;
    Ok(ok(json!({
        "owner": owner,
        "positions": positions,
        "totals": { "markets": markets, "sol_paid": sol_paid },
    })))
}

// ---- events ----------------------------------------------------------------------------------

macro_rules! events_query {
    ($filter:literal) => {
        concat!(
            "SELECT ",
            event_json!(),
            ", e.slot, e.tx_index, e.ix_index, e.ev_source, e.ev_seq \
             FROM events e CROSS JOIN ingest_state st WHERE ",
            $filter,
            " AND ($3::bigint IS NULL OR (e.slot, e.tx_index, e.ix_index, e.ev_source, e.ev_seq) \
                  < ($3, $4::int, $5::smallint, $6::smallint, $7::int)) \
             ORDER BY e.slot DESC, e.tx_index DESC, e.ix_index DESC, e.ev_source DESC, \
                  e.ev_seq DESC LIMIT $8"
        )
    };
}

const Q_MARKET_EVENTS: &str =
    events_query!("e.market = $1 AND ($2::text[] IS NULL OR e.kind = ANY($2))");
const Q_ACTOR_EVENTS: &str = events_query!("e.actor = $1 AND e.kind = ANY($2)");
const Q_KIND_EVENTS: &str = events_query!("($1::text IS NULL) AND e.kind = ANY($2)");

async fn events_page(
    st: &AppState,
    q: &Params,
    sql: &'static str,
    first: Option<&str>,
    kinds: Option<Vec<String>>,
) -> ApiResult {
    let limit = q.limit(st.cfg.default_page_limit, st.cfg.max_page_limit)?;
    let after = ChainKey::decode_opt(q.cursor()?, "ev")?;
    let (a, b, c, d, e) = key_binds(after);
    let rows: Vec<(Value, i64, i32, i16, i16, i32)> = sqlx::query_as(sql)
        .bind(first)
        .bind(kinds)
        .bind(a)
        .bind(b)
        .bind(c)
        .bind(d)
        .bind(e)
        .bind(limit + 1)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(chain_page(rows, limit, "ev")))
}

/// `kind=` (comma-separated `EventKind`s, deduplicated).
fn parse_kinds(q: &Params) -> Result<Option<Vec<String>>, ApiError> {
    let Some(raw) = q.string("kind", 1, 512)? else {
        return Ok(None);
    };
    let mut kinds: Vec<String> = Vec::new();
    for k in raw.split(',').map(str::trim) {
        let known = event_kinds()
            .find(|n| *n == k)
            .ok_or_else(|| ApiError::bad_request(format!("unknown event kind `{k}`")))?;
        if !kinds.iter().any(|x| x == known) {
            kinds.push(known.to_string());
        }
    }
    Ok(Some(kinds))
}

async fn list_market_events(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
    q: Params,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let kinds = parse_kinds(&q)?;
    let market = market_of(&st.pool, &mint).await?;
    events_page(&st, &q, Q_MARKET_EVENTS, Some(&market), kinds).await
}

async fn list_lender_events(
    State(st): State<AppState>,
    owner: Result<Path<String>, PathRejection>,
    q: Params,
) -> ApiResult {
    let owner = path_key(owner, "owner")?;
    let kinds = LENDER_KINDS.iter().map(|k| k.to_string()).collect();
    events_page(&st, &q, Q_ACTOR_EVENTS, Some(&owner), Some(kinds)).await
}

async fn list_admin_events(State(st): State<AppState>, q: Params) -> ApiResult {
    let kinds = ADMIN_KINDS.iter().map(|k| k.to_string()).collect();
    events_page(&st, &q, Q_KIND_EVENTS, None, Some(kinds)).await
}

// ---- exits and settlements -------------------------------------------------------------------

const Q_EXITS: &str = concat!(
    "SELECT ",
    exit_json!(),
    ", e.slot, e.tx_index, e.ix_index, e.ev_source, e.ev_seq \
     FROM events e JOIN positions p ON p.position = e.position \
     JOIN markets m ON m.market = e.market \
     LEFT JOIN settlements s ON s.position = e.position AND e.kind = 'SolSettle' \
     CROSS JOIN ingest_state st \
     WHERE e.kind IN ('Liquidate', 'Expire', 'SolSettle') \
       AND ($1::text IS NULL OR e.market = $1) \
       AND ($2::bigint IS NULL OR (e.slot, e.tx_index, e.ix_index, e.ev_source, e.ev_seq) \
            < ($2, $3::int, $4::smallint, $5::smallint, $6::int)) \
     ORDER BY e.slot DESC, e.tx_index DESC, e.ix_index DESC, e.ev_source DESC, e.ev_seq DESC \
     LIMIT $7"
);

async fn exits_page(st: &AppState, q: &Params, market: Option<&str>) -> ApiResult {
    let limit = q.limit(st.cfg.default_page_limit, st.cfg.max_page_limit)?;
    let after = ChainKey::decode_opt(q.cursor()?, "ex")?;
    let (a, b, c, d, e) = key_binds(after);
    let rows: Vec<(Value, i64, i32, i16, i16, i32)> = sqlx::query_as(Q_EXITS)
        .bind(market)
        .bind(a)
        .bind(b)
        .bind(c)
        .bind(d)
        .bind(e)
        .bind(limit + 1)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(chain_page(rows, limit, "ex")))
}

async fn list_market_liquidations(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
    q: Params,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let market = market_of(&st.pool, &mint).await?;
    exits_page(&st, &q, Some(&market)).await
}

async fn list_liquidations(State(st): State<AppState>, q: Params) -> ApiResult {
    exits_page(&st, &q, None).await
}

const Q_SETTLEMENTS: &str = concat!(
    "SELECT ",
    settlement_json!(),
    ", s.slot, s.tx_index, s.ix_index, s.ev_source, s.ev_seq \
     FROM settlements s JOIN markets sm ON sm.market = s.market CROSS JOIN ingest_state st \
     WHERE ($1::smallint IS NULL OR s.condition = $1) AND ($2::text IS NULL OR sm.mint = $2) \
       AND ($3::bigint IS NULL OR (s.slot, s.tx_index, s.ix_index, s.ev_source, s.ev_seq) \
            < ($3, $4::int, $5::smallint, $6::smallint, $7::int)) \
     ORDER BY s.slot DESC, s.tx_index DESC, s.ix_index DESC, s.ev_source DESC, s.ev_seq DESC \
     LIMIT $8"
);

async fn list_settlements(State(st): State<AppState>, q: Params) -> ApiResult {
    let condition = q
        .choice("condition", &["S1", "S2", "S3"])?
        .map(|c| match c {
            "S1" => 1i16,
            "S2" => 2,
            _ => 3,
        });
    let mint = q.pubkey("mint")?;
    let limit = q.limit(st.cfg.default_page_limit, st.cfg.max_page_limit)?;
    let after = ChainKey::decode_opt(q.cursor()?, "se")?;
    let (a, b, c, d, e) = key_binds(after);
    let rows: Vec<(Value, i64, i32, i16, i16, i32)> = sqlx::query_as(Q_SETTLEMENTS)
        .bind(condition)
        .bind(mint)
        .bind(a)
        .bind(b)
        .bind(c)
        .bind(d)
        .bind(e)
        .bind(limit + 1)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(chain_page(rows, limit, "se")))
}

/// Keepers by rewards; `failures` from the counters (`keeper_failure_counts`, one keeper's rows
/// per keeper listed — never every fee payer's failed transactions, M8-SEC-3).
const Q_KEEPERS: &str = "SELECT jsonb_build_object('keeper', k.keeper, \
    'liquidations', k.liquidations, 'expiries', k.expiries, 'settlements', k.settlements, \
    'rewards_lamports', k.rewards::text, 'markets', k.markets, \
    'failures', COALESCE(f.failures, 0)), k.rewards::text, k.keeper \
    FROM (SELECT keeper::text AS keeper, sum(liquidations)::bigint AS liquidations, \
          sum(expiries)::bigint AS expiries, sum(settlements)::bigint AS settlements, \
          COALESCE(sum(rewards_lamports), 0) AS rewards, count(DISTINCT market) AS markets \
          FROM v_keeper_activity WHERE keeper IS NOT NULL GROUP BY keeper) k \
    LEFT JOIN LATERAL (SELECT sum(c.failures)::bigint AS failures FROM keeper_failure_counts c \
               WHERE c.keeper = k.keeper) f ON TRUE \
    WHERE ($1::text IS NULL OR (k.rewards, k.keeper COLLATE \"C\") \
          < ($1::numeric, $2::text COLLATE \"C\")) \
    ORDER BY k.rewards DESC, k.keeper COLLATE \"C\" DESC LIMIT $3";

async fn list_keepers(State(st): State<AppState>, q: Params) -> ApiResult {
    let limit = q.limit(st.cfg.default_page_limit, st.cfg.max_page_limit)?;
    let after = match q.cursor()? {
        Some(raw) => {
            let c = Cursor::decode(raw, "kp", 2)?;
            Some((c.decimal(0)?, c.key(1)?))
        }
        None => None,
    };
    let (after_r, after_k) = after.unzip();
    let rows: Vec<(Value, String, String)> = sqlx::query_as(Q_KEEPERS)
        .bind(after_r)
        .bind(after_k)
        .bind(limit + 1)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(page(
        rows.into_iter().map(|(j, r, k)| (j, (r, k))).collect(),
        limit,
        |(r, k)| Cursor::encode("kp", vec![json!(r), json!(k)]),
    )))
}

// ---- market detail lists ---------------------------------------------------------------------

const Q_POOLS: &str = concat!(
    "SELECT jsonb_build_object('mint', m.mint::text, \
     'current', COALESCE(m.ray_pool::text, (SELECT pl.pool::text FROM pools pl \
         WHERE pl.market = m.market AND pl.is_current LIMIT 1)), \
     'window', CASE WHEN m.fp_candidate IS NULL THEN NULL ELSE jsonb_build_object(\
         'candidate', m.fp_candidate::text, 'd_cand', COALESCE(m.fp_d, 0)::text, \
         'window_ends', COALESCE(m.fp_window_ends, 0)) END, \
     'pools', COALESCE((SELECT jsonb_agg(jsonb_build_object('pool', pl.pool::text, \
         'venue', sw_api_venue(pl.venue), 'first_seen_ts', pl.first_seen_ts, \
         'last_d', pl.last_d::text, 'proposals', pl.proposals::bigint, \
         'adopted_ts', pl.adopted_ts, 'retired_ts', pl.retired_ts, 'is_current', pl.is_current) \
         ORDER BY pl.first_seen_slot, pl.pool::text COLLATE \"C\") \
         FROM pools pl WHERE pl.market = m.market), '[]'::jsonb), \
     'events', COALESCE((SELECT jsonb_agg(x.j ORDER BY x.slot DESC, x.tx_index DESC, \
         x.ix_index DESC, x.ev_source DESC, x.ev_seq DESC) FROM (SELECT ",
    event_json!(),
    " AS j, e.slot, e.tx_index, e.ix_index, e.ev_source, e.ev_seq FROM events e \
         WHERE e.market = m.market \
           AND e.kind IN ('PoolProposed', 'PoolSwitched', 'PoolSwitchCancelled') \
         ORDER BY e.slot DESC, e.tx_index DESC, e.ix_index DESC, e.ev_source DESC, \
           e.ev_seq DESC LIMIT 200) x), '[]'::jsonb)) \
     FROM markets m CROSS JOIN ingest_state st WHERE m.mint = $1"
);

async fn get_market_pools(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let v: Option<Value> = sqlx::query_scalar(Q_POOLS)
        .bind(&mint)
        .fetch_optional(&st.pool)
        .await?;
    v.map(ok)
        .ok_or_else(|| ApiError::not_found(format!("no market for mint {mint}")))
}

const Q_COVERAGE: &str = concat!(
    "SELECT jsonb_build_object('mint', m.mint::text, 'stats', ",
    coverage_stats_json!(),
    ", 'recent_gaps', COALESCE((SELECT jsonb_agg(x.j ORDER BY x.slot DESC, x.tx_index DESC, \
         x.ix_index DESC, x.ev_source DESC, x.ev_seq DESC) FROM (SELECT jsonb_build_object(",
    chain_ref_args!("c", "ts"),
    ", 'gap_s', COALESCE(c.gap_s, 0), 'open_positions', COALESCE(c.open_positions, 0)::bigint) \
         AS j, c.slot, c.tx_index, c.ix_index, c.ev_source, c.ev_seq FROM coverage_events c \
         WHERE c.market = m.market AND c.kind = 'gap' \
         ORDER BY c.slot DESC, c.tx_index DESC, c.ix_index DESC, c.ev_source DESC, \
           c.ev_seq DESC LIMIT 100) x), '[]'::jsonb), \
     'pauses', COALESCE((SELECT jsonb_agg(jsonb_build_object('paused_at', pz.paused_at, \
         'resumed_at', pz.resumed_at, 'paused_s', pz.paused_s, 'ongoing', pz.ongoing, \
         'by', pz.by_key::text) ORDER BY pz.paused_at DESC) \
         FROM (SELECT * FROM v_coverage_pauses vp WHERE vp.market = m.market \
               ORDER BY vp.paused_at DESC LIMIT 100) pz), '[]'::jsonb)) \
     FROM markets m CROSS JOIN LATERAL sw_market_coverage(m.market) v \
     CROSS JOIN ingest_state st WHERE m.mint = $1"
);

async fn get_market_coverage(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let v: Option<Value> = sqlx::query_scalar(Q_COVERAGE)
        .bind(&mint)
        .fetch_optional(&st.pool)
        .await?;
    v.map(ok)
        .ok_or_else(|| ApiError::not_found(format!("no market for mint {mint}")))
}

const Q_SNAPSHOTS: &str = concat!(
    "SELECT ",
    snapshot_json!(),
    ", ms.slot, ms.tx_index, ms.ix_index, ms.ev_source, ms.ev_seq \
     FROM market_snapshots ms CROSS JOIN ingest_state st \
     WHERE ms.market = $1 AND ($2::bigint IS NULL OR ms.ts >= $2) \
       AND ($3::bigint IS NULL OR ms.ts < $3) \
       AND ($4::bigint IS NULL OR (ms.slot, ms.tx_index, ms.ix_index, ms.ev_source, ms.ev_seq) \
            > ($4, $5::int, $6::smallint, $7::smallint, $8::int)) \
     ORDER BY ms.slot, ms.tx_index, ms.ix_index, ms.ev_source, ms.ev_seq LIMIT $9"
);

/// `from` / `to` of a list (no bucket limit), `from < to` when both are given.
fn time_filter(q: &Params) -> Result<(Option<i64>, Option<i64>), ApiError> {
    let from = q.int("from", 0, i64::MAX)?;
    let to = q.int("to", 0, i64::MAX)?;
    if let (Some(f), Some(t)) = (from, to)
        && f >= t
    {
        return Err(ApiError::bad_request("`from` must be before `to`"));
    }
    Ok((from, to))
}

async fn list_market_snapshots(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
    q: Params,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let (from, to) = time_filter(&q)?;
    let limit = q.limit(st.cfg.default_page_limit, st.cfg.max_page_limit)?;
    let after = ChainKey::decode_opt(q.cursor()?, "sn")?;
    let market = market_of(&st.pool, &mint).await?;
    let (a, b, c, d, e) = key_binds(after);
    let rows: Vec<(Value, i64, i32, i16, i16, i32)> = sqlx::query_as(Q_SNAPSHOTS)
        .bind(&market)
        .bind(from)
        .bind(to)
        .bind(a)
        .bind(b)
        .bind(c)
        .bind(d)
        .bind(e)
        .bind(limit + 1)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(chain_page(rows, limit, "sn")))
}

const Q_TRADES: &str = concat!(
    "SELECT ",
    trade_json!(),
    ", vt.slot, vt.tx_index, vt.ix_index, vt.ev_source, vt.ev_seq \
     FROM venue_trades vt CROSS JOIN ingest_state st \
     WHERE vt.market = $1 AND (NOT $2 OR vt.is_shortwin) \
       AND ($3::bigint IS NULL OR vt.ts >= $3) AND ($4::bigint IS NULL OR vt.ts < $4) \
       AND ($5::bigint IS NULL OR (vt.slot, vt.tx_index, vt.ix_index, vt.ev_source, vt.ev_seq) \
            < ($5, $6::int, $7::smallint, $8::smallint, $9::int)) \
     ORDER BY vt.slot DESC, vt.tx_index DESC, vt.ix_index DESC, vt.ev_source DESC, \
       vt.ev_seq DESC LIMIT $10"
);

async fn list_market_trades(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
    q: Params,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let shortwin_only = q.boolean("shortwin_only")?.unwrap_or(false);
    let (from, to) = time_filter(&q)?;
    let limit = q.limit(st.cfg.default_page_limit, st.cfg.max_page_limit)?;
    let after = ChainKey::decode_opt(q.cursor()?, "tr")?;
    // The market's trades: its pools' (M8-SEC-6), not every pool that names the mint.
    let market = market_of(&st.pool, &mint).await?;
    let (a, b, c, d, e) = key_binds(after);
    let rows: Vec<(Value, i64, i32, i16, i16, i32)> = sqlx::query_as(Q_TRADES)
        .bind(&market)
        .bind(shortwin_only)
        .bind(from)
        .bind(to)
        .bind(a)
        .bind(b)
        .bind(c)
        .bind(d)
        .bind(e)
        .bind(limit + 1)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(chain_page(rows, limit, "tr")))
}

const CANDLE_INTERVALS: [(&str, i64); 6] = [
    ("1m", 60),
    ("5m", 300),
    ("15m", 900),
    ("1h", 3_600),
    ("4h", 14_400),
    ("1d", 86_400),
];

const Q_CANDLES: &str = "SELECT jsonb_build_object('t', x.b, \
    'open', (array_agg(x.price_wad::numeric ORDER BY x.slot, x.tx_index, x.ix_index, \
        x.ev_source, x.ev_seq))[1]::text, \
    'high', max(x.price_wad::numeric)::text, 'low', min(x.price_wad::numeric)::text, \
    'close', (array_agg(x.price_wad::numeric ORDER BY x.slot DESC, x.tx_index DESC, \
        x.ix_index DESC, x.ev_source DESC, x.ev_seq DESC))[1]::text, \
    'volume_sol', sum(x.sol_amount)::text, 'volume_tokens', sum(x.token_amount)::text, \
    'trades', count(*)) \
    FROM (SELECT (vt.ts / $4) * $4 AS b, vt.slot, vt.tx_index, vt.ix_index, vt.ev_source, \
          vt.ev_seq, vt.price_wad, vt.sol_amount, vt.token_amount FROM venue_trades vt \
          WHERE vt.market = $1 AND vt.ts >= $2 AND vt.ts < $3 AND vt.price_wad IS NOT NULL) x \
    GROUP BY x.b ORDER BY x.b LIMIT 1000";

async fn get_market_candles(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
    q: Params,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let names: Vec<&'static str> = CANDLE_INTERVALS.iter().map(|(n, _)| *n).collect();
    let interval = Params::required("interval", q.choice("interval", &names)?)?;
    let iv = CANDLE_INTERVALS
        .iter()
        .find(|(n, _)| *n == interval)
        .map_or(60, |(_, s)| *s);
    let market = market_of(&st.pool, &mint).await?;
    let (from, to) = series_range(&st.pool, &q, iv).await?;
    let data: Vec<Value> = sqlx::query_scalar(Q_CANDLES)
        .bind(&market)
        .bind(from)
        .bind(to)
        .bind(iv)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(json!({ "interval": interval, "data": data })))
}

const STATS_INTERVALS: [(&str, i64); 2] = [("1h", 3_600), ("1d", 86_400)];

const Q_MARKET_STATS_HOURLY: &str = concat!(
    "SELECT ",
    stats_bucket_json!("bucket_start"),
    " FROM market_stats_hourly h WHERE h.market = $1 AND h.bucket_start >= $2 \
     AND h.bucket_start < $3 ORDER BY h.bucket_start LIMIT 1000"
);
const Q_MARKET_STATS_DAILY: &str = concat!(
    "SELECT ",
    stats_bucket_json!("day_start"),
    " FROM v_market_stats_daily h WHERE h.market = $1 AND h.day_start >= $2 \
     AND h.day_start < $3 ORDER BY h.day_start LIMIT 1000"
);

fn stats_interval(q: &Params) -> Result<(&'static str, i64), ApiError> {
    let name = q.choice("interval", &["1h", "1d"])?.unwrap_or("1h");
    let iv = STATS_INTERVALS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(3_600, |(_, s)| *s);
    Ok((name, iv))
}

async fn get_market_stats(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
    q: Params,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let (interval, iv) = stats_interval(&q)?;
    let market = market_of(&st.pool, &mint).await?;
    let (from, to) = series_range(&st.pool, &q, iv).await?;
    let sql = if iv == 3_600 {
        Q_MARKET_STATS_HOURLY
    } else {
        Q_MARKET_STATS_DAILY
    };
    let data: Vec<Value> = sqlx::query_scalar(sql)
        .bind(&market)
        .bind(from)
        .bind(to)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(json!({ "interval": interval, "data": data })))
}

const Q_MARKET_FEES: &str = concat!(
    "SELECT ",
    fee_line_json!(),
    " FROM v_market_fees f WHERE f.market = $1 ORDER BY f.fee_type"
);

async fn get_market_fees(
    State(st): State<AppState>,
    mint: Result<Path<String>, PathRejection>,
) -> ApiResult {
    let mint = path_key(mint, "mint")?;
    let market = market_of(&st.pool, &mint).await?;
    let data: Vec<Value> = sqlx::query_scalar(Q_MARKET_FEES)
        .bind(&market)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(json!({ "data": data })))
}

// ---- global analytics ------------------------------------------------------------------------

/// `GlobalStats` (also the WebSocket `stats` channel's payload).
pub(crate) const Q_GLOBAL_STATS: &str = concat!(
    "SELECT ",
    global_stats_json!(),
    " FROM v_global_stats g LEFT JOIN v_sol_usd u ON TRUE"
);

/// `/v1/stats` aggregates the whole protocol: served from memory for `STATS_TTL` (its
/// `max-age`), so any number of clients cost one query per period (M8-SEC-3).
async fn global_stats(State(st): State<AppState>) -> ApiResult {
    let mut cached = st.shared.stats.lock().await;
    if let Some((at, v)) = cached.as_ref()
        && at.elapsed() < super::STATS_TTL
    {
        return Ok(ok(v.clone()));
    }
    let v: Value = sqlx::query_scalar(Q_GLOBAL_STATS)
        .fetch_one(&st.pool)
        .await?;
    *cached = Some((std::time::Instant::now(), v.clone()));
    Ok(ok(v))
}

/// `getGlobalTimeseries` metrics → their column in `global_stats_hourly` and
/// `v_global_stats_daily` (the allowlist of §10.2). Every column is NOT NULL in both.
pub const TIMESERIES_METRICS: [(&str, &str); 28] = [
    ("tvl", "tvl_lamports"),
    ("oi", "oi_lamports"),
    ("open_positions", "open_positions"),
    ("fees_protocol", "fees_protocol"),
    ("fees_lenders", "fees_lenders"),
    ("fees_keeper", "fees_keeper"),
    ("opens", "opens"),
    ("closes", "closes"),
    ("liquidations", "liquidations"),
    ("expiries", "expiries"),
    ("settlements_s1", "settlements_s1"),
    ("settlements_s2", "settlements_s2"),
    ("settlements_s3", "settlements_s3"),
    ("realised_lender_loss", "realised_lender_loss"),
    ("shortwin_volume", "shortwin_volume"),
    ("creator_fees", "creator_fees"),
    ("coverage_gaps", "coverage_gaps"),
    ("coverage_gap_s", "coverage_gap_s"),
    ("markets", "markets"),
    ("lender_sol", "lender_sol"),
    ("inventory_value", "inventory_value_lamports"),
    ("notional_opened", "notional_opened"),
    ("settlement_lender_pnl", "settlement_lender_pnl"),
    ("deposits", "deposits"),
    ("withdrawals", "withdrawals"),
    ("positions_open_s", "positions_open_s"),
    ("coverage_paused_s", "coverage_paused_s"),
    ("observations", "observations"),
];

async fn global_timeseries(State(st): State<AppState>, q: Params) -> ApiResult {
    let names: Vec<&'static str> = TIMESERIES_METRICS.iter().map(|(n, _)| *n).collect();
    let metric = Params::required("metric", q.choice("metric", &names)?)?;
    let col = TIMESERIES_METRICS
        .iter()
        .find(|(n, _)| *n == metric)
        .map_or("tvl_lamports", |(_, c)| *c);
    let (interval, iv) = stats_interval(&q)?;
    let (from, to) = series_range(&st.pool, &q, iv).await?;
    let (table, t) = if iv == 3_600 {
        ("global_stats_hourly", "bucket_start")
    } else {
        ("v_global_stats_daily", "day_start")
    };
    // `col`, `table` and `t` come from the allowlists above, never from the request.
    let sql = format!(
        "SELECT jsonb_build_object('t', g.{t}, 'value', (g.{col})::text) FROM {table} g \
         WHERE g.{t} >= $1 AND g.{t} < $2 ORDER BY g.{t} LIMIT 1000"
    );
    let points: Vec<Value> = sqlx::query_scalar(AssertSqlSafe(sql))
        .bind(from)
        .bind(to)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(
        json!({ "metric": metric, "interval": interval, "points": points }),
    ))
}

// ---- admin -----------------------------------------------------------------------------------

const Q_PROTOCOL_CONFIG: &str = "SELECT jsonb_build_object(\
    'as_of_slot', GREATEST(h.slot, ps.last_slot), 'source', h.source, \
    'governance', h.governance::text, 'pause_authority', h.pause_authority::text, \
    'coverage_authority', h.coverage_authority::text, 'fee_recipient', h.fee_recipient::text, \
    'timelock_s', h.timelock_s::bigint, \
    'venue_enabled', COALESCE(ps.venue_enabled, h.venue_enabled)::bigint, \
    'paused_opens', ps.paused_opens, 'route_broken_bits', ps.route_broken_bits::bigint, \
    'params', h.params, 'tiers', h.tiers, 'ema', h.ema, \
    'util_kink_bps', h.util_kink_bps::bigint, 'pyth_sol_usd', h.pyth_sol_usd::text, \
    'pyth_feed_id', h.pyth_feed_id) \
    FROM protocol_params_history h CROSS JOIN protocol_state ps \
    ORDER BY h.slot DESC, h.tx_index DESC, h.ix_index DESC LIMIT 1";

async fn protocol_config(State(st): State<AppState>) -> ApiResult {
    let v: Option<Value> = sqlx::query_scalar(Q_PROTOCOL_CONFIG)
        .fetch_optional(&st.pool)
        .await?;
    v.map(ok)
        .ok_or_else(|| ApiError::not_found("the protocol configuration has not been ingested yet"))
}

const Q_CONFIG_CHANGES: &str = "SELECT jsonb_build_object(\
    'kind', c.kind_name, 'hash', c.hash, 'payload', c.payload, \
    'payload_verified', c.payload_verified, 'not_before', c.not_before, 'status', c.status, \
    'proposed', jsonb_build_object('signature', c.proposed_signature::text, 'slot', pe.slot, \
        'tx_index', pe.tx_index, 'ix_index', c.proposed_ix_index, 'ts', c.proposed_ts, \
        'finalized', pe.slot <= st.finalized_slot), \
    'applied', CASE WHEN c.applied_signature IS NULL THEN NULL ELSE jsonb_build_object(\
        'signature', c.applied_signature::text, 'slot', c.applied_slot, \
        'tx_index', COALESCE(atx.tx_index, 0), 'ix_index', COALESCE(ae.ix_index, 0), \
        'ts', c.applied_ts, 'finalized', c.applied_slot <= st.finalized_slot) END, \
    'cancelled', CASE WHEN c.cancelled_signature IS NULL THEN NULL ELSE jsonb_build_object(\
        'signature', c.cancelled_signature::text, 'slot', c.cancelled_slot, \
        'tx_index', COALESCE(ctx.tx_index, 0), 'ix_index', COALESCE(ce.ix_index, 0), \
        'ts', c.cancelled_ts, 'finalized', c.cancelled_slot <= st.finalized_slot, \
        'by', COALESCE(c.cancelled_by::text, ce.actor::text)) END), \
    pe.slot, pe.tx_index, pe.ix_index, pe.ev_source, pe.ev_seq \
    FROM config_changes c \
    JOIN events pe ON pe.signature = c.proposed_signature AND pe.ix_index = c.proposed_ix_index \
        AND pe.ev_source = c.proposed_ev_source AND pe.ev_seq = c.proposed_ev_seq \
    LEFT JOIN ingest_txs atx ON atx.signature = c.applied_signature \
    LEFT JOIN LATERAL (SELECT e.ix_index FROM events e WHERE e.signature = c.applied_signature \
        AND e.kind = 'ConfigApplied' ORDER BY (e.data->>'hash' = c.hash) DESC NULLS LAST, \
        e.ix_index LIMIT 1) ae ON TRUE \
    LEFT JOIN ingest_txs ctx ON ctx.signature = c.cancelled_signature \
    LEFT JOIN LATERAL (SELECT e.ix_index, e.actor FROM events e \
        WHERE e.signature = c.cancelled_signature AND e.kind = 'ConfigCancelled' \
        ORDER BY (e.data->>'hash' = c.hash) DESC NULLS LAST, e.ix_index LIMIT 1) ce ON TRUE \
    CROSS JOIN ingest_state st \
    WHERE ($1::text IS NULL OR c.status = $1) \
      AND ($2::bigint IS NULL OR (pe.slot, pe.tx_index, pe.ix_index, pe.ev_source, pe.ev_seq) \
           < ($2, $3::int, $4::smallint, $5::smallint, $6::int)) \
    ORDER BY pe.slot DESC, pe.tx_index DESC, pe.ix_index DESC, pe.ev_source DESC, \
      pe.ev_seq DESC LIMIT $7";

async fn list_config_changes(State(st): State<AppState>, q: Params) -> ApiResult {
    let status = q.choice("status", &["pending", "applied", "cancelled", "superseded"])?;
    let limit = q.limit(st.cfg.default_page_limit, st.cfg.max_page_limit)?;
    let after = ChainKey::decode_opt(q.cursor()?, "cc")?;
    let (a, b, c, d, e) = key_binds(after);
    let rows: Vec<(Value, i64, i32, i16, i16, i32)> = sqlx::query_as(Q_CONFIG_CHANGES)
        .bind(status)
        .bind(a)
        .bind(b)
        .bind(c)
        .bind(d)
        .bind(e)
        .bind(limit + 1)
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(chain_page(rows, limit, "cc")))
}

const Q_VENUES: &str = "SELECT jsonb_build_object(\
    'venue_enabled', COALESCE(ps.venue_enabled, (SELECT h.venue_enabled \
        FROM protocol_params_history h \
        ORDER BY h.slot DESC, h.tx_index DESC, h.ix_index DESC LIMIT 1))::bigint, \
    'route_broken_bits', ps.route_broken_bits::bigint, 'paused_opens', ps.paused_opens, \
    'guards', COALESCE((SELECT jsonb_agg(jsonb_build_object('venue', g.venue::int, \
        'name', sw_api_guard(g.venue), 'upgraded', g.upgraded, \
        'guard_slot', g.guard_slot::text, 'programdata_slot', g.programdata_slot::text, \
        'flagged_at', g.flagged_at, 'last_ts', g.last_ts) ORDER BY g.venue) \
        FROM venue_guards g), '[]'::jsonb), \
    'extras', COALESCE((SELECT jsonb_agg(jsonb_build_object('venue', x.venue::int, \
        'ix', x.ix::int, 'version', x.version::bigint, 'keys', to_jsonb(x.keys), \
        'stable_version', x.stable_version::bigint) ORDER BY x.venue, x.ix) \
        FROM venue_extras x), '[]'::jsonb), \
    'admin_changes', COALESCE((SELECT jsonb_agg(a.j ORDER BY a.observed_slot DESC, a.account, \
        a.field) FROM (SELECT jsonb_build_object('account', s.account::text, 'field', s.field, \
        'venue', s.venue, 'old_value', s.old_value, 'new_value', s.new_value, \
        'observed_slot', s.observed_slot) AS j, s.observed_slot, \
        s.account::text COLLATE \"C\" AS account, s.field COLLATE \"C\" AS field \
        FROM venue_admin_states s ORDER BY s.observed_slot DESC LIMIT 100) a), '[]'::jsonb)) \
    FROM protocol_state ps";

async fn venues(State(st): State<AppState>) -> ApiResult {
    let v: Value = sqlx::query_scalar(Q_VENUES).fetch_one(&st.pool).await?;
    Ok(ok(v))
}

// ---- misc ------------------------------------------------------------------------------------

const Q_SEARCH: &str = "WITH hits AS (\
    SELECT o.mint::text AS mint, o.market::text AS market, left(o.symbol, 16) AS symbol, \
           left(o.name, 64) AS name, o.tvl_lamports AS tvl, 0 AS rank, 'mint' AS match \
    FROM v_market_overview o WHERE o.mint = $1 \
    UNION ALL \
    SELECT o.mint::text, o.market::text, left(o.symbol, 16), left(o.name, 64), \
           o.tvl_lamports, 1, 'symbol' \
    FROM v_market_overview o WHERE lower(o.symbol) LIKE lower($2) ESCAPE '\\' \
    UNION ALL \
    SELECT o.mint::text, o.market::text, left(o.symbol, 16), left(o.name, 64), \
           o.tvl_lamports, 2, 'name' \
    FROM v_market_overview o WHERE lower(o.name) LIKE lower($2) ESCAPE '\\') \
    SELECT jsonb_build_object('mint', h.mint, 'market', h.market, 'symbol', h.symbol, \
        'name', h.name, 'match', h.match) \
    FROM (SELECT DISTINCT ON (mint) * FROM hits ORDER BY mint, rank) h \
    ORDER BY h.rank, h.tvl DESC, h.mint COLLATE \"C\" LIMIT 20";

async fn search(State(st): State<AppState>, q: Params) -> ApiResult {
    let text = Params::required("q", q.string("q", 1, 64)?)?;
    let data: Vec<Value> = sqlx::query_scalar(Q_SEARCH)
        .bind(canonical_key(text))
        .bind(like_prefix(text))
        .fetch_all(&st.pool)
        .await?;
    Ok(ok(json!({ "data": data })))
}

const Q_TX: &str = concat!(
    "SELECT jsonb_build_object('signature', t.signature::text, 'slot', t.slot, \
     'tx_index', t.tx_index, 'block_time', t.block_time, 'commitment', t.commitment, \
     'fee_payer', t.fee_payer::text, 'fee', t.fee::text, 'ok', t.err IS NULL, \
     'err_code', t.err_code, \
     'events', COALESCE((SELECT jsonb_agg(",
    event_json!(),
    " ORDER BY e.ix_index, e.ev_source, e.ev_seq) FROM events e \
         WHERE e.signature = t.signature), '[]'::jsonb), \
     'invocations', COALESCE((SELECT jsonb_agg(jsonb_build_object('ix_index', i.ix_index, \
         'inner_index', i.inner_index, 'stack_height', i.stack_height, \
         'program', i.program::text, 'name', i.name, 'market', i.market::text, \
         'args', i.args) ORDER BY i.ix_index, i.inner_index) FROM invocations i \
         WHERE i.signature = t.signature), '[]'::jsonb), \
     'venue_trades', COALESCE((SELECT jsonb_agg(",
    trade_json!(),
    " ORDER BY vt.ix_index, vt.ev_source, vt.ev_seq) FROM venue_trades vt \
         WHERE vt.signature = t.signature), '[]'::jsonb)), \
     (t.commitment = 'finalized' OR t.slot <= st.finalized_slot) \
     FROM ingest_txs t CROSS JOIN ingest_state st WHERE t.signature = $1"
);

async fn get_transaction(
    State(st): State<AppState>,
    sig: Result<Path<String>, PathRejection>,
) -> ApiResult {
    let sig = path_signature(sig, "signature")?;
    let row: Option<(Value, bool)> = sqlx::query_as(Q_TX)
        .bind(&sig)
        .fetch_optional(&st.pool)
        .await?;
    let (v, finalized) =
        row.ok_or_else(|| ApiError::not_found(format!("transaction {sig} is not indexed")))?;
    Ok(if finalized {
        with_cache(v, "public, max-age=3600")
    } else {
        ok(v)
    })
}
