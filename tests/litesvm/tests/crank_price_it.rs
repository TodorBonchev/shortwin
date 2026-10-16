//! `crank_price` and the market prologue it runs (ARCH §3.0 steps 2–8, §3.9, §4.4; ECON §5.1–
//! §5.2, §5.8; A16, A17, A59, A63, A64, A69, A72; R3A-9), on the mock venue:
//!
//! - **Coverage gap** (A69, T-COVERAGE's emission part): `CoverageGap` exactly when an
//!   integration follows more than `COVERAGE_GAP_S` = 40 s with open positions — at 40 s not,
//!   at 41 s yes (40 s since the owner's decision of 2026-10-01, OWNER-M7-12; was 30); measured from the last **integration** (`ema_ts`), not from the last crank,
//!   so per-second cranks on a halted venue still report the gap when the venue returns; from
//!   every prologue that rolls (`crank_price`, `open_short`, `close_short`, `deposit`,
//!   `withdraw`); a log event that costs no trace entry; `open_positions` as it stands.
//! - **Same-second floods** (C0-5 c, ECON §5.1–§5.2): any number of cranks and trades in one
//!   second — in one transaction, in many, across slots — fill one bucket and never touch
//!   `P_ema`, `L_ema`, `L_slow`, the ring or the last completed bucket; the bucket is exactly
//!   `observe::record` over the pre-trade states the cranks saw; the next second integrates it
//!   once, exactly as `observe::integrate` (differential); a flood of pushes that shares its
//!   second with an honest observation cannot lift `P_ema` above `max(P_ema, honest low)`
//!   (directional aggregation); a flood on a halted venue records nothing (I16).
//! - **Ring rewrite on a venue change** (A72; T-VENUE-SWITCH's shape): M2 has no venue
//!   transition (`sync_venue` is a no-op until M4–M6), so the program's own
//!   `engine::observe::rewrite_liquidity` is applied to the market's stored observation state
//!   (as a transition would) and written back; the invariant hooks accept it (I13), every ring
//!   entry carries the reset liquidity with its `(x, y)` recomputed, the box is 0 wide in `L`
//!   from the first second, and the cranks continue from it.
//! - **Forced EMA-parameter adoption** (A17, A59, R3A-9; V2-13): with no open position at the
//!   next prologue (also a lender instruction's); with positions only from `force_at` on —
//!   `force_at − 1` not, `force_at` yes; the adopted parameters drive the same prologue's
//!   integration (step 2 before step 5; differential); the ring keeps its entries; two changes
//!   in a row are adopted as the latest; the kink never with positions open, not even in the
//!   close that drains the last one, and at the next prologue after it; position snapshots are
//!   untouched.
//! - The crank never writes the `MarketVault`; `PriceCranked` reports the stored state; T-TRACE
//!   (2 entries, 3 when a route chain starts or clears) and CU.
//!
//! Every instruction goes through `Env::send`, so the invariant hooks run after each one.

#![allow(clippy::result_large_err)]

// The engine tests' shared helpers (not every one is used here).
#[path = "engine_it/common.rs"]
#[allow(unused_imports)]
mod common;

use anchor_lang::solana_program::instruction::Instruction;
use shortwin::events::{CoverageGap, PriceCranked, RouteChainChanged};
use shortwin::state::{ConfigChange, EmaParamsArgs, Market};
use shortwin_litesvm_tests::drive::Actor;
use shortwin_litesvm_tests::fixtures;
use shortwin_litesvm_tests::ix;
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};
use shortwin_math::consts::COVERAGE_GAP_S;
use shortwin_math::observe::{self, Bucket, EmaParams, Observation, VenueChange};
use shortwin_math::quote::{liquidity, reprice};
use shortwin_math::risk::price_wad;
use solana_signer::Signer;

use common::*;

/// ARCH §6.1 `crank_price` transaction limit.
const CRANK_CU_MAX: u64 = 60_000;

// ---- helpers ------------------------------------------------------------------------------

/// Proposes `change`, waits the timelock and applies it (both must succeed).
fn govern(env: &mut Env, change: ConfigChange) {
    let gov = env.governance.insecure_clone();
    ok(env.send(&[ix::propose_config(&gov.pubkey(), change)], &[&gov]));
    env.warp(i64::from(env.config().timelock_s));
    let apply = env.apply_config_ix();
    ok(env.send(&[apply], &[]));
}

