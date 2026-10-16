//! ★The global keeper budget (owner decision A of 2026-10-01 on OWNER-M7-13 / -14; ARCH §7.1
//! duty 8, T-COVERAGE; ECON §5.8; the keeper's `budget` module): one daily budget `G` funding
//! markets in order of their lenders' value at risk `V = ⌊ΣB_Q·min(P_ema, P_ACC_MAX)/10¹⁸⌋`.
//!
//! - `a_burst_storm_on_one_market_spends_its_entitlement_and_the_free_pool_and_nothing_else`: an
//!   attacker's reverted round trip starts a per-slot burst on one covered market; the burst draws
//!   on that market's entitlement and then the free pool — the market is not degraded while they
//!   last — and never on the other markets' entitlements; once both are spent the burst's next
//!   charge is refused (degraded, opens paused) and no other market is touched.
//! - `junk_markets_below_the_floor_get_the_idle_cadence_only_and_stop_at_the_tail`: markets with
//!   small positions (below `var_floor_lamports`) are read and cranked at the 30-s idle cadence
//!   only — no burst from a reverted round trip, no per-second coverage —, their cranks drawn from
//!   the free pool within `tail_share_bps` of `G`; once the tail is spent they are refused
//!   (degraded); the real market's entitlement is never touched.
//! - `a_junk_market_above_the_floor_outranks_and_preempts_the_lowest_ranked_first`: a market that
//!   becomes covered with more value at risk than the others takes its prorated entitlement from
//!   the free pool, then from the lowest-ranked market — never from a market ranked above that one
//!   while the lowest still has some — and the preempted market degrades first.
//! - `exhaustion_degrades_the_lowest_ranked_markets_first`: three covered markets whose
//!   entitlements the budget funds in rank order run out lowest-ranked first; at 00:00 UTC the new
//!   day's budget funds the top market first, and the others stay degraded.
//!
//! The scenarios run close to 00:00 UTC (a market covered at second `s` of the day gets
//! `⌈R·(86,400 − s)/86,400⌉`), with one crank per transaction (5,000 lamports each), so the
//! entitlements are small and every number exact.

use anchor_lang::prelude::Pubkey;
use shortwin_keeper::DegradedReason;
use shortwin_keeper::budget::{BudgetParams, ChargeClass, DAY_S, Tier};
use shortwin_litesvm_tests::drive::swap_in_ix;
use shortwin_litesvm_tests::engine::crank_price_ix;
use shortwin_litesvm_tests::mock::Dir;

use crate::common::*;

/// One crank per transaction: every crank costs exactly the 5,000-lamport signature fee.
fn single_crank_cfg(global: u64) -> KeeperConfig {
    let mut cfg = KeeperConfig {
        batch_v1: 1,
        batch_v0: 1,
        ..KeeperConfig::default()
    };
    cfg.budget.global_daily_lamports = global;
    cfg
}

/// The next UTC midnight after `h`'s clock.
fn next_midnight(h: &Harness) -> i64 {
    (h.now().div_euclid(DAY_S) + 1) * DAY_S
}

/// Warps (before any position is open: no coverage gap) to `lead` seconds before `at`.
fn warp_to(h: &Harness, at: i64, lead: i64) {
    let now = h.now();
    assert!(
        at - lead > now,
        "the scenario runs before the next midnight"
    );
    h.with_env(|env| env.warp(at - lead - now));
}

/// Idles so that the next `h.second()` ticks at `at` (at most 20 s: no coverage gap).
fn first_tick_at(h: &mut Harness, at: i64) {
    let lead = at - (h.now() + 1);
    assert!((0..20).contains(&lead), "{lead}");
    h.idle(lead);
}

/// The quiet day's entitlement of a market whose crank costs 5,000 lamports, at the defaults
/// (twice 2,880 cranks), prorated to `left` seconds of the day.
fn entitlement(left: i64) -> u64 {
    let p = BudgetParams {
        global: 1,
        var_floor: 0,
        reserve_margin_bps: 10_000,
        tail_share_bps: 0,
        idle_interval_s: 30,
        first_charge: 5_000,
    };
    BudgetParams::prorated(p.entitlement(5_000), DAY_S - left)
}

fn budget_degraded(r: &TickReport, key: &Pubkey) -> Option<Option<u32>> {
    r.alerts.iter().find_map(|a| match a {
        Alert::Degraded {
            market,
            reason: DegradedReason::Budget { rank, .. },
        } if market == key => Some(*rank),
        _ => None,
    })
}

fn any_degraded(r: &TickReport, key: &Pubkey) -> bool {
    r.alerts
        .iter()
        .any(|a| matches!(a, Alert::Degraded { market, .. } if market == key))
}

