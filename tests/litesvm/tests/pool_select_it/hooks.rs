//! The I15 hook (`invariants::pool_select`) and I17's M4 part fire on what a program mutant
//! would leave: each case starts from a real, passing adoption, window or open on the fork and
//! changes one thing — the state before, the event, or the state after.

use shortwin::events::{
    Open, POOL_KIND_FAST, POOL_KIND_LISTING_WINDOW, PoolProposed, PoolSwitched,
};
use shortwin_litesvm_tests::invariants::{self, IxEvents, TxSnapshot, pool_select::PoolFacts};

use crate::common::*;
use crate::support::*;

/// I15 on `ev` against `pre` (the pool-selection part alone).
fn i15(env: &Env, pre: &TxSnapshot, ev: &IxEvents) -> Result<(), &'static str> {
    invariants::pool_select::check_ix(
        &env.svm,
        &pre.positions,
        &PoolFacts {
            created: &ev.created,
            opens: &ev.opens,
            switched: &ev.switched,
            proposed: &ev.proposed,
            cancelled: &ev.cancelled,
            venue_changed: &ev.venue_changed,
        },
    )
    .map_err(|v| v.invariant)
}

/// A zero-position adoption on Fartcoin: the environment after it, the snapshot before it and
/// its events.
fn adopted() -> (Env, RayMarket, TxSnapshot, IxEvents) {
    let (mut env, rm, _) = setup(FART);
    let lam = lambda_of(&env, &rm.pool);
    let next = plant(&mut env, &rm.pool, Plant::Cpmm, true, (2, 1), lam);
    env.warp(1);
    let pre = invariants::snapshot(&env.svm, &[rm.market()]);
    let meta = ok(env.propose(Some(&rm.pool), &next));
    let ev = IxEvents::of(&meta, 0);
    assert_eq!(ev.switched.len(), 1);
    let rm2 = rm.on_pool(&env.svm, next);
    (env, rm2, pre, ev)
}

fn switched(ev: &IxEvents, f: impl FnOnce(&mut PoolSwitched)) -> IxEvents {
    let s = &ev.switched[0];
    let mut e = PoolSwitched {
        market: s.market,
        slot: s.slot,
        ts: s.ts,
        candidate: s.candidate,
        kind: s.kind,
        d_cand: s.d_cand,
        d_inc: s.d_inc,
        window_ends: s.window_ends,
    };
    f(&mut e);
    IxEvents {
        switched: vec![e],
        ..IxEvents::default()
    }
}

