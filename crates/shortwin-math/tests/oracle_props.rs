//! Properties of `observe`, `persist` and `bounds` (ARCHITECTURE §8.1: EMA step bounds and
//! "own observation never read" (I-E7), directional aggregation, the ring's bucket
//! extremes and medians, the venue-change rewrite, the liquidation EMA leg (I-E8), exit
//! references and `max_in` incl. the rent-and-fee seniority of an expiry (I-E2), the
//! liquidation liveness invariant I-E11, M4R's post-change warm-up seed P1–P4, and M4F-1's
//! depth cap on the reference liquidity).
#![allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]

mod common;

use num_bigint::BigUint;
use proptest::prelude::*;
use shortwin_math::bounds::{
    ExitParams, expiry_reference, kappa_eff, liq_reference, max_in_exp, max_in_liq, pi_ref,
    ref_depth_cap, ref_liquidity,
};
use shortwin_math::consts::EMA_TAU_S;
use shortwin_math::consts::EMA_WARMUP_BUCKETS;
use shortwin_math::observe::{
    Bucket, EmaParams, LastBucket, Observation, RING_LEN, Ring, RingEntry, VenueChange,
    ring_liq_lower_median, ring_medians, robust_spot, sustained_growth,
};
use shortwin_math::persist::liq_ema_leg;
use shortwin_math::quote::reprice;

fn big(x: u128) -> BigUint {
    BigUint::from(x)
}

/// Any in-bound EMA parameters (ARCH §2.3.3), and some beyond them (Δt_max to 10, C from
/// 10 %): `integrate` does not validate.
fn ema_params() -> impl Strategy<Value = EmaParams> {
    (1u16..=10, 1_000u32..=10_000, 600u32..=86_400, 1u32..=600)
        .prop_map(|(d, c, st, sd)| EmaParams::new(d, c, st, sd))
}

/// Prices across the whole range, weighted towards realistic WAD prices.
fn price() -> impl Strategy<Value = u128> {
    prop_oneof![
        4 => 1_000u128..10u128.pow(30),
        1 => 1u128..=u128::MAX,
        1 => Just(u128::MAX),
        1 => 1u128..1_000,
    ]
}

/// One integration of a single-second bucket `[lo, hi]` after `gap` seconds.
fn step(p: u128, lo: u128, hi: u128, gap: i64, prm: &EmaParams) -> Observation {
    let mut o = Observation::new(0, p, 1).unwrap();
    o.record(gap - 1, lo, 1).unwrap();
    o.record(gap - 1, hi, 1).unwrap();
    assert_eq!(o.roll(gap, prm).unwrap(), Some(gap));
    o
}

fn entry(ts: i64, p: u128, lo: u128, hi: u128, llo: u128, lhi: u128) -> RingEntry {
    RingEntry::reprojected(ts, p, 1_000_000, lo, hi, llo, lhi)
}

/// One sample of the candidate pool in a warm-up second (M4R, §4.1 properties): the honest
/// `(p*, H)`, or an instruction wrapped by an unlocked LP's withdrawal (`L = D`), a deposit of
/// `k` times the pool (`L = (1 + k)·H`), or a pump / dump of the price by `m/10⁴`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    Honest,
    Withdraw,
    Deposit(u8),
    Pump(u32),
    Dump(u32),
}

fn sample() -> impl Strategy<Value = Sample> {
    prop_oneof![
        1 => Just(Sample::Honest),
        2 => Just(Sample::Withdraw),
        1 => (1u8..=8).prop_map(Sample::Deposit),
        1 => (10_001u32..=60_000).prop_map(Sample::Pump),
        1 => (10_001u32..=60_000).prop_map(Sample::Dump),
    ]
}

/// A warm-up second: whether an honest observer (the keeper) recorded, and the attacker's
/// samples. A second with neither is observed by the keeper alone.
fn second() -> impl Strategy<Value = (bool, Vec<Sample>)> {
    (any::<bool>(), prop::collection::vec(sample(), 0..3))
}

/// `(P, L)` of `s` on a pool of honest price `p`, depth `h` and locked depth `d`.
fn value(s: Sample, p: u128, h: u128, d: u128) -> (u128, u128) {
    match s {
        Sample::Honest => (p, h),
        Sample::Withdraw => (p, d),
        Sample::Deposit(k) => (p, h * (1 + u128::from(k))),
        Sample::Pump(m) => (p * u128::from(m) / 10_000, h),
        Sample::Dump(m) => ((p * 10_000 / u128::from(m)).max(1), h),
    }
}

/// A market whose `P_ema` converged at `p_pre` adopted onto the candidate at second 100
/// (anchor `cap`, `L_slow ≤ D`, marked), then the warm-up seconds `secs` observed; returns the
/// state after each integration.
fn warm_up(
    p: u128,
    p_pre: u128,
    h: u128,
    d: u128,
    cap: u128,
    c_bps: u32,
    secs: &[(bool, Vec<Sample>)],
) -> Vec<Observation> {
    let prm = EmaParams::new(2, c_bps, 3_600, 60);
    let mut o = Observation::new(0, p_pre, 7 * h).unwrap();
    for t in 1..6i64 {
        o.record(t, p_pre, 5 * h).unwrap();
        o.roll(t + 1, &prm).unwrap();
    }
    o.rewrite_liquidity(cap, VenueChange::RaydiumLocked { slow_cap: d })
        .unwrap();
    o.mark_restart();
    let mut out = Vec::new();
    for (i, (honest, att)) in secs.iter().enumerate() {
        let t = 100 + i as i64;
        if *honest || att.is_empty() {
            o.record(t, p, h).unwrap();
        }
        for &s in att {
            let (sp, sl) = value(s, p, h, d);
            o.record(t, sp, sl).unwrap();
        }
        o.roll(t + 1, &prm).unwrap();
        out.push(o);
    }
    out
}