/// An attacker's reverted round trip on `mm` in one transaction: `[push +20 %, crank_price,
/// dump back]` (the duty-2 burst trigger, ECON §12.11).
fn round_trip(h: &Harness, mm: &MockMarket) {
    h.with_env(|env| {
        let attacker = env.actor(mm, 0, 1_000_000_000_000);
        let (_, amount, pump) = env
            .push_price_ix(mm, &attacker, env.spot(mm) * 12 / 10)
            .unwrap();
        let bought = mock::quote_exact_in(&env.svm, mm.mock(), Dir::QuoteIn, amount)
            .unwrap()
            .proceeds;
        let dump = swap_in_ix(mm, &attacker, Dir::BaseIn, bought, 0);
        let kp = attacker.kp.insecure_clone();
        ok(env.bundle(&[pump, crank_price_ix(mm), dump], &[&kp]));
    });
}

/// ★Decision A (T-COVERAGE): price bursts are discretionary — drawn on the attacked market's own
/// entitlement and then the free pool, never on another market's. Three covered markets ranked by
/// value at risk, funded 200,000 lamports each 600 s before midnight, the free pool 300,000. An
/// attacker's reverted round trip on the middle one (rank 2) starts a per-slot burst: every slot
/// observed, the market not degraded while its entitlement and the free pool last (99 burst
/// cranks); the next charge is refused — degraded at once, `Degraded{Budget{rank: 2}}` — with the
/// free pool and its entitlement at 0, and the other two markets' entitlements less exactly their
/// own idle cranks.
#[test]
fn a_burst_storm_on_one_market_spends_its_entitlement_and_the_free_pool_and_nothing_else() {
    let funded = entitlement(600);
    assert_eq!(funded, 200_000);
    let free = 300_000;
    let (mut h, mms) = setup(3, single_crank_cfg(3 * funded + free));
    let midnight = next_midnight(&h);
    warp_to(&h, midnight, 620);
    for (mm, q) in mms.iter().zip([Q, Q * 2 / 3, Q / 2]) {
        let _ = open(&h, mm, q);
    }
    first_tick_at(&mut h, midnight - 600);
    let keys: Vec<Pubkey> = mms.iter().map(MockMarket::market).collect();
    h.second();
    let b = h.keeper.budget();
    assert_eq!(b.ranked(), keys, "ranked by value at risk");
    for k in &keys {
        assert_eq!(b.markets[k].funded, funded);
        assert_eq!(b.markets[k].tier, Tier::Covered);
    }
    assert_eq!(b.free, free);
    h.seconds(4);

    let target = &mms[1];
    let tk = target.market();
    let attacked_at = h.now();
    round_trip(&h, target);
    let mut slots = 0u32;
    let mut refused_at = None;
    while h.now() < attacked_at + 60 {
        let r = h.slot().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        for k in [&keys[0], &keys[2]] {
            assert!(
                !any_degraded(&r, k),
                "another market degraded: {:?}",
                r.alerts
            );
        }
        if let Some(rank) = budget_degraded(&r, &tk) {
            assert_eq!(rank, Some(2));
            refused_at = Some(slots);
            break;
        }
        assert!(!any_degraded(&r, &tk), "{:?}", r.alerts);
        assert!(r.cranked.contains(&tk), "burst slot {slots} observed");
        slots += 1;
        if slots.is_multiple_of(2) {
            h.with_env(|env| env.warp(1));
        }
    }
    let refused_at = refused_at.expect("the storm outlasted the budget");
    let b = h.keeper.budget();
    let t = &b.markets[&tk];
    assert_eq!(
        (t.unspent, b.free),
        (0, 0),
        "its entitlement and the free pool spent"
    );
    assert_eq!(t.spent[1], 5_000, "its first crank (baseline)");
    assert_eq!(
        t.spent[2],
        funded - 5_000 + free,
        "every burst crank discretionary: its entitlement, then the free pool"
    );
    for k in [&keys[0], &keys[2]] {
        let m = &b.markets[k];
        assert_eq!(m.unspent + m.spent_today(), funded, "nothing drawn on {k}");
        assert_eq!(m.spent[2], 0);
    }
    assert_eq!(b.overdraft, 0);
    // The rest of the burst: its charges refused; the other markets keep their idle cadence.
    let mut others = 0;
    while h.now() < attacked_at + 60 {
        let r = h.slot().clone();
        others += r.cranked.iter().filter(|k| **k != tk).count();
        assert!(!r.cranked.contains(&tk), "refused");
        for k in [&keys[0], &keys[2]] {
            assert!(!any_degraded(&r, k));
        }
        h.with_env(|env| env.warp(1));
    }
    assert!(others >= 2, "the others' idle cranks continue");
    for k in [&keys[0], &keys[2]] {
        let m = &h.keeper.budget().markets[k];
        assert_eq!(m.unspent + m.spent_today(), funded);
    }
    println!(
        "decision A: a burst storm on rank 2 — {refused_at} burst cranks admitted ({} lamports: its \
         entitlement {} + the free pool {free}) before its first refusal; ranks 1 and 3 untouched",
        funded - 5_000 + free,
        funded - 5_000
    );
}