#[test]
fn i15_adoption_rules() {
    let (mut env, rm, pre, ev) = adopted();
    let same = switched(&ev, |_| {});
    assert_eq!(i15(&env, &pre, &same), Ok(()), "the real adoption passes");
    assert!(invariants::check_ix(&env.svm, &pre, false, &same).is_ok());
    // Decision 7: a non-fast adoption with a position open.
    let mut positions = pre.clone();
    positions.positions.markets[0].state.open_positions = 1;
    assert_eq!(i15(&env, &positions, &same), Err("I15"));
    // The wrong kind: a listing-window adoption after the window, a fast one without a window.
    for kind in [POOL_KIND_LISTING_WINDOW, POOL_KIND_FAST, 9] {
        assert_eq!(
            i15(&env, &pre, &switched(&ev, |e| e.kind = kind)),
            Err("I15"),
            "kind {kind}"
        );
    }
    // The event's figures.
    assert_eq!(
        i15(&env, &pre, &switched(&ev, |e| e.d_cand += 1)),
        Err("I15")
    );
    assert_eq!(
        i15(&env, &pre, &switched(&ev, |e| e.d_inc -= 1)),
        Err("I15")
    );
    assert_eq!(
        i15(&env, &pre, &switched(&ev, |e| e.window_ends += 1)),
        Err("I15")
    );
    assert_eq!(
        i15(
            &env,
            &pre,
            &switched(&ev, |e| e.candidate = Pubkey::new_unique())
        ),
        Err("I15")
    );
    // No event for a pool change.
    assert_eq!(i15(&env, &pre, &IxEvents::default()), Err("I15"));
    // I17: an adoption of this CPMM pool while the CPMM program was flagged just before; the
    // AMM v4 flag alone does not latch it (M4-SEC-8: the candidate's own program only).
    for (bit, want) in [
        (shortwin::constants::GUARD_RAYDIUM_CPMM, Err("I17")),
        (shortwin::constants::GUARD_RAYDIUM_V4, Ok(())),
    ] {
        let mut latched = pre.clone();
        if let Some(g) = latched.guard.as_mut() {
            g.upgraded |= 1 << bit;
        }
        assert_eq!(
            invariants::check_ix(&env.svm, &latched, false, &same).map_err(|v| v.invariant),
            want,
            "flag {bit}"
        );
    }
    // The state after: each effect undone once (M4R: `L_ema` and the ring the anchor, `L_slow`
    // at most its locked depth, the restart marks).
    let market = rm.market();
    let good = env.svm.get_account(&market).unwrap();
    type Mutate = fn(&mut Vec<u8>);
    let mutants: [(&str, Mutate); 10] = [
        ("ring l", |d| d[1_712 + 24] ^= 1),
        ("L_ema", |d| d[1_656] ^= 1),
        ("ema_warm", |d| d[22] = 7),
        ("ema_guard", |d| d[23] = 0),
        ("L_slow above D", |d| d[1_672 + 8] ^= 1),
        ("pool_fail chain", |d| d[1_216] = 1),
        ("S3 chain", |d| d[1_200] = 1),
        ("window", |d| d[1_160] = 1),
        ("graduated_at", |d| d[1_184] ^= 1),
        ("pool_seen_at", |d| d[928] ^= 1),
    ];
    for (what, mutate) in mutants {
        let mut acc = good.clone();
        mutate(&mut acc.data);
        env.svm.set_account(market, acc).unwrap();
        assert_eq!(i15(&env, &pre, &same), Err("I15"), "{what}");
    }
    env.svm.set_account(market, good).unwrap();
    // The adopted pool not eligible: every LP token redeemable (λ = 0, so D = 0, under the
    // floor; λ merely under λ_min is no ineligibility since M4-SEC-1).
    let counter = raydium::lp_counter(&env.svm, &rm.pool);
    raydium::set_mint_supply(&mut env.svm, &raydium::lp_mint(&rm.pool), counter);
    assert_eq!(i15(&env, &pre, &same), Err("I15"), "ineligible");
}

