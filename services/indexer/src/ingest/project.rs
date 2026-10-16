//! Projections (INDEXER.md §6). Owner: INGEST.
//!
//! [`apply`] updates `markets`, `positions`, `lender_positions`, `pools`, `share_epochs`,
//! `settlements`, `fees`, `market_snapshots`, `coverage_events`, `pool_events`,
//! `protocol_sweeps`, `market_state_log`, `protocol_state`, `venue_guards`, `venue_extras`,
//! `config_changes` and `protocol_params_history` by the rules of §5.3 and §6.2–§6.6, in chain
//! order. Its input is exactly what `events` stores (kind, the §4.2 JSON `data`, `derived`, the
//! chain key, the transaction's fee payer and block time) plus the `initialize_config`
//! invocations, so incremental application and [`rebuild`] run the same code on the same input:
//! they give identical rows (§3.4, asserted by the replay harness §11.4).
//!
//! **Order (§3.3).** Each market keeps a watermark (the chain key of the last event applied). An
//! event at or below it (a gap-fill delivering history after newer events) is not applied: the
//! market is marked `needs_rebuild` and the writer rebuilds it after the batch. A market whose
//! `MarketCreated` arrives after some of its events is marked the same way. Global events
//! (`market` NULL: governance, the latch, global pauses) are checked against every global event
//! already stored; a late one marks the global projection (`protocol_state.needs_rebuild`).
//!
//! **Rebuild.** A market: delete its `markets` row (cascading to `positions`, `lender_positions`,
//! `pools`, `share_epochs`, `market_state_log`) and its per-event typed rows (`fees`,
//! `settlements`, `pool_events`, `market_snapshots`, `coverage_events`, `protocol_sweeps`), then
//! re-apply every event of the market in chain order; the account-sourced columns [A] are then
//! re-derived from `account_snapshots` (the refresher's table, which a rebuild never deletes).
//! The global projection: delete `config_changes` and `protocol_params_history`, reset
//! `protocol_state`, `venue_guards`, `venue_extras`, then re-apply the global events and the
//! `initialize_config` invocations in chain order.

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use serde_json::Value;
use solana_pubkey::Pubkey;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};

use super::decode::shortwin_events::{
    bool_field, i64_field, i128_field, key_field, u8_field, u32_field, u64_field, u128_field,
};
use super::store::{
    ChainKey, get_key, get_opt_key, get_opt_u64, get_opt_u128, get_u8, get_u32, get_u64, get_u128,
    i16_of, i64_of, opt_key, opt_text,
};
use crate::analytics::formulas;

/// The single writer's advisory lock key ("SWINDX", §3.1).
pub const WRITER_LOCK: i64 = 0x5357_494e_4458;

/// Event kinds of the global projection (their `market` field is `Pubkey::default()`).
pub const GLOBAL_KINDS: [&str; 9] = [
    "ConfigProposed",
    "ConfigApplied",
    "ConfigCancelled",
    "PauseSet",
    "VenueDisabled",
    "RouteBrokenSet",
    "VenueExtrasSet",
    "VenueUpgradeFlagged",
    "VenueRevalidated",
];

/// One event as the projection sees it: what `events` stores, plus the transaction's fee payer
/// and block time.
#[derive(Clone, Debug, PartialEq)]
pub struct EventIn {
    pub signature: String,
    pub key: ChainKey,
    pub block_time: Option<i64>,
    pub fee_payer: Pubkey,
    pub invocation: String,
    pub kind: String,
    pub market: Option<Pubkey>,
    pub ev_ts: i64,
    pub data: Value,
    pub derived: Option<Value>,
}

/// A successful `initialize_config` invocation (no event; the first parameters snapshot).
#[derive(Clone, Debug, PartialEq)]
pub struct InitConfigIn {
    pub signature: String,
    /// `(slot, tx_index, ix_index, -1, inner_index)`: before the events of its instruction.
    pub key: ChainKey,
    pub block_time: Option<i64>,
    pub args: Value,
}

/// A projection input, in chain order.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Event(EventIn),
    Init(InitConfigIn),
}

impl Item {
    pub fn key(&self) -> ChainKey {
        match self {
            Self::Event(e) => e.key,
            Self::Init(i) => i.key,
        }
    }
}

/// What [`apply`] left for the writer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Markets marked `needs_rebuild` (late events, §3.3).
    pub rebuild_markets: HashSet<Pubkey>,
    /// The global projection needs a rebuild.
    pub rebuild_global: bool,
}

// ---- the markets row -----------------------------------------------------------------------

/// The `markets` row in memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MarketRow {
    market: Pubkey,
    mint: Pubkey,
    token_program: Pubkey,
    creator: Pubkey,
    coin_creator: Option<Pubkey>,
    coin_creator_source: Option<String>,
    created_slot: i64,
    created_ts: i64,
    created_signature: String,
    creation_fee: u64,
    venue_at_listing: u8,
    tier_at_listing: u8,
    bonding_curve: Option<Pubkey>,
    canonical_pool: Option<Pubkey>,
    venue: u8,
    ray_pool: Option<Pubkey>,
    venue_pool: Option<Pubkey>,
    graduated_at: Option<i64>,
    tier: u8,
    paused_opens: bool,
    paused_coverage: bool,
    coverage_cleared_at: Option<i64>,
    mint_unsafe: bool,
    route_broken: bool,
    wipe_live: bool,
    tokens_free: u64,
    borrowed_q: u64,
    borrowed_q_tier: [u64; 4],
    total_shares: u128,
    share_epoch: u32,
    open_positions: u32,
    total_notional_open: u64,
    max_q_hwm: u64,
    open_vault_lamports: u64,
    lender_sol: u64,
    protocol_fees: u64,
    lender_count: u32,
    spot_wad: Option<u128>,
    ema_price: Option<u128>,
    ema_liq: Option<u128>,
    liq_slow: Option<u128>,
    ema_count: Option<u64>,
    lb_lo: Option<u128>,
    lb_hi: Option<u128>,
    util_bps: Option<u32>,
    vest_bal: Option<u64>,
    rent_basis: [Option<u128>; 3],
    price_slot: Option<i64>,
    price_ts: Option<i64>,
    price_source: Option<String>,
    no_route_since: i64,
    last_no_route_obs: i64,
    pool_fail_since: i64,
    last_pool_fail_obs: i64,
    fp_candidate: Option<Pubkey>,
    fp_venue: Option<u8>,
    fp_d: Option<u128>,
    fp_window_ends: Option<i64>,
    pool_d: Option<u128>,
    watermark: ChainKey,
    needs_rebuild: bool,
    /// The last `P_ema` an event carried (`PriceCranked.ema_price`, `Open.ema_wad`) and its
    /// time — a settlement's `P_last` (§8.4); never the account-sourced price
    /// (`markets.ema_event`, `ema_event_ts`).
    last_ema: Option<(u128, i64)>,
}

/// The accounting after-state `market_state_log` records (§6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogState {
    tokens_free: u64,
    borrowed_q: u64,
    total_shares: u128,
    open_positions: u32,
    total_notional_open: u64,
    open_vault_lamports: u64,
    lender_sol: u64,
    protocol_fees: u64,
    ema_price: Option<u128>,
}

impl MarketRow {
    /// The logged after-state: event-derived only — `ema_price` is the last `P_ema` an event
    /// carried, never a refreshed account value, so incremental application and a rebuild log
    /// the same rows whatever the refresher (or a replayed checkpoint) did in between.
    fn log_state(&self) -> LogState {
        LogState {
            tokens_free: self.tokens_free,
            borrowed_q: self.borrowed_q,
            total_shares: self.total_shares,
            open_positions: self.open_positions,
            total_notional_open: self.total_notional_open,
            open_vault_lamports: self.open_vault_lamports,
            lender_sol: self.lender_sol,
            protocol_fees: self.protocol_fees,
            ema_price: self.last_ema.map(|(p, _)| p),
        }
    }
}

const MARKET_SELECT: &str = "SELECT market::text, mint::text, token_program::text, creator::text, \
    coin_creator::text, coin_creator_source, created_slot, created_ts, created_signature::text, \
    creation_fee::text, venue_at_listing::smallint, tier_at_listing::smallint, \
    bonding_curve::text, canonical_pool::text, venue::smallint, ray_pool::text, \
    venue_pool::text, graduated_at, tier::smallint, paused_opens, paused_coverage, \
    coverage_cleared_at, mint_unsafe, route_broken, wipe_live, tokens_free::text, \
    borrowed_q::text, borrowed_q_tier0::text, borrowed_q_tier1::text, borrowed_q_tier2::text, \
    borrowed_q_tier3::text, total_shares::text, share_epoch::bigint, open_positions::bigint, \
    total_notional_open::text, max_q_hwm::text, open_vault_lamports::text, lender_sol::text, \
    protocol_fees::text, lender_count::bigint, spot_wad::text, ema_price::text, ema_liq::text, \
    liq_slow::text, ema_count::text, lb_lo::text, lb_hi::text, util_bps::bigint, \
    vest_bal::text, rent_basis0::text, rent_basis1::text, rent_basis2::text, price_slot, \
    price_ts, price_source, no_route_since, last_no_route_obs, pool_fail_since, \
    last_pool_fail_obs, fp_candidate::text, fp_venue::smallint, fp_d::text, fp_window_ends, \
    pool_d::text, last_slot, last_tx_index, last_ix_index, last_ev_source, last_ev_seq, \
    needs_rebuild, ema_event::text, ema_event_ts FROM markets WHERE market = $1 FOR UPDATE";

fn market_from_row(r: &PgRow) -> anyhow::Result<MarketRow> {
    let opt_u8 = |c: &str| -> anyhow::Result<Option<u8>> {
        let v: Option<i16> = r.try_get(c)?;
        Ok(v.map(u8::try_from).transpose()?)
    };
    let opt_u32 = |c: &str| -> anyhow::Result<Option<u32>> {
        let v: Option<i64> = r.try_get(c)?;
        Ok(v.map(u32::try_from).transpose()?)
    };
    Ok(MarketRow {
        market: get_key(r, "market")?,
        mint: get_key(r, "mint")?,
        token_program: get_key(r, "token_program")?,
        creator: get_key(r, "creator")?,
        coin_creator: get_opt_key(r, "coin_creator")?,
        coin_creator_source: r.try_get("coin_creator_source")?,
        created_slot: r.try_get("created_slot")?,
        created_ts: r.try_get("created_ts")?,
        created_signature: r.try_get("created_signature")?,
        creation_fee: get_u64(r, "creation_fee")?,
        venue_at_listing: get_u8(r, "venue_at_listing")?,
        tier_at_listing: get_u8(r, "tier_at_listing")?,
        bonding_curve: get_opt_key(r, "bonding_curve")?,
        canonical_pool: get_opt_key(r, "canonical_pool")?,
        venue: get_u8(r, "venue")?,
        ray_pool: get_opt_key(r, "ray_pool")?,
        venue_pool: get_opt_key(r, "venue_pool")?,
        graduated_at: r.try_get("graduated_at")?,
        tier: get_u8(r, "tier")?,
        paused_opens: r.try_get("paused_opens")?,
        paused_coverage: r.try_get("paused_coverage")?,
        coverage_cleared_at: r.try_get("coverage_cleared_at")?,
        mint_unsafe: r.try_get("mint_unsafe")?,
        route_broken: r.try_get("route_broken")?,
        wipe_live: r.try_get("wipe_live")?,
        tokens_free: get_u64(r, "tokens_free")?,
        borrowed_q: get_u64(r, "borrowed_q")?,
        borrowed_q_tier: [
            get_u64(r, "borrowed_q_tier0")?,
            get_u64(r, "borrowed_q_tier1")?,
            get_u64(r, "borrowed_q_tier2")?,
            get_u64(r, "borrowed_q_tier3")?,
        ],
        total_shares: get_u128(r, "total_shares")?,
        share_epoch: get_u32(r, "share_epoch")?,
        open_positions: get_u32(r, "open_positions")?,
        total_notional_open: get_u64(r, "total_notional_open")?,
        max_q_hwm: get_u64(r, "max_q_hwm")?,
        open_vault_lamports: get_u64(r, "open_vault_lamports")?,
        lender_sol: get_u64(r, "lender_sol")?,
        protocol_fees: get_u64(r, "protocol_fees")?,
        lender_count: get_u32(r, "lender_count")?,
        spot_wad: get_opt_u128(r, "spot_wad")?,
        ema_price: get_opt_u128(r, "ema_price")?,
        ema_liq: get_opt_u128(r, "ema_liq")?,
        liq_slow: get_opt_u128(r, "liq_slow")?,
        ema_count: get_opt_u64(r, "ema_count")?,
        lb_lo: get_opt_u128(r, "lb_lo")?,
        lb_hi: get_opt_u128(r, "lb_hi")?,
        util_bps: opt_u32("util_bps")?,
        vest_bal: get_opt_u64(r, "vest_bal")?,
        rent_basis: [
            get_opt_u128(r, "rent_basis0")?,
            get_opt_u128(r, "rent_basis1")?,
            get_opt_u128(r, "rent_basis2")?,
        ],
        price_slot: r.try_get("price_slot")?,
        price_ts: r.try_get("price_ts")?,
        price_source: r.try_get("price_source")?,
        no_route_since: r.try_get("no_route_since")?,
        last_no_route_obs: r.try_get("last_no_route_obs")?,
        pool_fail_since: r.try_get("pool_fail_since")?,
        last_pool_fail_obs: r.try_get("last_pool_fail_obs")?,
        fp_candidate: get_opt_key(r, "fp_candidate")?,
        fp_venue: opt_u8("fp_venue")?,
        fp_d: get_opt_u128(r, "fp_d")?,
        fp_window_ends: r.try_get("fp_window_ends")?,
        pool_d: get_opt_u128(r, "pool_d")?,
        watermark: ChainKey {
            slot: r.try_get("last_slot")?,
            tx_index: r.try_get("last_tx_index")?,
            ix_index: r.try_get("last_ix_index")?,
            ev_source: r.try_get("last_ev_source")?,
            ev_seq: r.try_get("last_ev_seq")?,
        },
        needs_rebuild: r.try_get("needs_rebuild")?,
        last_ema: get_opt_u128(r, "ema_event")?.zip(r.try_get::<Option<i64>, _>("ema_event_ts")?),
    })
}