/// ★Decision A (T-COVERAGE): junk below the floor. One real market (a 3-SOL position: covered) and
/// ten junk markets with 0.3-SOL positions (below the 1-SOL floor) under `G` = 0.02 SOL with a
/// 0.25 % tail (50,000 lamports: ten cranks). The junk markets are read and cranked at the idle
/// cadence only — a reverted round trip on one starts no burst — and their first ten cranks fill
/// the tail; from their next idle crank they are refused (degraded, `rank: None`). The real
/// market's entitlement pays only its own cranks, and the free pool only the tail.
#[test]
fn junk_markets_below_the_floor_get_the_idle_cadence_only_and_stop_at_the_tail() {
    const JUNK: usize = 10;
    let mut cfg = single_crank_cfg(20_000_000);
    cfg.budget.tail_share_bps = 25;
    let (mut h, mms) = setup(1 + JUNK, cfg);
    let real = mms[0].market();
    let _ = open(&h, &mms[0], Q);
    for mm in &mms[1..] {
        let _ = open(&h, mm, Q / 10);
    }
    let junk: Vec<Pubkey> = mms[1..].iter().map(MockMarket::market).collect();
    let mut cranks: Vec<Vec<i64>> = vec![Vec::new(); JUNK];
    let mut reads = [0u32; JUNK];
    let mut refused = [false; JUNK];
    let mut real_cranks = 0;
    let t0 = h.now() + 1;
    for s in 0..95 {
        if s == 5 {
            round_trip(&h, &mms[1]);
        }
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(!any_degraded(&r, &real), "{:?}", r.alerts);
        assert!(
            !r.bursts.iter().any(|(m, _)| junk.contains(m)),
            "no burst below the floor: {:?}",
            r.bursts
        );
        real_cranks += usize::from(r.cranked.contains(&real));
        for (i, k) in junk.iter().enumerate() {
            if r.cranked.contains(k) {
                cranks[i].push(h.now());
            }
            if r.needs.contains_key(k) {
                reads[i] += 1;
            }
            if let Some(rank) = budget_degraded(&r, k) {
                assert_eq!(rank, None);
                refused[i] = true;
            }
        }
    }
    let b = h.keeper.budget();
    assert_eq!(b.markets[&real].tier, Tier::Covered);
    for (i, k) in junk.iter().enumerate() {
        assert_eq!(b.markets[k].tier, Tier::BelowFloor, "junk {i}");
        // The first idle crank fit the tail; every later one was refused.
        assert_eq!(cranks[i], vec![t0], "junk {i}: one crank, at first sight");
        assert!(refused[i], "junk {i} refused once the tail was spent");
        // Read at first sight and then at its idle cadence only (3 more reads in 95 s).
        assert!(reads[i] <= 4, "junk {i} read {} times in 95 s", reads[i]);
    }
    assert_eq!(b.tail, 50_000, "the tail cap, exactly");
    let rm = &b.markets[&real];
    assert_eq!(
        rm.unspent + rm.spent_today(),
        rm.funded,
        "the real market paid itself"
    );
    assert_eq!(b.free, 20_000_000 - rm.funded - b.tail);
    assert!(real_cranks >= 4);
    assert!(h.keeper.metrics.markets_idle_skipped >= 80 * JUNK as u64);
    println!(
        "decision A: {JUNK} junk markets below the floor: 1 crank each ({} lamports, the tail), \
         then refused; reads per junk market in 95 s: {reads:?}; the real market's entitlement \
         untouched",
        b.tail
    );
}