/// The largest v0.4.1 price step (ECON §5.2, τ = 5; the observation at `+C`), recomputed:
/// `⌊(P·τ + ⌊P·(10⁴ + C)/10⁴⌋·dt)/(τ + dt)⌋`.
fn up(pe: u128, dt: u128, c_bps: u32) -> u128 {
    (pe * 5 + pe * (10_000 + u128::from(c_bps)) / 10_000 * dt) / (5 + dt)
}

/// The largest v0.4.1 price fall (the observation at the lower clamp), recomputed (M6-SEC-1):
/// `⌊(P·τ + ⌊P·10⁴/(10⁴ + C)⌋·dt)/(τ + dt)⌋`.
fn down(pe: u128, dt: u128, c_bps: u32) -> u128 {
    (pe * 5 + pe * 10_000 / (10_000 + u128::from(c_bps)) * dt) / (5 + dt)
}

/// The §4.1 seed over bucket extremes `(lo, lhi)`, the price clamped to `[lower, bound]` (the
/// largest v0.4.1 fall and step from the previous `P_ema`, M6-SEC-1 and M4X-1).
fn seed_of(buckets: &[(u128, u128)], c_bps: u32, lower: u128, bound: u128) -> (u128, u128) {
    let p = buckets
        .iter()
        .map(|b| b.0)
        .min()
        .unwrap()
        .min(bound)
        .max(lower);
    let mut hs: Vec<u128> = buckets.iter().map(|b| b.1).collect();
    hs.sort_unstable();
    let lomed = hs[(hs.len() - 1) / 2];
    (
        p,
        (*hs.last().unwrap()).min(lomed * (10_000 + u128::from(c_bps)) / 10_000),
    )
}

