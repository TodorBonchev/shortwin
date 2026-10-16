//! The WebSocket at `/v1/ws` (INDEXER.md §9.3). Owner: API.
//!
//! JSON text frames: `{"op": "subscribe" | "unsubscribe", "channels": [...], "since": <id>?}`;
//! pushes `{"type": "event", "channel", "id", "data"}`, `{"type": "rollback", "slot"}`,
//! `{"type": "stats", ...}`; heartbeats every 20 s. One `LISTEN shortwin_events` /
//! `shortwin_rollback` connection per process fans out through a broadcast channel; a client
//! whose outbound buffer fills is closed with code 4008.
//!
//! **The hub** (one per process, started by the first connection): a `LISTEN` task wakes a pump
//! task, which also polls every second (a missed notification only delays a push), reads
//! `events WHERE id > last` in id order (the writer is single, so ids commit in order, §3.1) and
//! broadcasts each event with the channels it belongs to; it coalesces `markets` updates to one
//! `MarketSummary` per changed market per second, follows `venue_trades` in chain order for the
//! `trades:<mint>` channels and publishes `GlobalStats` every `ws_stats_interval_s` — the last
//! three only while someone subscribes to them. A rollback notification becomes a `rollback`
//! message to every subscribed connection.
//!
//! **A connection** routes the hub's messages through its subscription set into an outbound
//! queue of `ws_outbound_buffer` messages drained by a writer task; a full queue (or a
//! connection that falls behind the hub) is closed with an `error{lagged}` frame and code 4008.
//! `since` replays up to 1,000 stored events with `id > since` of the newly subscribed event
//! channels before live ones (live events at or below the replayed ones are not sent twice).
//! A replay is a database read like a REST request (M8-SEC-4): it costs the client
//! [`HEAVY_COST`] requests of its GCRA bucket (`error{rate_limited}` when it does not fit), runs
//! under a query permit (`error{unavailable}` after 1 s), and reads per channel through the
//! `(market, id)` / `(position, id)` / `(actor, kind, id)` / `(kind, id)` indexes, at most 1,000
//! ids each — never a scan of `events`, whatever `since` is. Limits (§10.1): connections per
//! client (429 at the upgrade) and per process (`ws_max_conns_total`, 503), subscriptions per
//! connection (`error{too_many_subscriptions}`), inbound messages per second
//! (`error{rate_limited}` once, the excess ignored; twice the limit within a second closes with
//! 1008), frames ≤ 16 KiB.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::error::ApiError;
use super::pagination::ChainKey;
use super::params::canonical_key;
use super::ratelimit::{Client, ConnGuard, ConnRefusal, HEAVY_COST, Tier};
use super::routes::{ADMIN_KINDS, COVERAGE_KINDS, EXIT_KINDS, LENDER_KINDS, Q_GLOBAL_STATS};
use super::{ApiConfig, AppState};
use crate::db::{NOTIFY_EVENTS, NOTIFY_ROLLBACK};

/// Largest inbound frame / message.
const MAX_INBOUND: usize = 16 * 1024;
/// The hub's broadcast ring (a connection further behind than this is lagged).
const HUB_CAPACITY: usize = 8_192;
/// Events replayed by `since`, and events read per hub poll.
const REPLAY_MAX: i64 = 1_000;
/// A send to a client that takes longer than this ends the connection.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// Close codes.
const CLOSE_LAGGED: u16 = 4008;
const CLOSE_POLICY: u16 = 1008;

/// What the hub broadcasts.
#[derive(Debug)]
pub(crate) enum HubMsg {
    /// An event and every channel it belongs to; `data` is the `Event` object's JSON.
    Event {
        id: i64,
        channels: Vec<String>,
        data: Arc<str>,
    },
    /// A `MarketSummary` (the `markets` channel).
    Market { data: Arc<str> },
    /// A `VenueTrade` of `trades:<mint>`.
    Trade { channel: String, data: Arc<str> },
    /// `GlobalStats`.
    Stats { data: Arc<str> },
    /// Rows at or above `slot` that were not finalized were removed.
    Rollback { slot: i64 },
}

/// Subscribers of the channels the hub computes only on demand.
#[derive(Default)]
struct Demand {
    markets: AtomicUsize,
    trades: AtomicUsize,
    stats: AtomicUsize,
}

