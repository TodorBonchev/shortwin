//! M7 security review regressions on the keeper (the review's reproducers, fixed):
//!
//! - **M7-SEC-1** (timing): (a) an idle crank landing a second later relative to its read than
//!   the previous one is a 31-s gap — landing jitter of the idle cadence. As built at M7 the
//!   program emitted `CoverageGap{31}` for it (`COVERAGE_GAP_S` = 30) and the keeper tolerated it
//!   up to `idle_interval_s + gap_tolerance_s` (34 s); since the owner raised `COVERAGE_GAP_S` to
//!   40 s (OWNER-M7-12, 2026-10-01) the program emits nothing up to 40 s, and the first gap it
//!   emits (41 s) degrades. (b) Keeper transactions landing 0–2 s after the read and ticks that wait for their
//!   confirmations (the next tick a second later) keep a quiet market with a position open for
//!   two hours — as built at M7 it was coverage-paused 99.7 % of the time. (c) After a real
//!   outage the pause lifts ≈ 600 s later with slow ticks too — as built it never did.
//! - **M7-SEC-2** (bursts): a plain `crank_price` after an honest ≥ 2 % move, the keeper's own
//!   crank landing after a trade, and a second keeper instance on the other side of a trade start
//!   no per-slot burst (the foreign price is still there at the next read: a genuine move); a
//!   price round trip still does, in one bundle or in separate transactions.
//! - **M7-SEC-3** (keeper side): a clear of the flag this instance did not send is alerted and set
//!   again; an open that landed while the keeper held the market degraded is alerted.
//! - **M7-SEC-8** (load): list-only junk markets no longer grow every tick's reads.

use std::cell::Cell;
use std::rc::Rc;

use shortwin::errors::ShortwinError;
use shortwin_keeper::{DegradedReason, Keeper};
use shortwin_keeper_litesvm::{LiteSvmChain, Rpc, block_on};
use shortwin_litesvm_tests::engine::crank_price_ix;
use shortwin_litesvm_tests::ix;
use solana_signer::Signer;

use crate::common::*;
use crate::coverage_pause::{paused, pauser_cfg, pauser_setup, try_open};

fn tick_other(k: &mut Keeper<LiteSvmChain>) -> TickReport {
    block_on(k.tick()).expect("keeper tick")
}

/// The gap of a `Degraded{CoverageGap}` alert in `r`.
fn gap_degraded(r: &TickReport) -> Option<i64> {
    r.alerts.iter().find_map(|a| match a {
        Alert::Degraded {
            reason: DegradedReason::CoverageGap { gap_s, .. },
            ..
        } => Some(*gap_s),
        _ => None,
    })
}

fn any_degraded(r: &TickReport) -> bool {
    r.alerts.iter().any(|a| matches!(a, Alert::Degraded { .. }))
}

/// A deterministic xorshift64 stream.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// M7-SEC-1 (a): two minutes of the idle cadence, then the keeper's next transaction lands
/// `late` seconds after the clock it read. Returns the tick report of the tick after the late
/// crank (the first that can see its outcome) and the harness.
fn late_idle_crank(late: i64) -> (Harness, MockMarket, TickReport) {
    let (mut h, mms, _auth) = pauser_setup(1, pauser_cfg());
    let mm = mms.into_iter().next().expect("one market");
    let _ = open(&h, &mm, Q);
    for s in 0..120 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{s}: {:?}", r.errors);
        assert!(r.coverage_pauses.is_empty() && !any_degraded(&r), "{s}");
    }
    h.chain().before_next_send(move |env| env.warp(late));
    let key = mm.market();
    for _ in 0..40 {
        let r = h.second().clone();
        if r.cranked.contains(&key) {
            return (h, mm, r);
        }
    }
    panic!("no idle crank");
}

