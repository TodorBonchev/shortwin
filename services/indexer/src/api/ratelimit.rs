//! Rate and abuse limits (INDEXER.md §10). Owner: API.
//!
//! A GCRA token bucket per client (the peer address, or `X-Forwarded-For`'s last untrusted hop
//! behind `trusted_proxies`; an `X-Api-Key` found in `api_keys` gets its tier's limits), costs
//! per route class (§10.1), `429` with `Retry-After` and the `RateLimit-*` headers, the query
//! semaphore (`max_concurrent_queries`) and the per-statement timeout.
//!
//! - **Client:** an anonymous client is its address (IPv6 by its `/ipv6_bucket_prefix`, /64 by
//!   default: one subscriber's allocation); a client with a known, enabled key is the key (its
//!   SHA-256), whatever its address.
//! - **GCRA** (generic cell rate algorithm): one theoretical arrival time per client; a request
//!   of cost `c` is admitted when it fits within `burst` cells of `1/rps` each. `RateLimit-Limit`
//!   is the burst, `RateLimit-Remaining` the cells left, `RateLimit-Reset` the seconds until the
//!   bucket is full again; a refusal carries `Retry-After` (seconds until the request would fit).
//! - **Keys** are looked up by SHA-256 only (`api_keys.key_sha256`), cached for a minute
//!   (unknown keys too); a cache miss first costs the client's address one anonymous request, so
//!   random keys cannot turn requests into free database lookups — on the rate-limit-exempt
//!   probes a miss is not looked up at all (M8-SEC-9). Known keys and unknown ones are cached
//!   apart: a flood of random keys evicts only other unknown keys, least recently used first,
//!   never a partner's key.
//! - **Route cost** is decided on the decoded query (the handlers' own parser): a
//!   percent-encoded parameter name is the parameter (M8-SEC-10).
//! - **WebSocket connections** are counted per client (§10.1 table) and per process
//!   (`ws_max_conns_total`, M8-SEC-8).

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use sqlx::PgPool;

use super::ApiConfig;

/// `partner` keys' WebSocket connections and every tier's subscriptions per connection, unless
/// the key's row overrides them (§10.1).
pub const PARTNER_WS_CONNS: u32 = 50;
pub const INTERNAL_WS_CONNS: u32 = 500;
pub const INTERNAL_WS_SUBSCRIPTIONS: u32 = 500;
/// The cost of the expensive route classes (§10.1).
pub const HEAVY_COST: u32 = 5;
/// How long a key lookup (found or not) is trusted.
const KEY_CACHE_TTL: Duration = Duration::from_secs(60);
const LIMITER_SHARDS: usize = 16;
const SHARD_SWEEP_AT: usize = 4_096;

/// A client's access tier (`api_keys.tier`; anonymous without a key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Anonymous,
    Partner,
    Internal,
}

/// What the limits are counted against.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ClientKey {
    /// An anonymous client's address (IPv6 by /64).
    Ip(IpAddr),
    /// A key's SHA-256 (hex).
    ApiKey(String),
}

/// The identified client of a request (a request extension set by the API's middleware).
#[derive(Clone, Debug)]
pub struct Client {
    pub key: ClientKey,
    pub ip: IpAddr,
    pub tier: Tier,
    pub rps: u32,
    pub burst: u32,
    pub ws_conns: u32,
    pub ws_subscriptions: u32,
}

impl Client {
    /// The limits of `tier` from the configuration and the key's overrides.
    pub fn new(ip: IpAddr, key: Option<(String, KeyInfo)>, cfg: &ApiConfig) -> Self {
        match key {
            None => Self {
                key: ClientKey::Ip(bucket_ip_prefix(ip, cfg.ipv6_bucket_prefix)),
                ip,
                tier: Tier::Anonymous,
                rps: cfg.anon_rps.max(1),
                burst: cfg.anon_burst.max(1),
                ws_conns: cfg.ws_max_conns_per_ip,
                ws_subscriptions: cfg.ws_max_subscriptions,
            },
            Some((hash, info)) => {
                let internal = info.tier == Tier::Internal;
                Self {
                    key: ClientKey::ApiKey(hash),
                    ip,
                    tier: info.tier,
                    rps: info.rps.unwrap_or(cfg.key_rps).max(1),
                    burst: info.burst.unwrap_or(cfg.key_burst).max(1),
                    ws_conns: info.ws_conns.unwrap_or(if internal {
                        INTERNAL_WS_CONNS
                    } else {
                        PARTNER_WS_CONNS
                    }),
                    ws_subscriptions: if internal {
                        INTERNAL_WS_SUBSCRIPTIONS
                    } else {
                        cfg.ws_max_subscriptions
                    },
                }
            }
        }
    }
}