impl Demand {
    fn counter(&self, channel: &str) -> Option<&AtomicUsize> {
        if channel == "markets" {
            Some(&self.markets)
        } else if channel == "stats" {
            Some(&self.stats)
        } else if channel.starts_with("trades:") {
            Some(&self.trades)
        } else {
            None
        }
    }

    fn add(&self, channel: &str) {
        if let Some(c) = self.counter(channel) {
            c.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn remove(&self, channel: &str) {
        if let Some(c) = self.counter(channel) {
            let _ = c.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_sub(1))
            });
        }
    }
}

/// The process's fan-out (§9.3).
#[derive(Clone)]
pub(crate) struct Hub {
    tx: broadcast::Sender<Arc<HubMsg>>,
    demand: Arc<Demand>,
}

enum Wake {
    Events,
    Rollback(i64),
}

const Q_HUB_EVENTS: &str = concat!(
    "SELECT ",
    ws_event_cols!(),
    " FROM events e LEFT JOIN markets m ON m.market = e.market CROSS JOIN ingest_state st \
     WHERE e.id > $1 ORDER BY e.id LIMIT 1000"
);

/// The `since` replay (module docs): per channel an index range scan of at most `$6` ids
/// (`$2` mints → their markets, `$3` positions, `$4` lenders × `$5` lender kinds, `$7` the kinds
/// of the global channels subscribed), the lowest `$6` of them, then their rows.
const Q_REPLAY: &str = concat!(
    "WITH ids AS (SELECT u.id FROM ( \
        SELECT x.id FROM markets mk CROSS JOIN LATERAL (SELECT e.id FROM events e \
            WHERE e.market = mk.market AND e.id > $1 ORDER BY e.id LIMIT $6) x \
        WHERE mk.mint = ANY($2::text[]) \
        UNION ALL \
        SELECT x.id FROM unnest($3::text[]) p(k) CROSS JOIN LATERAL (SELECT e.id FROM events e \
            WHERE e.position = p.k AND e.id > $1 ORDER BY e.id LIMIT $6) x \
        UNION ALL \
        SELECT x.id FROM unnest($4::text[]) a(k) CROSS JOIN unnest($5::text[]) lk(kind) \
            CROSS JOIN LATERAL (SELECT e.id FROM events e \
            WHERE e.actor = a.k AND e.kind = lk.kind AND e.id > $1 ORDER BY e.id LIMIT $6) x \
        UNION ALL \
        SELECT x.id FROM unnest($7::text[]) gk(kind) CROSS JOIN LATERAL (SELECT e.id FROM events e \
            WHERE e.kind = gk.kind AND e.id > $1 ORDER BY e.id LIMIT $6) x \
     ) u GROUP BY u.id ORDER BY u.id LIMIT $6) \
     SELECT ",
    ws_event_cols!(),
    " FROM ids JOIN events e ON e.id = ids.id LEFT JOIN markets m ON m.market = e.market \
     CROSS JOIN ingest_state st ORDER BY e.id"
);

const Q_HUB_MARKETS: &str = concat!(
    "SELECT (",
    market_summary_json!(),
    ")::text FROM v_market_overview o LEFT JOIN v_sol_usd u ON TRUE \
     WHERE o.market::text = ANY($1::text[])"
);

/// The market's trades only (`market` set: one of its pools, M8-SEC-6), pushed on
/// `trades:<mint>`.
const Q_HUB_TRADES: &str = concat!(
    "SELECT (",
    trade_json!(),
    ")::text, vt.mint::text, vt.market IS NOT NULL, vt.slot, vt.tx_index, vt.ix_index, \
     vt.ev_source, vt.ev_seq \
     FROM venue_trades vt CROSS JOIN ingest_state st \
     WHERE (vt.slot, vt.tx_index, vt.ix_index, vt.ev_source, vt.ev_seq) \
         > ($1, $2::int, $3::smallint, $4::smallint, $5::int) \
     ORDER BY vt.slot, vt.tx_index, vt.ix_index, vt.ev_source, vt.ev_seq LIMIT 1000"
);

/// `(id, Event JSON, mint, position, actor, kind, market)` (`ws_event_cols!`).
type EventRow = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

