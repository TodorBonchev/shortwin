//! Duty 11 (ARCH §7.1, §4.7, §3.9; C1-12; M7 K3): `crank_sol_usd` every ≤ 30 s — the USD tiers read
//! the cached Pyth SOL/USD price, and a cache older than `oracle_stale_s` (600 s since the owner's
//! decision of 2026-10-01, OWNER-M7-15; was 3,600) puts every graduated market in Tier C. The recorded mainnet `PriceUpdateV2` (`tests/fixtures`) is planted at the configured
//! address with fresh publish times:
//!
//! - the keeper cranks the first update at its first tick, then only newer ones — each within
//!   `oracle.interval_s` (30 s) of its publication — and reads nothing but the feed in between;
//! - an update the program's own rules refuse (a step above `oracle_max_step_bps` against a fresh
//!   cache) is not sent: `Alert::OracleRefused` once, the cache kept; an acceptable one after it
//!   lands and clears the alert;
//! - a feed that stops updating: `Alert::OracleAging` once the cache is older than
//!   `aging_alert_s` (300 s; once per stall, hourly while it lasts), and the market's tier falls to
//!   C at the keeper's first crank after `oracle_stale_s` (600 s; the program's rule); a second
//!   stall within the hour is alerted too, before Tier C (M7D-6);
//! - the keeper's crank is a global cost line (`oracle_lamports`), never a market's budget.

use shortwin::constants::PYTH_SOL_USD_ACCOUNT;
use shortwin_venue_state::pyth::{self, PriceUpdate};
use solana_account::Account;

use crate::common::*;

/// The mainnet dump (`tests/fixtures/accounts/pyth_sol_usd_shard0.bin`).
const DUMP: &[u8] = include_bytes!("../../../../tests/fixtures/accounts/pyth_sol_usd_shard0.bin");

pub(crate) fn dump() -> PriceUpdate {
    pyth::read(&pyth::RECEIVER, DUMP).expect("the dump decodes")
}