fn ema_change(dtmax: u16, clamp: u32, slow_tau: u32, slow_dtmax: u32) -> ConfigChange {
    ConfigChange::EmaParams {
        ema: EmaParamsArgs {
            ema_dtmax_s: dtmax,
            ema_clamp_bps: clamp,
            liq_slow_tau_s: slow_tau,
            liq_slow_dtmax_s: slow_dtmax,
        },
    }
}

/// The pre-trade observation `(P, L)` a short.win instruction records now (the mock's pricing
/// reserves; `VenueState::{price, liquidity}`).
fn observed(env: &Env, mm: &MockMarket) -> (u128, u128) {
    let r = mock::reserves(&env.svm, mm.mock());
    let (x, y) = (u128::from(r.base), u128::from(r.quote));
    (price_wad(x, y).unwrap(), liquidity(x, y).unwrap())
}

fn crank_meta(env: &mut Env, mm: &MockMarket) -> litesvm::types::TransactionMetadata {
    ok(env.send(&[crank_price_ix(mm)], &[]))
}

fn gaps(meta: &litesvm::types::TransactionMetadata) -> Vec<CoverageGap> {
    events::logged(meta)
}

/// Everything an integration writes (the EMA, the ring, the last completed bucket).
fn decision_state(m: &Market) -> (u128, u128, u128, i64, i64, u64, observe::Ring, i64, u128) {
    (
        m.ema_price.get(),
        m.ema_liq.get(),
        m.liq_slow.get(),
        m.ema_ts,
        m.slow_ts,
        m.ema_count,
        m.ring_math(),
        m.lb_sec,
        m.lb_lo.get(),
    )
}

fn obs_eq(a: &Observation, b: &Observation) {
    assert_eq!(a.ema, b.ema, "EMA");
    assert_eq!(a.ring, b.ring, "ring");
    assert_eq!(a.last, b.last, "last completed bucket");
    assert_eq!(a.bucket, b.bucket, "open bucket");
}

/// A market with one open position and a few seconds of per-second cranks.
fn with_position() -> (Env, MockMarket, Borrower, u64) {
    let (mut env, mm) = setup();
    env.crank(&mm);
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 3);
    (env, mm, b, nonce)
}

fn big_actor(env: &mut Env, mm: &MockMarket) -> Actor {
    env.actor(mm, 200_000_000_000_000, 2_000_000_000_000)
}

// ---- coverage gap -------------------------------------------------------------------------

/// `gap = now − ema_ts > 40` emits; `= 40` does not (OWNER-M7-12, 2026-10-01: 40 s, was 30 —
/// the keeper's 30-s idle cadence with 0–2 s of landing jitter, 31–32-s gaps, emits nothing); the
/// event carries the gap and the open positions; it is a log event (no trace entry).
#[test]
fn coverage_gap_boundary_and_fields() {
    let (mut env, mm, _b, _n) = with_position();
    let ema_ts = env.market(&mm.market()).ema_ts;
    assert_eq!(COVERAGE_GAP_S, 40);
    let saved = env.svm.clone();
    // The idle cadence with landing jitter (31 and 32 s) and the boundary itself: no event.
    for gap in [31, 32, COVERAGE_GAP_S] {
        env.svm = saved.clone();
        env.warp(ema_ts + gap - env.now());
        let meta = crank_meta(&mut env, &mm);
        assert!(gaps(&meta).is_empty(), "a gap of {gap} s is not reported");
    }
    env.svm = saved;
    env.warp(ema_ts + COVERAGE_GAP_S + 1 - env.now());
    let meta = crank_meta(&mut env, &mm);
    let g = gaps(&meta);
    assert_eq!(g.len(), 1);
    assert_eq!(
        (g[0].market, g[0].gap_s, g[0].open_positions, g[0].ts),
        (mm.market(), 41, 1, env.now())
    );
    assert_eq!(
        trace_entries(&meta, 0),
        2,
        "CoverageGap is emit!: the crank and its PriceCranked CPI only"
    );
    // The same second again: no roll, no second report.
    let meta = crank_meta(&mut env, &mm);
    assert!(gaps(&meta).is_empty());
}