async fn load_market(
    conn: &mut PgConnection,
    market: &Pubkey,
) -> anyhow::Result<Option<MarketRow>> {
    let Some(row) = sqlx::query(MARKET_SELECT)
        .bind(market.to_string())
        .fetch_optional(&mut *conn)
        .await
        .context("loading a market")?
    else {
        return Ok(None);
    };
    Ok(Some(market_from_row(&row)?))
}

async fn store_market(conn: &mut PgConnection, m: &MarketRow) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO markets (market, mint, token_program, creator, coin_creator, \
           coin_creator_source, created_slot, created_ts, created_signature, creation_fee, \
           venue_at_listing, tier_at_listing, bonding_curve, canonical_pool, venue, ray_pool, \
           venue_pool, graduated_at, tier, paused_opens, paused_coverage, coverage_cleared_at, \
           mint_unsafe, route_broken, wipe_live, tokens_free, borrowed_q, borrowed_q_tier0, \
           borrowed_q_tier1, borrowed_q_tier2, borrowed_q_tier3, total_shares, share_epoch, \
           open_positions, total_notional_open, max_q_hwm, open_vault_lamports, lender_sol, \
           protocol_fees, lender_count, spot_wad, ema_price, ema_liq, liq_slow, ema_count, lb_lo, \
           lb_hi, util_bps, vest_bal, rent_basis0, rent_basis1, rent_basis2, price_slot, price_ts, \
           price_source, no_route_since, last_no_route_obs, pool_fail_since, last_pool_fail_obs, \
           fp_candidate, fp_venue, fp_d, fp_window_ends, pool_d, last_slot, last_tx_index, \
           last_ix_index, last_ev_source, last_ev_seq, needs_rebuild, ema_event, ema_event_ts, \
           updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::numeric, $11, $12, $13, $14, $15, $16, \
           $17, $18, $19, $20, $21, $22, $23, $24, $25, $26::numeric, $27::numeric, $28::numeric, \
           $29::numeric, $30::numeric, $31::numeric, $32::numeric, $33, $34, $35::numeric, \
           $36::numeric, $37::numeric, $38::numeric, $39::numeric, $40, $41::numeric, \
           $42::numeric, $43::numeric, $44::numeric, $45::numeric, $46::numeric, $47::numeric, \
           $48, $49::numeric, $50::numeric, $51::numeric, $52::numeric, $53, $54, $55, $56, $57, \
           $58, $59, $60, $61, $62::numeric, $63, $64::numeric, $65, $66, $67, $68, $69, $70, \
           $71::numeric, $72, now()) \
         ON CONFLICT (market) DO UPDATE SET mint = EXCLUDED.mint, \
           token_program = EXCLUDED.token_program, creator = EXCLUDED.creator, \
           coin_creator = EXCLUDED.coin_creator, coin_creator_source = EXCLUDED.coin_creator_source, \
           created_slot = EXCLUDED.created_slot, created_ts = EXCLUDED.created_ts, \
           created_signature = EXCLUDED.created_signature, creation_fee = EXCLUDED.creation_fee, \
           venue_at_listing = EXCLUDED.venue_at_listing, tier_at_listing = EXCLUDED.tier_at_listing, \
           bonding_curve = EXCLUDED.bonding_curve, canonical_pool = EXCLUDED.canonical_pool, \
           venue = EXCLUDED.venue, ray_pool = EXCLUDED.ray_pool, venue_pool = EXCLUDED.venue_pool, \
           graduated_at = EXCLUDED.graduated_at, tier = EXCLUDED.tier, \
           paused_opens = EXCLUDED.paused_opens, paused_coverage = EXCLUDED.paused_coverage, \
           coverage_cleared_at = EXCLUDED.coverage_cleared_at, mint_unsafe = EXCLUDED.mint_unsafe, \
           route_broken = EXCLUDED.route_broken, wipe_live = EXCLUDED.wipe_live, \
           tokens_free = EXCLUDED.tokens_free, borrowed_q = EXCLUDED.borrowed_q, \
           borrowed_q_tier0 = EXCLUDED.borrowed_q_tier0, borrowed_q_tier1 = EXCLUDED.borrowed_q_tier1, \
           borrowed_q_tier2 = EXCLUDED.borrowed_q_tier2, borrowed_q_tier3 = EXCLUDED.borrowed_q_tier3, \
           total_shares = EXCLUDED.total_shares, share_epoch = EXCLUDED.share_epoch, \
           open_positions = EXCLUDED.open_positions, \
           total_notional_open = EXCLUDED.total_notional_open, max_q_hwm = EXCLUDED.max_q_hwm, \
           open_vault_lamports = EXCLUDED.open_vault_lamports, lender_sol = EXCLUDED.lender_sol, \
           protocol_fees = EXCLUDED.protocol_fees, lender_count = EXCLUDED.lender_count, \
           spot_wad = EXCLUDED.spot_wad, ema_price = EXCLUDED.ema_price, ema_liq = EXCLUDED.ema_liq, \
           liq_slow = EXCLUDED.liq_slow, ema_count = EXCLUDED.ema_count, lb_lo = EXCLUDED.lb_lo, \
           lb_hi = EXCLUDED.lb_hi, util_bps = EXCLUDED.util_bps, vest_bal = EXCLUDED.vest_bal, \
           rent_basis0 = EXCLUDED.rent_basis0, rent_basis1 = EXCLUDED.rent_basis1, \
           rent_basis2 = EXCLUDED.rent_basis2, price_slot = EXCLUDED.price_slot, \
           price_ts = EXCLUDED.price_ts, price_source = EXCLUDED.price_source, \
           no_route_since = EXCLUDED.no_route_since, last_no_route_obs = EXCLUDED.last_no_route_obs, \
           pool_fail_since = EXCLUDED.pool_fail_since, \
           last_pool_fail_obs = EXCLUDED.last_pool_fail_obs, fp_candidate = EXCLUDED.fp_candidate, \
           fp_venue = EXCLUDED.fp_venue, fp_d = EXCLUDED.fp_d, \
           fp_window_ends = EXCLUDED.fp_window_ends, pool_d = EXCLUDED.pool_d, \
           last_slot = EXCLUDED.last_slot, last_tx_index = EXCLUDED.last_tx_index, \
           last_ix_index = EXCLUDED.last_ix_index, last_ev_source = EXCLUDED.last_ev_source, \
           last_ev_seq = EXCLUDED.last_ev_seq, needs_rebuild = EXCLUDED.needs_rebuild, \
           ema_event = EXCLUDED.ema_event, ema_event_ts = EXCLUDED.ema_event_ts, \
           updated_at = now()",
    )
    .bind(m.market.to_string())
    .bind(m.mint.to_string())
    .bind(m.token_program.to_string())
    .bind(m.creator.to_string())
    .bind(opt_key(m.coin_creator))
    .bind(m.coin_creator_source.clone())
    .bind(m.created_slot)
    .bind(m.created_ts)
    .bind(m.created_signature.clone())
    .bind(m.creation_fee.to_string())
    .bind(i16_of(m.venue_at_listing))
    .bind(i16_of(m.tier_at_listing))
    .bind(opt_key(m.bonding_curve))
    .bind(opt_key(m.canonical_pool))
    .bind(i16_of(m.venue))
    .bind(opt_key(m.ray_pool))
    .bind(opt_key(m.venue_pool))
    .bind(m.graduated_at)
    .bind(i16_of(m.tier))
    .bind(m.paused_opens)
    .bind(m.paused_coverage)
    .bind(m.coverage_cleared_at)
    .bind(m.mint_unsafe)
    .bind(m.route_broken)
    .bind(m.wipe_live)
    .bind(m.tokens_free.to_string())
    .bind(m.borrowed_q.to_string())
    .bind(m.borrowed_q_tier[0].to_string())
    .bind(m.borrowed_q_tier[1].to_string())
    .bind(m.borrowed_q_tier[2].to_string())
    .bind(m.borrowed_q_tier[3].to_string())
    .bind(m.total_shares.to_string())
    .bind(i64_of(m.share_epoch))
    .bind(i64_of(m.open_positions))
    .bind(m.total_notional_open.to_string())
    .bind(m.max_q_hwm.to_string())
    .bind(m.open_vault_lamports.to_string())
    .bind(m.lender_sol.to_string())
    .bind(m.protocol_fees.to_string())
    .bind(i64_of(m.lender_count))
    .bind(opt_text(m.spot_wad))
    .bind(opt_text(m.ema_price))
    .bind(opt_text(m.ema_liq))
    .bind(opt_text(m.liq_slow))
    .bind(opt_text(m.ema_count))
    .bind(opt_text(m.lb_lo))
    .bind(opt_text(m.lb_hi))
    .bind(m.util_bps.map(i64_of))
    .bind(opt_text(m.vest_bal))
    .bind(opt_text(m.rent_basis[0]))
    .bind(opt_text(m.rent_basis[1]))
    .bind(opt_text(m.rent_basis[2]))
    .bind(m.price_slot)
    .bind(m.price_ts)
    .bind(m.price_source.clone())
    .bind(m.no_route_since)
    .bind(m.last_no_route_obs)
    .bind(m.pool_fail_since)
    .bind(m.last_pool_fail_obs)
    .bind(opt_key(m.fp_candidate))
    .bind(m.fp_venue.map(i16_of))
    .bind(opt_text(m.fp_d))
    .bind(m.fp_window_ends)
    .bind(opt_text(m.pool_d))
    .bind(m.watermark.slot)
    .bind(m.watermark.tx_index)
    .bind(m.watermark.ix_index)
    .bind(m.watermark.ev_source)
    .bind(m.watermark.ev_seq)
    .bind(m.needs_rebuild)
    .bind(opt_text(m.last_ema.map(|(p, _)| p)))
    .bind(m.last_ema.map(|(_, ts)| ts))
    .execute(&mut *conn)
    .await
    .with_context(|| format!("storing market {}", m.market))?;
    Ok(())
}

// ---- the projector ---------------------------------------------------------------------------

/// The markets touched by one [`apply`] call, flushed at its end.
struct Projector {
    markets: HashMap<Pubkey, MarketRow>,
    /// Markets known not to exist (an event before its `MarketCreated`).
    absent: HashSet<Pubkey>,
    dirty: HashSet<Pubkey>,
    /// Markets whose set of pools may have grown (listing, graduation, a proposed or adopted
    /// pool): their pool-less rows are attributed after the flush ([`fill_market_pools`]).
    pools_changed: HashSet<Pubkey>,
    outcome: ApplyOutcome,
    check_order: bool,
}

impl Projector {
    fn new(check_order: bool) -> Self {
        Self {
            markets: HashMap::new(),
            absent: HashSet::new(),
            dirty: HashSet::new(),
            pools_changed: HashSet::new(),
            outcome: ApplyOutcome::default(),
            check_order,
        }
    }

    async fn market(
        &mut self,
        conn: &mut PgConnection,
        key: &Pubkey,
    ) -> anyhow::Result<Option<&mut MarketRow>> {
        if !self.markets.contains_key(key) && !self.absent.contains(key) {
            match load_market(conn, key).await? {
                Some(m) => {
                    self.markets.insert(*key, m);
                }
                None => {
                    self.absent.insert(*key);
                }
            }
        }
        Ok(self.markets.get_mut(key))
    }

    async fn flush(&mut self, conn: &mut PgConnection) -> anyhow::Result<()> {
        let mut keys: Vec<Pubkey> = self.dirty.drain().collect();
        keys.sort();
        for k in keys {
            if let Some(m) = self.markets.get(&k) {
                store_market(conn, m).await?;
            }
        }
        Ok(())
    }
}

/// Applies `items` (one transaction's, or a rebuild's, in chain order). `check_order`: the
/// incremental path's lateness checks (§3.3); a rebuild applies in order by construction.
pub async fn apply(
    conn: &mut PgConnection,
    items: &[Item],
    check_order: bool,
) -> anyhow::Result<ApplyOutcome> {
    let mut p = Projector::new(check_order);
    for item in items {
        match item {
            Item::Init(init) => {
                if check_order && global_is_late(conn, &init.signature, init.key).await? {
                    p.outcome.rebuild_global = true;
                    continue;
                }
                apply_initialize_config(conn, init).await?;
            }
            Item::Event(ev) => match ev.market {
                None if GLOBAL_KINDS.contains(&ev.kind.as_str()) => {
                    if check_order && global_is_late(conn, &ev.signature, ev.key).await? {
                        p.outcome.rebuild_global = true;
                        continue;
                    }
                    apply_global(conn, ev).await?;
                }
                None => {}
                Some(market) => apply_market_event(conn, &mut p, &market, ev).await?,
            },
        }
    }
    p.flush(conn).await?;
    let mut changed: Vec<Pubkey> = p.pools_changed.drain().collect();
    changed.sort();
    for market in changed {
        fill_market_pools(conn, &market).await?;
    }
    if p.outcome.rebuild_global {
        sqlx::query("UPDATE protocol_state SET needs_rebuild = TRUE WHERE id")
            .execute(&mut *conn)
            .await?;
    }
    Ok(p.outcome)
}