/// IPv6 addresses bucketed by their /64; IPv4 (and IPv4-mapped IPv6) as is.
pub fn bucket_ip(ip: IpAddr) -> IpAddr {
    bucket_ip_prefix(ip, 64)
}

/// IPv6 addresses bucketed by their `/prefix` (clamped to 32..=128: an operator may widen the
/// bucket to /56 or /48 so one allocation cannot spread over many buckets, M8-SEC-8); IPv4 (and
/// IPv4-mapped IPv6) as is.
pub fn bucket_ip_prefix(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let prefix = u32::from(prefix.clamp(32, 128));
                let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
                IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
            }
        },
    }
}

/// A trusted proxy network (`trusted_proxies`: `a.b.c.d`, `a.b.c.d/n`, IPv6 likewise).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    net: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Option<Self> {
        let (addr, prefix) = match s.trim().split_once('/') {
            Some((a, p)) => (a, Some(p.parse::<u8>().ok()?)),
            None => (s.trim(), None),
        };
        let net: IpAddr = addr.parse().ok()?;
        let max = if net.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(max);
        (prefix <= max).then_some(Self { net, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) if self.net.is_ipv4() => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => return false,
            },
            other => other,
        };
        match (self.net, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(n) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(n) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

/// The client address (§10.1): the TCP peer, or — when the peer is a trusted proxy — the last
/// `X-Forwarded-For` hop that is not one. A malformed hop stops the walk at the hop after it.
pub fn client_ip<'a>(
    peer: Option<IpAddr>,
    forwarded_for: impl Iterator<Item = &'a str>,
    trusted: &[Cidr],
) -> IpAddr {
    let peer = peer.unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    let is_trusted = |ip: IpAddr| trusted.iter().any(|c| c.contains(ip));
    if !is_trusted(peer) {
        return peer;
    }
    let hops: Vec<&str> = forwarded_for
        .flat_map(|h| h.split(','))
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .collect();
    let mut client = peer;
    for hop in hops.iter().rev() {
        let Ok(ip) = hop.parse::<IpAddr>() else {
            break;
        };
        client = ip;
        if !is_trusted(ip) {
            break;
        }
    }
    client
}

/// The query's parameter names, decoded exactly as the handlers decode them
/// (`axum::extract::Query`, `application/x-www-form-urlencoded`); `None` if it does not parse
/// (the handler answers 400 without touching the database).
fn query_names(query: Option<&str>) -> Option<Vec<String>> {
    let Some(q) = query else {
        return Some(Vec::new());
    };
    let uri: axum::http::Uri = format!("/?{q}").parse().ok()?;
    let axum::extract::Query(pairs) =
        axum::extract::Query::<Vec<(String, String)>>::try_from_uri(&uri).ok()?;
    Some(pairs.into_iter().map(|(k, _)| k).collect())
}

/// The cost of a request (§10.1): the expensive route classes cost [`HEAVY_COST`] — candles,
/// the global time series, search, the keepers' aggregate and a market search (`q`, however
/// its name is percent-encoded; M8-SEC-10).
pub fn route_cost(path: &str, query: Option<&str>) -> u32 {
    let has_q = || query_names(query).is_some_and(|names| names.iter().any(|n| n == "q"));
    if path.ends_with("/candles")
        || path == "/v1/stats/timeseries"
        || path == "/v1/search"
        || path == "/v1/keepers"
        || (path == "/v1/markets" && has_q())
    {
        HEAVY_COST
    } else {
        1
    }
}