/// The collection window: a `PoolProposed` without an armed fast path, and `fp_*` changing
/// without an event.
#[test]
fn i15_window_rules() {
    let (mut env, rm, q) = setup(FART);
    let mut b = env.borrower();
    open_ok(&mut env, &rm, &mut b, q);
    halt(&mut env, &rm.pool);
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    let pre = invariants::snapshot(&env.svm, &[rm.market()]);
    let meta = ok(env.propose(Some(&rm.pool), &cand));
    let ev = IxEvents::of(&meta, 0);
    assert_eq!(i15(&env, &pre, &ev), Ok(()));
    let p: &PoolProposed = &ev.proposed[0];
    let copy = |f: fn(&mut PoolProposed)| {
        let mut e = PoolProposed {
            market: p.market,
            slot: p.slot,
            ts: p.ts,
            candidate: p.candidate,
            kind: p.kind,
            d_cand: p.d_cand,
            d_inc: p.d_inc,
            window_ends: p.window_ends,
        };
        f(&mut e);
        IxEvents {
            proposed: vec![e],
            ..IxEvents::default()
        }
    };
    assert_eq!(i15(&env, &pre, &copy(|e| e.d_cand += 1)), Err("I15"));
    // M4V-1: a window candidate under the window floor (a `pool_min_real_sol` its locked SOL
    // misses while its locked SOL over `λ_min` clears it).
    let config = env.config();
    let p_ema = env.market(&rm.market()).ema_price.get();
    let locked = shortwin_math::tier::locked_sol(d_of(&env, &cand), p_ema);
    let mut tight = config;
    tight.params.pool_min_real_sol = u64::try_from(locked + 1).unwrap();
    shortwin_litesvm_tests::fixtures::set_zc(&mut env.svm, &pda::config(), &tight, 0);
    assert_eq!(i15(&env, &pre, &ev), Err("I15"), "under the window floor");
    shortwin_litesvm_tests::fixtures::set_zc(&mut env.svm, &pda::config(), &config, 0);
    assert_eq!(i15(&env, &pre, &ev), Ok(()));
    // Not armed: the chain younger than pool_fast_route_s just before.
    let mut young = pre.clone();
    young.positions.markets[0].state.pool_fail_since += 1;
    let market = rm.market();
    let good = env.svm.get_account(&market).unwrap();
    let mut acc = good.clone();
    let since = i64::from_le_bytes(acc.data[1_216..1_224].try_into().unwrap()) + 1;
    acc.data[1_216..1_224].copy_from_slice(&since.to_le_bytes());
    env.svm.set_account(market, acc).unwrap();
    assert_eq!(i15(&env, &young, &ev), Err("I15"), "not armed");
    env.svm.set_account(market, good.clone()).unwrap();
    // M4R-2: the window floor at min(P_ema, P_cand). A stale-high `P_ema` (4×, as after a dump
    // during the halt) with a `pool_min_real_sol` the candidate's locked SOL clears at `P_ema`
    // but not at its own spot: the proposal must have been refused.
    let p_cand = {
        let (x, y) = cand
            .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
            .unwrap();
        u128::from(y) * shortwin_math::num::WAD / u128::from(x)
    };
    let d = d_of(&env, &cand);
    let high = p_ema.max(p_cand) * 4;
    let mut acc = good.clone();
    acc.data[1_640..1_656].copy_from_slice(&high.to_le_bytes());
    env.svm.set_account(market, acc).unwrap();
    let mut tight = config;
    tight.params.pool_min_real_sol =
        u64::try_from(shortwin_math::tier::locked_sol(d, p_cand) + 1).unwrap();
    assert!(shortwin_math::pool_select::position_floor_ok(
        d,
        high,
        tight.params.pool_min_real_sol
    ));
    shortwin_litesvm_tests::fixtures::set_zc(&mut env.svm, &pda::config(), &tight, 0);
    assert_eq!(i15(&env, &pre, &ev), Err("I15"), "floor at the stale P_ema");
    shortwin_litesvm_tests::fixtures::set_zc(&mut env.svm, &pda::config(), &config, 0);
    // The retired `fp_lambda_bps` bytes stay zero: a change is a window change without an event.
    let mut acc = good.clone();
    acc.data[1_170] ^= 1;
    env.svm.set_account(market, acc).unwrap();
    assert_eq!(i15(&env, &pre, &IxEvents::default()), Err("I15"), "_pad1a");
    env.svm.set_account(market, good).unwrap();
    // fp_* changed with no event.
    assert_eq!(i15(&env, &pre, &IxEvents::default()), Err("I15"));
}