/// A global input is late if a global event or an `initialize_config` of another transaction
/// with a greater chain key is already stored.
async fn global_is_late(
    conn: &mut PgConnection,
    signature: &str,
    key: ChainKey,
) -> anyhow::Result<bool> {
    let kinds: Vec<String> = GLOBAL_KINDS.iter().map(|s| (*s).to_string()).collect();
    let late: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events WHERE market IS NULL AND kind = ANY($1) \
             AND signature <> $2 AND (slot, tx_index, ix_index, ev_source, ev_seq) > ($3, $4, $5, $6, $7)) \
         OR EXISTS (SELECT 1 FROM invocations i JOIN ingest_txs t USING (signature) \
             WHERE i.name = 'initialize_config' AND NOT i.failed AND i.signature <> $2 \
             AND (i.slot, i.tx_index, i.ix_index) > ($3, $4, $5)) \
         OR (SELECT needs_rebuild FROM protocol_state WHERE id)",
    )
    .bind(kinds)
    .bind(signature)
    .bind(key.slot)
    .bind(key.tx_index)
    .bind(key.ix_index)
    .bind(key.ev_source)
    .bind(key.ev_seq)
    .fetch_one(&mut *conn)
    .await
    .context("the global order check")?;
    Ok(late)
}

/// Kinds that never change a projection (log-only advisories).
fn no_effect(ev: &EventIn) -> bool {
    ev.kind == "MintSafetyTripped" && ev.key.ev_source == 1
}

async fn apply_market_event(
    conn: &mut PgConnection,
    p: &mut Projector,
    market: &Pubkey,
    ev: &EventIn,
) -> anyhow::Result<()> {
    if no_effect(ev) {
        return Ok(());
    }
    let check = p.check_order;
    if ev.kind == "MarketCreated" {
        if let Some(m) = p.market(conn, market).await? {
            // A second MarketCreated cannot happen on one chain: the rows are stale.
            if !m.needs_rebuild {
                m.needs_rebuild = true;
                p.dirty.insert(*market);
                p.outcome.rebuild_markets.insert(*market);
            }
            return Ok(());
        }
        let m = market_created(ev);
        // The row must exist before its children (positions, lenders, logs) reference it.
        store_market(conn, &m).await?;
        p.pools_changed.insert(*market);
        p.absent.remove(market);
        p.markets.insert(*market, m);
        let mut m = p.markets.get(market).cloned().unwrap_or_default();
        let fee = u64_field(&ev.data, "creation_fee");
        fee_row(conn, ev, "creation_fee", None, market, None, fee, false).await?;
        m.protocol_fees = m.protocol_fees.saturating_add(fee);
        if let Some(pool) = m.ray_pool {
            pool_adopted(conn, market, &pool, Some(m.venue), ev, 0, 0).await?;
        }
        log_state(conn, ev, market, &m).await?;
        // Events of this market already stored after it (a gap-fill delivered them first).
        if check && later_events_exist(conn, market, &ev.signature, ev.key).await? {
            m.needs_rebuild = true;
            p.outcome.rebuild_markets.insert(*market);
        }
        p.markets.insert(*market, m);
        p.dirty.insert(*market);
        return Ok(());
    }
    let Some(m) = p.market(conn, market).await? else {
        tracing::debug!(market = %market, kind = %ev.kind, "event before its MarketCreated: skipped");
        return Ok(());
    };
    if m.needs_rebuild {
        p.outcome.rebuild_markets.insert(*market);
        return Ok(());
    }
    if check && ev.key <= m.watermark {
        m.needs_rebuild = true;
        p.dirty.insert(*market);
        p.outcome.rebuild_markets.insert(*market);
        return Ok(());
    }
    let mut m = m.clone();
    let before = m.log_state();
    apply_to_market(conn, &mut m, market, ev).await?;
    if matches!(
        ev.kind.as_str(),
        "VenueStateChanged" | "PoolProposed" | "PoolSwitched"
    ) {
        p.pools_changed.insert(*market);
    }
    m.watermark = ev.key;
    if m.log_state() != before {
        log_state(conn, ev, market, &m).await?;
    }
    p.markets.insert(*market, m);
    p.dirty.insert(*market);
    Ok(())
}

/// Rows that name a mint but not a market take the market once it exists, so their `market`
/// does not depend on the order of ingestion (§3.3): venue trades and venue events only on one
/// of the market's pools (`sw_is_market_pool`; any pool can name a listed mint, M8-SEC-6) and a
/// pool-less venue event by the mint. Run after the projection is flushed, at the listing and
/// whenever the market's pools grow; a rebuild runs it the same way.
async fn fill_market_pools(conn: &mut PgConnection, market: &Pubkey) -> anyhow::Result<()> {
    let m = market.to_string();
    for sql in [
        "UPDATE venue_trades vt SET market = mk.market FROM markets mk \
         WHERE mk.market = $1 AND vt.market IS NULL AND vt.mint = mk.mint \
           AND sw_is_market_pool(mk.market, vt.pool)",
        "UPDATE venue_events ve SET market = mk.market FROM markets mk \
         WHERE mk.market = $1 AND ve.market IS NULL AND ve.mint = mk.mint \
           AND (ve.pool IS NULL OR sw_is_market_pool(mk.market, ve.pool))",
    ] {
        sqlx::query(sql).bind(&m).execute(&mut *conn).await?;
    }
    Ok(())
}

async fn later_events_exist(
    conn: &mut PgConnection,
    market: &Pubkey,
    signature: &str,
    key: ChainKey,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events WHERE market = $1 AND signature <> $2 \
           AND (slot, tx_index, ix_index, ev_source, ev_seq) > ($3, $4, $5, $6, $7))",
    )
    .bind(market.to_string())
    .bind(signature)
    .bind(key.slot)
    .bind(key.tx_index)
    .bind(key.ix_index)
    .bind(key.ev_source)
    .bind(key.ev_seq)
    .fetch_one(&mut *conn)
    .await?)
}

fn market_created(ev: &EventIn) -> MarketRow {
    let d = &ev.data;
    let venue = u8_field(d, "venue");
    let tier = u8_field(d, "tier");
    MarketRow {
        market: key_field(d, "market").unwrap_or_default(),
        mint: key_field(d, "mint").unwrap_or_default(),
        token_program: key_field(d, "token_program").unwrap_or_default(),
        creator: key_field(d, "creator").unwrap_or_default(),
        created_slot: ev.key.slot,
        created_ts: ev.ev_ts,
        created_signature: ev.signature.clone(),
        creation_fee: u64_field(d, "creation_fee"),
        venue_at_listing: venue,
        tier_at_listing: tier,
        bonding_curve: key_field(d, "bonding_curve"),
        canonical_pool: key_field(d, "canonical_pool"),
        venue,
        ray_pool: key_field(d, "ray_pool"),
        tier,
        watermark: ev.key,
        ..MarketRow::default()
    }
}

/// `market_state_log` (§6.1): the after-state of an accounting change.
async fn log_state(
    conn: &mut PgConnection,
    ev: &EventIn,
    market: &Pubkey,
    m: &MarketRow,
) -> anyhow::Result<()> {
    let s = m.log_state();
    sqlx::query(
        "INSERT INTO market_state_log (signature, ix_index, ev_source, ev_seq, market, slot, \
           tx_index, ts, kind, tokens_free, borrowed_q, total_shares, open_positions, \
           total_notional_open, open_vault_lamports, lender_sol, protocol_fees, ema_price) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::numeric, $11::numeric, $12::numeric, \
           $13, $14::numeric, $15::numeric, $16::numeric, $17::numeric, $18::numeric) \
         ON CONFLICT DO NOTHING",
    )
    .bind(&ev.signature)
    .bind(ev.key.ix_index)
    .bind(ev.key.ev_source)
    .bind(ev.key.ev_seq)
    .bind(market.to_string())
    .bind(ev.key.slot)
    .bind(ev.key.tx_index)
    .bind(ev.ev_ts)
    .bind(&ev.kind)
    .bind(s.tokens_free.to_string())
    .bind(s.borrowed_q.to_string())
    .bind(s.total_shares.to_string())
    .bind(i64_of(s.open_positions))
    .bind(s.total_notional_open.to_string())
    .bind(s.open_vault_lamports.to_string())
    .bind(s.lender_sol.to_string())
    .bind(s.protocol_fees.to_string())
    .bind(opt_text(s.ema_price))
    .execute(&mut *conn)
    .await
    .context("market_state_log")?;
    Ok(())
}

/// The recipient of a fee type (`model::FEE_TYPES`).
fn recipient(fee_type: &str) -> &'static str {
    crate::model::FEE_TYPES
        .iter()
        .find(|(t, _)| *t == fee_type)
        .map_or("protocol", |(_, r)| r)
}

/// One `fees` row (zero amounts are not written, §8.2).
#[allow(clippy::too_many_arguments)]
async fn fee_row(
    conn: &mut PgConnection,
    ev: &EventIn,
    fee_type: &str,
    recipient_key: Option<Pubkey>,
    market: &Pubkey,
    position: Option<Pubkey>,
    amount: u64,
    derived: bool,
) -> anyhow::Result<()> {
    if amount == 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO fees (signature, ix_index, ev_source, ev_seq, fee_type, recipient, \
           recipient_key, market, position, amount, derived, slot, tx_index, ts) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::numeric, $11, $12, $13, $14) \
         ON CONFLICT DO NOTHING",
    )
    .bind(&ev.signature)
    .bind(ev.key.ix_index)
    .bind(ev.key.ev_source)
    .bind(ev.key.ev_seq)
    .bind(fee_type)
    .bind(recipient(fee_type))
    .bind(opt_key(recipient_key))
    .bind(market.to_string())
    .bind(opt_key(position))
    .bind(amount.to_string())
    .bind(derived)
    .bind(ev.key.slot)
    .bind(ev.key.tx_index)
    .bind(ev.ev_ts)
    .execute(&mut *conn)
    .await
    .with_context(|| format!("fee {fee_type}"))?;
    Ok(())
}

/// The position row fields an exit needs.
#[derive(Clone, Copy, Debug, Default)]
struct OpenPosition {
    q: u64,
    tier: u8,
    n_open: u64,
    v: u64,
    m: u64,
    margin_added: u64,
    fee_open: u64,
    opened_ts: i64,
    deadline: i64,
    rent_protocol_bps: Option<u16>,
    status_open: bool,
}

async fn load_position(
    conn: &mut PgConnection,
    position: &Pubkey,
) -> anyhow::Result<Option<OpenPosition>> {
    let Some(r) = sqlx::query(
        "SELECT q::text, tier::smallint, n_open::text, v::text, m::text, margin_added::text, \
           fee_open::text, opened_ts, deadline, rent_protocol_bps::integer, status \
         FROM positions WHERE position = $1",
    )
    .bind(position.to_string())
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let status: String = r.try_get("status")?;
    let rp: Option<i32> = r.try_get("rent_protocol_bps")?;
    Ok(Some(OpenPosition {
        q: get_u64(&r, "q")?,
        tier: get_u8(&r, "tier")?,
        n_open: get_u64(&r, "n_open")?,
        v: get_u64(&r, "v")?,
        m: get_u64(&r, "m")?,
        margin_added: get_u64(&r, "margin_added")?,
        fee_open: get_u64(&r, "fee_open")?,
        opened_ts: r.try_get("opened_ts")?,
        deadline: r.try_get("deadline")?,
        rent_protocol_bps: rp.map(u16::try_from).transpose()?,
        status_open: status == "open",
    }))
}

/// `vest_bal` after event `kind` with fields `d` (INDEXER.md §6.2, [E\*]): the last
/// `PriceCranked` snapshot, and 0 after every flush, which pays the whole unreleased vest to the
/// shares at once:
/// - `SolSettle.vest_flushed > 0`: a settlement that left at most 1/1,000 of the token claims it
///   found, a full wipe included (AIR-07; `pool::settle`'s `Settled::flushed`), with or without an
///   epoch roll;
/// - a lender event with `s_after = 0`: the last shares left, and their lender was paid the vest
///   first (R3B-2); the program holds no vest while `S = 0`;
/// - `EpochRolled` (after its settlement's `SolSettle`): a full wipe holds no vest (the record's
///   donation tops up the wipe record); a rebase that flushed (`vest_flushed > 0`) holds only the
///   record's donation, vested anew (`record_donation`, §6.4); a rebase without a flush keeps
///   the snapshot.
fn vest_after(vest_bal: Option<u64>, kind: &str, d: &Value, record_donation: u64) -> Option<u64> {
    match kind {
        "PriceCranked" => Some(u64_field(d, "vest_bal")),
        "SolSettle" if u64_field(d, "vest_flushed") > 0 => Some(0),
        "Deposit" | "Withdraw" | "ClaimSol" | "DeficitCovered" | "LenderSynced"
            if u128_field(d, "s_after") == 0 =>
        {
            Some(0)
        }
        "EpochRolled" if u8_field(d, "num") == 0 => Some(0),
        "EpochRolled" if u64_field(d, "vest_flushed") > 0 => Some(record_donation),
        _ => vest_bal,
    }
}

/// The `ShareEpoch` record donation a rolling `sol_settle` swept into `lender_sol` (§6.4;
/// `events.derived` of its `EpochRolled`), 0 elsewhere.
fn record_donation(ev: &EventIn) -> u64 {
    ev.derived
        .as_ref()
        .map_or(0, |x| u64_field(x, "record_donation"))
}

/// The inverse of an open at an exit (`engine::waterfall::remove_position`).
fn remove_position(m: &mut MarketRow, pos: &OpenPosition) {
    let tier = usize::from(pos.tier.min(3));
    m.borrowed_q = m.borrowed_q.saturating_sub(pos.q);
    m.borrowed_q_tier[tier] = m.borrowed_q_tier[tier].saturating_sub(pos.q);
    m.open_positions = m.open_positions.saturating_sub(1);
    m.total_notional_open = m.total_notional_open.saturating_sub(pos.n_open);
    m.open_vault_lamports = m.open_vault_lamports.saturating_sub(pos.v);
    if m.open_positions == 0 {
        m.max_q_hwm = 0;
    }
}