/// A rate-limit decision (and the `RateLimit-*` header values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub allowed: bool,
    pub limit: u32,
    pub remaining: u32,
    pub reset_s: u64,
    pub retry_after_s: u64,
}

impl Decision {
    /// No limit applies (`internal` keys, §10.1).
    pub fn unlimited(limit: u32) -> Self {
        Self {
            allowed: true,
            limit,
            remaining: limit,
            reset_s: 0,
            retry_after_s: 0,
        }
    }
}

/// The GCRA state of every client, sharded.
pub struct Limiter {
    shards: Box<[Mutex<HashMap<ClientKey, Instant>>]>,
    hasher: std::collections::hash_map::RandomState,
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

fn ceil_s(d: Duration) -> u64 {
    let s = d.as_secs();
    if d.subsec_nanos() > 0 { s + 1 } else { s }
}

impl Limiter {
    pub fn new() -> Self {
        Self {
            shards: (0..LIMITER_SHARDS)
                .map(|_| Mutex::new(HashMap::new()))
                .collect(),
            hasher: std::collections::hash_map::RandomState::new(),
        }
    }

    /// Admits (and records) or refuses a request of `cost` for `key` at `now`.
    pub fn check(
        &self,
        key: &ClientKey,
        rps: u32,
        burst: u32,
        cost: u32,
        now: Instant,
    ) -> Decision {
        let rps = rps.max(1);
        let burst = burst.max(1);
        // A request never costs more than a full bucket (it would never pass otherwise).
        let cost = cost.clamp(1, burst);
        let cell = Duration::from_nanos(1_000_000_000 / u64::from(rps));
        let horizon = cell * burst;
        let shard = usize::try_from(self.hasher.hash_one(key)).unwrap_or(0) % self.shards.len();
        let mut map = self.shards[shard]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tat = map.get(key).copied().unwrap_or(now).max(now);
        let new_tat = tat + cell * cost;
        let ahead = new_tat.duration_since(now);
        let cells_left = |used: Duration| -> u32 {
            let left = horizon.saturating_sub(used);
            u32::try_from(left.as_nanos() / cell.as_nanos().max(1)).unwrap_or(u32::MAX)
        };
        let decision = if ahead > horizon {
            let used = tat.duration_since(now);
            Decision {
                allowed: false,
                limit: burst,
                remaining: cells_left(used),
                reset_s: ceil_s(used),
                retry_after_s: ceil_s(ahead - horizon).max(1),
            }
        } else {
            map.insert(key.clone(), new_tat);
            Decision {
                allowed: true,
                limit: burst,
                remaining: cells_left(ahead),
                reset_s: ceil_s(ahead),
                retry_after_s: 0,
            }
        };
        if map.len() > SHARD_SWEEP_AT {
            map.retain(|_, t| *t > now);
        }
        decision
    }
}

/// An `api_keys` row (§10.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    pub tier: Tier,
    pub rps: Option<u32>,
    pub burst: Option<u32>,
    pub ws_conns: Option<u32>,
}

/// SHA-256 (lowercase hex) of a key as presented.
pub fn key_hash(raw: &str) -> String {
    crate::numeric::hex(&Sha256::digest(raw.as_bytes()))
}

/// `api_keys (tier, rps, burst, ws_conns)`.
type KeyRow = (String, Option<i32>, Option<i32>, Option<i32>);

/// Unknown keys remembered at most (least recently used evicted first).
const UNKNOWN_KEYS_MAX: usize = 10_000;
/// Known keys remembered at most (the `api_keys` table is operator-written and small).
const KNOWN_KEYS_MAX: usize = 10_000;

/// A cached lookup: when it was looked up, when it was last used, and the row (`None`: unknown
/// or disabled).
#[derive(Clone, Debug)]
struct CachedKey {
    at: Instant,
    used: Instant,
    info: Option<KeyInfo>,
}

#[derive(Default)]
struct KeyMaps {
    known: HashMap<String, CachedKey>,
    unknown: HashMap<String, CachedKey>,
}

/// Key lookups, cached: known keys and unknown ones apart (module docs).
#[derive(Default)]
pub struct KeyCache {
    maps: Mutex<KeyMaps>,
}