/// The channels an event is pushed on (§9.3).
pub fn event_channels(
    kind: &str,
    mint: Option<&str>,
    position: Option<&str>,
    actor: Option<&str>,
) -> Vec<String> {
    let mut out = Vec::with_capacity(4);
    if let Some(m) = mint {
        out.push(format!("market:{m}"));
    }
    if let Some(p) = position {
        out.push(format!("position:{p}"));
    }
    if let Some(a) = actor
        && LENDER_KINDS.contains(&kind)
    {
        out.push(format!("lender:{a}"));
    }
    if EXIT_KINDS.contains(&kind) {
        out.push("liquidations".to_string());
    }
    if ADMIN_KINDS.contains(&kind) {
        out.push("admin".to_string());
    }
    if COVERAGE_KINDS.contains(&kind) {
        out.push("coverage".to_string());
    }
    out
}

/// The canonical form of a channel name (`openapi.yaml` `WsChannel`), if it is one.
pub fn parse_channel(s: &str) -> Option<String> {
    match s {
        "markets" | "liquidations" | "admin" | "coverage" | "stats" => Some(s.to_string()),
        _ => {
            let (prefix, key) = s.split_once(':')?;
            if !matches!(prefix, "market" | "position" | "lender" | "trades") {
                return None;
            }
            Some(format!("{prefix}:{}", canonical_key(key)?))
        }
    }
}

impl Hub {
    /// Starts the hub's tasks; they stop when `shutdown`'s sender is dropped.
    pub(crate) async fn start(
        pool: PgPool,
        cfg: Arc<ApiConfig>,
        shutdown: watch::Receiver<()>,
    ) -> Result<Self, ApiError> {
        let last: i64 = sqlx::query_scalar("SELECT COALESCE(max(id), 0) FROM events")
            .fetch_one(&pool)
            .await?;
        let (tx, _) = broadcast::channel(HUB_CAPACITY);
        let demand = Arc::new(Demand::default());
        let (wake_tx, wake_rx) = mpsc::unbounded_channel();
        tokio::spawn(listen(pool.clone(), wake_tx, shutdown.clone()));
        tokio::spawn(pump(Pump {
            pool,
            stats_every: Duration::from_secs(cfg.ws_stats_interval_s.max(1)),
            tx: tx.clone(),
            demand: Arc::clone(&demand),
            last_id: last,
            trade_mark: None,
            pending: HashSet::new(),
            wake: wake_rx,
            shutdown,
        }));
        Ok(Self { tx, demand })
    }
}

/// The `LISTEN` task: one connection, re-established on failure.
async fn listen(
    pool: PgPool,
    wake: mpsc::UnboundedSender<Wake>,
    mut shutdown: watch::Receiver<()>,
) {
    loop {
        let listener = async {
            let mut l = PgListener::connect_with(&pool).await?;
            l.listen_all([NOTIFY_EVENTS, NOTIFY_ROLLBACK]).await?;
            Ok::<_, sqlx::Error>(l)
        };
        let mut l = tokio::select! {
            r = shutdown.changed() => if r.is_err() { return } else { continue },
            l = listener => match l {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(error = %e, "ws hub: LISTEN failed; polling only, retrying");
                    tokio::select! {
                        r = shutdown.changed() => if r.is_err() { return },
                        () = tokio::time::sleep(Duration::from_secs(2)) => {}
                    }
                    continue;
                }
            },
        };
        // Catch up on anything committed before the LISTEN.
        if wake.send(Wake::Events).is_err() {
            return;
        }
        loop {
            tokio::select! {
                r = shutdown.changed() => if r.is_err() { return },
                n = l.recv() => match n {
                    Ok(n) => {
                        let w = if n.channel() == NOTIFY_ROLLBACK {
                            Wake::Rollback(n.payload().trim().parse().unwrap_or(0))
                        } else {
                            Wake::Events
                        };
                        if wake.send(w).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "ws hub: LISTEN connection lost");
                        tokio::select! {
                            r = shutdown.changed() => if r.is_err() { return },
                            () = tokio::time::sleep(Duration::from_secs(1)) => {}
                        }
                        // `PgListener` reconnects on the next `recv`; what was missed is read
                        // from the table.
                        if wake.send(Wake::Events).is_err() {
                            return;
                        }
                    }
                },
            }
        }
    }
}

struct Pump {
    pool: PgPool,
    stats_every: Duration,
    tx: broadcast::Sender<Arc<HubMsg>>,
    demand: Arc<Demand>,
    last_id: i64,
    trade_mark: Option<ChainKey>,
    pending: HashSet<String>,
    wake: mpsc::UnboundedReceiver<Wake>,
    shutdown: watch::Receiver<()>,
}