/// M4-SEC-2 in the hook: a fast adoption whose arming chain did not hold — the chain just
/// before started less than `pool_fast_route_s` before the window opened (it had been cleared
/// and restarted), or its last observation is more than `S3_OBS_GAP_S` old (this observation
/// restarts it) — fails I15; the real execution passes.
#[test]
fn i15_fast_adoption_needs_the_arming_chain() {
    let (mut env, rm, q) = setup(FART);
    let mut b = env.borrower();
    open_ok(&mut env, &rm, &mut b, q);
    halt(&mut env, &rm.pool);
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    let pre = invariants::snapshot(&env.svm, &[rm.market()]);
    let meta = ok(env.execute_switch(&rm.pool, &cand));
    let ev = IxEvents::of(&meta, 0);
    assert_eq!(ev.switched.len(), 1);
    assert_eq!(i15(&env, &pre, &ev), Ok(()));
    let st = pre.positions.markets[0].state;
    let route = i64::from(env.config().params.pool_fast_route_s);
    let mut restarted = pre.clone();
    restarted.positions.markets[0].state.pool_fail_since = st.fp_t0 - route + 1;
    assert_eq!(i15(&env, &restarted, &ev), Err("I15"), "restarted chain");
    let mut gap = pre.clone();
    gap.positions.markets[0].state.last_pool_fail_obs = env.now() - 61;
    assert_eq!(i15(&env, &gap, &ev), Err("I15"), "a gap restarts the chain");
    let mut cleared = pre.clone();
    cleared.positions.markets[0].state.pool_fail_since = 0;
    cleared.positions.markets[0].state.last_pool_fail_obs = 0;
    assert_eq!(i15(&env, &cleared, &ev), Err("I15"), "cleared chain");
    // M4V-1 in the hook: the same adoption under a `pool_min_real_sol` its locked SOL misses but
    // its locked SOL over `λ_min` clears (the listing floor passes, the window floor does not).
    let config = env.config();
    let adopted = env.market(&rm.market());
    let d = d_of(&env, &cand);
    let locked = shortwin_math::tier::locked_sol(d, adopted.ema_price.get());
    let mut tight = config;
    tight.params.pool_min_real_sol = u64::try_from(locked + 1).unwrap();
    assert!(shortwin_math::pool_select::locked_floor_ok(
        d,
        adopted.ema_price.get(),
        u64::from(tight.params.pool_lock_min_bps),
        tight.params.pool_min_real_sol
    ));
    shortwin_litesvm_tests::fixtures::set_zc(&mut env.svm, &pda::config(), &tight, 0);
    assert_eq!(i15(&env, &pre, &ev), Err("I15"), "under the window floor");
    shortwin_litesvm_tests::fixtures::set_zc(&mut env.svm, &pda::config(), &config, 0);
    assert_eq!(i15(&env, &pre, &ev), Ok(()));
}

/// Switch only if all fit (owner decision of 2026-09-29) in the hook: a real window proposal and
/// its execution with a position that fits pass; the same instructions with the position's own
/// `q` (in the state before) above the candidate's per-position cap — or two such positions
/// whose sum is above its OI cap — fail I15. The state part: a mark `max_q_hwm` below an open
/// position's `q`, or non-zero with no position open, fails I15.
#[test]
fn i15_switch_only_if_all_fit() {
    let (mut env, rm, q) = setup(FART);
    let mut b = env.borrower();
    open_ok(&mut env, &rm, &mut b, q);
    halt(&mut env, &rm.pool);
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    let (cap, oi) = shortwin_litesvm_tests::pool_select::carried_caps(&env, &cand);
    assert!(q <= cap);
    let pre = invariants::snapshot(&env.svm, &[rm.market()]);
    let meta = ok(env.propose(Some(&rm.pool), &cand));
    let ev = IxEvents::of(&meta, 0);
    assert_eq!(i15(&env, &pre, &ev), Ok(()), "the real proposal");
    let with_q = |pre: &TxSnapshot, qs: &[u64]| {
        let mut m = pre.clone();
        let (k, p, l) = m.positions.markets[0].positions[0].clone();
        m.positions.markets[0].positions = qs
            .iter()
            .map(|&q| {
                let mut p = p.clone();
                p.q = q;
                (k, p, l)
            })
            .collect();
        m
    };
    assert_eq!(i15(&env, &with_q(&pre, &[cap]), &ev), Ok(()), "at the cap");
    assert_eq!(
        i15(&env, &with_q(&pre, &[cap + 1]), &ev),
        Err("I15"),
        "a window candidate the position does not fit"
    );
    assert_eq!(
        i15(&env, &with_q(&pre, &[oi / 2 + 1, oi / 2 + 1]), &ev),
        Err("I15"),
        "positions whose sum is above its OI cap"
    );
    crank_every(&mut env, &rm, 30, 2);
    let pre = invariants::snapshot(&env.svm, &[rm.market()]);
    let meta = ok(env.execute_switch(&rm.pool, &cand));
    let ev = IxEvents::of(&meta, 0);
    assert_eq!(ev.switched.len(), 1);
    assert_eq!(i15(&env, &pre, &ev), Ok(()), "the real adoption");
    assert_eq!(
        i15(&env, &with_q(&pre, &[cap.saturating_mul(3)]), &ev),
        Err("I15"),
        "an adoption carrying a position that does not fit"
    );
    // The state part.
    let market = rm.market();
    let m = env.market(&market);
    assert_eq!(m.max_q_hwm, q);
    assert_eq!(
        invariants::check_all(&env.svm, &market).map_err(|v| v.invariant),
        Ok(())
    );
    let mut low = m;
    low.max_q_hwm = q - 1;
    shortwin_litesvm_tests::fixtures::set_zc(&mut env.svm, &market, &low, 0);
    assert_eq!(
        invariants::check_all(&env.svm, &market).map_err(|v| v.invariant),
        Err("I15"),
        "a mark below an open q"
    );
    shortwin_litesvm_tests::fixtures::set_zc(&mut env.svm, &market, &m, 0);
    assert_eq!(
        invariants::check_all(&env.svm, &market).map_err(|v| v.invariant),
        Ok(())
    );
}