/// The gap is counted from the last **integration**, not the last crank: per-second cranks on
/// a halted venue record nothing, so the first integration after the venue returns reports
/// the whole outage.
#[test]
fn coverage_gap_counts_from_the_last_integration_not_the_last_crank() {
    let (mut env, mm, _b, _n) = with_position();
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = true);
    // First halted second: rolls the last honest bucket (gap 1), records nothing.
    env.warp(1);
    let meta = crank_meta(&mut env, &mm);
    assert!(gaps(&meta).is_empty());
    let ema_ts = env.market(&mm.market()).ema_ts;
    for _ in 0..40 {
        env.warp(1);
        let meta = crank_meta(&mut env, &mm);
        assert!(gaps(&meta).is_empty(), "no bucket, no roll, no report");
    }
    let m = env.market(&mm.market());
    assert_eq!(m.ema_ts, ema_ts, "the EMA did not move while halted");
    assert_eq!(m.b_lo.get(), 0, "I16: nothing recorded");
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = false);
    env.warp(1);
    let meta = crank_meta(&mut env, &mm);
    assert!(
        gaps(&meta).is_empty(),
        "the first observation opens a bucket; nothing rolls yet"
    );
    env.warp(1);
    let meta = crank_meta(&mut env, &mm);
    let g = gaps(&meta);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].gap_s, env.now() - ema_ts);
    assert_eq!(
        g[0].gap_s, 42,
        "41 halted cranks, then one recording second"
    );
}

/// Without open positions a gap is not reported; with two it reports two.
#[test]
fn coverage_gap_needs_open_positions_and_counts_them() {
    let (mut env, mm) = setup();
    env.crank(&mm);
    env.warp(100);
    let meta = crank_meta(&mut env, &mm);
    assert!(gaps(&meta).is_empty(), "no open position");
    let mut b1 = env.borrower();
    let mut b2 = env.borrower();
    open_ok(&mut env, &mm, &mut b1, Q);
    open_ok(&mut env, &mm, &mut b2, Q / 2);
    env.tick(&mm, 1);
    env.warp(45);
    let meta = crank_meta(&mut env, &mm);
    let g = gaps(&meta);
    assert_eq!(g.len(), 1);
    assert_eq!((g[0].gap_s, g[0].open_positions), (45, 2));
}

/// Every prologue that rolls reports the gap: the lender instructions (A16 step 5), an open and
/// a close — not only the crank.
#[test]
fn coverage_gap_from_every_rolling_prologue() {
    // deposit and withdraw
    let (mut env, mm, _b, _n) = with_position();
    env.warp(50);
    let ema_ts = env.market(&mm.market()).ema_ts;
    let lender = mm.lender.insecure_clone();
    // A small deposit from the market's lender (it holds no tokens left: mint some).
    let payer = env.payer.insecure_clone();
    shortwin_litesvm_tests::tokens::mint_to(
        &mut env.svm,
        &payer,
        &mm.mint(),
        &mm.lender_account,
        &mm.pool.mint_authority,
        1_000_000,
        mm.spec.program,
    );
    let meta = ok(env.send(&[ix::deposit(&mm.lender_keys(), 1_000_000, 0)], &[&lender]));
    let g = gaps(&meta);
    assert_eq!(g.len(), 1, "deposit rolls (A16)");
    assert_eq!(g[0].gap_s, env.now() - ema_ts);
    assert_eq!(g[0].gap_s, 50);
    // The bucket was rolled and nothing recorded: a crank now opens a new one, and a withdraw
    // 45 s later rolls it and reports again (gaps above `COVERAGE_GAP_S` = 40).
    env.warp(1);
    env.crank(&mm);
    env.warp(45);
    let shares = env.lender(&mm.market(), &lender.pubkey()).unwrap().shares / 1_000;
    let meta = ok(env.send(&[ix::withdraw(&mm.lender_keys(), shares, 0)], &[&lender]));
    assert_eq!(gaps(&meta).len(), 1, "withdraw rolls (A16)");

    // open and close
    let (mut env, mm, b, nonce) = with_position();
    env.warp(45);
    let mut b2 = env.borrower();
    let (_o, _n2, meta) = open_ok(&mut env, &mm, &mut b2, Q / 2);
    assert_eq!(gaps(&meta).len(), 1, "open_short's prologue reports");
    env.warp(45);
    let (_c, meta) = close_ok(&mut env, &mm, &b, nonce);
    let g = gaps(&meta);
    assert_eq!(g.len(), 1, "close_short's prologue reports");
    assert_eq!(
        g[0].open_positions, 2,
        "open_positions before the close's own update"
    );
}

// ---- same-second floods -------------------------------------------------------------------