/// ★Decision A (T-COVERAGE): a junk market that becomes covered with more value at risk than the
/// real ones outranks them and takes its prorated entitlement from the free pool (5,000 lamports
/// here), then from the **lowest-ranked** market — which has enough, so the higher-ranked real
/// market keeps all of its entitlement — and the preempted one is refused (degraded) at its next
/// idle crank.
#[test]
fn a_junk_market_above_the_floor_outranks_and_preempts_the_lowest_ranked_first() {
    let funded = entitlement(600);
    let (mut h, mms) = setup(3, single_crank_cfg(2 * funded + 5_000));
    let midnight = next_midnight(&h);
    warp_to(&h, midnight, 620);
    let (a, b_, j) = (mms[0].market(), mms[1].market(), mms[2].market());
    let _ = open(&h, &mms[0], Q);
    let _ = open(&h, &mms[1], Q / 2);
    first_tick_at(&mut h, midnight - 600);
    h.second();
    assert_eq!(h.keeper.budget().ranked(), vec![a, b_]);
    assert_eq!(h.keeper.budget().free, 5_000);
    h.seconds(4);
    // The junk market opens a larger position than either.
    let _ = open(&h, &mms[2], Q * 3 / 2);
    let mut b_refused = None;
    let mut joined = false;
    for _ in 0..40 {
        let r = h.second().clone();
        assert!(!any_degraded(&r, &a), "{:?}", r.alerts);
        assert!(!any_degraded(&r, &j), "{:?}", r.alerts);
        if !joined && h.keeper.budget().tier(&j) == Some(Tier::Covered) {
            joined = true;
            let r_j = entitlement(midnight - h.now());
            let bg = h.keeper.budget();
            assert_eq!(bg.ranked(), vec![j, a, b_], "the junk market outranks both");
            // J's prorated entitlement: the free pool's 5,000, then B's.
            assert_eq!(bg.markets[&j].funded, r_j);
            assert_eq!(bg.free, 0);
            let bm = &bg.markets[&b_];
            assert_eq!(bm.unspent, funded - 5_000 - (r_j - 5_000));
            assert_eq!(bm.unspent + bm.spent_today(), bm.funded);
            // A's entitlement untouched: only its own cranks.
            let am = &bg.markets[&a];
            assert_eq!(am.unspent + am.spent_today(), funded);
            assert_eq!(am.funded, funded);
        }
        if let Some(rank) = budget_degraded(&r, &b_) {
            assert_eq!(rank, Some(3));
            b_refused = Some(h.now());
            break;
        }
    }
    assert!(joined);
    assert!(b_refused.is_some(), "the preempted market degrades first");
    let bg = h.keeper.budget();
    let am = &bg.markets[&a];
    assert_eq!(am.unspent + am.spent_today(), funded, "A never preempted");
    assert_eq!(bg.markets[&b_].spent[ChargeClass::Baseline as usize], 5_000);
}

/// ★Decision A (T-COVERAGE): exhaustion degrades the lowest-ranked markets first. Three covered
/// markets 90 s before midnight: entitlements of 30,000 lamports each (six cranks) funded in rank
/// order from `G` = 70,000 — 30,000, 30,000, 10,000. The lowest runs out at −30 s (its third idle
/// crank refused); at 00:00 UTC the new day's `G` funds the top market first (all 70,000 of it:
/// its entitlement is a full quiet day's twice), so the second is refused at its next crank (0 s)
/// and the lowest stays refused; the top market runs out last, after 14 cranks (+420 s).
#[test]
fn exhaustion_degrades_the_lowest_ranked_markets_first() {
    let funded = entitlement(90);
    assert_eq!(funded, 30_000);
    let (mut h, mms) = setup(3, single_crank_cfg(70_000));
    let midnight = next_midnight(&h);
    warp_to(&h, midnight, 110);
    for (mm, q) in mms.iter().zip([Q, Q * 2 / 3, Q / 2]) {
        let _ = open(&h, mm, q);
    }
    first_tick_at(&mut h, midnight - 90);
    let keys: Vec<Pubkey> = mms.iter().map(MockMarket::market).collect();
    let mut degraded: [Option<i64>; 3] = [None; 3];
    let mut exhausted = Vec::new();
    for _ in 0..600 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if h.now() == midnight - 90 {
            let b = h.keeper.budget();
            assert_eq!(b.ranked(), keys);
            let f: Vec<u64> = keys.iter().map(|k| b.markets[k].funded).collect();
            assert_eq!(f, vec![30_000, 30_000, 10_000], "funded in rank order");
        }
        for a in &r.alerts {
            if let Alert::BudgetExhausted { market, .. } = a {
                exhausted.push((h.now() - midnight, *market));
            }
        }
        for (i, k) in keys.iter().enumerate() {
            if degraded[i].is_none()
                && let Some(rank) = budget_degraded(&r, k)
            {
                assert_eq!(rank, Some(u32::try_from(i + 1).unwrap()));
                degraded[i] = Some(h.now() - midnight);
            }
        }
        if degraded.iter().all(Option::is_some) {
            break;
        }
    }
    assert_eq!(degraded, [Some(420), Some(0), Some(-30)]);
    // One `BudgetExhausted` a UTC day: the lowest at −30 s, the second at the new day's 0 s.
    assert_eq!(exhausted, vec![(-30, keys[2]), (0, keys[1])]);
    assert_eq!(h.keeper.budget().overdraft, 0);
    println!(
        "decision A: exhaustion order — rank 3 at {:?} s, rank 2 at {:?} s, rank 1 at {:?} s from \
         midnight",
        degraded[2], degraded[1], degraded[0]
    );
}