/// A 31-s gap (one second of landing jitter on the 30-s idle cadence): since `COVERAGE_GAP_S` is
/// 40 s (OWNER-M7-12) the program emits no `CoverageGap` for it at all (as built at M7, with 30 s,
/// it emitted `CoverageGap{31}`, which the keeper tolerated).
#[test]
fn m7_sec_1_a_a_one_second_late_idle_crank_is_jitter_not_an_outage() {
    let (mut h, mm, r) = late_idle_crank(1);
    let key = mm.market();
    let m = &h.keeper.metrics.markets[&key];
    assert_eq!(
        (m.coverage_gaps, m.coverage_gaps_tolerated),
        (0, 0),
        "no event at 31 s"
    );
    assert!(gap_degraded(&r).is_none() && r.coverage_pauses.is_empty());
    assert!(!paused(&h, &key));
    assert_eq!(try_open(&h, &mm), Ok(()), "opens keep landing");
    // And the market stays healthy on its idle cadence afterwards.
    for _ in 0..120 {
        let r = h.second().clone();
        assert!(!any_degraded(&r) && r.coverage_pauses.is_empty());
    }
    assert_eq!(h.keeper.metrics.markets[&key].coverage_gaps, 0);
    println!(
        "M7-SEC-1 (a): an idle crank 1 s late -> a 31-s gap, no CoverageGap event; opens land"
    );
}

/// The boundary (OWNER-M7-12): a 40-s gap (the idle crank 10 s late) is not reported and does
/// not degrade; a 41-s gap is the program's `CoverageGap{41}`, and every gap it reports degrades
/// (`gap_degrades_above` = max(30 + 4, 40) = 40): opens paused at once.
#[test]
fn m7_sec_1_a_the_program_reports_a_gap_above_40_s_and_every_reported_gap_degrades() {
    assert_eq!(shortwin_math::consts::COVERAGE_GAP_S, 40);
    assert_eq!(KeeperConfig::default().coverage.gap_degrades_above(), 40);
    let (mut h, mm, r) = late_idle_crank(10);
    let key = mm.market();
    let r2 = h.second().clone();
    assert_eq!(
        h.keeper.metrics.markets[&key].coverage_gaps, 0,
        "40 s: no event"
    );
    assert!(gap_degraded(&r).is_none() && gap_degraded(&r2).is_none() && !paused(&h, &key));

    let (h, mm, r) = late_idle_crank(11);
    let key = mm.market();
    assert_eq!(gap_degraded(&r), Some(41));
    assert_eq!(h.keeper.metrics.markets[&key].coverage_gaps, 1);
    assert_eq!(h.keeper.metrics.markets[&key].coverage_gaps_tolerated, 0);
    assert!(
        r.coverage_pauses
            .iter()
            .any(|p| p.paused && p.market == key && p.result.landed().is_some()),
        "{:?}",
        r.coverage_pauses
    );
    assert!(paused(&h, &key));
    assert_eq!(
        try_open(&h, &mm),
        Err(Some(u32::from(ShortwinError::CoveragePaused)))
    );
}