/// A flood of cranks and trades inside one second: one bucket, nothing integrated, the
/// bucket equal to `observe::record` over what the cranks saw; the next second integrates it
/// once, exactly as `observe::integrate` does.
#[test]
fn same_second_flood_is_one_bucket_and_one_integration() {
    let (mut env, mm, _b, _n) = with_position();
    let actor = big_actor(&mut env, &mm);
    env.warp(1);
    let t = env.now();
    let mut seen: Vec<(u128, u128)> = Vec::new();

    // 20 cranks in one transaction (the hooks check after each).
    let flood: Vec<Instruction> = (0..20).map(|_| crank_price_ix(&mm)).collect();
    seen.extend(std::iter::repeat_n(observed(&env, &mm), 20));
    let meta = ok(env.send(&flood, &[]));
    assert_eq!(events::of::<PriceCranked>(&meta).len(), 20);
    let after_first = env.market(&mm.market());
    assert_eq!(after_first.b_sec, t);
    let frozen = decision_state(&after_first);

    // Trades and cranks in many transactions and slots of the same second.
    let spot0 = env.spot(&mm);
    for (k, pct) in [105u128, 97, 112, 90, 101, 130, 88].into_iter().enumerate() {
        let c = env.clock();
        env.set_clock(t, c.slot + 1 + k as u64);
        env.push_price(&mm, &actor, spot0 * pct / 100);
        seen.push(observed(&env, &mm));
        env.crank(&mm);
        // A trade and a crank in one transaction: the crank sees the post-trade pool.
        if k % 2 == 0 {
            let (_, _, push) = env
                .push_price_ix(&mm, &actor, spot0 * (pct + 3) / 100)
                .unwrap();
            let saved = env.svm.clone();
            ok(env.send(std::slice::from_ref(&push), &[&actor.kp]));
            let after_push = observed(&env, &mm);
            env.svm = saved;
            ok(env.send(&[push, crank_price_ix(&mm)], &[&actor.kp]));
            seen.push(after_push);
        }
        let m = env.market(&mm.market());
        assert_eq!(
            decision_state(&m),
            frozen,
            "nothing integrates within the second"
        );
    }
    // The bucket is `record` over every observation of the second.
    let mut want = Bucket::EMPTY;
    for (p, l) in &seen {
        observe::record(&mut want, t, *p, *l).unwrap();
    }
    let m = env.market(&mm.market());
    assert_eq!(m.observation().bucket, want, "the flood's bucket");
    assert!(want.hi > want.lo);

    // The next second: exactly one integration, as `integrate` computes it.
    env.warp(1);
    let pre = m.observation();
    let params = m.ema_params();
    let now_obs = observed(&env, &mm);
    let meta = crank_meta(&mut env, &mm);
    let post = env.market(&mm.market());
    assert_eq!(post.ema_count, m.ema_count + 1, "one integration");
    let mut expect = *pre;
    let gap = expect.roll(env.now(), &params).unwrap();
    assert_eq!(gap, Some(1));
    expect.record(env.now(), now_obs.0, now_obs.1).unwrap();
    obs_eq(&post.observation(), &expect);
    let cranked: PriceCranked = events::one(&meta);
    assert_eq!(
        (cranked.ema_price, cranked.ema_liq, cranked.ema_count),
        (post.ema_price.get(), post.ema_liq.get(), post.ema_count)
    );
}

/// Pushes that share their second with an honest observation cannot lift `P_ema` above
/// `max(P_ema, honest low)` (directional aggregation, ECON §5.2): a +50 % flood after the
/// keeper's crank leaves at most a freeze.
#[test]
fn a_pushed_flood_with_an_honest_observation_cannot_lift_the_ema() {
    let (mut env, mm, _b, _n) = with_position();
    let actor = big_actor(&mut env, &mm);
    env.warp(1);
    env.crank(&mm); // the keeper's honest observation of this second
    let honest = observed(&env, &mm).0;
    let spot = env.spot(&mm);
    for pct in [120u128, 135, 150] {
        env.push_price(&mm, &actor, spot * pct / 100);
        env.crank(&mm);
    }
    let m0 = env.market(&mm.market());
    assert!(m0.b_hi.get() > honest * 14 / 10);
    env.warp(1);
    env.crank(&mm);
    let m1 = env.market(&mm.market());
    let cap = m0.ema_price.get().max(honest);
    assert!(
        m1.ema_price.get() <= cap,
        "P_ema {} above max(P_ema {}, honest {honest})",
        m1.ema_price.get(),
        m0.ema_price.get()
    );
}