fn publish(tx: &broadcast::Sender<Arc<HubMsg>>, m: HubMsg) {
    // No receiver is not an error (nobody is connected right now).
    let _ = tx.send(Arc::new(m));
}

async fn pump(mut p: Pump) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stats_tick = tokio::time::interval(p.stats_every);
    stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_flush = Instant::now();
    loop {
        let mut stats_due = false;
        tokio::select! {
            r = p.shutdown.changed() => if r.is_err() { return },
            w = p.wake.recv() => match w {
                None => return,
                Some(Wake::Events) => {}
                Some(Wake::Rollback(slot)) => {
                    publish(&p.tx, HubMsg::Rollback { slot });
                    // Re-ingested trades at or above the slot are pushed again.
                    let floor = ChainKey { slot, tx_index: -1, ix_index: -1, ev_source: -1, ev_seq: -1 };
                    if let Some(m) = p.trade_mark.as_mut()
                        && (m.slot, m.tx_index) >= (floor.slot, floor.tx_index)
                    {
                        *m = floor;
                    }
                }
            },
            _ = tick.tick() => {}
            _ = stats_tick.tick() => stats_due = true,
        }
        if let Err(e) = p.catch_up_events().await {
            tracing::warn!(error = %e, "ws hub: reading events failed");
        }
        if p.demand.trades.load(Ordering::Relaxed) > 0 {
            if let Err(e) = p.catch_up_trades().await {
                tracing::warn!(error = %e, "ws hub: reading trades failed");
            }
        } else {
            p.trade_mark = None;
        }
        if last_flush.elapsed() >= Duration::from_secs(1) {
            last_flush = Instant::now();
            let markets: Vec<String> = p.pending.drain().collect();
            if !markets.is_empty()
                && p.demand.markets.load(Ordering::Relaxed) > 0
                && let Err(e) = p.flush_markets(markets).await
            {
                tracing::warn!(error = %e, "ws hub: reading market summaries failed");
            }
        }
        if stats_due && p.demand.stats.load(Ordering::Relaxed) > 0 {
            match sqlx::query_scalar::<_, Value>(Q_GLOBAL_STATS)
                .fetch_one(&p.pool)
                .await
            {
                Ok(v) => publish(
                    &p.tx,
                    HubMsg::Stats {
                        data: v.to_string().into(),
                    },
                ),
                Err(e) => tracing::warn!(error = %e, "ws hub: reading global stats failed"),
            }
        }
    }
}

impl Pump {
    async fn catch_up_events(&mut self) -> Result<(), sqlx::Error> {
        loop {
            let rows: Vec<EventRow> = sqlx::query_as(Q_HUB_EVENTS)
                .bind(self.last_id)
                .fetch_all(&self.pool)
                .await?;
            let n = rows.len();
            for (id, data, mint, position, actor, kind, market) in rows {
                let channels = event_channels(
                    &kind,
                    mint.as_deref(),
                    position.as_deref(),
                    actor.as_deref(),
                );
                if let Some(m) = market {
                    self.pending.insert(m);
                }
                publish(
                    &self.tx,
                    HubMsg::Event {
                        id,
                        channels,
                        data: data.into(),
                    },
                );
                self.last_id = self.last_id.max(id);
            }
            if i64::try_from(n).unwrap_or(0) < REPLAY_MAX {
                return Ok(());
            }
        }
    }

    async fn catch_up_trades(&mut self) -> Result<(), sqlx::Error> {
        let mut mark = match self.trade_mark {
            Some(m) => m,
            // First subscriber: start from the newest trade.
            None => {
                let newest: Option<(i64, i32, i16, i16, i32)> = sqlx::query_as(
                    "SELECT slot, tx_index, ix_index, ev_source, ev_seq FROM venue_trades \
                     ORDER BY slot DESC, tx_index DESC, ix_index DESC, ev_source DESC, \
                     ev_seq DESC LIMIT 1",
                )
                .fetch_optional(&self.pool)
                .await?;
                let m = newest.map_or(
                    ChainKey {
                        slot: -1,
                        tx_index: -1,
                        ix_index: -1,
                        ev_source: -1,
                        ev_seq: -1,
                    },
                    ChainKey::from,
                );
                self.trade_mark = Some(m);
                return Ok(());
            }
        };
        loop {
            type TradeRow = (String, String, bool, i64, i32, i16, i16, i32);
            let rows: Vec<TradeRow> = sqlx::query_as(Q_HUB_TRADES)
                .bind(mark.slot)
                .bind(mark.tx_index)
                .bind(mark.ix_index)
                .bind(mark.ev_source)
                .bind(mark.ev_seq)
                .fetch_all(&self.pool)
                .await?;
            let n = rows.len();
            for (data, mint, of_market, a, b, c, d, e) in rows {
                if of_market {
                    publish(
                        &self.tx,
                        HubMsg::Trade {
                            channel: format!("trades:{mint}"),
                            data: data.into(),
                        },
                    );
                }
                mark = ChainKey::from((a, b, c, d, e));
            }
            self.trade_mark = Some(mark);
            if i64::try_from(n).unwrap_or(0) < REPLAY_MAX {
                return Ok(());
            }
        }
    }