/// A key as presented: its hash and, when cached and fresh, what the cache knows.
pub enum KeyLookup {
    /// Not a key the API would ever look up (empty or longer than 256 bytes): anonymous.
    Ignored,
    /// Answered by the cache (`None`: unknown or disabled).
    Cached(Option<(String, KeyInfo)>),
    /// Needs a database lookup ([`KeyCache::lookup`]).
    Miss,
}

/// Drops the least recently used tenth of `map` (after the expired entries) when it is full.
fn evict(map: &mut HashMap<String, CachedKey>, max: usize) {
    if map.len() < max {
        return;
    }
    map.retain(|_, c| c.at.elapsed() < KEY_CACHE_TTL);
    if map.len() < max {
        return;
    }
    let mut by_use: Vec<(Instant, String)> = map.iter().map(|(k, c)| (c.used, k.clone())).collect();
    by_use.sort();
    for (_, k) in by_use.into_iter().take(max / 10 + 1) {
        map.remove(&k);
    }
}

impl KeyCache {
    /// What the cache knows about `raw`, without touching the database.
    pub fn peek(&self, raw: &str) -> KeyLookup {
        if raw.is_empty() || raw.len() > 256 {
            return KeyLookup::Ignored;
        }
        let hash = key_hash(raw);
        let mut maps = self
            .maps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let maps = &mut *maps;
        for map in [&mut maps.known, &mut maps.unknown] {
            if let Some(c) = map.get_mut(&hash)
                && c.at.elapsed() < KEY_CACHE_TTL
            {
                c.used = Instant::now();
                return KeyLookup::Cached(c.info.clone().map(|i| (hash, i)));
            }
        }
        KeyLookup::Miss
    }

    /// Entries cached (known, unknown).
    pub fn len(&self) -> (usize, usize) {
        let maps = self
            .maps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (maps.known.len(), maps.unknown.len())
    }

    /// The key's hash and row, when `raw` is a known, enabled key (cached either way).
    pub async fn lookup(&self, pool: &PgPool, raw: &str) -> Option<(String, KeyInfo)> {
        match self.peek(raw) {
            KeyLookup::Ignored => return None,
            KeyLookup::Cached(hit) => return hit,
            KeyLookup::Miss => {}
        }
        let hash = key_hash(raw);
        let row: Result<Option<KeyRow>, _> = sqlx::query_as(
            "SELECT tier, rps, burst, ws_conns FROM api_keys \
                 WHERE key_sha256 = $1 AND NOT disabled",
        )
        .bind(&hash)
        .fetch_optional(pool)
        .await;
        let info = match row {
            // A failed lookup is not cached: the request is served as anonymous.
            Err(e) => {
                tracing::warn!(error = %e, "api key lookup failed");
                return None;
            }
            Ok(r) => r.map(|(tier, rps, burst, ws)| KeyInfo {
                tier: if tier == "internal" {
                    Tier::Internal
                } else {
                    Tier::Partner
                },
                rps: rps.and_then(|v| u32::try_from(v).ok()),
                burst: burst.and_then(|v| u32::try_from(v).ok()),
                ws_conns: ws.and_then(|v| u32::try_from(v).ok()),
            }),
        };
        let mut maps = self
            .maps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let entry = CachedKey {
            at: now,
            used: now,
            info: info.clone(),
        };
        if info.is_some() {
            maps.unknown.remove(&hash);
            evict(&mut maps.known, KNOWN_KEYS_MAX);
            maps.known.insert(hash.clone(), entry);
        } else {
            maps.known.remove(&hash);
            evict(&mut maps.unknown, UNKNOWN_KEYS_MAX);
            maps.unknown.insert(hash.clone(), entry);
        }
        info.map(|i| (hash, i))
    }
}

/// Open WebSocket connections per client and in total.
#[derive(Default)]
struct ConnCounts {
    per: HashMap<ClientKey, u32>,
    total: u32,
}

/// Open WebSocket connections per client and per process.
#[derive(Clone, Default)]
pub struct ConnCounter {
    counts: Arc<Mutex<ConnCounts>>,
}