/// Slots of one second are one bucket; a flood on a halted venue records nothing (I16).
#[test]
fn slots_share_the_second_and_a_halted_flood_records_nothing() {
    let (mut env, mm, _b, _n) = with_position();
    env.warp(1);
    let t = env.now();
    env.crank(&mm);
    let m0 = env.market(&mm.market());
    for k in 1..=5u64 {
        let c = env.clock();
        env.set_clock(t, c.slot + k);
        env.crank(&mm);
    }
    let m1 = env.market(&mm.market());
    assert_eq!(decision_state(&m1), decision_state(&m0));
    assert_eq!(m1.b_sec, t);
    // Halted: a flood in the next seconds records nothing and integrates nothing after the
    // first roll.
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = true);
    env.warp(1);
    let flood: Vec<Instruction> = (0..10).map(|_| crank_price_ix(&mm)).collect();
    ok(env.send(&flood, &[]));
    let m2 = env.market(&mm.market());
    assert_eq!(m2.ema_count, m1.ema_count + 1, "the honest bucket rolled");
    assert_eq!(m2.b_lo.get(), 0, "nothing recorded while halted");
    env.warp(1);
    ok(env.send(&flood, &[]));
    let m3 = env.market(&mm.market());
    assert_eq!(decision_state(&m3), decision_state(&m2));
}

/// Floods cost the same per crank: the first crank of a second (roll + record) and the rest
/// (record only) stay within the §6.1 limit, and each is 2 trace entries.
#[test]
fn flood_cost_per_crank() {
    let (mut env, mm, _b, _n) = with_position();
    env.warp(1);
    let first = crank_meta(&mut env, &mm);
    let mut rest = Vec::new();
    for _ in 0..30 {
        let meta = crank_meta(&mut env, &mm);
        assert_eq!(trace_entries(&meta, 0), 2);
        rest.push(meta.compute_units_consumed);
    }
    let (lo, hi) = (*rest.iter().min().unwrap(), *rest.iter().max().unwrap());
    println!(
        "crank_price: first of the second {} CU, flood {lo}–{hi} CU",
        first.compute_units_consumed
    );
    assert!(first.compute_units_consumed <= CRANK_CU_MAX);
    assert!(hi <= CRANK_CU_MAX);
    assert!(hi - lo <= 200, "a flood crank does not get dearer");
}

// ---- ring rewrite on a venue change -------------------------------------------------------

fn write_market(env: &mut Env, mm: &MockMarket, m: &Market) {
    let extra =
        lamports(env, &mm.market()) - env.svm.minimum_balance_for_rent_exemption(Market::SPACE);
    fixtures::set_zc(&mut env.svm, &mm.market(), m, extra);
}