    async fn flush_markets(&self, markets: Vec<String>) -> Result<(), sqlx::Error> {
        let rows: Vec<String> = sqlx::query_scalar(Q_HUB_MARKETS)
            .bind(&markets)
            .fetch_all(&self.pool)
            .await?;
        for data in rows {
            publish(&self.tx, HubMsg::Market { data: data.into() });
        }
        Ok(())
    }
}

// ---- connections -----------------------------------------------------------------------------

/// `GET /v1/ws`.
pub(crate) async fn upgrade(
    State(st): State<AppState>,
    Extension(client): Extension<Client>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Ok(ws) = ws else {
        return ApiError::bad_request("expected a WebSocket upgrade (`Upgrade: websocket`)")
            .into_response();
    };
    let guard = match st.shared.ws_conns.acquire_within(
        &client.key,
        client.ws_conns,
        st.cfg.ws_max_conns_total,
    ) {
        Ok(g) => g,
        Err(ConnRefusal::Client) => {
            return ApiError::rate_limited(1, "too many WebSocket connections").into_response();
        }
        Err(ConnRefusal::Process) => {
            return ApiError::unavailable("the server's WebSocket capacity is reached")
                .into_response();
        }
    };
    let hub = match st.hub().await {
        Ok(h) => h.clone(),
        Err(e) => return e.into_response(),
    };
    let conn = Conn {
        pool: st.pool.clone(),
        cfg: Arc::clone(&st.cfg),
        st: st.clone(),
        max_subscriptions: usize::try_from(client.ws_subscriptions).unwrap_or(usize::MAX),
        client,
        hub,
        subs: HashSet::new(),
        replayed: HashMap::new(),
    };
    ws.max_message_size(MAX_INBOUND)
        .max_frame_size(MAX_INBOUND)
        .on_upgrade(move |socket| conn.run(socket, guard))
}

/// Why the server ends a connection.
struct Kill {
    code: u16,
    frame: Option<String>,
    /// Send what is already queued first (a policy close); a lagged connection's backlog is
    /// dropped.
    flush: bool,
}

impl Kill {
    fn lagged() -> Self {
        Self {
            code: CLOSE_LAGGED,
            frame: Some(error_msg("lagged", "the outbound buffer is full")),
            flush: false,
        }
    }
}

fn error_msg(code: &str, message: &str) -> String {
    json!({ "type": "error", "code": code, "message": message }).to_string()
}

/// Outbound queue errors.
enum Push {
    /// The queue is full: the client is too slow (4008).
    Full,
    /// The writer is gone (the socket closed).
    Closed,
}

fn push(out: &mpsc::Sender<Message>, s: String) -> Result<(), Push> {
    out.try_send(Message::Text(s.into())).map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => Push::Full,
        mpsc::error::TrySendError::Closed(_) => Push::Closed,
    })
}

/// [`push`] that waits (at most [`SEND_TIMEOUT`]) for room: a `since` replay sends up to
/// [`REPLAY_MAX`] events at once, as many as the whole outbound queue holds, and must not count
/// as a slow client while the writer drains it.
async fn push_wait(out: &mpsc::Sender<Message>, s: String) -> Result<(), Push> {
    match tokio::time::timeout(SEND_TIMEOUT, out.send(Message::Text(s.into()))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(Push::Closed),
        Err(_) => Err(Push::Full),
    }
}