/// Writes `u` at the configured feed address (the receiver's layout, owner the receiver).
pub(crate) fn plant(env: &mut Env, u: &PriceUpdate) {
    let data = pyth::encode(u).to_vec();
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            PYTH_SOL_USD_ACCOUNT,
            Account {
                lamports,
                data,
                owner: shortwin::constants::PYTH_RECEIVER_ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .expect("feed");
}

/// The dump republished at `publish_time` with `price`.
pub(crate) fn update(publish_time: i64, price: i64) -> PriceUpdate {
    let u = dump();
    PriceUpdate {
        price,
        publish_time,
        prev_publish_time: publish_time - 1,
        ..u
    }
}

fn cranked(r: &TickReport) -> Option<i64> {
    r.oracle
        .as_ref()
        .filter(|(_, x)| x.landed().is_some())
        .map(|(t, _)| *t)
}

/// A mock market deep enough for Tier A (10,000 SOL a side: `Λ` ≈ 20,000 SOL, far above θ_A =
/// $500 k at the recorded price) and listed 8 days ago (age ≥ 7 d), with one position.
fn aged_deep_market() -> (Harness, MockMarket) {
    let mut env = Env::test_venue();
    let mm = env.mock_market(MarketSpec {
        base: 10_000_000_000_000_000,
        quote: 10_000_000_000_000,
        lend: 1_000_000_000_000_000,
        warmup: 8 * 86_400,
        ..MarketSpec::default()
    });
    let h = Harness::new(env, KeeperConfig::default());
    let _ = open(&h, &mm, Q);
    (h, mm)
}

#[test]
fn the_keeper_keeps_the_sol_usd_cache_fresh() {
    let (mut h, mm) = aged_deep_market();
    let key = mm.market();
    let p0 = dump().price;
    let now = h.now();
    h.with_env(|env| plant(env, &update(now - 5, p0)));
    // The harness turns duty 11 off where no feed is planted (`Harness::new`); on here.
    h.keeper.config_mut().oracle.enabled = true;
    let r = h.second().clone();
    assert_eq!(cranked(&r), Some(now - 5), "{:?}", r.oracle);
    let cache = h.with_env(|env| env.oracle_cache());
    assert_eq!((cache.price, cache.publish_time), (p0, now - 5));
    // Nothing newer: reads only.
    let mut sent = 0;
    for _ in 0..90 {
        let r = h.second().clone();
        sent += usize::from(r.oracle.is_some());
    }
    assert_eq!(sent, 0);
    assert!(
        (3..=4).contains(&h.keeper.metrics.oracle_reads),
        "{}",
        h.keeper.metrics.oracle_reads
    );
    // The feed publishes every 55 s (the sponsored heartbeat): each update is cached within 30 s.
    let mut lags = Vec::new();
    let mut next_pub = h.now() + 1;
    let mut published: Vec<i64> = Vec::new();
    for _ in 0..330 {
        if h.now() + 1 >= next_pub {
            let t = h.now() + 1;
            let price = p0 + (published.len() as i64 + 1) * p0 / 1_000;
            h.with_env(|env| plant(env, &update(t, price)));
            published.push(t);
            next_pub = t + 55;
        }
        let r = h.second().clone();
        if let Some(t) = cranked(&r) {
            lags.push(h.now() - t);
        }
    }
    assert_eq!(lags.len(), published.len(), "{lags:?} {published:?}");
    assert!(lags.iter().all(|l| (0..=30).contains(l)), "{lags:?}");
    println!("duty 11: {} updates cached, lags {lags:?} s", lags.len());
    let last = *published.last().unwrap();
    assert_eq!(h.with_env(|env| env.oracle_cache()).publish_time, last);

    // A 50 % step against the fresh cache: refused by the program's rules — not sent, one alert.
    let t = h.now() + 1;
    h.with_env(|env| plant(env, &update(t, p0 * 3 / 2)));
    let mut refused = Vec::new();
    for _ in 0..60 {
        let r = h.second().clone();
        assert!(cranked(&r).is_none());
        refused.extend(r.alerts.iter().filter_map(|a| match a {
            Alert::OracleRefused { reason, .. } => Some(reason.clone()),
            _ => None,
        }));
    }
    assert_eq!(refused, vec!["Step".to_string()]);
    assert_eq!(h.with_env(|env| env.oracle_cache()).publish_time, last);
    // An acceptable update lands.
    let t = h.now() + 1;
    let fine = h.with_env(|env| env.oracle_cache()).price * 1_002 / 1_000;
    h.with_env(|env| plant(env, &update(t, fine)));
    let landed = (0..31).any(|_| cranked(&h.second().clone()).is_some());
    assert!(landed);

    // The feed stops: aging alerted once past `aging_alert_s` (300 s); the tier falls to C at the
    // first crank after `oracle_stale_s` (600 s; OWNER-M7-15).
    let tier = |h: &Harness| h.with_env(|env| env.market(&key).tier);
    let tier_fresh = tier(&h);
    let stale_s = h.with_env(|env| i64::from(env.config().params.oracle_stale_s));
    assert_eq!(stale_s, 600);
    assert_eq!(h.keeper.config().oracle.aging_alert_s, 300);
    let mut aging = Vec::new();
    let mut c_at = None;
    for _ in 0..3_700 {
        let r = h.second().clone();
        aging.extend(r.alerts.iter().filter_map(|a| match a {
            Alert::OracleAging { age_s, .. } => Some(*age_s),
            _ => None,
        }));
        if c_at.is_none() && tier(&h) == shortwin_math::tier::Tier::C as u8 {
            c_at = Some(h.now() - t);
        }
    }
    assert_eq!(aging.len(), 1, "{aging:?}");
    assert!((301..=302).contains(&aging[0]), "{aging:?}");
    let c_at = c_at.expect("Tier C once stale");
    assert!(
        c_at > stale_s && c_at <= stale_s + 31,
        "Tier C {c_at} s after the last update"
    );
    let tier_stale = tier(&h);
    println!(
        "duty 11: tier {tier_fresh} with a fresh cache; aging alerted at {} s, Tier C from {c_at} s \
         (oracle_stale_s {stale_s}), {tier_stale} 3,700 s after the last update",
        aging[0]
    );
    // With the keeper's cache fresh the aged, deep market is Tier A; once the feed stops, the
    // keeper's next crank_price after `oracle_stale_s` re-tiers it C (the program's rule).
    assert_eq!(tier_fresh, shortwin_math::tier::Tier::A as u8);
    assert_eq!(tier_stale, shortwin_math::tier::Tier::C as u8);
    // The oracle's cost is a global line; the market's budget paid only its observations.
    let m = &h.keeper.metrics;
    assert!(m.oracle_cranks >= 8 && m.oracle_lamports >= 5_000 * m.oracle_cranks);
    let spent = h.keeper.track(&key).unwrap().spent_today;
    let cranks_fee = m.markets[&key].cranks * 5_000;
    assert!(spent <= cranks_fee, "{spent} vs {cranks_fee}");
}

/// Duty 11 with the configured feed account missing (a misconfigured `pyth_sol_usd`, or the
/// account closed): `Alert::OracleUnreadable` once, nothing sent, the markets still covered; the
/// feed appearing is cranked at the next read.
#[test]
fn a_missing_feed_alerts_once_and_is_cranked_once_it_appears() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let key = mms[0].market();
    let _ = open(&h, &mms[0], Q);
    h.keeper.config_mut().oracle.enabled = true;
    let mut unreadable = 0;
    for _ in 0..95 {
        let r = h.second().clone();
        assert!(r.oracle.is_none() && r.errors.is_empty(), "{r:?}");
        unreadable += r
            .alerts
            .iter()
            .filter(|a| matches!(a, Alert::OracleUnreadable { detail, .. } if detail == "missing"))
            .count();
    }
    assert_eq!(unreadable, 1);
    assert_eq!(h.keeper.metrics.oracle_reads, 4, "one read every 30 s");
    assert_eq!(h.keeper.metrics.markets[&key].misses, 0);
    let now = h.now();
    h.with_env(|env| plant(env, &update(now, dump().price)));
    let landed = (0..31).find(|_| cranked(&h.second().clone()).is_some());
    assert!(landed.is_some());
    assert_eq!(h.with_env(|env| env.oracle_cache()).publish_time, now);
}