/// One counted connection (released on drop).
pub struct ConnGuard {
    counts: Arc<Mutex<ConnCounts>>,
    key: ClientKey,
}

/// Why a connection was not counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnRefusal {
    /// The client has its `max` open.
    Client,
    /// The process has `total_max` open (M8-SEC-8).
    Process,
}

impl ConnCounter {
    /// Counts a connection of `key` if it has fewer than `max` and the process fewer than
    /// `total_max`.
    pub fn acquire_within(
        &self,
        key: &ClientKey,
        max: u32,
        total_max: u32,
    ) -> Result<ConnGuard, ConnRefusal> {
        let mut c = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if c.total >= total_max {
            return Err(ConnRefusal::Process);
        }
        let n = c.per.get(key).copied().unwrap_or(0);
        if n >= max {
            return Err(ConnRefusal::Client);
        }
        c.per.insert(key.clone(), n + 1);
        c.total += 1;
        Ok(ConnGuard {
            counts: Arc::clone(&self.counts),
            key: key.clone(),
        })
    }

    /// Counts a connection of `key` if it has fewer than `max` (no process cap).
    pub fn acquire(&self, key: &ClientKey, max: u32) -> Option<ConnGuard> {
        self.acquire_within(key, max, u32::MAX).ok()
    }

    /// Connections open for `key`.
    pub fn count(&self, key: &ClientKey) -> u32 {
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .per
            .get(key)
            .copied()
            .unwrap_or(0)
    }