/// A72 / T-VENUE-SWITCH (M2 shape): the program's `rewrite_liquidity` on a full ring, stored
/// as a transition would store it; the hooks accept it, the ring carries the new liquidity,
/// the box is 0 wide in `L`, and the cranks continue from it. `RaydiumLocked` is the variant
/// the program's adoptions use since M4-SEC-5 / M4-SEC-6 (`L_slow` to at most the locked
/// depth, here half the new pool's liquidity).
#[test]
fn ring_rewrite_on_a_venue_change() {
    for which in 0..3 {
        let (mut env, mm, _b, _n) = with_position();
        let actor = big_actor(&mut env, &mm);
        // A full ring of varied seconds.
        let spot = env.spot(&mm);
        for pct in [101u128, 103, 99, 104, 102, 106, 100, 105, 103] {
            env.push_price(&mm, &actor, spot * pct / 100);
            env.tick(&mm, 1);
        }
        let m0 = env.market(&mm.market());
        assert_eq!(usize::from(m0.ring_count), observe::RING_LEN);
        // The "new venue": half the pool's liquidity removed.
        let r = mock::reserves(&env.svm, mm.mock());
        let removed = mock::remove_liquidity(&mut env, &mm.pool, r.base / 2, r.quote / 2);
        ok(removed);
        let l_new = observed(&env, &mm).1;
        assert!(l_new < m0.ema_liq.get());
        let kind = match which {
            0 => VenueChange::Graduation,
            1 => VenueChange::Raydium,
            _ => VenueChange::RaydiumLocked {
                slow_cap: l_new / 2,
            },
        };
        let mut obs = m0.observation();
        shortwin::engine::observe::rewrite_liquidity(&mut obs, l_new, kind).unwrap();
        let mut m = m0;
        m.set_observation(&obs);
        write_market(&mut env, &mm, &m);
        // The hooks accept the rewritten state (I13 reprojection, newest = EMA, I6 …).
        env.assert_invariants();
        let m1 = env.market(&mm.market());
        let l = m1.ema_liq.get();
        assert_eq!(l, l_new, "{kind:?}: L_ema reset (the new pool is thinner)");
        match kind {
            VenueChange::Graduation => assert_eq!(m1.liq_slow.get(), l_new),
            VenueChange::Raydium => {
                assert_eq!(m1.liq_slow.get(), l_new.min(m0.liq_slow.get()))
            }
            VenueChange::RaydiumLocked { slow_cap } => {
                assert_eq!(m1.liq_slow.get(), slow_cap.min(m0.liq_slow.get()))
            }
        }
        let (r0, r1) = (m0.ring_math(), m1.ring_math());
        for (a, b) in r0.iter().zip(r1.iter()) {
            assert_eq!((b.ts, b.p, b.lo, b.hi), (a.ts, a.p, a.lo, a.hi));
            assert_eq!((b.l, b.llo, b.lhi), (l, l, l));
            assert_eq!(Some((b.x, b.y)), reprice(l, b.p));
        }
        assert_eq!(m1.b_lo.get(), 0, "the open bucket is cleared");
        assert_eq!(
            (m1.ema_price.get(), m1.ema_ts, m1.ema_count, m1.lb_sec),
            (m0.ema_price.get(), m0.ema_ts, m0.ema_count, m0.lb_sec)
        );
        let (_, _, l_mlo, l_mhi) = observe::ring_medians(&r1).unwrap();
        assert_eq!((l_mlo, l_mhi), (l, l), "the box is 0 wide in L");
        // The cranks continue from the rewritten state; every step is `integrate`'s.
        for _ in 0..3 {
            env.warp(1);
            let pre = env.market(&mm.market());
            let now_obs = observed(&env, &mm);
            env.crank(&mm);
            let mut expect = *pre.observation();
            expect.roll(env.now(), &pre.ema_params()).unwrap();
            expect.record(env.now(), now_obs.0, now_obs.1).unwrap();
            obs_eq(&env.market(&mm.market()).observation(), &expect);
        }
        let m2 = env.market(&mm.market());
        let (_, _, l_mlo, l_mhi) = observe::ring_medians(&m2.ring_math()).unwrap();
        let (lo, hi) = (l_mlo.min(m2.ema_liq.get()), l_mhi.max(m2.ema_liq.get()));
        assert!(
            (hi - lo) * 100 <= lo,
            "{kind:?}: the box stays within 1 % in L"
        );
    }
}

// ---- forced EMA-parameter adoption --------------------------------------------------------

/// With no open position the next prologue adopts new EMA parameters at once — a crank, or a
/// lender instruction (A16 runs step 2).
#[test]
fn ema_params_adopted_at_once_without_positions() {
    for via_deposit in [false, true] {
        let (mut env, mm) = setup();
        env.crank(&mm);
        let v0 = env.market(&mm.market()).ema_params_version;
        govern(&mut env, ema_change(1, 6_000, 7_200, 30));
        let config = env.config();
        assert!(env.now() < config.ema_params.force_at);
        assert_eq!(env.market(&mm.market()).ema_params_version, v0);
        if via_deposit {
            let lender = mm.lender.insecure_clone();
            let payer = env.payer.insecure_clone();
            shortwin_litesvm_tests::tokens::mint_to(
                &mut env.svm,
                &payer,
                &mm.mint(),
                &mm.lender_account,
                &mm.pool.mint_authority,
                1_000,
                mm.spec.program,
            );
            ok(env.send(&[ix::deposit(&mm.lender_keys(), 1_000, 0)], &[&lender]));
        } else {
            env.crank(&mm);
        }
        let m = env.market(&mm.market());
        assert_eq!(m.ema_params_version, config.ema_params.version);
        assert_eq!(
            (
                m.ema_dtmax_s,
                m.ema_clamp_bps,
                m.liq_slow_tau_s,
                m.liq_slow_dtmax_s
            ),
            (1, 6_000, 7_200, 30),
            "via deposit: {via_deposit}"
        );
    }
}