/// The locked terms of a position opened at `key` (§6.5): the parameters in force then
/// (`protocol_params_history`), else the `ShortPosition` account the refresher (or a replayed
/// checkpoint) saw, else unknown.
async fn locked_terms(
    conn: &mut PgConnection,
    key: ChainKey,
    position: &Pubkey,
) -> anyhow::Result<(Option<[u16; 5]>, Option<&'static str>)> {
    let row = sqlx::query(
        "SELECT params FROM protocol_params_history \
         WHERE (slot, tx_index, ix_index) <= ($1, $2, $3) \
         ORDER BY slot DESC, tx_index DESC, ix_index DESC LIMIT 1",
    )
    .bind(key.slot)
    .bind(key.tx_index)
    .bind(key.ix_index)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(r) = row {
        let p: Value = r.try_get("params")?;
        let f = |n: &str| u16::try_from(u32_field(&p, n)).unwrap_or(u16::MAX);
        return Ok((
            Some([
                f("rent_protocol_bps"),
                f("penalty_lender_bps"),
                f("penalty_protocol_bps"),
                f("penalty_keeper_bps"),
                f("expiry_fee_bps"),
            ]),
            Some("config_history"),
        ));
    }
    let row = sqlx::query(
        "SELECT data FROM account_snapshots WHERE pubkey = $1 AND account_type = 'ShortPosition'",
    )
    .bind(position.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(r) = row {
        let d: Value = r.try_get("data")?;
        return Ok((Some(terms_of_position_account(&d)), Some("account")));
    }
    Ok((None, None))
}

/// `[rent_protocol, penalty_lender, penalty_protocol, penalty_keeper, expiry_fee]` bps of a
/// decoded `ShortPosition`.
pub fn terms_of_position_account(d: &Value) -> [u16; 5] {
    let split = |i: usize| {
        d.get("penalty_split")
            .and_then(|a| a.get(i))
            .and_then(Value::as_u64)
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(0)
    };
    let f = |n: &str| u16::try_from(u32_field(d, n)).unwrap_or(0);
    [
        f("rent_protocol_bps"),
        split(0),
        split(1),
        split(2),
        f("expiry_fee_bps"),
    ]
}

#[allow(clippy::too_many_lines)]
async fn apply_to_market(
    conn: &mut PgConnection,
    m: &mut MarketRow,
    market: &Pubkey,
    ev: &EventIn,
) -> anyhow::Result<()> {
    let d = &ev.data;
    let kind = ev.kind.as_str();
    let mkt = market.to_string();
    match kind {
        "Deposit" | "Withdraw" | "ClaimSol" | "DeficitCovered" | "LenderSynced" => {
            apply_lender(conn, m, market, ev).await?;
            m.tokens_free = u64_field(d, "t_after");
            m.total_shares = u128_field(d, "s_after");
            let sol_paid = u64_field(d, "sol_paid");
            if kind == "DeficitCovered" {
                m.lender_sol = m.lender_sol.saturating_add(u64_field(d, "amount"));
            }
            m.lender_sol = m.lender_sol.saturating_sub(sol_paid);
            if kind == "Deposit" {
                m.wipe_live = m.wipe_live && m.total_shares == 0;
            }
            m.lender_count = count_lenders(conn, market, m.share_epoch).await?;
        }
        "Open" => {
            let position = key_field(d, "position").unwrap_or_default();
            let q = u64_field(d, "q");
            let tier = u8_field(d, "tier");
            let v = u64_field(d, "v");
            let fee_open = u64_field(d, "fee_open");
            let n_open = u64_field(d, "n_open");
            let (terms, source) = locked_terms(conn, ev.key, &position).await?;
            let t = |i: usize| terms.map(|t| i32::from(t[i]));
            sqlx::query(
                "INSERT INTO positions (position, market, borrower, nonce, tier, venue, q, s, m, \
                   v_open, margin_added, v, n_open, fee_open, venue_rent, entry_price, \
                   spot_wad_open, ema_wad_open, b_spot_open, b_ema_open, deadline, opened_slot, \
                   opened_tx_index, opened_ts, open_signature, rent_protocol_bps, \
                   penalty_lender_bps, penalty_protocol_bps, penalty_keeper_bps, expiry_fee_bps, \
                   terms_source) \
                 VALUES ($1, $2, $3, $4::numeric, $5, $6, $7::numeric, $8::numeric, $9::numeric, \
                   $10::numeric, 0, $10::numeric, $11::numeric, $12::numeric, $13::numeric, \
                   $14::numeric, $15::numeric, $16::numeric, $17::numeric, $18::numeric, $19, $20, \
                   $21, $22, $23, $24, $25, $26, $27, $28, $29) \
                 ON CONFLICT (position) DO NOTHING",
            )
            .bind(position.to_string())
            .bind(&mkt)
            .bind(key_field(d, "borrower").unwrap_or_default().to_string())
            .bind(u64_field(d, "nonce").to_string())
            .bind(i16_of(tier))
            .bind(i16_of(u8_field(d, "venue")))
            .bind(q.to_string())
            .bind(u64_field(d, "s").to_string())
            .bind(u64_field(d, "m").to_string())
            .bind(v.to_string())
            .bind(n_open.to_string())
            .bind(fee_open.to_string())
            .bind(u64_field(d, "venue_rent").to_string())
            .bind(u128_field(d, "entry_price").to_string())
            .bind(u128_field(d, "spot_wad").to_string())
            .bind(u128_field(d, "ema_wad").to_string())
            .bind(u64_field(d, "b_spot").to_string())
            .bind(u64_field(d, "b_ema").to_string())
            .bind(i64_field(d, "deadline"))
            .bind(ev.key.slot)
            .bind(ev.key.tx_index)
            .bind(ev.ev_ts)
            .bind(&ev.signature)
            .bind(t(0))
            .bind(t(1))
            .bind(t(2))
            .bind(t(3))
            .bind(t(4))
            .bind(source)
            .execute(&mut *conn)
            .await
            .context("positions (Open)")?;
            let ti = usize::from(tier.min(3));
            m.tokens_free = m.tokens_free.saturating_sub(q);
            m.borrowed_q = m.borrowed_q.saturating_add(q);
            m.borrowed_q_tier[ti] = m.borrowed_q_tier[ti].saturating_add(q);
            m.open_positions = m.open_positions.saturating_add(1);
            m.total_notional_open = m.total_notional_open.saturating_add(n_open);
            m.max_q_hwm = m.max_q_hwm.max(q);
            m.open_vault_lamports = m.open_vault_lamports.saturating_add(v);
            m.protocol_fees = m.protocol_fees.saturating_add(fee_open);
            let ema = u128_field(d, "ema_wad");
            m.spot_wad = Some(u128_field(d, "spot_wad"));
            m.ema_price = Some(ema);
            m.price_slot = Some(ev.key.slot);
            m.price_ts = Some(ev.ev_ts);
            m.price_source = Some("event".into());
            m.tier = tier;
            m.last_ema = Some((ema, ev.ev_ts));
            fee_row(
                conn,
                ev,
                "open_fee",
                None,
                market,
                Some(position),
                fee_open,
                false,
            )
            .await?;
            fee_row(
                conn,
                ev,
                "venue_rent",
                None,
                market,
                Some(position),
                u64_field(d, "venue_rent"),
                false,
            )
            .await?;
        }
        "Close" | "Liquidate" | "Expire" | "SolSettle" => {
            apply_exit(conn, m, market, ev).await?;
        }
        "SettleArmed" | "SettleDisarmed" => {
            let armed = kind == "SettleArmed";
            sqlx::query(
                "UPDATE positions SET armed = $2, s_count = $3::numeric, s_since = $4, \
                   arm_count = arm_count + $5, disarm_count = disarm_count + $6 \
                 WHERE position = $1",
            )
            .bind(key_field(d, "position").unwrap_or_default().to_string())
            .bind(armed)
            .bind(u64_field(d, "s_count").to_string())
            .bind(i64_field(d, "s_since"))
            .bind(i64::from(armed))
            .bind(i64::from(!armed))
            .execute(&mut *conn)
            .await?;
        }
        "EpochRolled" => {
            let epoch = u32_field(d, "epoch");
            let num = u8_field(d, "num");
            let den = u128_field(d, "den");
            let full_wipe = num == 0;
            sqlx::query(
                "INSERT INTO share_epochs (market, epoch, acc_end, num, den, vest_flushed, \
                   full_wipe, signature, ix_index, ev_source, ev_seq, slot, ts) \
                 VALUES ($1, $2, $3::numeric, $4, $5::numeric, $6::numeric, $7, $8, $9, $10, $11, \
                   $12, $13) ON CONFLICT (market, epoch) DO NOTHING",
            )
            .bind(&mkt)
            .bind(i64_of(epoch))
            .bind(i128_field(d, "acc_end").to_string())
            .bind(i16_of(num))
            .bind(den.to_string())
            .bind(u64_field(d, "vest_flushed").to_string())
            .bind(full_wipe)
            .bind(&ev.signature)
            .bind(ev.key.ix_index)
            .bind(ev.key.ev_source)
            .bind(ev.key.ev_seq)
            .bind(ev.key.slot)
            .bind(ev.ev_ts)
            .execute(&mut *conn)
            .await
            .context("share_epochs")?;
            m.total_shares = formulas::convert_shares(m.total_shares, num, den);
            m.share_epoch = epoch.saturating_add(1);
            m.wipe_live = full_wipe;
            m.lender_sol = m.lender_sol.saturating_add(record_donation(ev));
            m.lender_count = count_lenders(conn, market, m.share_epoch).await?;
        }
        "ProtocolFeesSwept" => {
            let amount = u64_field(d, "amount");
            sqlx::query(
                "INSERT INTO protocol_sweeps (signature, ix_index, ev_source, ev_seq, market, \
                   amount, recipient, slot, ts) \
                 VALUES ($1, $2, $3, $4, $5, $6::numeric, $7, $8, $9) ON CONFLICT DO NOTHING",
            )
            .bind(&ev.signature)
            .bind(ev.key.ix_index)
            .bind(ev.key.ev_source)
            .bind(ev.key.ev_seq)
            .bind(&mkt)
            .bind(amount.to_string())
            .bind(key_field(d, "recipient").unwrap_or_default().to_string())
            .bind(ev.key.slot)
            .bind(ev.ev_ts)
            .execute(&mut *conn)
            .await?;
            m.protocol_fees = m.protocol_fees.saturating_sub(amount);
        }
        "MarginAdded" => {
            let amount = u64_field(d, "amount");
            sqlx::query(
                "UPDATE positions SET margin_added = margin_added + $2::numeric, \
                   v = v + $2::numeric WHERE position = $1",
            )
            .bind(key_field(d, "position").unwrap_or_default().to_string())
            .bind(amount.to_string())
            .execute(&mut *conn)
            .await?;
            m.open_vault_lamports = m.open_vault_lamports.saturating_add(amount);
        }
        "PriceCranked" => {
            let basis: Vec<u128> = (0..3)
                .map(|i| {
                    d.get("rent_basis")
                        .and_then(|a| a.get(i))
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0)
                })
                .collect();
            sqlx::query(
                "INSERT INTO market_snapshots (signature, ix_index, ev_source, ev_seq, market, \
                   slot, tx_index, ts, spot_wad, ema_price, ema_liq, liq_slow, ema_count, lb_lo, \
                   lb_hi, tier, util_bps, rent_basis0, rent_basis1, rent_basis2, vest_bal, cranker) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::numeric, $10::numeric, $11::numeric, \
                   $12::numeric, $13::numeric, $14::numeric, $15::numeric, $16, $17, \
                   $18::numeric, $19::numeric, $20::numeric, $21::numeric, $22) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(&ev.signature)
            .bind(ev.key.ix_index)
            .bind(ev.key.ev_source)
            .bind(ev.key.ev_seq)
            .bind(&mkt)
            .bind(ev.key.slot)
            .bind(ev.key.tx_index)
            .bind(ev.ev_ts)
            .bind(u128_field(d, "spot_wad").to_string())
            .bind(u128_field(d, "ema_price").to_string())
            .bind(u128_field(d, "ema_liq").to_string())
            .bind(u128_field(d, "liq_slow").to_string())
            .bind(u64_field(d, "ema_count").to_string())
            .bind(u128_field(d, "lb_lo").to_string())
            .bind(u128_field(d, "lb_hi").to_string())
            .bind(i16_of(u8_field(d, "tier")))
            .bind(i64_of(u32_field(d, "util_bps")))
            .bind(basis[0].to_string())
            .bind(basis[1].to_string())
            .bind(basis[2].to_string())
            .bind(u64_field(d, "vest_bal").to_string())
            .bind(ev.fee_payer.to_string())
            .execute(&mut *conn)
            .await
            .context("market_snapshots")?;
            let ema = u128_field(d, "ema_price");
            m.spot_wad = Some(u128_field(d, "spot_wad"));
            m.ema_price = Some(ema);
            m.ema_liq = Some(u128_field(d, "ema_liq"));
            m.liq_slow = Some(u128_field(d, "liq_slow"));
            m.ema_count = Some(u64_field(d, "ema_count"));
            m.lb_lo = Some(u128_field(d, "lb_lo"));
            m.lb_hi = Some(u128_field(d, "lb_hi"));
            m.util_bps = Some(u32_field(d, "util_bps"));
            m.rent_basis = [Some(basis[0]), Some(basis[1]), Some(basis[2])];
            m.tier = u8_field(d, "tier");
            m.price_slot = Some(ev.key.slot);
            m.price_ts = Some(ev.ev_ts);
            m.price_source = Some("event".into());
            m.last_ema = Some((ema, ev.ev_ts));
        }
        "VenueStateChanged" => {
            m.venue = u8_field(d, "to");
            m.venue_pool = key_field(d, "pool");
            let g = i64_field(d, "graduated_at");
            if g != 0 {
                m.graduated_at = Some(g);
            }
        }
        "RouteChainChanged" => {
            m.no_route_since = i64_field(d, "no_route_since");
            m.last_no_route_obs = i64_field(d, "last_no_route_obs");
            m.pool_fail_since = i64_field(d, "pool_fail_since");
            m.last_pool_fail_obs = i64_field(d, "last_pool_fail_obs");
        }
        "PoolProposed" | "PoolSwitched" | "PoolSwitchCancelled" => {
            apply_pool_event(conn, m, market, ev).await?;
        }
        "TraderSwept" | "VenueRewardsClaimed" => {
            let lamports = u64_field(d, "lamports");
            let prefix = if kind == "TraderSwept" {
                "trader_sweep"
            } else {
                "venue_rewards"
            };
            let (suffix, to_lenders) = match u8_field(d, "to") {
                0 => ("lenders", true),
                1 => ("wipe", true),
                _ => ("protocol", false),
            };
            fee_row(
                conn,
                ev,
                &format!("{prefix}_{suffix}"),
                None,
                market,
                None,
                lamports,
                false,
            )
            .await?;
            if to_lenders {
                m.lender_sol = m.lender_sol.saturating_add(lamports);
            } else {
                m.protocol_fees = m.protocol_fees.saturating_add(lamports);
            }
        }
        "MintSafetyTripped" => {
            if ev.key.ev_source == 0 {
                m.mint_unsafe = true;
            }
        }
        "CoverageGap" => {
            coverage_row(conn, ev, market, "gap").await?;
        }
        "CoveragePauseSet" => {
            coverage_row(conn, ev, market, "pause_set").await?;
            let paused = bool_field(d, "paused");
            if !paused && m.paused_coverage {
                m.coverage_cleared_at = Some(i64_field(d, "ts"));
            }
            m.paused_coverage = paused;
        }
        "PauseSet" => {
            m.paused_opens = bool_field(d, "paused");
        }
        "RouteBrokenSet" => {
            m.route_broken = bool_field(d, "broken");
        }
        other => {
            tracing::debug!(kind = other, market = %market, "no market projection");
        }
    }
    // `vest_bal` ([E*]): the crank's snapshot, 0 after a reported flush (AIR-07).
    m.vest_bal = vest_after(m.vest_bal, kind, d, record_donation(ev));
    Ok(())
}

async fn coverage_row(
    conn: &mut PgConnection,
    ev: &EventIn,
    market: &Pubkey,
    kind: &str,
) -> anyhow::Result<()> {
    let d = &ev.data;
    let gap = kind == "gap";
    sqlx::query(
        "INSERT INTO coverage_events (signature, ix_index, ev_source, ev_seq, market, kind, gap_s, \
           open_positions, paused, reason, by_key, slot, tx_index, ts) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) ON CONFLICT DO NOTHING",
    )
    .bind(&ev.signature)
    .bind(ev.key.ix_index)
    .bind(ev.key.ev_source)
    .bind(ev.key.ev_seq)
    .bind(market.to_string())
    .bind(kind)
    .bind(gap.then(|| i64_field(d, "gap_s")))
    .bind(gap.then(|| i64_of(u32_field(d, "open_positions"))))
    .bind((!gap).then(|| bool_field(d, "paused")))
    .bind((!gap).then(|| i16_of(u8_field(d, "reason"))))
    .bind(if gap { None } else { opt_key(key_field(d, "by")) })
    .bind(ev.key.slot)
    .bind(ev.key.tx_index)
    .bind(ev.ev_ts)
    .execute(&mut *conn)
    .await
    .context("coverage_events")?;
    Ok(())
}

/// Close / Liquidate / Expire / SolSettle (§5.3, §6.4, §6.5, §8.4).
#[allow(clippy::too_many_lines)]
async fn apply_exit(
    conn: &mut PgConnection,
    m: &mut MarketRow,
    market: &Pubkey,
    ev: &EventIn,
) -> anyhow::Result<()> {
    let d = &ev.data;
    let kind = ev.kind.as_str();
    let position = key_field(d, "position").unwrap_or_default();
    let Some(pos) = load_position(conn, &position).await? else {
        tracing::warn!(position = %position, kind, "exit of a position never opened in the index");
        return Ok(());
    };
    if !pos.status_open {
        tracing::warn!(position = %position, kind, "exit of a position already exited");
        return Ok(());
    }
    let keeper = key_field(d, "keeper");
    let rent = u64_field(d, "rent");
    let p_bps = pos.rent_protocol_bps.unwrap_or(0);
    let derived_rp = formulas::rent_protocol_share(rent, p_bps);
    let (status, borrower_payout, keeper_paid) = match kind {
        "Close" => ("closed", u64_field(d, "borrower_payout"), None),
        "Liquidate" => (
            "liquidated",
            u64_field(d, "borrower_payout"),
            Some(u64_field(d, "penalty_keeper").saturating_add(u64_field(d, "keeper_topup"))),
        ),
        "Expire" => (
            "expired",
            u64_field(d, "borrower_payout"),
            Some(u64_field(d, "keeper_reward")),
        ),
        _ => (
            "settled",
            u64_field(d, "borrower_amount"),
            Some(u64_field(d, "k_s")),
        ),
    };
    let rent_protocol = match kind {
        "Close" => Some(u64_field(d, "rent_protocol")),
        "Liquidate" | "Expire" => Some(derived_rp),
        _ => None,
    };
    let pnl = formulas::borrower_pnl(borrower_payout, pos.m, pos.margin_added, pos.fee_open);
    sqlx::query(
        "UPDATE positions SET status = $2, exit_slot = $3, exit_ts = $4, exit_signature = $5, \
           exit_keeper = $6, buyback_cost = $7::numeric, b_live_exit = $8::numeric, \
           rent_paid = $9::numeric, rent_protocol = $10::numeric, penalty = $11::numeric, \
           expiry_fee = $12::numeric, keeper_paid = $13::numeric, borrower_payout = $14::numeric, \
           token_surplus = $15::numeric, liquidated_expired = $16, settle_condition = $17, \
           borrower_pnl = $18::numeric, duration_s = $19, armed = FALSE \
         WHERE position = $1",
    )
    .bind(position.to_string())
    .bind(status)
    .bind(ev.key.slot)
    .bind(ev.ev_ts)
    .bind(&ev.signature)
    .bind(opt_key(keeper))
    .bind((kind != "SolSettle").then(|| u64_field(d, "buyback_cost").to_string()))
    .bind(u64_field(d, "b_live").to_string())
    .bind((kind != "SolSettle").then(|| rent.to_string()))
    .bind(opt_text(rent_protocol))
    .bind((kind == "Liquidate").then(|| u64_field(d, "penalty").to_string()))
    .bind((kind == "Expire").then(|| u64_field(d, "expiry_fee").to_string()))
    .bind(opt_text(keeper_paid))
    .bind(borrower_payout.to_string())
    .bind((kind != "SolSettle").then(|| u64_field(d, "token_surplus").to_string()))
    .bind((kind == "Liquidate").then(|| bool_field(d, "expired")))
    .bind((kind == "SolSettle").then(|| i16_of(u8_field(d, "condition"))))
    .bind(pnl.to_string())
    .bind(ev.ev_ts - pos.opened_ts)
    .execute(&mut *conn)
    .await
    .context("positions (exit)")?;
    remove_position(m, &pos);
    let pos_key = Some(position);
    match kind {
        "Close" => {
            let rp = u64_field(d, "rent_protocol");
            let rl = rent.saturating_sub(rp);
            m.tokens_free = m.tokens_free.saturating_add(pos.q);
            m.lender_sol = m.lender_sol.saturating_add(rl);
            m.protocol_fees = m.protocol_fees.saturating_add(rp);
            fee_row(conn, ev, "rent_lender", None, market, pos_key, rl, false).await?;
            fee_row(conn, ev, "rent_protocol", None, market, pos_key, rp, false).await?;
        }
        "Liquidate" => {
            let rl = rent.saturating_sub(derived_rp);
            let pl = u64_field(d, "penalty_lender");
            let pp = u64_field(d, "penalty_protocol");
            m.tokens_free = m.tokens_free.saturating_add(pos.q);
            m.lender_sol = m.lender_sol.saturating_add(rl).saturating_add(pl);
            m.protocol_fees = m
                .protocol_fees
                .saturating_add(derived_rp)
                .saturating_add(pp);
            fee_row(conn, ev, "rent_lender", None, market, pos_key, rl, true).await?;
            fee_row(
                conn,
                ev,
                "rent_protocol",
                None,
                market,
                pos_key,
                derived_rp,
                true,
            )
            .await?;
            fee_row(
                conn,
                ev,
                "liq_penalty_lender",
                None,
                market,
                pos_key,
                pl,
                false,
            )
            .await?;
            fee_row(
                conn,
                ev,
                "liq_penalty_protocol",
                None,
                market,
                pos_key,
                pp,
                false,
            )
            .await?;
            fee_row(
                conn,
                ev,
                "liq_penalty_keeper",
                keeper,
                market,
                pos_key,
                u64_field(d, "penalty_keeper"),
                false,
            )
            .await?;
            fee_row(
                conn,
                ev,
                "liq_keeper_topup",
                keeper,
                market,
                pos_key,
                u64_field(d, "keeper_topup"),
                false,
            )
            .await?;
        }
        "Expire" => {
            let rl = rent.saturating_sub(derived_rp);
            let xf = u64_field(d, "expiry_fee");
            m.tokens_free = m.tokens_free.saturating_add(pos.q);
            m.lender_sol = m.lender_sol.saturating_add(rl).saturating_add(xf);
            m.protocol_fees = m.protocol_fees.saturating_add(derived_rp);
            fee_row(conn, ev, "rent_lender", None, market, pos_key, rl, true).await?;
            fee_row(
                conn,
                ev,
                "rent_protocol",
                None,
                market,
                pos_key,
                derived_rp,
                true,
            )
            .await?;
            fee_row(conn, ev, "expiry_fee", None, market, pos_key, xf, false).await?;
            fee_row(
                conn,
                ev,
                "expiry_keeper_reward",
                keeper,
                market,
                pos_key,
                u64_field(d, "keeper_reward"),
                false,
            )
            .await?;
        }
        _ => {
            // SOL settlement: the tokens are extinguished (T unchanged).
            let lender_amount = u64_field(d, "lender_amount");
            m.lender_sol = m.lender_sol.saturating_add(lender_amount);
            let k_s = u64_field(d, "k_s");
            fee_row(
                conn,
                ev,
                "settle_keeper_reward",
                keeper,
                market,
                pos_key,
                k_s,
                false,
            )
            .await?;
            let p_lo = u128_field(d, "p_lo");
            let p_hi = u128_field(d, "p_hi");
            let (p_ref, source) = formulas::p_ref(m.last_ema.map(|(p, _)| p), p_lo, p_hi);
            let age = match source {
                formulas::PRefSource::CrankClamped => m.last_ema.map(|(_, ts)| ev.ev_ts - ts),
                _ => None,
            };
            let out = formulas::settlement_outcome(lender_amount, pos.q, p_ref);
            sqlx::query(
                "INSERT INTO settlements (position, market, keeper, condition, b_live, b_rs, b_ema, \
                   p_lo, p_hi, l_lo, l_hi, x_lo, x_hi, pprime_lo, pprime_hi, fv, k_s, \
                   lender_amount, lump, borrower_amount, ema_age_s, q, n_open, tier, expired, \
                   p_ref, p_ref_source, p_ref_age_s, token_value, lender_pnl, \
                   realised_lender_loss, signature, ix_index, ev_source, ev_seq, slot, tx_index, ts) \
                 VALUES ($1, $2, $3, $4, $5::numeric, $6::numeric, $7::numeric, $8::numeric, \
                   $9::numeric, $10::numeric, $11::numeric, $12::numeric, $13::numeric, $14, $15, \
                   $16::numeric, $17::numeric, $18::numeric, $19::numeric, $20::numeric, $21, \
                   $22::numeric, $23::numeric, $24, $25, $26::numeric, $27, $28, $29::numeric, \
                   $30::numeric, $31::numeric, $32, $33, $34, $35, $36, $37, $38) \
                 ON CONFLICT (position) DO NOTHING",
            )
            .bind(position.to_string())
            .bind(market.to_string())
            .bind(keeper.unwrap_or_default().to_string())
            .bind(i16_of(u8_field(d, "condition")))
            .bind(u64_field(d, "b_live").to_string())
            .bind(u64_field(d, "b_rs").to_string())
            .bind(u64_field(d, "b_ema").to_string())
            .bind(p_lo.to_string())
            .bind(p_hi.to_string())
            .bind(u128_field(d, "l_lo").to_string())
            .bind(u128_field(d, "l_hi").to_string())
            .bind(u64_field(d, "x_lo").to_string())
            .bind(u64_field(d, "x_hi").to_string())
            .bind(i32::try_from(u32_field(d, "pprime_lo")).unwrap_or(0))
            .bind(i32::try_from(u32_field(d, "pprime_hi")).unwrap_or(0))
            .bind(u64_field(d, "fv").to_string())
            .bind(k_s.to_string())
            .bind(lender_amount.to_string())
            .bind(u64_field(d, "lump").to_string())
            .bind(u64_field(d, "borrower_amount").to_string())
            .bind(i64_field(d, "ema_age_s"))
            .bind(pos.q.to_string())
            .bind(pos.n_open.to_string())
            .bind(i16_of(pos.tier))
            .bind(ev.ev_ts >= pos.deadline)
            .bind(p_ref.to_string())
            .bind(source.as_str())
            .bind(age)
            .bind(u64::try_from(out.token_value).unwrap_or(u64::MAX).to_string())
            .bind(out.lender_pnl.to_string())
            .bind(u64::try_from(out.realised_lender_loss).unwrap_or(u64::MAX).to_string())
            .bind(&ev.signature)
            .bind(ev.key.ix_index)
            .bind(ev.key.ev_source)
            .bind(ev.key.ev_seq)
            .bind(ev.key.slot)
            .bind(ev.key.tx_index)
            .bind(ev.ev_ts)
            .execute(&mut *conn)
            .await
            .context("settlements")?;
        }
    }
    Ok(())
}

/// PoolProposed / PoolSwitched / PoolSwitchCancelled (§5.3).
async fn apply_pool_event(
    conn: &mut PgConnection,
    m: &mut MarketRow,
    market: &Pubkey,
    ev: &EventIn,
) -> anyhow::Result<()> {
    let d = &ev.data;
    let candidate = key_field(d, "candidate").unwrap_or_default();
    let pool_kind = u8_field(d, "kind");
    let d_cand = u128_field(d, "d_cand");
    let d_inc = u128_field(d, "d_inc");
    let window_ends = i64_field(d, "window_ends");
    let candidate_kind = ev
        .derived
        .as_ref()
        .and_then(|x| x.get("candidate_kind"))
        .and_then(Value::as_u64)
        .and_then(|v| u8::try_from(v).ok());
    let (kind, candidate_venue) = match ev.kind.as_str() {
        "PoolProposed" => ("proposed", candidate_kind),
        "PoolSwitched" => ("switched", candidate_kind.or(m.fp_venue)),
        _ => ("cancelled", m.fp_venue),
    };
    sqlx::query(
        "INSERT INTO pool_events (signature, ix_index, ev_source, ev_seq, market, kind, candidate, \
           pool_kind, candidate_venue, d_cand, d_inc, window_ends, invocation, slot, tx_index, ts) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::numeric, $11::numeric, $12, $13, $14, \
           $15, $16) ON CONFLICT DO NOTHING",
    )
    .bind(&ev.signature)
    .bind(ev.key.ix_index)
    .bind(ev.key.ev_source)
    .bind(ev.key.ev_seq)
    .bind(market.to_string())
    .bind(kind)
    .bind(candidate.to_string())
    .bind(i16_of(pool_kind))
    .bind(candidate_venue.map(i16_of))
    .bind(d_cand.to_string())
    .bind(d_inc.to_string())
    .bind(window_ends)
    .bind(&ev.invocation)
    .bind(ev.key.slot)
    .bind(ev.key.tx_index)
    .bind(ev.ev_ts)
    .execute(&mut *conn)
    .await
    .context("pool_events")?;
    match kind {
        "proposed" => {
            m.fp_candidate = Some(candidate);
            m.fp_venue = candidate_kind.or(m.fp_venue);
            m.fp_d = Some(d_cand);
            m.fp_window_ends = Some(window_ends);
            m.pool_d = Some(d_inc);
            sqlx::query(
                "INSERT INTO pools (market, pool, venue, first_seen_slot, first_seen_ts, last_d, proposals) \
                 VALUES ($1, $2, $3, $4, $5, $6::numeric, 1) \
                 ON CONFLICT (market, pool) DO UPDATE SET last_d = EXCLUDED.last_d, \
                   proposals = pools.proposals + 1, venue = COALESCE(EXCLUDED.venue, pools.venue)",
            )
            .bind(market.to_string())
            .bind(candidate.to_string())
            .bind(candidate_venue.map(i16_of))
            .bind(ev.key.slot)
            .bind(ev.ev_ts)
            .bind(d_cand.to_string())
            .execute(&mut *conn)
            .await?;
        }
        "switched" => {
            if let Some(v) = candidate_venue {
                m.venue = v;
            }
            m.ray_pool = Some(candidate);
            m.pool_d = Some(d_cand);
            m.fp_candidate = None;
            m.fp_venue = None;
            m.fp_d = None;
            m.fp_window_ends = None;
            m.no_route_since = 0;
            m.last_no_route_obs = 0;
            m.pool_fail_since = 0;
            m.last_pool_fail_obs = 0;
            pool_adopted(
                conn,
                market,
                &candidate,
                candidate_venue,
                ev,
                pool_kind,
                d_cand,
            )
            .await?;
        }
        _ => {
            m.fp_candidate = None;
            m.fp_venue = None;
            m.fp_d = None;
            m.fp_window_ends = None;
        }
    }
    Ok(())
}

/// `pools`: `pool` adopted (the previous current pool retired).
async fn pool_adopted(
    conn: &mut PgConnection,
    market: &Pubkey,
    pool: &Pubkey,
    venue: Option<u8>,
    ev: &EventIn,
    adoption_kind: u8,
    d: u128,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE pools SET is_current = FALSE, retired_slot = $3, retired_ts = $4 \
         WHERE market = $1 AND is_current AND pool <> $2",
    )
    .bind(market.to_string())
    .bind(pool.to_string())
    .bind(ev.key.slot)
    .bind(ev.ev_ts)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO pools (market, pool, venue, first_seen_slot, first_seen_ts, last_d, \
           adopted_slot, adopted_ts, adoption_kind, is_current) \
         VALUES ($1, $2, $3, $4, $5, $6::numeric, $4, $5, $7, TRUE) \
         ON CONFLICT (market, pool) DO UPDATE SET venue = COALESCE(EXCLUDED.venue, pools.venue), \
           last_d = EXCLUDED.last_d, adopted_slot = EXCLUDED.adopted_slot, \
           adopted_ts = EXCLUDED.adopted_ts, adoption_kind = EXCLUDED.adoption_kind, \
           is_current = TRUE, retired_slot = NULL, retired_ts = NULL",
    )
    .bind(market.to_string())
    .bind(pool.to_string())
    .bind(venue.map(i16_of))
    .bind(ev.key.slot)
    .bind(ev.ev_ts)
    .bind(d.to_string())
    .bind(i16_of(adoption_kind))
    .execute(&mut *conn)
    .await
    .context("pools")?;
    Ok(())
}