    /// Connections open in the process.
    pub fn total(&self) -> u32 {
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let mut c = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        c.total = c.total.saturating_sub(1);
        if let Some(n) = c.per.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.per.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn gcra_admits_the_burst_then_refills_at_the_rate() {
        let l = Limiter::new();
        let k = ClientKey::Ip(ip("1.2.3.4"));
        let t0 = Instant::now();
        for left in (0..3).rev() {
            let d = l.check(&k, 1, 3, 1, t0);
            assert!(d.allowed);
            assert_eq!((d.limit, d.remaining), (3, left));
        }
        let d = l.check(&k, 1, 3, 1, t0);
        assert!(!d.allowed);
        assert_eq!(d.retry_after_s, 1);
        assert_eq!(d.reset_s, 3);
        // One cell later one request fits again.
        assert!(l.check(&k, 1, 3, 1, t0 + Duration::from_secs(1)).allowed);
        // Another client is independent.
        assert!(l.check(&ClientKey::Ip(ip("1.2.3.5")), 1, 3, 1, t0).allowed);
    }

    #[test]
    fn heavy_requests_cost_more_but_never_more_than_the_burst() {
        let l = Limiter::new();
        let k = ClientKey::Ip(ip("1.2.3.4"));
        let t0 = Instant::now();
        let d = l.check(&k, 1, 6, HEAVY_COST, t0);
        assert!(d.allowed);
        assert_eq!(d.remaining, 1);
        assert!(!l.check(&k, 1, 6, HEAVY_COST, t0).allowed);
        let k2 = ClientKey::Ip(ip("1.2.3.6"));
        assert!(l.check(&k2, 1, 1, HEAVY_COST, t0).allowed);
        assert_eq!(
            route_cost("/v1/markets", Some("q=bonk&limit=2")),
            HEAVY_COST
        );
        assert_eq!(route_cost("/v1/markets", Some("sort=tvl&qq=1")), 1);
        assert_eq!(route_cost("/v1/markets/x/candles", None), HEAVY_COST);
        // M8-SEC-10: the decoded name decides (`%71` is `q`), as the handler reads it.
        assert_eq!(route_cost("/v1/markets", Some("%71=bonk")), HEAVY_COST);
        assert_eq!(route_cost("/v1/markets", Some("limit=2&%71")), HEAVY_COST);
        assert_eq!(route_cost("/v1/markets", Some("%71%71=x")), 1);
        assert_eq!(route_cost("/v1/keepers", None), HEAVY_COST);
    }

    #[test]
    fn forwarded_for_is_trusted_only_behind_a_trusted_proxy() {
        let trusted = [
            Cidr::parse("10.0.0.0/8").unwrap(),
            Cidr::parse("::1").unwrap(),
        ];
        let xff = ["203.0.113.9, 10.1.1.1"];
        // From a trusted proxy: the last untrusted hop.
        assert_eq!(
            client_ip(Some(ip("10.0.0.2")), xff.iter().copied(), &trusted),
            ip("203.0.113.9")
        );
        // From anyone else: the peer.
        assert_eq!(
            client_ip(Some(ip("198.51.100.1")), xff.iter().copied(), &trusted),
            ip("198.51.100.1")
        );
        // A malformed hop stops the walk.
        assert_eq!(
            client_ip(
                Some(ip("::1")),
                ["junk, 10.2.2.2"].iter().copied(),
                &trusted
            ),
            ip("10.2.2.2")
        );
        assert!(Cidr::parse("10.0.0.0/33").is_none());
        assert!(
            Cidr::parse("::ffff:10.0.0.1")
                .unwrap()
                .contains(ip("::ffff:10.0.0.1"))
        );
        assert!(
            Cidr::parse("10.0.0.0/8")
                .unwrap()
                .contains(ip("::ffff:10.9.9.9"))
        );
        assert_eq!(
            bucket_ip(ip("2001:db8::1")),
            bucket_ip(ip("2001:db8::ffff"))
        );
        // A wider bucket (M8-SEC-8): one /56 is one client.
        assert_eq!(
            bucket_ip_prefix(ip("2001:db8:0:1::1"), 56),
            bucket_ip_prefix(ip("2001:db8:0:ff::1"), 56)
        );
        assert_ne!(
            bucket_ip_prefix(ip("2001:db8:0:1::1"), 64),
            bucket_ip_prefix(ip("2001:db8:0:ff::1"), 64)
        );
        assert_eq!(bucket_ip_prefix(ip("1.2.3.4"), 48), ip("1.2.3.4"));
    }

    #[test]
    fn connections_are_counted_and_released() {
        let c = ConnCounter::default();
        let k = ClientKey::Ip(ip("1.1.1.1"));
        let a = c.acquire(&k, 2).unwrap();
        let _b = c.acquire(&k, 2).unwrap();
        assert!(c.acquire(&k, 2).is_none());
        drop(a);
        assert_eq!(c.count(&k), 1);
        assert!(c.acquire(&k, 2).is_some());
        assert!(c.acquire(&k, 0).is_none());
        // The process cap (M8-SEC-8) holds across clients.
        let c = ConnCounter::default();
        let g1 = c
            .acquire_within(&ClientKey::Ip(ip("1.1.1.1")), 5, 2)
            .unwrap();
        let _g2 = c
            .acquire_within(&ClientKey::Ip(ip("1.1.1.2")), 5, 2)
            .unwrap();
        assert_eq!(
            c.acquire_within(&ClientKey::Ip(ip("1.1.1.3")), 5, 2).err(),
            Some(ConnRefusal::Process)
        );
        drop(g1);
        assert_eq!(c.total(), 1);
        assert!(
            c.acquire_within(&ClientKey::Ip(ip("1.1.1.3")), 5, 2)
                .is_ok()
        );
    }

    #[test]
    fn random_unknown_keys_never_evict_a_known_key() {
        let cache = KeyCache::default();
        let now = Instant::now();
        {
            let mut maps = cache.maps.lock().unwrap();
            maps.known.insert(
                key_hash("partner"),
                CachedKey {
                    at: now,
                    used: now,
                    info: Some(KeyInfo {
                        tier: Tier::Partner,
                        rps: None,
                        burst: None,
                        ws_conns: None,
                    }),
                },
            );
            for i in 0..UNKNOWN_KEYS_MAX + 5 {
                evict(&mut maps.unknown, UNKNOWN_KEYS_MAX);
                maps.unknown.insert(
                    key_hash(&format!("random-{i}")),
                    CachedKey {
                        at: now,
                        used: now,
                        info: None,
                    },
                );
            }
        }
        let (known, unknown) = cache.len();
        assert_eq!(known, 1);
        assert!(unknown <= UNKNOWN_KEYS_MAX);
        assert!(matches!(cache.peek("partner"), KeyLookup::Cached(Some(_))));
    }
}