/// With a position open: `force_at − 1` keeps the old parameters, `force_at` adopts; the
/// adopted parameters drive that same prologue's integration (sync is step 2, the roll step
/// 5), exactly as `integrate` with the new `EmaParams` and not the old; the ring keeps its
/// entries; the position's snapshot is untouched; the kink is not adopted.
#[test]
fn forced_adoption_boundary_and_same_prologue_effect() {
    let (mut env, mm, b, nonce) = with_position();
    let actor = big_actor(&mut env, &mm);
    let p0 = env.position(&mm, &b.key(), nonce);
    let old = env.market(&mm.market());
    govern(&mut env, ema_change(1, 5_000, 600, 1));
    govern(
        &mut env,
        ConfigChange::Kink {
            util_kink_bps: 7_500,
        },
    );
    let config = env.config();
    let force_at = config.ema_params.force_at;
    // Up to force_at − 2, per-second cranks keep the old parameters.
    env.warp(force_at - 2 - env.now());
    env.crank(&mm);
    // Push the price so the integration at the boundary has an observation off P_ema, and
    // leave a 2 s gap so Δt_max (2 → 1) matters.
    let spot = env.spot(&mm);
    env.push_price(&mm, &actor, spot * 180 / 100);
    env.crank(&mm);
    let saved = env.svm.clone();
    env.warp(1); // force_at − 1
    env.crank(&mm);
    let m = env.market(&mm.market());
    assert_eq!(env.now(), force_at - 1);
    assert_eq!(m.ema_params_version, old.ema_params_version, "not yet");
    assert_eq!(
        (m.ema_dtmax_s, m.ema_clamp_bps),
        (old.ema_dtmax_s, old.ema_clamp_bps)
    );
    env.svm = saved;
    env.warp(2); // force_at, a 2 s gap
    assert_eq!(env.now(), force_at);
    let pre = env.market(&mm.market());
    let now_obs = observed(&env, &mm);
    env.crank(&mm);
    let post = env.market(&mm.market());
    assert_eq!(post.ema_params_version, config.ema_params.version);
    assert_eq!(
        (
            post.ema_dtmax_s,
            post.ema_clamp_bps,
            post.liq_slow_tau_s,
            post.liq_slow_dtmax_s
        ),
        (1, 5_000, 600, 1)
    );
    let new_params = EmaParams::new(1, 5_000, 600, 1);
    let mut expect = *pre.observation();
    expect.roll(force_at, &new_params).unwrap();
    expect.record(force_at, now_obs.0, now_obs.1).unwrap();
    obs_eq(&post.observation(), &expect);
    let mut with_old = *pre.observation();
    with_old.roll(force_at, &pre.ema_params()).unwrap();
    assert_ne!(
        with_old.ema, expect.ema,
        "the old parameters would have integrated differently"
    );
    // The ring kept its entries: the new one on top of the old seven.
    let (r0, r1) = (pre.ring_math(), post.ring_math());
    for k in 0..observe::RING_LEN - 1 {
        assert_eq!(r1.get(k + 1), r0.get(k));
    }
    // The kink waits for zero positions; the position's terms are the open's.
    assert_eq!(
        (post.kink_version, post.util_kink_bps),
        (old.kink_version, old.util_kink_bps)
    );
    assert_eq!(env.position(&mm, &b.key(), nonce), p0);
}

/// Two EMA changes before any adoption (a position open): the market adopts the latest one's
/// values and version directly. `force_at` is the config's current one — the second apply's —
/// so the first change's `force_at` passing adopts nothing: each apply restarts the force
/// delay (recorded in the report: a governance change more often than every
/// `ema_force_delay_s` defers a forced adoption indefinitely; zero positions still adopt).
#[test]
fn two_changes_adopt_the_latest() {
    let (mut env, mm, _b, _n) = with_position();
    let v0 = env.market(&mm.market()).ema_params_version;
    govern(&mut env, ema_change(1, 7_000, 1_200, 10));
    let first = env.config().ema_params;
    govern(&mut env, ema_change(2, 8_000, 2_400, 20));
    let second = env.config().ema_params;
    assert!(second.version > first.version);
    // The first change's force_at is gone: the config keeps the latest.
    assert_eq!(
        second.force_at - first.force_at,
        i64::from(env.config().timelock_s)
    );
    let first_force_at = second.force_at - i64::from(env.config().timelock_s);
    env.warp(first_force_at - env.now());
    env.crank(&mm);
    assert_eq!(env.market(&mm.market()).ema_params_version, v0);
    env.warp(second.force_at - env.now());
    env.crank(&mm);
    let m = env.market(&mm.market());
    assert_eq!(
        (
            m.ema_params_version,
            m.ema_dtmax_s,
            m.ema_clamp_bps,
            m.liq_slow_tau_s,
            m.liq_slow_dtmax_s
        ),
        (second.version, 2, 8_000, 2_400, 20),
        "the latest set, never the first"
    );
}