proptest! {
    #![proptest_config(common::proptest_config())]

    /// ECON §5.2 step bounds: one integration moves P_ema at most to
    /// (τ + dt·(1 + C))/(τ + dt) and at least to (τ + dt/(1 + C))/(τ + dt), up to the floors;
    /// the ring entry keeps the bucket's extremes and caches reprice(L_ema, P_ema).
    #[test]
    fn ema_step_is_clamped(p in price(), a in price(), b in price(), gap in 1i64..30, prm in ema_params()) {
        let (lo, hi) = (a.min(b), a.max(b));
        let o = step(p, lo, hi, gap, &prm);
        let new = o.ema.price;
        let (tau, c) = (u128::from(EMA_TAU_S), u128::from(prm.clamp_bps));
        let dt = u128::try_from(gap).unwrap().min(u128::from(prm.dtmax_s));
        let w = big(tau + dt);
        // Upper: new·(τ+dt)·10⁴ ≤ P·(τ·10⁴ + dt·(10⁴ + C)).
        prop_assert!(big(new) * &w * big(10_000) <= big(p) * big(tau * 10_000 + dt * (10_000 + c)));
        // Lower: (new + 1)·(τ+dt)·(10⁴+C) > P·(τ·(10⁴+C) + dt·10⁴) − dt·(10⁴+C).
        let lhs = (big(new) + 1u8) * &w * big(10_000 + c) + big(dt * (10_000 + c));
        prop_assert!(lhs > big(p) * big(tau * (10_000 + c) + dt * 10_000));
        // The new EMA lies between the old one and the bucket.
        prop_assert!(new >= p.min(lo) && new <= p.max(hi));
        let e = o.ring.newest().unwrap();
        prop_assert_eq!((e.lo, e.hi, e.llo, e.lhi), (lo, hi, 1, 1));
        prop_assert_eq!((e.x, e.y), reprice(e.l, e.p).unwrap_or((0, 0)));
    }

    /// Directional aggregation (ECON §5.2): a bucket with observations on both sides of the
    /// EMA leaves it exactly where it was (+0.00 % with one honest observation), while the
    /// ring still records the second's honest extremes (the "frozen" second of R4B-2).
    #[test]
    fn mixed_bucket_freezes_ema(p in price(), below in 0u128..=10_000, above in 0u128..=10_000,
                                gap in 1i64..10, prm in ema_params()) {
        let lo = (p - p / 10_001 * below).max(1);
        let hi = p.saturating_add(p / 10_001 * above);
        let o = step(p, lo, hi, gap, &prm);
        prop_assert_eq!(o.ema.price, p);
        let e = o.ring.newest().unwrap();
        prop_assert_eq!((e.p, e.lo, e.hi), (p, lo, hi));
    }

    /// The new EMA is monotone in every observation and in the old EMA.
    #[test]
    fn ema_is_monotone(p in price(), a in price(), b in price(), up in price(), gap in 1i64..10,
                       prm in ema_params()) {
        let (lo, hi) = (a.min(b), a.max(b));
        let base = step(p, lo, hi, gap, &prm).ema.price;
        let hi2 = hi.saturating_add(up);
        prop_assert!(step(p, lo, hi2, gap, &prm).ema.price >= base);
        prop_assert!(step(p, lo.saturating_add(up).min(hi), hi, gap, &prm).ema.price >= base);
        prop_assert!(step(p.saturating_add(up), lo, hi, gap, &prm).ema.price >= base);
    }

    /// At most one integration per second, only of completed seconds, and an instruction
    /// never reads its own observation (ECON §5.2 one-second lag; I-E7). The pre-roll gap is
    /// `now − ema_ts`. Two markets that differ only in the observations of the current second
    /// have identical EMA state.
    #[test]
    fn one_update_per_second(p in price(),
                             seconds in prop::collection::vec((0i64..3, prop::collection::vec(price(), 1..4)), 1..40),
                             extra in prop::collection::vec(price(), 1..4), prm in ema_params()) {
        let mut o = Observation::new(0, p, 7).unwrap();
        let mut now = 0i64;
        let mut distinct = 0u64;
        for (dt, obs) in &seconds {
            let before = o.ema;
            now += dt;
            let rolled = o.roll(now, &prm).unwrap();
            prop_assert!(o.roll(now, &prm).unwrap().is_none(), "a second roll in the same second");
            if let Some(gap) = rolled {
                prop_assert_eq!(gap, now - before.ts);
                prop_assert_eq!(o.ema.count, before.count + 1);
                prop_assert_eq!(o.ring.newest().unwrap().ts, now);
                prop_assert_eq!(o.last.sec, now - dt);
            }
            let open_second = o.bucket.is_open().then_some(o.bucket.sec);
            prop_assert!(open_second.is_none_or(|s| s == now));
            if !(o.bucket.is_open() && o.bucket.sec == now) {
                distinct += 1;
            }
            let frozen = o.ema;
            for &x in obs {
                o.record(now, x, 7).unwrap();
            }
            prop_assert_eq!(o.ema, frozen, "an observation is read in its own second");
        }
        // Rolled exactly once per distinct observed second except the still-open one.
        prop_assert_eq!(o.ema.count + 1, distinct);
        let mut twin = o;
        for &x in &extra {
            twin.record(now, x, 7).unwrap();
        }
        prop_assert_eq!(twin.ema, o.ema);
        prop_assert_eq!(twin.ring, o.ring);
        // Ring: bounded, strictly increasing timestamps, newest = EMA state.
        prop_assert!(o.ring.len() <= RING_LEN);
        let ts: Vec<i64> = o.ring.iter().map(|e| e.ts).collect();
        prop_assert!(ts.windows(2).all(|w| w[0] > w[1]));
        let newest = o.ring.newest().unwrap();
        prop_assert_eq!((newest.p, newest.l), (o.ema.price, o.ema.liq));
    }

    /// Robust spot (ECON §5.4): never above the live spot or the last bucket low, and
    /// defined exactly while the completed bucket is fresh.
    #[test]
    fn robust_spot_is_conservative(live in price(), lo in price(), sec in -100i64..100, now in -100i64..200,
                                   age in 0u8..6) {
        let last = LastBucket { lo, hi: lo, sec, _pad: [0; 8] };
        match robust_spot(live, &last, now, age) {
            Some(p) => {
                prop_assert!(p <= live && p <= lo && (p == live || p == lo));
                prop_assert!(now - sec <= i64::from(age));
            }
            None => prop_assert!(now - sec > i64::from(age)),
        }
    }

    /// Sustained growth (ECON §5.4; AIR-22): 0 unless the last n steps are in time order; then
    /// the lower median of their rates, a flat step's rate 0 — so 0 iff more than ⌊(n − 1)/2⌋ of
    /// them are flat (2 of 3, 2 of 4), and one flat step among 3 or more never zeroes it.
    #[test]
    fn sustained_growth_is_the_lower_median_step(p0 in 1_000u128..10u128.pow(30),
                                                 steps in prop::collection::vec((0u128..3_000, 0i64..4), 0..8),
                                                 n in 0u8..9) {
        let mut ring = Ring::new(0, p0, 1).unwrap();
        let (mut p, mut ts) = (p0, 0i64);
        for &(bps, dt) in &steps {
            p += p * bps / 10_000;
            ts += dt;
            ring.push(entry(ts, p, p, p, 1, 1)).unwrap();
        }
        let g = sustained_growth(&ring, n).unwrap();
        let ents: Vec<RingEntry> = ring.iter().copied().collect();
        let n = usize::from(n);
        if n == 0 || ents.len() <= n || (0..n).any(|k| ents[k].ts <= ents[k + 1].ts) {
            prop_assert_eq!(g, 0);
        } else {
            let mut rates: Vec<u128> = (0..n)
                .map(|k| {
                    let (new, old) = (ents[k], ents[k + 1]);
                    if new.p <= old.p {
                        0
                    } else {
                        (new.p - old.p) * 10_000 / old.p / u128::try_from(new.ts - old.ts).unwrap()
                    }
                })
                .collect();
            rates.sort_unstable();
            prop_assert_eq!(u128::from(g), rates[(n - 1) / 2]);
            let zero = rates.iter().filter(|&&r| r == 0).count();
            prop_assert_eq!(g == 0, zero > (n - 1) / 2);
            if n >= 3 && zero == 1 {
                prop_assert!(g > 0);
            }
        }
    }

    /// Ring medians (ECON §2.1, §5.6): the lower median of the lows and the upper median of
    /// the highs are elements of their columns with the right ranks, and no set of at most
    /// ⌊(n − 1)/2⌋ entries (4 of 8) can raise P_mlo above the other entries' lows or lower
    /// P_mhi below their highs, whatever it records (R4B-2, R4A-9: "5 of 8").
    #[test]
    fn medians_need_five_of_eight(cols in prop::collection::vec((1u128..1_000, 1u128..1_000, 1u128..1_000, 1u128..1_000), 1..=8),
                                  attacker in prop::collection::vec((any::<bool>(), 1u128..10_000), 8)) {
        let mut ring = Ring::default();
        for (i, &(a, b, c, d)) in cols.iter().enumerate() {
            ring.push(entry(i as i64, 1, a, b, c, d)).unwrap();
        }
        let n = ring.len();
        let (pl, ph, ll, lh) = ring_medians(&ring).unwrap();
        let col = |f: fn(&RingEntry) -> u128| { let mut v: Vec<u128> = ring.iter().map(f).collect(); v.sort(); v };
        prop_assert_eq!(pl, col(|e| e.lo)[(n - 1) / 2]);
        prop_assert_eq!(ph, col(|e| e.hi)[n / 2]);
        prop_assert_eq!(ll, col(|e| e.llo)[(n - 1) / 2]);
        prop_assert_eq!(lh, col(|e| e.lhi)[n / 2]);
        // An attacker controls at most ⌊(n − 1)/2⌋ entries.
        let limit = (n - 1) / 2;
        let chosen: Vec<usize> = (0..n).filter(|&k| attacker[k].0).take(limit).collect();
        let mut pushed = ring;
        for (k, e) in pushed.entries.iter_mut().enumerate() {
            if chosen.contains(&k) {
                e.lo += attacker[k].1;
                e.hi = e.hi.saturating_sub(attacker[k].1).max(1);
            }
        }
        let (pl2, ph2, _, _) = ring_medians(&pushed).unwrap();
        let honest: Vec<&RingEntry> = pushed.entries.iter().take(n).enumerate()
            .filter(|(k, _)| !chosen.contains(k)).map(|(_, e)| e).collect();
        prop_assert!(pl2 <= honest.iter().map(|e| e.lo).max().unwrap());
        prop_assert!(ph2 >= honest.iter().map(|e| e.hi).min().unwrap());
    }

    /// Venue changes (ECON §5.2; ARCH §4.4, A72): every entry's l, llo, lhi become the reset
    /// L_ema with (x, y) re-cached, prices and price extremes carry over, the bucket clears;
    /// v0.4.1's Raydium change never raises L_ema or L_slow, a graduation sets both; the
    /// program's Raydium change (M4-SEC-5/-6) sets L_ema and lowers L_slow to at most the cap.
    #[test]
    fn rewrite_liquidity_resets_the_ring(p in 1_000u128..10u128.pow(24), l in 1u128..10u128.pow(18),
                                         slow in 1u128..10u128.pow(18), l_new in 1u128..10u128.pow(18),
                                         prices in prop::collection::vec((1_000u128..10u128.pow(24), 1u128..10u128.pow(18)), 0..8),
                                         which in 0u8..3, cap in 1u128..10u128.pow(18)) {
        let mut o = Observation::new(0, p, l).unwrap();
        o.ema.liq_slow = slow;
        for (i, &(q, lq)) in prices.iter().enumerate() {
            o.ring.push(RingEntry::reprojected(i as i64 + 1, q, lq, q / 2, q * 2, lq / 2, lq * 2)).unwrap();
        }
        o.record(9, p, l).unwrap();
        let before = o;
        let kind = match which {
            0 => VenueChange::Raydium,
            1 => VenueChange::RaydiumLocked { slow_cap: cap },
            _ => VenueChange::Graduation,
        };
        o.rewrite_liquidity(l_new, kind).unwrap();
        match kind {
            VenueChange::Raydium => {
                prop_assert_eq!((o.ema.liq, o.ema.liq_slow), (l_new.min(l), l_new.min(slow)));
            }
            VenueChange::RaydiumLocked { .. } => {
                prop_assert_eq!((o.ema.liq, o.ema.liq_slow), (l_new, slow.min(cap)));
            }
            VenueChange::Graduation => {
                prop_assert_eq!((o.ema.liq, o.ema.liq_slow), (l_new, l_new));
            }
        }
        prop_assert_eq!((o.ema.price, o.ema.ts, o.ema.count), (before.ema.price, before.ema.ts, before.ema.count));
        prop_assert_eq!(o.bucket, Bucket::EMPTY);
        prop_assert_eq!(o.last, before.last);
        for (e, b) in o.ring.iter().zip(before.ring.iter()) {
            prop_assert_eq!((e.ts, e.p, e.lo, e.hi), (b.ts, b.p, b.lo, b.hi));
            prop_assert_eq!((e.l, e.llo, e.lhi), (o.ema.liq, o.ema.liq, o.ema.liq));
            prop_assert_eq!((e.x, e.y), reprice(o.ema.liq, e.p).unwrap_or((0, 0)));
        }
    }

    /// M4R §4.1 (P1–P6): the post-change warm-up seed over random schedules of 8 observed
    /// seconds, each with or without an honest observation and with withdraw-, deposit-, pump-
    /// and dump-wrapped samples, after a `P_ema` carried at ¼–4× the pool's price. At every step
    /// the seed is the §4.1 formula over the post-change buckets, the price clamped by the
    /// largest v0.4.1 fall and step; (P1) one honest bucket bounds `P_ema ≤ max(p*, the fall
    /// chained from the `P_ema` it was integrated on)` from then on (≤ `p*` itself before
    /// M6-SEC-1), and a withdraw-wrapped sample in a bucket with an honest one changes nothing;
    /// (P2) `L_ema ≥ min(H, ⌊D·(10⁴ + C)/10⁴⌋)` with one honest bucket; (P3) at most 4 buckets above `H` bound `L_ema ≤ ⌊H·(10⁴ + C)/10⁴⌋`; (P4) constant
    /// honest buckets give `L_ema = H` and `P_ema` = `p*` clamped step by step between the
    /// chained fall and step exactly, and clear the guard at the gate; (P5, M4X-1) no step
    /// raises `P_ema` above the largest v0.4.1 step from the previous one, so the seed stays
    /// below the chained bound whatever the samples; (P6, M6-SEC-1) no step lowers it below the
    /// largest v0.4.1 fall, so the seed stays above the chained fall whatever the samples (a
    /// recorded low moves it one steady step a second). `warm` counts down, `L_slow` never
    /// moves, the guard is never set.
    #[test]
    fn warm_up_seed_properties(p in 10u128.pow(9)..10u128.pow(27), h in 10u128.pow(6)..10u128.pow(18),
                               pre_bps in 2_500u128..=40_000,
                               lam in 1_000u128..=10_000, lam_min in 2_000u64..=10_000, c_bps in 5_000u32..=10_000,
                               secs in prop::collection::vec(second(), 8)) {
        let d = (h * lam / 10_000).max(1);
        let cap = shortwin_math::pool_select::depth_at_lambda(d, lam_min).max(1);
        let p_pre = (p * pre_bps / 10_000).max(1);
        let states = warm_up(p, p_pre, h, d, cap, c_bps, &secs);
        let slow = states[0].ema.liq_slow;
        prop_assert!(slow <= d);
        let mut seen = Vec::new();
        let (mut prev, mut chain, mut chain_down, mut honest_p) = (p_pre, p_pre, p_pre, p_pre);
        let mut first_honest: Option<u128> = None;
        for (i, o) in states.iter().enumerate() {
            let e = o.ring.newest().unwrap();
            seen.push((e.lo, e.lhi));
            // The first warm-up roll comes 95 s after the last one (dt = Δt_max = 2), then 1 s.
            let dt = if i == 0 { 2 } else { 1 };
            let (lower, bound) = (down(prev, dt, c_bps), up(prev, dt, c_bps));
            prop_assert_eq!((o.ema.price, o.ema.liq), seed_of(&seen, c_bps, lower, bound), "step {}", i + 1);
            prop_assert_eq!((e.p, e.l), (o.ema.price, o.ema.liq));
            // P5 and P6.
            chain = up(chain, dt, c_bps);
            chain_down = down(chain_down, dt, c_bps);
            prop_assert!(o.ema.price <= bound && o.ema.price <= chain, "step {}", i + 1);
            prop_assert!(o.ema.price >= lower && o.ema.price >= chain_down, "step {}", i + 1);
            honest_p = p.min(up(honest_p, dt, c_bps)).max(down(honest_p, dt, c_bps));
            // P1: from the first honest bucket on, the fall chained from the P_ema it was
            // integrated on, or p*, bounds the seed.
            let (hon, att) = &secs[i];
            if first_honest.is_none() && (*hon || att.is_empty()) {
                first_honest = Some(prev);
            }
            if let Some(f) = first_honest.as_mut() {
                *f = down(*f, dt, c_bps);
                prop_assert!(o.ema.price <= p.max(*f), "P1 at step {}", i + 1);
            }
            prev = o.ema.price;
            prop_assert_eq!(o.ema.warm, EMA_WARMUP_BUCKETS - 1 - i as u8);
            prop_assert_eq!(o.ema.liq_slow, slow);
            if i < 7 {
                prop_assert_eq!(o.ema.guard, 1);
            }
        }
        let end = states[7];
        let honest_any = secs.iter().any(|(hon, att)| *hon || att.is_empty());
        let c = u128::from(c_bps);
        if honest_any {
            // P1 and P2.
            prop_assert!(end.ema.price <= p.max(first_honest.unwrap()));
            prop_assert!(end.ema.liq >= h.min(d * (10_000 + c) / 10_000));
            let stripped: Vec<(bool, Vec<Sample>)> = secs.iter().map(|(hon, att)| {
                if *hon || att.is_empty() {
                    (true, att.iter().copied().filter(|s| *s != Sample::Withdraw).collect())
                } else {
                    (false, att.clone())
                }
            }).collect();
            let alt = warm_up(p, p_pre, h, d, cap, c_bps, &stripped)[7];
            prop_assert_eq!((alt.ema.price, alt.ema.liq), (end.ema.price, end.ema.liq));
        }
        // P3.
        let above = secs.iter().filter(|(_, att)| att.iter().any(|s| matches!(s, Sample::Deposit(_)))).count();
        if above <= 4 {
            prop_assert!(end.ema.liq <= h * (10_000 + c) / 10_000);
        }
        // P4.
        if secs.iter().all(|(_, att)| att.is_empty()) {
            prop_assert_eq!((end.ema.price, end.ema.liq), (honest_p, h));
            prop_assert_eq!((end.ema.warm, end.ema.guard), (0, 0));
            prop_assert_eq!(ring_liq_lower_median(&end.ring).unwrap(), h);
        }
        // Once confirmed, the guard only ever clears.
        if end.ema.guard == 0 {
            prop_assert!(end.ema.liq * 10_000 <= ring_medians(&end.ring).unwrap().2 * 10_100);
        }
    }

    /// M4X-2: while the post-change guard is set, an integration never lifts `L_slow` above
    /// `max(L_slow before, L_mlo after)` — whatever the samples (deposit freezes, withdrawals,
    /// honest), `L_slow` follows at most the ring's honest lows, never a pushed `L_ema`; with the
    /// guard clear it is the v0.4.1 step.
    #[test]
    fn guarded_l_slow_follows_the_lows(p in 10u128.pow(9)..10u128.pow(27), h in 10u128.pow(9)..10u128.pow(18),
                                        lam in 1_000u128..=10_000, c_bps in 5_000u32..=10_000,
                                        secs in prop::collection::vec(second(), 8..60)) {
        let d = (h * lam / 10_000).max(1);
        let cap = shortwin_math::pool_select::depth_at_lambda(d, 5_000).max(1);
        let prm = EmaParams::new(2, c_bps, 3_600, 60);
        let mut o = Observation::new(0, p, h).unwrap();
        o.rewrite_liquidity(cap, VenueChange::RaydiumLocked { slow_cap: d }).unwrap();
        o.mark_restart();
        for (i, (honest, att)) in secs.iter().enumerate() {
            let t = 100 + i as i64;
            if *honest || att.is_empty() {
                o.record(t, p, h).unwrap();
            }
            for &s in att {
                let (sp, sl) = value(s, p, h, d);
                o.record(t, sp, sl).unwrap();
            }
            let pre = o.ema;
            o.roll(t + 1, &prm).unwrap();
            if pre.warm > 0 {
                prop_assert_eq!(o.ema.liq_slow, pre.liq_slow);
            } else if pre.guard == 1 {
                let l_mlo = ring_liq_lower_median(&o.ring).unwrap();
                prop_assert!(o.ema.liq_slow <= pre.liq_slow.max(l_mlo),
                    "t {}: L_slow {} -> {}, L_mlo {}, L_ema {}", t, pre.liq_slow, o.ema.liq_slow, l_mlo, o.ema.liq);
            }
        }
    }

    /// Liquidation EMA leg (ECON §5.5; I-E8): holds iff the newest n entries all have their
    /// cached state below L, and is monotone in n.
    #[test]
    fn liq_ema_leg_is_the_newest_n(flags in prop::collection::vec(any::<bool>(), 1..=8), n in 0u8..10) {
        let mut ring = Ring::default();
        for (i, _) in flags.iter().enumerate() {
            ring.push(RingEntry { x: i as u128 + 1, y: 1, ts: i as i64, ..RingEntry::default() }).unwrap();
        }
        let below = |x: u128, _y: u128| Ok(flags[usize::try_from(x).unwrap() - 1]);
        let leg = liq_ema_leg(&ring, below, n).unwrap();
        let k = usize::from(n);
        let want = k <= flags.len() && flags.iter().rev().take(k).all(|&f| f);
        prop_assert_eq!(leg, want);
        if leg && n > 0 {
            prop_assert!(liq_ema_leg(&ring, below, n - 1).unwrap());
        }
    }

    /// Exit references (ECON §8.2): the liquidation reference lies in [min(P_ema, lo_x),
    /// ⌊P_ema·(1 + κ_max)⌋] and never above ⌊lo_x·(1 + κ_eff)⌋ — without sustained growth a
    /// pushed-and-frozen P_ema cannot lift it above the bucket low (R4B-2); the expiry
    /// reference is at most P_ema, every ring price and the bucket low; both need a fresh
    /// completed bucket.
    #[test]
    fn references_are_bounded(p in 1_000u128..10u128.pow(33), steps in prop::collection::vec(0u128..2_000, 1..8),
                              lo_bps in 5_000u128..20_000, ema_bps in 5_000u128..20_000, age in 0i64..5,
                              kmax in 0u16..=5_000, kbase in 0u16..=1_000, max_age in 1u8..=5, gn in 2u8..=7) {
        let mut ring = Ring::new(0, p, 1).unwrap();
        let (mut q, mut ts) = (p, 0);
        for &bps in &steps {
            q += q * bps / 10_000;
            ts += 1;
            ring.push(entry(ts, q, q, q, 1, 1)).unwrap();
        }
        let mut o = Observation::new(0, p, 1).unwrap();
        o.ring = ring;
        o.ema.price = q * ema_bps / 10_000;
        let lo = q * lo_bps / 10_000;
        o.last = LastBucket { lo, hi: q, sec: ts, _pad: [0; 8] };
        let now = ts + age;
        let prm = ExitParams { kappa_liq_bps: kmax, kappa_base_bps: kbase, ref_max_age_s: max_age,
                               kappa_growth_n: gn, ..ExitParams::DEFAULT };
        let fresh = age <= i64::from(max_age);
        let g = sustained_growth(&o.ring, gn).unwrap();
        let kappa = u128::from(kappa_eff(g, &prm));
        prop_assert!(kappa <= u128::from(kmax));
        let r = liq_reference(&o.ema, &o.ring, &o.last, now, &prm).unwrap();
        prop_assert_eq!(r.is_some(), fresh);
        if let Some(r) = r {
            let lo_x = lo * (10_000 + u128::from(g) * u128::try_from(age).unwrap()) / 10_000;
            prop_assert!(r >= o.ema.price.min(lo_x));
            prop_assert!(r <= o.ema.price * (10_000 + u128::from(kmax)) / 10_000);
            prop_assert!(r <= lo_x * (10_000 + kappa) / 10_000);
            if g == 0 {
                prop_assert!(r <= lo * (10_000 + u128::from(kbase.min(kmax))) / 10_000);
            }
        }
        let e = expiry_reference(&o.ema, &o.ring, &o.last, now, &prm).unwrap();
        prop_assert_eq!(e.is_some(), fresh);
        if let Some(e) = e {
            prop_assert!(e <= o.ema.price && e <= lo && o.ring.iter().all(|x| e <= x.p));
        }
    }

    /// Reference liquidity (ECON §8.2, V2-14, M4R, M4F-1): `L_b = max(L_live, min(max(L_ema',
    /// L_slow), cap))` exactly; never below the live depth; without a cap the M4R form; with one,
    /// never above `max(L_live, cap)` nor the uncapped value, equal to it while the EMA terms fit
    /// under the cap, and monotone in the cap; the guard still bounds the EMA term by `L_mlo`.
    #[test]
    fn reference_liquidity_properties(l_live in 1u128..10u128.pow(20), l_ema in 1u128..10u128.pow(20),
                                      l_slow in 1u128..10u128.pow(20), guard in any::<bool>(),
                                      lows in prop::collection::vec(1u128..10u128.pow(20), 1..=8),
                                      cap in prop_oneof![1 => Just(0u128), 1 => Just(u128::MAX),
                                                         6 => 1u128..10u128.pow(20)],
                                      more in 0u128..10u128.pow(20)) {
        let mut o = Observation::new(0, 1_000, 1).unwrap();
        o.ring = Ring::default();
        for (i, &llo) in lows.iter().enumerate() {
            o.ring.push(entry(i as i64, 1_000, 1_000, 1_000, llo, llo)).unwrap();
        }
        o.ema.liq = l_ema;
        o.ema.liq_slow = l_slow;
        o.ema.guard = u8::from(guard);
        let l_mlo = ring_liq_lower_median(&o.ring).unwrap();
        let l_e = if guard { l_ema.min(l_mlo) } else { l_ema };
        let uncapped = ref_liquidity(l_live, &o.ema, &o.ring, None).unwrap();
        prop_assert_eq!(uncapped, l_live.max(l_e).max(l_slow));
        let got = ref_liquidity(l_live, &o.ema, &o.ring, Some(cap)).unwrap();
        prop_assert_eq!(got, l_live.max(l_e.max(l_slow).min(cap)));
        prop_assert!(got >= l_live && got <= uncapped && got <= l_live.max(cap));
        if l_e.max(l_slow) <= cap {
            prop_assert_eq!(got, uncapped);
        }
        if guard {
            prop_assert!(got <= l_live.max(l_mlo).max(l_slow));
        }
        let wider = ref_liquidity(l_live, &o.ema, &o.ring, Some(cap.saturating_add(more))).unwrap();
        prop_assert!(wider >= got);
    }

    /// M4F-1: on a Raydium pool of depth `H = isqrt(X·Y)` and locked share `λ` (`D` its locked
    /// depth), EMA terms pushed to any depth — a deposit of `k` times the pool wrapped around
    /// attacker-only cranks, frozen afterwards — lift the reference by at most `max(1, λ/λ_min)`
    /// over the live depth: `L_b ≤ max(H, ⌊λ·H/λ_min⌋)`. The cap moves by at most the whole-bps
    /// floor of `λ` when the same deposit (or a withdrawal of unlocked LP) wraps the exit itself,
    /// and an honest pool at `λ ≥ λ_min` (EMA terms at `H`) keeps its reference.
    #[test]
    fn the_depth_cap_bounds_deposit_inflation(x in 10u64.pow(9)..10u64.pow(16), y in 10u64.pow(9)..10u64.pow(15),
                                              counter in 10u64.pow(6)..10u64.pow(12), lam in 0u64..=10_000,
                                              lam_min in 2_000u32..=10_000, k in 1u64..=15,
                                              push in 1u128..=16, guard in any::<bool>(),
                                              w_bps in 0u64..=10_000) {
        let h = (u128::from(x) * u128::from(y)).isqrt();
        let supply = counter - counter * lam / 10_000;
        let lam_bps = shortwin_math::pool_select::lambda_bps(counter, supply);
        let d = shortwin_math::pool_select::locked_depth(counter, supply, x, y).unwrap();
        let cap = ref_depth_cap(d, lam_min);
        // The attacker's EMA terms: up to `push`·(1 + k)·H, lows as pushed (the guard cleared by
        // attacker-only seconds) or honest.
        let pushed = h * (1 + u128::from(k)) * push;
        let mut o = Observation::new(0, 1_000, h).unwrap();
        o.ring = Ring::default();
        for i in 0..8 {
            let llo = if guard { h } else { pushed };
            o.ring.push(entry(i, 1_000, 1_000, 1_000, llo, pushed)).unwrap();
        }
        o.ema.liq = pushed;
        // While the guard is set `L_slow` integrates no more than the lows (M4X-2).
        o.ema.liq_slow = if guard { h } else { pushed };
        o.ema.guard = u8::from(guard);
        let l_b = ref_liquidity(h, &o.ema, &o.ring, Some(cap)).unwrap();
        let bound = h.max(u128::from(lam_bps) * h / u128::from(lam_min));
        prop_assert!(l_b <= bound, "L_b {} > max(H, λ·H/λ_min) {} (H {}, λ {}, λ_min {})", l_b, bound, h, lam_bps, lam_min);
        if guard {
            prop_assert_eq!(l_b, h, "the guard holds the reference at the lows");
        }
        // The same deposit around the exit: D (hence the cap) moves by at most 1 bp of the
        // depth plus the floors.
        let (kc, ks) = (counter * k, counter * k);
        let d_dep = shortwin_math::pool_select::locked_depth(counter + kc, supply + ks, x * (1 + k), y * (1 + k)).unwrap();
        let h_dep = h * u128::from(1 + k);
        prop_assert!(d_dep <= d + h_dep / 10_000 + 2, "deposit: D {} -> {}", d, d_dep);
        // An unlocked LP's withdrawal of `w_bps` of the unlocked (circulating) LP.
        let w = supply * w_bps / 10_000;
        if w < counter {
            let (xw, yw) = (x - (u128::from(x) * u128::from(w) / u128::from(counter)) as u64,
                            y - (u128::from(y) * u128::from(w) / u128::from(counter)) as u64);
            let d_wd = shortwin_math::pool_select::locked_depth(counter - w, supply - w, xw, yw).unwrap();
            prop_assert!(d_wd <= d + h / 10_000 + 2, "withdraw: D {} -> {}", d, d_wd);
        }
        // Honest EMA terms at H on a pool at or above λ_min: the cap does not bind.
        if lam_bps >= lam_min {
            let mut honest = o;
            honest.ema.liq = h;
            honest.ema.liq_slow = h;
            honest.ema.guard = 0;
            let slack = h / 10_000 + 1;
            prop_assert!(cap + slack >= h, "cap {} under H {}", cap, h);
            prop_assert!(ref_liquidity(h, &honest.ema, &honest.ring, Some(cap)).unwrap() == h);
        }
    }

    /// `max_in` (ECON §8.2 v0.4.1): a liquidation's bound is exactly
    /// min(⌊B̂_ref·(1 + δ)⌋, V − K_s − ert), so it never waits below it (I-E11); an expiry's
    /// is below its bound and the S1 budget, and keeps rent and the expiry fee senior whenever
    /// the fair cost leaves room for them (I-E2). Both are monotone in each input.
    #[test]
    fn max_in_properties(b in 0u64..10u64.pow(13), d in 0u16..=2_000, e in 0u16..=100, fee in 0u16..=500,
                         v in 0u64..10u64.pow(13), k in 0u64..10u64.pow(9), r in 0u64..10u64.pow(12),
                         ert in 0u64..10u64.pow(7), live in 0u64..10u64.pow(13), bump in 1u64..10u64.pow(10)) {
        let bound = i128::from(b) * (10_000 + i128::from(d)) / 10_000;
        let budget = i128::from(v) - i128::from(k) - i128::from(ert);
        let ml = max_in_liq(b, d, v, k, ert);
        prop_assert_eq!(ml.map(i128::from), (budget >= 0).then(|| bound.min(budget)));
        // I-E11: a live cost within the bound and the S1 budget always executes.
        if i128::from(live) <= bound && i128::from(live) <= budget {
            prop_assert!(ml.is_some_and(|m| live <= m));
        }
        let pi = pi_ref(b, fee).unwrap();
        prop_assert_eq!(u128::from(pi), (u128::from(b) * u128::from(fee)).div_ceil(10_000));
        let me = max_in_exp(b, d, e, v, k, r, pi, ert);
        let eps = i128::from(b) * (10_000 + i128::from(e)) / 10_000;
        let room = i128::from(v) - i128::from(r) - i128::from(pi) - i128::from(ert);
        match me {
            None => prop_assert!(budget < 0),
            Some(m) => {
                let m = i128::from(m);
                prop_assert!(m <= bound && m <= budget);
                if room >= eps {
                    prop_assert!(m <= room, "rent and fee must stay senior");
                }
            }
        }
        let key = |x: Option<u64>| x.map_or(-1i128, i128::from);
        prop_assert!(key(max_in_liq(b.saturating_add(bump), d, v, k, ert)) >= key(ml));
        prop_assert!(key(max_in_liq(b, d, v.saturating_add(bump), k, ert)) >= key(ml));
        prop_assert!(key(max_in_liq(b, d, v, k.saturating_add(bump), ert)) <= key(ml));
        prop_assert!(key(max_in_exp(b, d.saturating_add(100), e, v, k, r, pi, ert)) >= key(me));
        prop_assert!(key(max_in_exp(b, d, e, v.saturating_add(bump), k, r, pi, ert)) >= key(me));
        prop_assert!(key(max_in_exp(b, d, e, v, k, r.saturating_add(bump), pi, ert)) <= key(me));
        prop_assert!(key(max_in_exp(b, d, e, v, k, r, pi, ert.saturating_add(bump))) <= key(me));
    }
}

/// A bucket is open iff its low is non-zero; `record` refuses zero observations, so the
/// sentinel can never be forged.
#[test]
fn bucket_sentinel() {
    let mut b = Bucket::EMPTY;
    assert!(!b.is_open());
    assert!(shortwin_math::observe::record(&mut b, 0, 0, 1).is_err());
    shortwin_math::observe::record(&mut b, 0, 1, 1).unwrap();
    assert!(b.is_open());
}