async fn write_loop(
    mut sink: SplitSink<WebSocket, Message>,
    mut rx: mpsc::Receiver<Message>,
    mut kill: oneshot::Receiver<Kill>,
) {
    loop {
        tokio::select! {
            biased;
            k = &mut kill => {
                if let Ok(k) = k {
                    if k.flush {
                        while let Ok(m) = rx.try_recv() {
                            if !matches!(tokio::time::timeout(SEND_TIMEOUT, sink.send(m)).await, Ok(Ok(()))) {
                                return;
                            }
                        }
                    }
                    if let Some(f) = k.frame {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(1),
                            sink.send(Message::Text(f.into())),
                        )
                        .await;
                    }
                    let close = Message::Close(Some(CloseFrame { code: k.code, reason: "".into() }));
                    let _ = tokio::time::timeout(Duration::from_secs(1), sink.send(close)).await;
                } else {
                    let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
                }
                return;
            }
            m = rx.recv() => match m {
                Some(m) => {
                    if !matches!(tokio::time::timeout(SEND_TIMEOUT, sink.send(m)).await, Ok(Ok(()))) {
                        return;
                    }
                }
                None => return,
            },
        }
    }
}

struct Conn {
    pool: PgPool,
    cfg: Arc<ApiConfig>,
    /// The process state: the rate limiter and the query permits a replay takes.
    st: AppState,
    /// Who is connected (the replay is charged to its bucket).
    client: Client,
    hub: Hub,
    max_subscriptions: usize,
    subs: HashSet<String>,
    /// Per event channel: the highest id a `since` replay already sent (live events at or below
    /// it are skipped).
    replayed: HashMap<String, i64>,
}

impl Conn {
    async fn run(mut self, socket: WebSocket, _guard: ConnGuard) {
        let (sink, mut stream) = socket.split();
        let (out_tx, out_rx) = mpsc::channel::<Message>(self.cfg.ws_outbound_buffer.max(1));
        let (kill_tx, kill_rx) = oneshot::channel::<Kill>();
        let writer = tokio::spawn(write_loop(sink, out_rx, kill_rx));
        let mut hub_rx = self.hub.tx.subscribe();
        let hb = Duration::from_secs(self.cfg.ws_heartbeat_s.max(1));
        let mut heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + hb, hb);
        let max_in = self.cfg.ws_max_inbound_per_s.max(1);
        let mut window = (Instant::now(), 0u32);
        let kill: Option<Kill> = loop {
            let r: Result<(), Push> = tokio::select! {
                m = stream.next() => match m {
                    Some(Ok(frame @ (Message::Text(_) | Message::Binary(_)))) => {
                        // Every data frame counts against the inbound limit (§10.1).
                        if window.0.elapsed() >= Duration::from_secs(1) {
                            window = (Instant::now(), 0);
                        }
                        window.1 = window.1.saturating_add(1);
                        if window.1 > max_in.saturating_mul(2) {
                            break Some(Kill {
                                code: CLOSE_POLICY,
                                frame: Some(error_msg("rate_limited", "too many messages")),
                                flush: true,
                            });
                        } else if window.1 == max_in + 1 {
                            push(&out_tx, error_msg(
                                "rate_limited",
                                &format!("at most {max_in} messages per second"),
                            ))
                        } else if window.1 > max_in {
                            Ok(())
                        } else if let Message::Text(t) = frame {
                            self.handle(t.as_str(), &out_tx).await
                        } else {
                            push(&out_tx, error_msg("bad_message", "text frames only"))
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => Ok(()),
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break None,
                },
                r = hub_rx.recv() => match r {
                    Ok(msg) => self.route(&msg, &out_tx),
                    Err(RecvError::Lagged(_)) => Err(Push::Full),
                    Err(RecvError::Closed) => break None,
                },
                _ = heartbeat.tick() => push(&out_tx, json!({ "type": "heartbeat" }).to_string()),
            };
            match r {
                Ok(()) => {}
                Err(Push::Full) => break Some(Kill::lagged()),
                Err(Push::Closed) => break None,
            }
        };
        for c in &self.subs {
            self.hub.demand.remove(c);
        }
        match kill {
            Some(k) => {
                let _ = kill_tx.send(k);
            }
            None => drop(kill_tx),
        }
        drop(out_tx);
        let _ = tokio::time::timeout(Duration::from_secs(3), writer).await;
    }

    /// One client message.
    async fn handle(&mut self, text: &str, out: &mpsc::Sender<Message>) -> Result<(), Push> {
        let bad = |m: &str| push(out, error_msg("bad_message", m));
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return bad("not JSON");
        };
        let op = v.get("op").and_then(Value::as_str);
        match op {
            Some("ping") => push(out, json!({ "type": "pong" }).to_string()),
            Some(op @ ("subscribe" | "unsubscribe")) => {
                let Some(list) = v.get("channels").and_then(Value::as_array) else {
                    return bad("`channels` must be an array");
                };
                if list.is_empty() || list.len() > 50 {
                    return bad("`channels` must have 1 to 50 entries");
                }
                let mut channels: Vec<String> = Vec::with_capacity(list.len());
                for c in list {
                    match c.as_str().and_then(parse_channel) {
                        Some(c) if !channels.contains(&c) => channels.push(c),
                        Some(_) => {}
                        None => return bad("unknown channel (see WsChannel in openapi.yaml)"),
                    }
                }
                let since = match v.get("since") {
                    None | Some(Value::Null) => None,
                    Some(s) => match s.as_i64().filter(|s| *s >= 0) {
                        Some(s) => Some(s),
                        None => return bad("`since` must be a non-negative integer"),
                    },
                };
                if op == "unsubscribe" {
                    for c in &channels {
                        if self.subs.remove(c) {
                            self.hub.demand.remove(c);
                            self.replayed.remove(c);
                        }
                    }
                    return push(
                        out,
                        json!({ "type": "unsubscribed", "channels": channels }).to_string(),
                    );
                }
                let new: Vec<String> = channels
                    .iter()
                    .filter(|c| !self.subs.contains(*c))
                    .cloned()
                    .collect();
                if self.subs.len() + new.len() > self.max_subscriptions {
                    return push(
                        out,
                        error_msg(
                            "too_many_subscriptions",
                            &format!("at most {} subscriptions", self.max_subscriptions),
                        ),
                    );
                }
                for c in &new {
                    self.subs.insert(c.clone());
                    self.hub.demand.add(c);
                }
                push(
                    out,
                    json!({ "type": "subscribed", "channels": channels }).to_string(),
                )?;
                match since {
                    Some(since) => self.replay(since, &new, out).await,
                    None => Ok(()),
                }
            }
            _ => bad("`op` must be subscribe, unsubscribe or ping"),
        }
    }