/// R3A-9: a kink change waits for zero positions — not adopted by the close that drains the
/// last position (its prologue ran with one open), adopted by the next prologue.
#[test]
fn the_kink_waits_for_the_prologue_after_the_last_close() {
    let (mut env, mm, b, nonce) = with_position();
    let old = env.market(&mm.market());
    govern(
        &mut env,
        ConfigChange::Kink {
            util_kink_bps: 7_000,
        },
    );
    let kink = env.config().kink;
    env.crank(&mm);
    assert_eq!(env.market(&mm.market()).kink_version, old.kink_version);
    close_ok(&mut env, &mm, &b, nonce);
    let m = env.market(&mm.market());
    assert_eq!(m.open_positions, 0);
    assert_eq!(
        m.kink_version, old.kink_version,
        "the close's own prologue saw one open position"
    );
    env.warp(1);
    env.crank(&mm);
    let m = env.market(&mm.market());
    assert_eq!((m.kink_version, m.util_kink_bps), (kink.version, 7_000));
}

// ---- other crank properties ---------------------------------------------------------------

/// `crank_price` never writes the `MarketVault` (ARCH §3.9's account list has none): a running
/// vest is released on `Market` alone, and the vault's bytes and lamports stay.
#[test]
fn the_crank_never_writes_the_vault() {
    let (mut env, mm) = setup();
    let griefer = env.funded_keypair();
    // A donation to the trader becomes a vest at the next leg.
    let mut data = vec![2, 0, 0, 0];
    data.extend_from_slice(&21_600_000u64.to_le_bytes());
    let donate = Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![
            anchor_lang::solana_program::instruction::AccountMeta::new(griefer.pubkey(), true),
            anchor_lang::solana_program::instruction::AccountMeta::new(mm.keys.trader, false),
        ],
        data,
    };
    ok(env.send(&[donate], &[&griefer]));
    let mut b = env.borrower();
    open_ok(&mut env, &mm, &mut b, Q);
    let vault0 = env.svm.get_account(&mm.keys.market_vault).unwrap();
    let vest0 = env.market(&mm.market()).vest_bal;
    assert!(vest0 > 0);
    env.warp(3_600);
    env.crank(&mm);
    env.tick(&mm, 5);
    assert!(
        env.market(&mm.market()).vest_bal < vest0,
        "the vest released"
    );
    let vault1 = env.svm.get_account(&mm.keys.market_vault).unwrap();
    assert_eq!(vault1.data, vault0.data);
    assert_eq!(vault1.lamports, vault0.lamports);
}

/// `PriceCranked` reports the state it leaves; a crank that starts or clears a route chain
/// adds `RouteChainChanged` (3 trace entries: ARCH §6.2 lists 2 for `crank_price`).
#[test]
fn events_and_trace() {
    let (mut env, mm, _b, _n) = with_position();
    env.warp(1);
    let meta = crank_meta(&mut env, &mm);
    let m = env.market(&mm.market());
    let e: PriceCranked = events::one(&meta);
    assert_eq!(
        (
            e.market,
            e.ema_price,
            e.ema_liq,
            e.liq_slow,
            e.ema_count,
            e.lb_lo,
            e.lb_hi,
            e.tier,
            e.vest_bal,
            e.rent_basis
        ),
        (
            mm.market(),
            m.ema_price.get(),
            m.ema_liq.get(),
            m.liq_slow.get(),
            m.ema_count,
            m.lb_lo.get(),
            m.lb_hi.get(),
            m.tier,
            m.vest_bal,
            m.basis().j
        )
    );
    assert_eq!(e.spot_wad, observed(&env, &mm).0);
    assert_eq!(trace_entries(&meta, 0), 2);
    assert!(meta.compute_units_consumed <= CRANK_CU_MAX);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = true);
    env.warp(1);
    let meta = crank_meta(&mut env, &mm);
    assert_eq!(events::of::<RouteChainChanged>(&meta).len(), 1);
    assert_eq!(trace_entries(&meta, 0), 3, "a chain start adds its event");
    let e: PriceCranked = events::one(&meta);
    assert_eq!(e.spot_wad, 0, "not executable: no spot");
    println!(
        "crank_price with a chain change: {} CU",
        meta.compute_units_consumed
    );
}