/// FIT-2, the carried fee and the marks: a real open leaves `min_open_fee_ppm` at the venue's fee
/// (a post-state with another value, or with `max_q_hwm` lowered while the position is open, fails
/// I15); the last exit resets both marks, and a non-zero fee mark with no position open fails I15
/// (state part); a real window proposal passes, and the same proposal judged on a market whose
/// positions were margined at a (mutated) 1-ppm fee fails I15 (the fee-scaled caps take
/// `min(mark, φ_cand)`).
#[test]
fn i15_the_carried_fee_and_the_marks() {
    use shortwin::state::Market;
    let (mut env, rm, q) = setup(FART);
    let market = rm.market();
    let set = |env: &mut Env, m: &Market| {
        shortwin_litesvm_tests::fixtures::set_zc(&mut env.svm, &market, m, 0);
    };
    let check = |env: &Env| invariants::check_all(&env.svm, &market).map_err(|v| v.invariant);
    let pre = invariants::snapshot(&env.svm, &[market]);
    let mut b = env.borrower();
    let (_, nonce, meta) = open_ok(&mut env, &rm, &mut b, q);
    let ev = IxEvents::of(&meta, 0);
    let fee = pre.positions.markets[0].venue.expect("a view").fee_ppm;
    let m = env.market(&market);
    assert_eq!((m.min_open_fee_ppm, m.max_q_hwm), (fee, q));
    assert_eq!(i15(&env, &pre, &ev), Ok(()), "the real open");
    set(
        &mut env,
        &Market {
            min_open_fee_ppm: fee - 1,
            ..m
        },
    );
    assert_eq!(
        i15(&env, &pre, &ev),
        Err("I15"),
        "a fee mark other than the open's fee"
    );
    set(&mut env, &m);
    // Any other instruction (a crank) leaves both marks: one that lowered `max_q_hwm` (from a
    // mutated q + 5 before) or moved the fee mark with the position open fails I15.
    env.warp(1);
    let pre = invariants::snapshot(&env.svm, &[market]);
    let meta = ok(env.send(&[crank_price_ix(&rm)], &[]));
    let ev = IxEvents::of(&meta, 0);
    assert_eq!(i15(&env, &pre, &ev), Ok(()), "the real crank");
    let mut high = pre.clone();
    high.positions.markets[0].state.max_q_hwm = q + 5;
    assert_eq!(
        i15(&env, &high, &ev),
        Err("I15"),
        "max_q_hwm lowered with a position open"
    );
    let mut other = pre.clone();
    other.positions.markets[0].state.min_open_fee_ppm = fee + 1;
    assert_eq!(
        i15(&env, &other, &ev),
        Err("I15"),
        "the fee mark moved without an open or the last exit"
    );
    // The last exit resets both marks; a fee mark left on an empty market fails the state part.
    env.warp(1);
    env.crank(&rm);
    close_ok(&mut env, &rm, &b, nonce);
    let empty = env.market(&market);
    assert_eq!(
        (
            empty.open_positions,
            empty.max_q_hwm,
            empty.min_open_fee_ppm
        ),
        (0, 0, 0)
    );
    assert_eq!(check(&env), Ok(()));
    set(
        &mut env,
        &Market {
            min_open_fee_ppm: fee,
            ..empty
        },
    );
    assert_eq!(check(&env), Err("I15"), "a fee mark with no position open");
    set(&mut env, &empty);
    // A window proposal on the halted incumbent: real, then judged at a 1-ppm open fee.
    env.warp(1);
    env.crank(&rm);
    open_ok(&mut env, &rm, &mut b, q);
    halt(&mut env, &rm.pool);
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    let pre = invariants::snapshot(&env.svm, &[market]);
    let meta = ok(env.propose(Some(&rm.pool), &cand));
    let ev = IxEvents::of(&meta, 0);
    assert_eq!(ev.proposed.len(), 1);
    assert_eq!(i15(&env, &pre, &ev), Ok(()), "the real proposal");
    let mut cheap = pre.clone();
    cheap.positions.markets[0].state.min_open_fee_ppm = 1;
    assert_eq!(
        i15(&env, &cheap, &ev),
        Err("I15"),
        "positions margined at 1 ppm do not fit the candidate's fee-scaled caps"
    );
}