    /// `since`: the stored events of the newly subscribed event channels with `id > since`.
    async fn replay(
        &mut self,
        since: i64,
        new: &[String],
        out: &mpsc::Sender<Message>,
    ) -> Result<(), Push> {
        let mut mints = Vec::new();
        let mut positions = Vec::new();
        let mut lenders = Vec::new();
        let (mut liq, mut admin, mut cov) = (false, false, false);
        for c in new {
            match c.split_once(':') {
                Some(("market", k)) => mints.push(k.to_string()),
                Some(("position", k)) => positions.push(k.to_string()),
                Some(("lender", k)) => lenders.push(k.to_string()),
                Some(_) => {}
                None => match c.as_str() {
                    "liquidations" => liq = true,
                    "admin" => admin = true,
                    "coverage" => cov = true,
                    _ => {}
                },
            }
        }
        let event_channels_new: Vec<&String> = new
            .iter()
            .filter(|c| {
                !(c.as_str() == "markets" || c.as_str() == "stats" || c.starts_with("trades:"))
            })
            .collect();
        if event_channels_new.is_empty() {
            return Ok(());
        }
        // A replay is a database read (M8-SEC-4): it costs the client a heavy request and waits
        // for a query permit like any REST request.
        if self.client.tier != Tier::Internal {
            let d = self.st.shared.limiter.check(
                &self.client.key,
                self.client.rps,
                self.client.burst,
                HEAVY_COST,
                Instant::now(),
            );
            if !d.allowed {
                return push(
                    out,
                    error_msg(
                        "rate_limited",
                        &format!(
                            "a replay costs {HEAVY_COST} requests: retry in {} s or refetch \
                             through REST",
                            d.retry_after_s
                        ),
                    ),
                );
            }
        }
        let Some(permit) = self.st.permit(Duration::from_secs(1)).await else {
            return push(
                out,
                error_msg(
                    "unavailable",
                    "replay unavailable now; refetch through REST",
                ),
            );
        };
        let kinds = |k: &[&str]| k.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut global_kinds: Vec<String> = Vec::new();
        for (on, list) in [
            (liq, &EXIT_KINDS[..]),
            (admin, &ADMIN_KINDS[..]),
            (cov, &COVERAGE_KINDS[..]),
        ] {
            if on {
                global_kinds.extend(kinds(list));
            }
        }
        global_kinds.sort();
        global_kinds.dedup();
        let rows: Result<Vec<EventRow>, sqlx::Error> = sqlx::query_as(Q_REPLAY)
            .bind(since)
            .bind(&mints)
            .bind(&positions)
            .bind(&lenders)
            .bind(kinds(&LENDER_KINDS))
            .bind(REPLAY_MAX)
            .bind(&global_kinds)
            .fetch_all(&self.pool)
            .await;
        drop(permit);
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "ws: replay failed");
                return push(
                    out,
                    error_msg(
                        "unavailable",
                        "replay unavailable now; refetch through REST",
                    ),
                );
            }
        };
        let mut high = since;
        for (id, data, mint, position, actor, kind, _) in rows {
            high = high.max(id);
            for ch in event_channels(
                &kind,
                mint.as_deref(),
                position.as_deref(),
                actor.as_deref(),
            ) {
                if event_channels_new.iter().any(|c| **c == ch) {
                    push_wait(out, event_msg(&ch, id, &data)).await?;
                }
            }
        }
        for c in event_channels_new {
            self.replayed.insert(c.clone(), high);
        }
        Ok(())
    }

    /// One hub message through the subscriptions.
    fn route(&self, msg: &HubMsg, out: &mpsc::Sender<Message>) -> Result<(), Push> {
        match msg {
            HubMsg::Event { id, channels, data } => {
                for ch in channels {
                    if self.subs.contains(ch) && self.replayed.get(ch).is_none_or(|high| id > high)
                    {
                        push(out, event_msg(ch, *id, data))?;
                    }
                }
                Ok(())
            }
            HubMsg::Market { data } if self.subs.contains("markets") => {
                push(out, typed_msg("market", "markets", data))
            }
            HubMsg::Trade { channel, data } if self.subs.contains(channel) => {
                push(out, typed_msg("trade", channel, data))
            }
            HubMsg::Stats { data } if self.subs.contains("stats") => {
                push(out, typed_msg("stats", "stats", data))
            }
            HubMsg::Rollback { slot } if !self.subs.is_empty() => {
                push(out, json!({ "type": "rollback", "slot": slot }).to_string())
            }
            _ => Ok(()),
        }
    }
}