/// M7-SEC-1 (b), the CI latency scenario (M7-SEC-6): every keeper transaction lands 0, 1 or 2 s
/// after the clock the keeper read, and every tick that sends is followed by a skipped second
/// (its confirmation wait). Two hours of a quiet market with one healthy position: never
/// degraded, never paused, and an open lands at the end.
#[test]
fn m7_sec_1_b_landing_latency_and_slow_ticks_keep_a_quiet_market_open() {
    let (mut h, mms, _auth) = pauser_setup(1, pauser_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(5);
    let rng = Rc::new(Cell::new(0x2545_f491_4f6c_dd1du64));
    let r2 = rng.clone();
    let delays = Rc::new(Cell::new([0u32; 3]));
    let d2 = delays.clone();
    h.chain().set_landing_delays(Some(Box::new(move || {
        let mut g = Rng(r2.get());
        let d = g.next() % 3;
        r2.set(g.0);
        let mut c = d2.get();
        c[d as usize] += 1;
        d2.set(c);
        i64::try_from(d).unwrap_or(0)
    })));
    let t0 = h.now();
    let (mut paused_s, mut ticks) = (0i64, 0u32);
    while h.now() - t0 < 7_200 {
        let before = h.now();
        let r = h.second_slow(1);
        ticks += 1;
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(
            !any_degraded(&r),
            "degraded at +{}: {:?}",
            h.now() - t0,
            r.alerts
        );
        assert!(r.coverage_pauses.is_empty(), "{:?}", r.coverage_pauses);
        if paused(&h, &key) {
            paused_s += h.now() - before;
        }
    }
    h.chain().set_landing_delays(None);
    assert_eq!(paused_s, 0);
    assert_eq!(try_open(&h, mm), Ok(()));
    let m = &h.keeper.metrics.markets[&key];
    println!(
        "M7-SEC-1 (b): 7,200 s, {ticks} ticks, landing delays 0/1/2 s {:?}: {} observations, \
         {} CoverageGap events on chain (with COVERAGE_GAP_S = 30 there were 79 of 241, all \
         tolerated; at 40 none), 0 s paused (was 99.7 %)",
        delays.get(),
        m.observations,
        m.coverage_gaps,
    );
    assert_eq!((m.coverage_gaps, m.coverage_gaps_tolerated), (0, 0));
}

/// M7-SEC-1 (c): after the drill's 45-s outage, the pause lifts ≈ `recovery_s` later whether
/// the sending ticks are fast, all slow or every other one slow (a slow tick skipped a second).
fn recovery_run(skip_every_send: u32) -> Option<i64> {
    let (mut h, mms, _auth) = pauser_setup(1, pauser_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(5);
    h.idle(45);
    let r = h.second().clone();
    assert!(gap_degraded(&r).is_some(), "degraded by the outage");
    assert!(paused(&h, &key));
    let t0 = h.now();
    let mut sends = 0u32;
    while h.now() - t0 < 7_200 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if !paused(&h, &key) {
            return Some(h.now() - t0);
        }
        let sent = !r.cranked.is_empty() || !r.exits.is_empty() || !r.coverage_pauses.is_empty();
        if sent {
            sends += 1;
            if skip_every_send > 0 && sends.is_multiple_of(skip_every_send) {
                h.idle(1);
            }
        }
    }
    None
}

#[test]
fn m7_sec_1_c_the_recovery_completes_with_slow_ticks() {
    let control = recovery_run(0);
    let every = recovery_run(1);
    let half = recovery_run(2);
    println!(
        "M7-SEC-1 (c): pause lifted after {control:?} s (no slow ticks), {every:?} (every sending \
         tick slow), {half:?} (every other one) — as built: 601 / never / never"
    );
    for s in [control, every, half] {
        assert!(s.is_some_and(|s| (600..=640).contains(&s)), "{s:?}");
    }
}

/// The keeper's spend on `key` today.
fn spent(h: &Harness, key: &anchor_lang::prelude::Pubkey) -> u64 {
    h.keeper.track(key).map_or(0, |t| t.spent_today)
}

/// M7-SEC-2 (a): an honest trader moves the spot 3 % (alternately up and down) once a minute
/// right after the keeper's read; with `third_party`, someone sends one plain `crank_price` in
/// the same second. Returns (keeper spend, bursts, genuine moves, paused).
fn plain_crank_run(third_party: bool, minutes: u32) -> (u64, u64, u64, bool) {
    let (mut h, mms, _auth) = pauser_setup(1, pauser_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(5);
    let trader = h.with_env(|env| env.actor(mm, 50_000_000_000_000, 50_000_000_000_000));
    let mut up = true;
    let mut was_paused = false;
    for _ in 0..minutes {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        h.with_env(|env| {
            let s = env.spot(mm);
            let t = if up { s * 103 / 100 } else { s * 100 / 103 };
            env.push_price(mm, &trader, t);
        });
        up = !up;
        if third_party {
            h.with_env(|env| ok(env.send(&[crank_price_ix(mm)], &[])));
        }
        // The keeper's next read is in the next second (a sending tick waits for its
        // confirmation); two ticks a second for the rest of the minute.
        for _ in 0..59 {
            h.second();
            h.slot();
            was_paused |= paused(&h, &key);
        }
    }
    let m = &h.keeper.metrics.markets[&key];
    (spent(&h, &key), m.bursts, m.foreign_moves, was_paused)
}

#[test]
fn m7_sec_2_a_plain_cranks_after_honest_moves_start_no_burst() {
    let (ctl_spent, ctl_bursts, _, ctl_paused) = plain_crank_run(false, 60);
    let (spent, bursts, genuine, was_paused) = plain_crank_run(true, 60);
    println!(
        "M7-SEC-2 (a): 60 honest 3 % moves in an hour: control {ctl_spent} lamports / {ctl_bursts} \
         bursts; with a plain third-party crank after each: {spent} lamports, {bursts} bursts, \
         {genuine} foreign prices judged genuine (as built: 51 cranks spent 30,000,000 lamports and \
         paused opens)"
    );
    assert_eq!((ctl_bursts, bursts), (0, 0));
    assert_eq!(genuine, 60);
    assert!(!ctl_paused && !was_paused);
    assert!(spent <= ctl_spent + 60 * 10_000, "{spent} vs {ctl_spent}");
}

/// M7-SEC-2 (b): a trade lands between the keeper's read and its own crank: the keeper's own
/// observation is off its view of that second, but the next read shows the price still there.
#[test]
fn m7_sec_2_b_the_keepers_own_late_crank_starts_no_burst() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(40);
    let trader = h.with_env(|env| env.actor(mm, 50_000_000_000_000, 50_000_000_000_000));
    let (_, _, trade) = h
        .with_env(|env| env.push_price_ix(mm, &trader, env.spot(mm) * 103 / 100))
        .expect("a push");
    let kp = trader.kp.insecure_clone();
    let done = Rc::new(Cell::new(false));
    let d2 = done.clone();
    h.chain().before_next_send(move |env| {
        ok(env.send(&[trade], &[&kp]));
        d2.set(true);
    });
    for _ in 0..40 {
        let r = h.second().clone();
        assert!(r.bursts.is_empty(), "{:?}", r.bursts);
    }
    assert!(done.get(), "the trade ran before the keeper's crank");
    let m = &h.keeper.metrics.markets[&key];
    assert_eq!((m.bursts, m.foreign_moves), (0, 1));
}

/// M7-SEC-2 (c): two keeper instances, a genuine 3 % trade between their reads in a second both
/// observe: neither bursts.
#[test]
fn m7_sec_2_c_two_instances_do_not_trigger_each_other() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let mut b = h.another_keeper(KeeperConfig::default());
    let _ = open(&h, mm, Q);
    let trader = h.with_env(|env| env.actor(mm, 50_000_000_000_000, 50_000_000_000_000));
    for _ in 0..5 {
        h.second();
        tick_other(&mut b);
    }
    h.with_env(|env| {
        let s = env.spot(mm);
        env.push_price(mm, &trader, s * 103 / 100);
    });
    h.second();
    tick_other(&mut b);
    h.second();
    h.with_env(|env| {
        let s = env.spot(mm);
        env.push_price(mm, &trader, s * 103 / 100);
    });
    tick_other(&mut b);
    for _ in 0..5 {
        h.second();
        tick_other(&mut b);
    }
    let a_b = h.keeper.metrics.markets[&key].bursts;
    let b_b = b.metrics.markets.get(&key).map_or(0, |m| m.bursts);
    assert_eq!((a_b, b_b), (0, 0));
}