/// Opens sized on locked depth: an `Open` whose `q` exceeds the impact cap on the locked-depth
/// `X₀` fails I15; the real one passes.
#[test]
fn i15_open_sizing() {
    let (mut env, rm, q) = setup(FART);
    let pre = invariants::snapshot(&env.svm, &[rm.market()]);
    let mut b = env.borrower();
    let (_, _, meta) = open_ok(&mut env, &rm, &mut b, q);
    let ev = IxEvents::of(&meta, 0);
    assert_eq!(i15(&env, &pre, &ev), Ok(()));
    let o: &Open = &ev.opens[0];
    let mut big = IxEvents::default();
    let mut e: Open = events::one(&meta);
    e.q = o.q.saturating_mul(1_000);
    big.opens.push(e);
    assert_eq!(i15(&env, &pre, &big), Err("I15"));
    // Inside the listing window.
    let mut early = pre.clone();
    early.positions.markets[0].state.pool_seen_at = env.now();
    assert_eq!(i15(&env, &early, &ev), Err("I15"));
    // M4R / M4V-5: during the post-change warm-up (`ema_warm` @22 after the roll).
    let market = rm.market();
    let good = env.svm.get_account(&market).unwrap();
    let mut acc = good.clone();
    acc.data[22] = 1;
    env.svm.set_account(market, acc).unwrap();
    assert_eq!(env.market(&market).ema_warm, 1);
    assert_eq!(i15(&env, &pre, &ev), Err("I15"), "during the warm-up");
    env.svm.set_account(market, good).unwrap();
    assert_eq!(i15(&env, &pre, &ev), Ok(()));
}

/// I18 (M4R) on `ev` against `pre` (the post-change confirmation part alone).
fn i18(env: &Env, pre: &TxSnapshot, ev: &IxEvents) -> Result<(), &'static str> {
    invariants::confirm::check_ix(
        &env.svm,
        &pre.positions,
        &invariants::confirm::ConfirmFacts {
            created: &ev.created,
            switched: &ev.switched,
            venue_changed: &ev.venue_changed,
            opens: &ev.opens,
            liquidations: &ev.liquidations,
            expiries: &ev.expiries,
            settlements: &ev.settlements,
            armed: &ev.armed,
            disarmed: &ev.disarmed,
        },
    )
    .map_err(|v| v.invariant)
}