fn json_str(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

fn event_msg(channel: &str, id: i64, data: &str) -> String {
    format!(
        r#"{{"type":"event","channel":{},"id":{id},"data":{data}}}"#,
        json_str(channel)
    )
}

fn typed_msg(ty: &str, channel: &str, data: &str) -> String {
    format!(
        r#"{{"type":"{ty}","channel":{},"data":{data}}}"#,
        json_str(channel)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_are_canonical_and_events_route_by_kind() {
        let k = solana_pubkey::Pubkey::new_from_array([5; 32]).to_string();
        assert_eq!(parse_channel("markets").as_deref(), Some("markets"));
        assert_eq!(
            parse_channel(&format!("market:{k}")),
            Some(format!("market:{k}"))
        );
        assert_eq!(parse_channel("market:nope"), None);
        assert_eq!(parse_channel(&format!("pool:{k}")), None);
        assert_eq!(parse_channel("market"), None);
        let ch = event_channels("Liquidate", Some(&k), Some(&k), Some(&k));
        assert_eq!(
            ch,
            vec![
                format!("market:{k}"),
                format!("position:{k}"),
                "liquidations".to_string()
            ]
        );
        let ch = event_channels("Deposit", None, None, Some(&k));
        assert_eq!(ch, vec![format!("lender:{k}")]);
        let ch = event_channels("CoveragePauseSet", None, None, Some(&k));
        assert_eq!(ch, vec!["admin".to_string(), "coverage".to_string()]);
    }

    #[test]
    fn messages_are_json() {
        let m: Value = serde_json::from_str(&event_msg("market:x\"y", 7, r#"{"a":1}"#)).unwrap();
        assert_eq!(m["channel"], "market:x\"y");
        assert_eq!(m["data"]["a"], 1);
        let m: Value = serde_json::from_str(&typed_msg("stats", "stats", "{}")).unwrap();
        assert_eq!(m["type"], "stats");
    }
}