/// M7-SEC-2 (d): a price round trip still starts the per-slot burst when its legs are separate
/// transactions in one second: the keeper read before, the attacker pushes and cranks, then
/// dumps back; the next read shows the price back where it was.
#[test]
fn m7_sec_2_d_a_round_trip_in_separate_transactions_still_bursts() {
    use shortwin_litesvm_tests::drive::swap_in_ix;
    use shortwin_litesvm_tests::mock::Dir;
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(3);
    let at = h.now();
    h.with_env(|env| {
        let attacker = env.actor(mm, 0, 1_000_000_000_000);
        let (_, amount, pump) = env
            .push_price_ix(mm, &attacker, env.spot(mm) * 12 / 10)
            .unwrap();
        let bought = mock::quote_exact_in(&env.svm, mm.mock(), Dir::QuoteIn, amount)
            .unwrap()
            .proceeds;
        let kp = attacker.kp.insecure_clone();
        ok(env.send(&[pump, crank_price_ix(mm)], &[&kp]));
        let dump = swap_in_ix(mm, &attacker, Dir::BaseIn, bought, 0);
        ok(env.send(&[dump], &[&kp]));
    });
    let r = h.slot().clone();
    assert_eq!(r.bursts, vec![(key, at)], "{r:?}");
}

/// M7-SEC-3 (keeper side): the key holder outside this instance clears the flag of a market the
/// keeper holds degraded — alerted, and set again in the same tick; an open landed while the
/// keeper did not read (clear, the program's clear delay, open, set) is alerted at its next read.
#[test]
fn m7_sec_3_a_clear_this_instance_did_not_send_and_an_open_while_degraded_are_alerted() {
    let (mut h, mms, auth) = pauser_setup(1, pauser_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(5);
    h.idle(45);
    let r = h.second().clone();
    assert!(gap_degraded(&r).is_some() && paused(&h, &key));
    // (1) A clear the keeper did not send.
    let clear = ix::set_coverage_pause(&auth.pubkey(), &key, false, 1);
    let a2 = auth.insecure_clone();
    h.with_env(|env| ok(env.send(&[clear], &[&a2])));
    assert!(!paused(&h, &key));
    let r = h.second().clone();
    assert!(
        r.alerts
            .iter()
            .any(|a| matches!(a, Alert::CoveragePauseCleared { market } if *market == key)),
        "{:?}",
        r.alerts
    );
    assert!(
        r.coverage_pauses
            .iter()
            .any(|p| p.paused && p.result.landed().is_some())
    );
    assert!(paused(&h, &key));
    // (2) Between two of the keeper's reads: clear, the clear delay, an open, set.
    let delay = shortwin::constants::COVERAGE_CLEAR_DELAY_S;
    let (a3, a4) = (auth.insecure_clone(), auth.insecure_clone());
    let position = h.with_env(|env| {
        ok(env.send(
            &[ix::set_coverage_pause(&a3.pubkey(), &key, false, 1)],
            &[&a3],
        ));
        env.warp(delay);
        let mut b = env.borrower();
        let (res, nonce) = env.open(mm, &mut b, Q);
        ok(res);
        ok(env.send(
            &[ix::set_coverage_pause(&a4.pubkey(), &key, true, 1)],
            &[&a4],
        ));
        position_key(mm, &b.key(), nonce)
    });
    let r = h.second().clone();
    assert!(
        r.alerts.iter().any(|a| matches!(
            a,
            Alert::OpenWhileDegraded { market, position: p, .. } if *market == key && *p == position
        )),
        "{:?}",
        r.alerts
    );
    assert_eq!(h.keeper.metrics.markets[&key].opens_while_degraded, 1);
}

/// M7-SEC-8: one real market with a position plus `k` list-only junk markets (each 0.005 SOL
/// of fee and ≈ 0.041 SOL of rent to list). Per tick, from the first one after the listings
/// (their first sight included) for 100 s: `getMultipleAccounts` reads (each a round trip:
/// `RpcChain` sends a read's 100-key chunks in parallel), their chunked requests, and
/// `getProgramAccounts` calls. Returns (mean reads, max reads, mean requests, max requests, max
/// gPA calls) over the last 60 ticks, and the max requests of any tick.
struct JunkRun {
    mean_reads: f64,
    max_reads: usize,
    mean_reqs: f64,
    max_reqs: usize,
    max_gpa: usize,
    first_sight_reqs: usize,
    observations: u64,
}

fn junk_run(k: usize) -> JunkRun {
    use shortwin_litesvm_tests::fixtures;
    use shortwin_litesvm_tests::tokens::{self, TokenProgram};
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let _ = open(&h, mm, Q);
    h.seconds(5);
    h.with_env(|env| {
        let payer = env.payer.insecure_clone();
        let config = env.config();
        let now = env.now();
        for _ in 0..k {
            let auth = solana_keypair::Keypair::new();
            let mint =
                tokens::create_mint(&mut env.svm, &payer, &auth.pubkey(), 6, TokenProgram::Spl);
            let _ = fixtures::install_market(
                &mut env.svm,
                &mint,
                6,
                TokenProgram::Spl,
                &payer.pubkey(),
                now,
                &config,
            );
        }
    });
    let obs0 = h.keeper.metrics.markets[&mm.market()].observations;
    let mut ticks: Vec<(usize, usize, usize)> = Vec::new();
    for _ in 0..100 {
        let before = h.chain().rpc_log().len();
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let log = h.chain().rpc_log();
        let calls = &log[before..];
        let reads = calls
            .iter()
            .filter(|c| matches!(c, Rpc::Accounts(_)))
            .count();
        let reqs: usize = calls.iter().map(Rpc::calls).sum();
        let gpa = calls
            .iter()
            .filter(|c| matches!(c, Rpc::ProgramAccounts))
            .count();
        ticks.push((reads, reqs, gpa));
    }
    let last = &ticks[40..];
    let n = last.len() as f64;
    JunkRun {
        mean_reads: last.iter().map(|t| t.0).sum::<usize>() as f64 / n,
        max_reads: last.iter().map(|t| t.0).max().unwrap_or(0),
        mean_reqs: last.iter().map(|t| t.1).sum::<usize>() as f64 / n,
        max_reqs: last.iter().map(|t| t.1).max().unwrap_or(0),
        max_gpa: ticks.iter().map(|t| t.2).max().unwrap_or(0),
        first_sight_reqs: ticks.iter().map(|t| t.1).max().unwrap_or(0),
        observations: h.keeper.metrics.markets[&mm.market()].observations - obs0,
    }
}

#[test]
fn m7_sec_8_junk_markets_do_not_grow_every_ticks_reads() {
    let mut rows = Vec::new();
    for k in [0usize, 100, 400, 1_000] {
        rows.push((k, junk_run(k)));
    }
    for (k, r) in &rows {
        println!(
            "M7-SEC-8: {k:>5} junk markets: reads (round trips) / tick mean {:.1} max {}; requests / \
             tick mean {:.1} max {}; getProgramAccounts / tick max {}; the first-sight tick {} \
             requests (as built: 4.1 / 9.1 / 24.1 / 54.1 sequential requests a tick at 0 / 100 / \
             400 / 1,000, and 4 getProgramAccounts per junk market in the tick that first saw \
             them)",
            r.mean_reads, r.max_reads, r.mean_reqs, r.max_reqs, r.max_gpa, r.first_sight_reqs
        );
    }
    let base = &rows[0].1;
    for (k, r) in &rows {
        // The real market keeps its idle cadence (≥ 3 observations in 100 s).
        assert!(r.observations >= 3, "{k}: {}", r.observations);
        // Reads: the tick's two, the batched sweep facts, cold markets on their poll.
        assert!(
            r.max_reads <= base.max_reads + 2,
            "{k}: reads {} vs {}",
            r.max_reads,
            base.max_reads
        );
        // Discovery: at most `discoveries_per_tick` (2) markets' four calls, the positions and
        // the markets' own discovery.
        assert!(
            r.max_gpa <= 2 * 4 + 2,
            "{k}: {} getProgramAccounts in a tick",
            r.max_gpa
        );
        // Requests: cold markets (≈ 5 keys each every 30 s, staggered) and the discoveries.
        let bound = base.mean_reqs + 10.0 + (*k as f64) * 5.0 / 30.0 / 100.0 + 2.0;
        assert!(
            r.mean_reqs <= bound,
            "{k}: {} requests vs {bound}",
            r.mean_reqs
        );
    }
}