/// One crank of `rm` a second later: the snapshot before it, its events.
fn crank_step(env: &mut Env, rm: &RayMarket) -> (TxSnapshot, IxEvents) {
    env.warp(1);
    let pre = invariants::snapshot(&env.svm, &[rm.market()]);
    let meta = ok(env.send(&[crank_price_ix(rm)], &[]));
    (pre, IxEvents::of(&meta, 0))
}

/// I18 fires on what a program mutant would leave around a fast path with an open position:
/// a restart without `PoolSwitched`, a warm-up seed from `max(anchor, ·)` (the anchor kept), a
/// countdown skipped, a keeper exit or an arm change during the warm-up (the gate skipped), a
/// guard cleared early or never cleared; the real instructions pass.
#[test]
fn i18_confirmation_rules() {
    use shortwin::events::SettleArmed;

    let (mut env, rm, q) = setup(FART);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    let pre = invariants::snapshot(&env.svm, &[rm.market()]);
    let meta = ok(env.execute_switch(&rm.pool, &cand));
    let ev = IxEvents::of(&meta, 0);
    assert_eq!(i18(&env, &pre, &ev), Ok(()), "the real adoption");
    assert_eq!(
        i18(&env, &pre, &IxEvents::default()),
        Err("I18"),
        "a restart without PoolSwitched"
    );
    let market = rm.market();
    let rm2 = rm.on_pool(&env.svm, cand);
    let cap = env.market(&market).ema_liq.get();
    let position = pda::position(&market, &b.key(), nonce).0;
    let armed = |m: Pubkey| SettleArmed {
        market: m,
        slot: 0,
        ts: 0,
        position,
        s_count: 0,
        s_since: 0,
        reason: 0,
    };
    // The first crank on the new pool records; the next ones integrate.
    env.warp(1);
    env.crank(&rm2);
    for step in 1..=8u8 {
        let (pre, ev) = crank_step(&mut env, &rm2);
        assert_eq!(i18(&env, &pre, &ev), Ok(()), "warm-up step {step}");
        let m = env.market(&market);
        assert_eq!(m.ema_warm, 8 - step);
        let good = env.svm.get_account(&market).unwrap();
        type Mutate = fn(&mut Vec<u8>, u128);
        let mut mutants: Vec<(&str, Mutate)> = vec![
            ("countdown skipped", |d, _| d[22] += 1),
            ("guard cleared", |d, _| d[23] ^= 1),
        ];
        if step < 8 {
            // The seed from max(anchor, ·): the anchor kept in L_ema and the newest entry.
            mutants.push(("seed from the anchor", |d, cap| {
                d[1_656..1_672].copy_from_slice(&cap.to_le_bytes());
            }));
        }
        for (what, mutate) in mutants {
            let mut acc = good.clone();
            mutate(&mut acc.data, cap);
            env.svm.set_account(market, acc).unwrap();
            assert_eq!(i18(&env, &pre, &ev), Err("I18"), "step {step}: {what}");
        }
        env.svm.set_account(market, good.clone()).unwrap();
        if step < 8 {
            // The gate skipped: an arm change while warming on an executable venue.
            let gated = IxEvents {
                armed: vec![armed(market)],
                ..IxEvents::default()
            };
            assert_eq!(i18(&env, &pre, &gated), Err("I18"), "step {step}: gate");
        } else {
            // At the gate an honest pool clears the guard; never clearing it is a violation.
            assert_eq!((m.ema_warm, m.ema_guard), (0, 0));
            let mut acc = good.clone();
            acc.data[23] = 1;
            env.svm.set_account(market, acc).unwrap();
            assert_eq!(i18(&env, &pre, &ev), Err("I18"), "guard never cleared");
            env.svm.set_account(market, good.clone()).unwrap();
        }
    }
    close_ok(&mut env, &rm2, &b, nonce);
}