/// A lender event (§6.3): converts the row across the epochs the event's `epoch` says the
/// lender went through, then applies the event's shares, lock and epoch.
async fn apply_lender(
    conn: &mut PgConnection,
    _m: &MarketRow,
    market: &Pubkey,
    ev: &EventIn,
) -> anyhow::Result<()> {
    let d = &ev.data;
    let kind = ev.kind.as_str();
    let owner = key_field(d, "owner").unwrap_or_default();
    let row = sqlx::query(
        "SELECT shares::text, epoch::bigint, deposited_tokens::text, withdrawn_tokens::text, \
           sol_paid::text, pot_draw::text, deficit_covered::text, deposits::bigint, \
           withdrawals::bigint, claims::bigint, first_slot, first_ts \
         FROM lender_positions WHERE market = $1 AND owner = $2",
    )
    .bind(market.to_string())
    .bind(owner.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    let (
        mut shares,
        epoch,
        mut dep_t,
        mut wd_t,
        mut sol_paid,
        mut pot,
        mut cov,
        mut n_dep,
        mut n_wd,
        mut n_cl,
        first_slot,
        first_ts,
    ) = match &row {
        Some(r) => (
            get_u128(r, "shares")?,
            get_u32(r, "epoch")?,
            get_u64(r, "deposited_tokens")?,
            get_u64(r, "withdrawn_tokens")?,
            get_u64(r, "sol_paid")?,
            get_u64(r, "pot_draw")?,
            get_u64(r, "deficit_covered")?,
            get_u32(r, "deposits")?,
            get_u32(r, "withdrawals")?,
            get_u32(r, "claims")?,
            r.try_get::<i64, _>("first_slot")?,
            r.try_get::<i64, _>("first_ts")?,
        ),
        None => (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, ev.key.slot, ev.ev_ts),
    };
    let ev_epoch = u32_field(d, "epoch");
    if ev_epoch > epoch && shares > 0 {
        let recs = sqlx::query(
            "SELECT epoch::bigint, num::smallint, den::text FROM share_epochs \
             WHERE market = $1 AND epoch >= $2 AND epoch < $3 ORDER BY epoch",
        )
        .bind(market.to_string())
        .bind(i64_of(epoch))
        .bind(i64_of(ev_epoch))
        .fetch_all(&mut *conn)
        .await?;
        for r in recs {
            let num = get_u8(&r, "num")?;
            let den = get_u128(&r, "den")?;
            shares = formulas::convert_shares(shares, num, den);
        }
    }
    let ev_shares = u128_field(d, "shares");
    let amount = u64_field(d, "amount");
    match kind {
        "Deposit" => {
            shares = shares.saturating_add(ev_shares);
            dep_t = dep_t.saturating_add(amount);
            n_dep = n_dep.saturating_add(1);
        }
        "Withdraw" => {
            shares = shares.saturating_sub(ev_shares);
            wd_t = wd_t.saturating_add(amount);
            n_wd = n_wd.saturating_add(1);
        }
        "ClaimSol" => n_cl = n_cl.saturating_add(1),
        "DeficitCovered" => cov = cov.saturating_add(amount),
        _ => {}
    }
    sol_paid = sol_paid.saturating_add(u64_field(d, "sol_paid"));
    pot = pot.saturating_add(u64_field(d, "pot_draw"));
    sqlx::query(
        "INSERT INTO lender_positions (market, owner, shares, epoch, lock, lock_until, \
           deposited_tokens, withdrawn_tokens, sol_paid, pot_draw, deficit_covered, deposits, \
           withdrawals, claims, first_slot, first_ts, last_slot, last_ts) \
         VALUES ($1, $2, $3::numeric, $4, $5::numeric, $6, $7::numeric, $8::numeric, $9::numeric, \
           $10::numeric, $11::numeric, $12, $13, $14, $15, $16, $17, $18) \
         ON CONFLICT (market, owner) DO UPDATE SET shares = EXCLUDED.shares, \
           epoch = EXCLUDED.epoch, lock = EXCLUDED.lock, lock_until = EXCLUDED.lock_until, \
           deposited_tokens = EXCLUDED.deposited_tokens, \
           withdrawn_tokens = EXCLUDED.withdrawn_tokens, sol_paid = EXCLUDED.sol_paid, \
           pot_draw = EXCLUDED.pot_draw, deficit_covered = EXCLUDED.deficit_covered, \
           deposits = EXCLUDED.deposits, withdrawals = EXCLUDED.withdrawals, \
           claims = EXCLUDED.claims, last_slot = EXCLUDED.last_slot, last_ts = EXCLUDED.last_ts",
    )
    .bind(market.to_string())
    .bind(owner.to_string())
    .bind(shares.to_string())
    .bind(i64_of(ev_epoch))
    .bind(u128_field(d, "lock").to_string())
    .bind(i64_field(d, "lock_until"))
    .bind(dep_t.to_string())
    .bind(wd_t.to_string())
    .bind(sol_paid.to_string())
    .bind(pot.to_string())
    .bind(cov.to_string())
    .bind(i64_of(n_dep))
    .bind(i64_of(n_wd))
    .bind(i64_of(n_cl))
    .bind(first_slot)
    .bind(first_ts)
    .bind(ev.key.slot)
    .bind(ev.ev_ts)
    .execute(&mut *conn)
    .await
    .context("lender_positions")?;
    Ok(())
}

/// `lender_count` (§6.2): lenders whose shares, converted to the current epoch, are > 0.
async fn count_lenders(
    conn: &mut PgConnection,
    market: &Pubkey,
    share_epoch: u32,
) -> anyhow::Result<u32> {
    let recs = sqlx::query(
        "SELECT epoch::bigint, num::smallint, den::text FROM share_epochs WHERE market = $1",
    )
    .bind(market.to_string())
    .fetch_all(&mut *conn)
    .await?;
    let mut by_epoch: HashMap<u32, (u8, u128)> = HashMap::new();
    for r in recs {
        by_epoch.insert(
            get_u32(&r, "epoch")?,
            (get_u8(&r, "num")?, get_u128(&r, "den")?),
        );
    }
    let lenders = sqlx::query(
        "SELECT shares::text, epoch::bigint FROM lender_positions WHERE market = $1 AND shares > 0",
    )
    .bind(market.to_string())
    .fetch_all(&mut *conn)
    .await?;
    let mut n = 0u32;
    for r in lenders {
        let mut shares = get_u128(&r, "shares")?;
        let epoch = get_u32(&r, "epoch")?;
        for e in epoch..share_epoch {
            if let Some((num, den)) = by_epoch.get(&e) {
                shares = formulas::convert_shares(shares, *num, *den);
            }
        }
        if shares > 0 {
            n = n.saturating_add(1);
        }
    }
    Ok(n)
}

// ---- the global projection (§6.6) ----------------------------------------------------------

/// The configuration snapshot of `protocol_params_history`.
#[derive(Clone, Debug, PartialEq)]
struct ParamsSnapshot {
    params: Value,
    tiers: Value,
    ema: Value,
    util_kink_bps: u32,
    governance: Option<String>,
    pause_authority: Option<String>,
    coverage_authority: Option<String>,
    fee_recipient: Option<String>,
    timelock_s: u32,
    venue_enabled: u32,
    pyth_sol_usd: Option<String>,
    pyth_feed_id: String,
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

async fn insert_params(
    conn: &mut PgConnection,
    signature: &str,
    key: ChainKey,
    ts: i64,
    source: &str,
    s: &ParamsSnapshot,
) -> anyhow::Result<()> {
    let zero = || Pubkey::default().to_string();
    sqlx::query(
        "INSERT INTO protocol_params_history (signature, ix_index, slot, tx_index, ts, source, \
           params, tiers, ema, util_kink_bps, governance, pause_authority, coverage_authority, \
           fee_recipient, timelock_s, venue_enabled, pyth_sol_usd, pyth_feed_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) \
         ON CONFLICT (signature, ix_index) DO NOTHING",
    )
    .bind(signature)
    .bind(key.ix_index)
    .bind(key.slot)
    .bind(key.tx_index)
    .bind(ts)
    .bind(source)
    .bind(&s.params)
    .bind(&s.tiers)
    .bind(&s.ema)
    .bind(i64_of(s.util_kink_bps))
    .bind(s.governance.clone().unwrap_or_else(zero))
    .bind(s.pause_authority.clone().unwrap_or_else(zero))
    .bind(s.coverage_authority.clone())
    .bind(s.fee_recipient.clone().unwrap_or_else(zero))
    .bind(i64_of(s.timelock_s))
    .bind(i64_of(s.venue_enabled))
    .bind(s.pyth_sol_usd.clone().unwrap_or_else(zero))
    .bind(&s.pyth_feed_id)
    .execute(&mut *conn)
    .await
    .context("protocol_params_history")?;
    Ok(())
}

async fn last_params(conn: &mut PgConnection) -> anyhow::Result<Option<ParamsSnapshot>> {
    let Some(r) = sqlx::query(
        "SELECT params, tiers, ema, util_kink_bps::bigint, governance::text, \
           pause_authority::text, coverage_authority::text, fee_recipient::text, \
           timelock_s::bigint, venue_enabled::bigint, pyth_sol_usd::text, pyth_feed_id \
         FROM protocol_params_history ORDER BY slot DESC, tx_index DESC, ix_index DESC LIMIT 1",
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    Ok(Some(ParamsSnapshot {
        params: r.try_get("params")?,
        tiers: r.try_get("tiers")?,
        ema: r.try_get("ema")?,
        util_kink_bps: get_u32(&r, "util_kink_bps")?,
        governance: r.try_get("governance")?,
        pause_authority: r.try_get("pause_authority")?,
        coverage_authority: r.try_get("coverage_authority")?,
        fee_recipient: r.try_get("fee_recipient")?,
        timelock_s: get_u32(&r, "timelock_s")?,
        venue_enabled: get_u32(&r, "venue_enabled")?,
        pyth_sol_usd: r.try_get("pyth_sol_usd")?,
        pyth_feed_id: r.try_get("pyth_feed_id")?,
    }))
}

async fn apply_initialize_config(
    conn: &mut PgConnection,
    init: &InitConfigIn,
) -> anyhow::Result<()> {
    let a = init.args.get("args").cloned().unwrap_or(Value::Null);
    let snap = ParamsSnapshot {
        params: a.get("params").cloned().unwrap_or(Value::Null),
        tiers: a.get("tiers").cloned().unwrap_or(Value::Null),
        ema: a.get("ema").cloned().unwrap_or(Value::Null),
        util_kink_bps: u32_field(&a, "util_kink_bps"),
        governance: str_of(&a, "governance"),
        pause_authority: str_of(&a, "pause_authority"),
        coverage_authority: str_of(&a, "coverage_authority"),
        fee_recipient: str_of(&a, "fee_recipient"),
        timelock_s: u32_field(&a, "timelock_s"),
        venue_enabled: u32_field(&a, "venue_enabled"),
        pyth_sol_usd: str_of(&a, "pyth_sol_usd"),
        pyth_feed_id: str_of(&a, "pyth_feed_id").unwrap_or_default(),
    };
    let ts = init.block_time.unwrap_or(0);
    insert_params(
        conn,
        &init.signature,
        init.key,
        ts,
        "initialize_config",
        &snap,
    )
    .await?;
    sqlx::query(
        "UPDATE protocol_state SET venue_enabled = $1, last_slot = $2, last_ts = $3 WHERE id",
    )
    .bind(i64_of(snap.venue_enabled))
    .bind(init.key.slot)
    .bind(ts)
    .execute(&mut *conn)
    .await?;
    // The latch starts at the deploy slots validated at launch (VenueGuard.guard_slot).
    if let Some(slots) = a.get("guard_slot").and_then(Value::as_array) {
        for (i, s) in slots.iter().enumerate() {
            let slot = s.as_str().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            sqlx::query(
                "INSERT INTO venue_guards (venue, upgraded, guard_slot, programdata_slot, \
                   last_slot, last_ts) VALUES ($1, FALSE, $2::numeric, $2::numeric, $3, $4) \
                 ON CONFLICT (venue) DO UPDATE SET upgraded = FALSE, \
                   guard_slot = EXCLUDED.guard_slot, programdata_slot = EXCLUDED.programdata_slot, \
                   flagged_at = NULL, expected_slot = NULL, last_slot = EXCLUDED.last_slot, \
                   last_ts = EXCLUDED.last_ts",
            )
            .bind(i16::try_from(i).unwrap_or(0))
            .bind(slot.to_string())
            .bind(init.key.slot)
            .bind(ts)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

/// Applies a `ConfigChange` (its §4.2 JSON) to a snapshot (`GlobalConfig::apply`).
fn apply_change(s: &mut ParamsSnapshot, change: &Value) {
    let Some((name, v)) = change.as_object().and_then(|o| o.iter().next()) else {
        return;
    };
    match name.as_str() {
        "Params" => {
            s.params = v.get("params").cloned().unwrap_or(Value::Null);
            s.tiers = v.get("tiers").cloned().unwrap_or(Value::Null);
        }
        "EmaParams" => s.ema = v.get("ema").cloned().unwrap_or(Value::Null),
        "Kink" => s.util_kink_bps = u32_field(v, "util_kink_bps"),
        "Authorities" => {
            s.governance = str_of(v, "governance");
            s.pause_authority = str_of(v, "pause_authority");
            s.coverage_authority = str_of(v, "coverage_authority");
            s.fee_recipient = str_of(v, "fee_recipient");
        }
        "Timelock" => s.timelock_s = u32_field(v, "timelock_s"),
        "EnableVenues" => s.venue_enabled |= u32_field(v, "bits"),
        "OracleFeed" => {
            s.pyth_sol_usd = str_of(v, "account");
            s.pyth_feed_id = str_of(v, "feed_id").unwrap_or_default();
        }
        _ => {}
    }
}

#[allow(clippy::too_many_lines)]
async fn apply_global(conn: &mut PgConnection, ev: &EventIn) -> anyhow::Result<()> {
    let d = &ev.data;
    match ev.kind.as_str() {
        "ConfigProposed" => {
            let kind = u8_field(d, "kind");
            let hash = d
                .get("hash")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let derived = ev.derived.clone().unwrap_or(Value::Null);
            let payload = derived.get("payload").cloned();
            let verified = derived.get("payload_hash").and_then(Value::as_str)
                == Some(hash.as_str())
                && derived.get("change_kind").and_then(Value::as_u64) == Some(u64::from(kind));
            // One pending change at a time (`GlobalConfig.pending`): a proposal replaces it.
            sqlx::query("UPDATE config_changes SET status = 'superseded' WHERE status = 'pending'")
                .execute(&mut *conn)
                .await?;
            sqlx::query(
                "INSERT INTO config_changes (proposed_signature, proposed_ix_index, \
                   proposed_ev_source, proposed_ev_seq, kind, kind_name, hash, payload, \
                   payload_verified, not_before, proposed_slot, proposed_ts) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) ON CONFLICT DO NOTHING",
            )
            .bind(&ev.signature)
            .bind(ev.key.ix_index)
            .bind(ev.key.ev_source)
            .bind(ev.key.ev_seq)
            .bind(i16_of(kind))
            .bind(super::decode::shortwin_events::config_kind_name(kind))
            .bind(&hash)
            .bind(payload)
            .bind(verified)
            .bind(i64_field(d, "not_before"))
            .bind(ev.key.slot)
            .bind(ev.ev_ts)
            .execute(&mut *conn)
            .await
            .context("config_changes")?;
        }
        "ConfigApplied" => {
            let hash = d
                .get("hash")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let row = sqlx::query(
                "UPDATE config_changes SET status = 'applied', applied_signature = $2, \
                   applied_slot = $3, applied_ts = $4 \
                 WHERE (proposed_signature, proposed_ix_index, proposed_ev_source, proposed_ev_seq) = ( \
                   SELECT proposed_signature, proposed_ix_index, proposed_ev_source, proposed_ev_seq \
                   FROM config_changes WHERE status = 'pending' AND hash = $1 \
                   ORDER BY proposed_slot DESC LIMIT 1) \
                 RETURNING payload",
            )
            .bind(&hash)
            .bind(&ev.signature)
            .bind(ev.key.slot)
            .bind(ev.ev_ts)
            .fetch_optional(&mut *conn)
            .await?;
            let payload: Option<Value> = match row {
                Some(r) => r.try_get("payload")?,
                None => None,
            };
            match (last_params(conn).await?, payload) {
                (Some(mut snap), Some(change)) => {
                    apply_change(&mut snap, &change);
                    insert_params(conn, &ev.signature, ev.key, ev.ev_ts, "apply_config", &snap)
                        .await?;
                    // `GlobalConfig::apply`: only `EnableVenues` touches `venue_enabled`, and it
                    // ORs into the live value (a `disable_venue` since the last snapshot stays
                    // applied; M8-SEC-7). No other change may rewrite it from a snapshot.
                    if let Some(bits) = change.get("EnableVenues").map(|v| u32_field(v, "bits")) {
                        sqlx::query(
                            "UPDATE protocol_state SET venue_enabled = \
                               COALESCE(venue_enabled, 0) | $1::bigint WHERE id",
                        )
                        .bind(i64_of(bits))
                        .execute(&mut *conn)
                        .await?;
                    }
                }
                _ => tracing::warn!(
                    hash,
                    "ConfigApplied without its proposal or a parameters history"
                ),
            }
        }
        "ConfigCancelled" => {
            let hash = d
                .get("hash")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            sqlx::query(
                "UPDATE config_changes SET status = 'cancelled', cancelled_signature = $2, \
                   cancelled_slot = $3, cancelled_ts = $4, cancelled_by = $5 \
                 WHERE status = 'pending' AND hash = $1",
            )
            .bind(&hash)
            .bind(&ev.signature)
            .bind(ev.key.slot)
            .bind(ev.ev_ts)
            .bind(opt_key(key_field(d, "by")))
            .execute(&mut *conn)
            .await?;
        }
        "PauseSet" => {
            sqlx::query("UPDATE protocol_state SET paused_opens = $1 WHERE id")
                .bind(bool_field(d, "paused"))
                .execute(&mut *conn)
                .await?;
        }
        "VenueDisabled" => {
            let enabled = u32_field(d, "venue_enabled");
            sqlx::query("UPDATE protocol_state SET venue_enabled = $1 WHERE id")
                .bind(i64_of(enabled))
                .execute(&mut *conn)
                .await?;
            // The configuration history records it too, so a later `ConfigApplied` (which
            // starts from the last snapshot) keeps the bit cleared (M8-SEC-7).
            if let Some(mut snap) = last_params(conn).await? {
                snap.venue_enabled = enabled;
                insert_params(
                    conn,
                    &ev.signature,
                    ev.key,
                    ev.ev_ts,
                    "disable_venue",
                    &snap,
                )
                .await?;
            }
        }
        "RouteBrokenSet" => {
            let bit = 1i64 << u32::from(u8_field(d, "venue_bit").min(31));
            let sql = if bool_field(d, "broken") {
                "UPDATE protocol_state SET route_broken_bits = route_broken_bits | $1 WHERE id"
            } else {
                "UPDATE protocol_state SET route_broken_bits = route_broken_bits & ~$1::bigint WHERE id"
            };
            sqlx::query(sql).bind(bit).execute(&mut *conn).await?;
        }
        "VenueExtrasSet" => {
            // Every key as base58 (the API's `keys: [string]`): the unused slots' default key
            // is "11111111111111111111111111111111", not NULL, in this array.
            let keys: Vec<String> = d
                .get("keys")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|k| {
                            k.as_str()
                                .map_or_else(|| Pubkey::default().to_string(), str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default();
            sqlx::query(
                "INSERT INTO venue_extras (venue, ix, version, keys, stable_version, last_slot, last_ts) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (venue, ix) DO UPDATE SET version = EXCLUDED.version, \
                   keys = EXCLUDED.keys, stable_version = EXCLUDED.stable_version, \
                   last_slot = EXCLUDED.last_slot, last_ts = EXCLUDED.last_ts",
            )
            .bind(i16_of(u8_field(d, "venue")))
            .bind(i16_of(u8_field(d, "ix")))
            .bind(i64_of(u32_field(d, "version")))
            .bind(keys)
            .bind(i64_of(u32_field(d, "stable_version")))
            .bind(ev.key.slot)
            .bind(ev.ev_ts)
            .execute(&mut *conn)
            .await?;
        }
        "VenueUpgradeFlagged" => {
            sqlx::query(
                "INSERT INTO venue_guards (venue, upgraded, guard_slot, programdata_slot, \
                   flagged_at, last_slot, last_ts) VALUES ($1, TRUE, $2::numeric, $3::numeric, $4, $5, $6) \
                 ON CONFLICT (venue) DO UPDATE SET upgraded = TRUE, guard_slot = EXCLUDED.guard_slot, \
                   programdata_slot = EXCLUDED.programdata_slot, flagged_at = EXCLUDED.flagged_at, \
                   last_slot = EXCLUDED.last_slot, last_ts = EXCLUDED.last_ts",
            )
            .bind(i16_of(u8_field(d, "venue")))
            .bind(u64_field(d, "guard_slot").to_string())
            .bind(u64_field(d, "programdata_slot").to_string())
            .bind(i64_field(d, "flagged_at"))
            .bind(ev.key.slot)
            .bind(ev.ev_ts)
            .execute(&mut *conn)
            .await?;
        }
        "VenueRevalidated" => {
            let pd = u64_field(d, "programdata_slot").to_string();
            sqlx::query(
                "INSERT INTO venue_guards (venue, upgraded, guard_slot, programdata_slot, \
                   expected_slot, last_slot, last_ts) VALUES ($1, FALSE, $2::numeric, $2::numeric, $3::numeric, $4, $5) \
                 ON CONFLICT (venue) DO UPDATE SET upgraded = FALSE, guard_slot = EXCLUDED.guard_slot, \
                   programdata_slot = EXCLUDED.programdata_slot, flagged_at = NULL, \
                   expected_slot = EXCLUDED.expected_slot, last_slot = EXCLUDED.last_slot, \
                   last_ts = EXCLUDED.last_ts",
            )
            .bind(i16_of(u8_field(d, "venue")))
            .bind(pd)
            .bind(u64_field(d, "expected_slot").to_string())
            .bind(ev.key.slot)
            .bind(ev.ev_ts)
            .execute(&mut *conn)
            .await?;
        }
        _ => {}
    }
    sqlx::query("UPDATE protocol_state SET last_slot = GREATEST(last_slot, $1), last_ts = GREATEST(last_ts, $2) WHERE id")
        .bind(ev.key.slot)
        .bind(ev.ev_ts)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

// ---- reading inputs back from `events` -----------------------------------------------------

fn event_from_row(r: &PgRow) -> anyhow::Result<EventIn> {
    let market: Option<String> = r.try_get("market")?;
    let fee_payer: String = r.try_get("fee_payer")?;
    Ok(EventIn {
        signature: r.try_get("signature")?,
        key: ChainKey {
            slot: r.try_get("slot")?,
            tx_index: r.try_get("tx_index")?,
            ix_index: r.try_get("ix_index")?,
            ev_source: r.try_get("ev_source")?,
            ev_seq: r.try_get("ev_seq")?,
        },
        block_time: r.try_get("block_time")?,
        fee_payer: crate::numeric::parse_key(&fee_payer)?,
        invocation: r.try_get("invocation")?,
        kind: r.try_get("kind")?,
        market: market.map(|m| crate::numeric::parse_key(&m)).transpose()?,
        ev_ts: r.try_get("ev_ts")?,
        data: r.try_get("data")?,
        derived: r.try_get("derived")?,
    })
}

const EVENT_COLUMNS: &str = "e.signature::text AS signature, e.slot, e.tx_index, e.ix_index, \
    e.ev_source, e.ev_seq, e.block_time, t.fee_payer::text AS fee_payer, e.invocation, e.kind, \
    e.market::text AS market, e.ev_ts, e.data, e.derived";

// ---- rebuild -------------------------------------------------------------------------------

/// Rebuilds `market` (or every market and the global projection when `None`); returns the
/// number of markets rebuilt. Takes the writer lock (§3.1): not while a writer runs.
pub async fn rebuild(pool: &PgPool, market: Option<Pubkey>) -> anyhow::Result<u64> {
    let lock = super::writer::WriterLock::acquire(pool).await?;
    let res = async {
        match market {
            Some(m) => {
                rebuild_market(pool, &m).await?;
                Ok(1)
            }
            None => {
                rebuild_global(pool).await?;
                let markets: Vec<String> = sqlx::query_scalar(
                    "SELECT market::text FROM markets UNION \
                     SELECT DISTINCT market::text FROM events WHERE kind = 'MarketCreated'",
                )
                .fetch_all(pool)
                .await?;
                let mut n = 0;
                for m in markets {
                    rebuild_market(pool, &crate::numeric::parse_key(&m)?).await?;
                    n += 1;
                }
                Ok(n)
            }
        }
    }
    .await;
    lock.release().await?;
    res
}

/// Rebuilds every market marked `needs_rebuild` and, if marked, the global projection (the
/// writer's end of batch; the caller holds the writer lock).
pub async fn rebuild_marked(pool: &PgPool) -> anyhow::Result<u64> {
    let global: bool = sqlx::query_scalar("SELECT needs_rebuild FROM protocol_state WHERE id")
        .fetch_one(pool)
        .await?;
    if global {
        rebuild_global(pool).await?;
    }
    let markets: Vec<String> =
        sqlx::query_scalar("SELECT market::text FROM markets WHERE needs_rebuild")
            .fetch_all(pool)
            .await?;
    let mut n = 0;
    for m in markets {
        rebuild_market(pool, &crate::numeric::parse_key(&m)?).await?;
        n += 1;
    }
    Ok(n)
}

/// Rebuilds one market from its events (module docs). The caller holds the writer lock.
pub async fn rebuild_market(pool: &PgPool, market: &Pubkey) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    let mkt = market.to_string();
    for sql in [
        // Markets resolved by mint are re-resolved by the MarketCreated re-applied below.
        "UPDATE venue_trades SET market = NULL WHERE market = $1 AND NOT is_shortwin",
        "UPDATE venue_events SET market = NULL WHERE market = $1",
        "DELETE FROM fees WHERE market = $1",
        "DELETE FROM settlements WHERE market = $1",
        "DELETE FROM pool_events WHERE market = $1",
        "DELETE FROM market_snapshots WHERE market = $1",
        "DELETE FROM coverage_events WHERE market = $1",
        "DELETE FROM protocol_sweeps WHERE market = $1",
        "DELETE FROM markets WHERE market = $1",
    ] {
        sqlx::query(sql).bind(&mkt).execute(&mut *tx).await?;
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {EVENT_COLUMNS} FROM events e JOIN ingest_txs t USING (signature) \
         WHERE e.market = $1 ORDER BY e.slot, e.tx_index, e.ix_index, e.ev_source, e.ev_seq"
    )))
    .bind(&mkt)
    .fetch_all(&mut *tx)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        items.push(Item::Event(event_from_row(r)?));
    }
    // A market's first event must be its MarketCreated; anything before it is unplaceable.
    apply(&mut tx, &items, false).await?;
    super::refresher::refresh_market_from_snapshot(&mut tx, market, false).await?;
    tx.commit().await?;
    Ok(())
}

/// Rebuilds the global projection (module docs). The caller holds the writer lock.
pub async fn rebuild_global(pool: &PgPool) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for sql in [
        "DELETE FROM config_changes",
        "DELETE FROM protocol_params_history",
        "DELETE FROM venue_guards",
        "DELETE FROM venue_extras",
        "UPDATE protocol_state SET paused_opens = FALSE, route_broken_bits = 0, venue_enabled = NULL, \
           last_slot = 0, last_ts = 0, needs_rebuild = FALSE WHERE id",
    ] {
        sqlx::query(sql).execute(&mut *tx).await?;
    }
    let kinds: Vec<String> = GLOBAL_KINDS.iter().map(|s| (*s).to_string()).collect();
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {EVENT_COLUMNS} FROM events e JOIN ingest_txs t USING (signature) \
         WHERE e.market IS NULL AND e.kind = ANY($1)"
    )))
    .bind(kinds)
    .fetch_all(&mut *tx)
    .await?;
    let mut items = Vec::new();
    for r in &rows {
        items.push(Item::Event(event_from_row(r)?));
    }
    let inits = sqlx::query(
        "SELECT i.signature::text AS signature, i.slot, i.tx_index, i.ix_index, i.inner_index, \
           i.block_time, i.args FROM invocations i \
         WHERE i.name = 'initialize_config' AND NOT i.failed",
    )
    .fetch_all(&mut *tx)
    .await?;
    for r in &inits {
        let inner: i16 = r.try_get("inner_index")?;
        items.push(Item::Init(InitConfigIn {
            signature: r.try_get("signature")?,
            key: ChainKey {
                slot: r.try_get("slot")?,
                tx_index: r.try_get("tx_index")?,
                ix_index: r.try_get("ix_index")?,
                ev_source: -1,
                ev_seq: i32::from(inner),
            },
            block_time: r.try_get("block_time")?,
            args: r
                .try_get::<Option<Value>, _>("args")?
                .unwrap_or(Value::Null),
        }));
    }
    items.sort_by_key(Item::key);
    apply(&mut tx, &items, false).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn changes_apply_like_global_config_apply() {
        let mut s = ParamsSnapshot {
            params: json!({"rent_protocol_bps": 1500}),
            tiers: json!([]),
            ema: json!({}),
            util_kink_bps: 8_000,
            governance: Some("g".into()),
            pause_authority: Some("p".into()),
            coverage_authority: None,
            fee_recipient: Some("f".into()),
            timelock_s: 172_800,
            venue_enabled: 0b0011,
            pyth_sol_usd: None,
            pyth_feed_id: String::new(),
        };
        apply_change(&mut s, &json!({"EnableVenues": {"bits": 4}}));
        assert_eq!(s.venue_enabled, 0b0111);
        apply_change(&mut s, &json!({"Kink": {"util_kink_bps": 7_500}}));
        assert_eq!(s.util_kink_bps, 7_500);
        apply_change(&mut s, &json!({"Timelock": {"timelock_s": 90_000}}));
        assert_eq!(s.timelock_s, 90_000);
        apply_change(
            &mut s,
            &json!({"Authorities": {"governance": "G", "pause_authority": "P",
                "coverage_authority": null, "fee_recipient": "F"}}),
        );
        assert_eq!(s.governance.as_deref(), Some("G"));
        assert_eq!(s.coverage_authority, None);
        apply_change(
            &mut s,
            &json!({"Params": {"params": {"rent_protocol_bps": 1}, "tiers": [1]}}),
        );
        assert_eq!(s.params["rent_protocol_bps"], 1);
    }

    /// An event's §4.2 JSON as the decoder renders it from the program's own type.
    fn decoded<T: anchor_lang::AnchorSerialize + anchor_lang::Discriminator>(ev: &T) -> Value {
        let mut payload = T::DISCRIMINATOR.to_vec();
        payload.extend(crate::ingest::decode::borsh_bytes(ev));
        crate::ingest::decode::shortwin_events::decode_shortwin_event(&payload)
            .expect("decodes")
            .data
    }

    fn sol_settle(vest_flushed: u64) -> Value {
        decoded(&shortwin::events::SolSettle {
            market: Pubkey::new_from_array([1; 32]),
            slot: 1,
            ts: 2,
            position: Pubkey::new_from_array([2; 32]),
            keeper: Pubkey::new_from_array([3; 32]),
            condition: 1,
            b_live: 0,
            b_rs: 0,
            b_ema: 0,
            p_lo: 0,
            p_hi: 0,
            l_lo: 0,
            l_hi: 0,
            x_lo: 0,
            x_hi: 0,
            pprime_lo: 0,
            pprime_hi: 0,
            fv: 0,
            k_s: 5,
            lender_amount: 900,
            lump: 100,
            borrower_amount: 0,
            ema_age_s: 0,
            vest_flushed,
        })
    }

    fn rolled(num: u8, vest_flushed: u64) -> Value {
        decoded(&shortwin::events::EpochRolled {
            market: Pubkey::new_from_array([1; 32]),
            slot: 1,
            ts: 2,
            epoch: 0,
            acc_end: 0,
            num,
            den: if num == 0 { 1 } else { 1_000 },
            vest_flushed,
        })
    }

    /// AIR-07, the flush without a roll: a settlement that flushed the vest
    /// (`SolSettle.vest_flushed > 0`) leaves `vest_bal` at 0 whether or not an `EpochRolled`
    /// follows (before the field, nothing reported the flush without a roll and `vest_bal` kept
    /// the last crank's value until the next `PriceCranked`); a settlement that did not flush
    /// keeps the snapshot.
    #[test]
    fn a_flush_without_a_roll_zeroes_the_vest() {
        let crank = decoded(&shortwin::events::PriceCranked {
            market: Pubkey::new_from_array([1; 32]),
            slot: 1,
            ts: 2,
            spot_wad: 1,
            ema_price: 1,
            ema_liq: 1,
            liq_slow: 1,
            ema_count: 1,
            lb_lo: 1,
            lb_hi: 1,
            tier: 3,
            util_bps: 9_000,
            rent_basis: [0; 3],
            vest_bal: 5_000_000,
        });
        let snap = vest_after(None, "PriceCranked", &crank, 0);
        assert_eq!(snap, Some(5_000_000), "the crank's snapshot");
        assert_eq!(
            vest_after(snap, "SolSettle", &sol_settle(5_000_100), 0),
            Some(0),
            "a settlement that flushed, no roll"
        );
        assert_eq!(
            vest_after(None, "SolSettle", &sol_settle(1), 0),
            Some(0),
            "a flush is exact even with no snapshot yet"
        );
        assert_eq!(
            vest_after(snap, "SolSettle", &sol_settle(0), 0),
            snap,
            "no flush: the snapshot stands"
        );
        for kind in ["Close", "Liquidate", "Expire", "SettleArmed", "TraderSwept"] {
            assert_eq!(vest_after(snap, kind, &Value::Null, 0), snap, "{kind}");
        }
    }

    /// AIR-07: a roll after a flushing settlement (`vest_flushed > 0`) holds no vest but what the
    /// record's donation vested anew on a rebase; a full wipe holds none (the donation tops up
    /// the wipe record); a rebase without a flush keeps the snapshot.
    #[test]
    fn a_flushing_roll_zeroes_the_vest() {
        assert_eq!(
            vest_after(Some(7), "EpochRolled", &rolled(0, 7), 0),
            Some(0)
        );
        assert_eq!(
            vest_after(Some(7), "EpochRolled", &rolled(0, 0), 7_000_000),
            Some(0),
            "a full wipe; its donation tops up the wipe record"
        );
        assert_eq!(
            vest_after(Some(0), "EpochRolled", &rolled(1, 7), 0),
            Some(0),
            "a rebase that flushed"
        );
        assert_eq!(
            vest_after(Some(0), "EpochRolled", &rolled(1, 7), 7_000_000),
            Some(7_000_000),
            "a rebase that flushed vests the record's donation anew"
        );
        assert_eq!(
            vest_after(Some(7), "EpochRolled", &rolled(1, 0), 7_000_000),
            Some(7),
            "a rebase without a flush"
        );
        assert_eq!(vest_after(None, "EpochRolled", &rolled(1, 0), 0), None);
    }

    /// R3B-2: the last shares leaving are paid the unreleased vest first, and the program holds
    /// no vest while `S = 0`: a lender event with `s_after = 0` leaves `vest_bal` at 0.
    #[test]
    fn the_last_shares_leaving_zero_the_vest() {
        let ev = |s_after: u128| {
            decoded(&shortwin::events::Withdraw {
                market: Pubkey::new_from_array([1; 32]),
                slot: 1,
                ts: 2,
                owner: Pubkey::new_from_array([4; 32]),
                amount: 10,
                shares: 10_000,
                t_after: 0,
                s_after,
                sol_paid: 3,
                pot_draw: 0,
                lock: 0,
                lock_until: 0,
                epoch: 0,
            })
        };
        assert_eq!(vest_after(Some(9), "Withdraw", &ev(0), 0), Some(0));
        assert_eq!(vest_after(Some(9), "Withdraw", &ev(1), 0), Some(9));
        for kind in ["Deposit", "ClaimSol", "DeficitCovered", "LenderSynced"] {
            assert_eq!(vest_after(Some(9), kind, &ev(0), 0), Some(0), "{kind}");
        }
    }

    #[test]
    fn position_account_terms() {
        let d = json!({"rent_protocol_bps": 1500, "penalty_split": [6000, 2000, 2000], "expiry_fee_bps": 100});
        assert_eq!(terms_of_position_account(&d), [1500, 6000, 2000, 2000, 100]);
    }
}