/// ★M7D-6 (decision C keeps the aging alert ahead of Tier C): two feed stalls within the hour.
/// The first, 400 s, is alerted at ≈ 301 s; the feed resumes; the second is alerted again at
/// ≈ 301 s — before the market falls to Tier C at the first crank after 600 s. As reviewed the
/// alert was limited to once an hour across stalls (as when Tier C came after 3,600 s), so the
/// second stall reached Tier C (619 s) with no aging alert.
#[test]
fn a_second_stall_within_the_hour_is_alerted_before_tier_c() {
    let (mut h, mm) = aged_deep_market();
    let key = mm.market();
    let p0 = dump().price;
    let now = h.now();
    h.with_env(|env| plant(env, &update(now - 5, p0)));
    h.keeper.config_mut().oracle.enabled = true;
    h.second();
    let tier = |h: &Harness| h.with_env(|env| env.market(&key).tier);
    let aging_in = |r: &TickReport| {
        r.alerts
            .iter()
            .filter_map(|a| match a {
                Alert::OracleAging { age_s, .. } => Some(*age_s),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    // Stall 1: 400 s, then the feed resumes.
    let mut first = Vec::new();
    for _ in 0..400 {
        first.extend(aging_in(&h.second().clone()));
    }
    let t = h.now() + 1;
    let p = h.with_env(|env| env.oracle_cache()).price;
    h.with_env(|env| plant(env, &update(t, p)));
    for _ in 0..40 {
        h.second();
    }
    assert_eq!(
        h.with_env(|env| env.oracle_cache()).publish_time,
        t,
        "resumed"
    );
    assert_eq!(tier(&h), shortwin_math::tier::Tier::A as u8);
    // Stall 2: from `t`, until Tier C.
    let mut second = Vec::new();
    let mut c_at = None;
    for _ in 0..700 {
        let r = h.second().clone();
        if c_at.is_none() {
            second.extend(aging_in(&r));
        }
        if c_at.is_none() && tier(&h) == shortwin_math::tier::Tier::C as u8 {
            c_at = Some(h.now() - t);
        }
    }
    println!(
        "M7D-6: stall 1 aging alerts at {first:?} s; stall 2 at {second:?} s, Tier C at {c_at:?} s"
    );
    assert_eq!(first.len(), 1);
    assert!((301..=302).contains(&first[0]), "{first:?}");
    let c_at = c_at.expect("Tier C in the second stall");
    assert_eq!(
        second.len(),
        1,
        "the second stall alerted once, before Tier C"
    );
    assert!(
        (301..=302).contains(&second[0]) && second[0] < c_at,
        "{second:?} vs {c_at}"
    );
}
