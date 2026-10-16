//! ★The verification of owner decision A (the global keeper budget, 2026-10-01): its findings
//! M7D-1, -2, -4 and -5 as T-COVERAGE regressions (ARCH §7.1 duties 1 and 8, §8.2; ECON §5.8).
//! Each failed on the build the verification reviewed; M7D-3 and the arithmetic of M7D-5 are the
//! keeper's `budget` unit tests, M7D-6 the `oracle` scenario's second stall.
//!
//! - **M7D-1** (critical): below the value-at-risk floor the keeper read a market only when its
//!   idle crank was due — 30 s after its last observation —, so every read found its own reference
//!   older than `ref_max_age_s` (2 s): an exit needs a completed bucket at most that old, and the
//!   keeper was never there in the two seconds after its crank when the program accepts one. No
//!   below-floor position was ever liquidated or expired, and a cascade stopped where its market's
//!   `V` fell below the floor. Fixed: while a position is near an exit the market is read every
//!   second its last observation's reference is still fresh, so the crank at `t` is followed by
//!   the read and the exit at `t + 1`. `m7d_1_a_*` (expiry), `m7d_1_b_*` (a liquidation after an
//!   outside cranker stops), `m7d_1_c_*` (on the keeper's idle cranks alone), `m7d_1_d_*` (a
//!   cascade whose `V` crosses the floor), and the covered control.
//! - **M7D-2** (high): every below-floor crank was charged baseline — the tail —, including the
//!   crank an exit waits on; once junk had spent the tail no below-floor exit's reference was ever
//!   cranked. Fixed: that crank is exit-critical (the tail, then the overdraft).
//! - **M7D-4** (medium): on a covered market with a position near its threshold every per-slot
//!   crank of a price burst was charged exit-critical (≈ 2.5 a second, overdrawn without limit).
//!   Fixed: exit-critical is the first crank of a second; a burst's further cranks are
//!   discretionary.
//! - **M7D-5** (medium): a keeper (re)start funded every covered market at the unbatched
//!   first-crank estimate (5,000 lamports, 6× the batched charge) and never revisited it. Fixed:
//!   the estimate is `⌈5,000 / batch⌉` (then the measured average), re-estimated at each market's
//!   first crank.

use shortwin_keeper::budget::{BudgetParams, ChargeClass, DAY_S, Tier};
use shortwin_keeper::decide::Plan;
use shortwin_litesvm_tests::drive::swap_in_ix;
use shortwin_litesvm_tests::engine::{crank_price_ix, expire_ix, liquidate_ix};
use shortwin_litesvm_tests::mock::Dir;
use solana_signer::Signer;

use crate::common::*;

/// The keeper's reads of a market: (second, the first position's plan, cranked).
type Reads = Vec<(i64, Plan, bool)>;

/// A ×`k`/10 jump on a market whose only position is `q`, the keeper alone (no other cranker),
/// for at most `secs` seconds. Returns (the first second the program accepted a liquidation, the
/// second the keeper's liquidation landed — both counted from the jump —, the market's class, the
/// keeper's reads).
fn jump(
    cfg: KeeperConfig,
    q: u64,
    k: u128,
    secs: u32,
) -> (Option<i64>, Option<i64>, Option<Tier>, Reads) {
    let (mut h, mms) = setup(1, cfg);
    let mm = &mms[0];
    let key = mm.market();
    let (b, nonce) = open(&h, mm, q);
    let pos = position_key(mm, &b.key(), nonce);
    let oracle = Oracle::new(&h);
    let whale = h.with_env(|env| env.actor(mm, 0, 5_000_000_000_000));
    h.seconds(5);
    let mut first_ok = None;
    let mut landed = None;
    let mut reads = Vec::new();
    let t0 = h.now() + 1;
    for s in 0..secs {
        h.with_env(|env| {
            env.warp(1);
            if s == 0 {
                let target = env.spot(mm) * k / 10;
                env.push_price(mm, &whale, target);
            }
        });
        let now = h.now();
        if first_ok.is_none()
            && oracle
                .accepts(&h, liquidate_ix(mm, &oracle.kp.pubkey(), &b.key(), nonce))
                .is_ok()
        {
            first_ok = Some(now);
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if let Some(e) = r.evaluations.get(&key).and_then(|v| v.first()) {
            reads.push((now, e.plan, r.cranked.contains(&key)));
        }
        if landed_for(&r, ExitKind::Liquidate, &pos).next().is_some() {
            landed = Some(now);
            break;
        }
    }
    let tier = h.keeper.budget().tier(&key);
    (
        first_ok.map(|t| t - t0),
        landed.map(|t| t - t0),
        tier,
        reads,
    )
}

/// Control: the same ×2.3 jump on the same position, covered (`var_floor_lamports` 0): per-second
/// coverage, liquidated at the first second the program accepts.
#[test]
fn m7d_1_control_a_covered_market_is_liquidated_at_once() {
    let (first, landed, tier, _) = jump(covering_all(KeeperConfig::default()), Q / 10, 23, 240);
    println!(
        "M7D-1 control (covered): first accepted {first:?} s after the jump, landed {landed:?} s"
    );
    assert_eq!(tier, Some(Tier::Covered));
    assert!(first.is_some());
    assert_eq!(landed, first);
}

/// ★M7D-1 (expiry): a 0.3-SOL position (`V` below the 1-SOL floor, default config) reaching its
/// deadline. The keeper reads the market at its idle cadence; the read after the deadline finds the
/// reference stale (`Observe`) and cranks; the read the next second — the reference fresh —
/// expires it, at the first second the program accepts. As reviewed every read came 30 s after the
/// last and planned `Observe`: never expired (300 s).
#[test]
fn m7d_1_a_a_below_floor_position_is_expired_at_the_first_second_the_program_accepts() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let (b, nonce) = open(&h, mm, Q / 10);
    let pos = position_key(mm, &b.key(), nonce);
    let deadline = h.with_env(|env| env.position(mm, &b.key(), nonce).deadline);
    let oracle = Oracle::new(&h);
    h.seconds(3);
    let wait = deadline - 90 - h.now();
    h.idle(wait);
    let mut first_ok = None;
    let mut expired = None;
    let mut reads = Vec::new();
    for s in 0..300 {
        h.with_env(|env| env.warp(1));
        let now = h.now();
        if first_ok.is_none()
            && oracle
                .accepts(&h, expire_ix(mm, &oracle.kp.pubkey(), &b.key(), nonce))
                .is_ok()
        {
            first_ok = Some(now);
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if let Some(e) = r.evaluations.get(&key).and_then(|v| v.first()) {
            reads.push((now - deadline, e.plan, r.cranked.contains(&key)));
        }
        if landed_for(&r, ExitKind::Expire, &pos).next().is_some() {
            expired = Some(now);
            break;
        }
    }
    println!(
        "M7D-1 (a) expiry below the floor: first accepted {:?} s after the deadline, expired {:?}; \
         reads (s from the deadline, plan, cranked) {reads:?}",
        first_ok.map(|t| t - deadline),
        expired.map(|t| t - deadline)
    );
    assert_eq!(h.keeper.budget().tier(&key), Some(Tier::BelowFloor));
    let first = first_ok.expect("the program accepts the expiry");
    assert_eq!(
        expired,
        Some(first),
        "expired at the first second it could be"
    );
    // The idle cadence only: the crank that made it possible came at most 30 s after the deadline.
    assert!(first - deadline <= 32, "{}", first - deadline);
    let cranks = reads.iter().filter(|(_, _, c)| *c).count();
    assert!(
        cranks <= 4,
        "{cranks} cranks in the last 90 s: the idle cadence"
    );
}

/// ★M7D-1 (liquidation after an outside cranker stops): a ×2.3 jump; an outside cranker observes
/// every second until the program first accepts a liquidation, then stops. The keeper's next idle
/// crank is followed by reads while its reference is fresh, and the liquidation lands at the first
/// second the program accepts after that. As reviewed: 18 accepted seconds in 286 s, each 1–2 s
/// after a keeper crank, and no read in any of them — never liquidated.
#[test]
fn m7d_1_b_a_below_floor_liquidation_lands_after_the_outside_cranks_stop() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let (b, nonce) = open(&h, mm, Q / 10);
    let pos = position_key(mm, &b.key(), nonce);
    let oracle = Oracle::new(&h);
    let whale = h.with_env(|env| env.actor(mm, 0, 5_000_000_000_000));
    h.seconds(5);
    let liq = || liquidate_ix(mm, &oracle.kp.pubkey(), &b.key(), nonce);
    let mut outside = true;
    let mut stopped = None;
    let mut accepted = Vec::new();
    let mut landed = None;
    let mut reads = Vec::new();
    for s in 0..400 {
        h.with_env(|env| {
            env.warp(1);
            if s == 0 {
                let target = env.spot(mm) * 23 / 10;
                env.push_price(mm, &whale, target);
            }
            if outside {
                env.crank(mm);
            }
        });
        let now = h.now();
        if oracle.accepts(&h, liq()).is_ok() {
            accepted.push(now);
            if outside {
                outside = false;
                stopped = Some(now);
            }
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if let Some(e) = r.evaluations.get(&key).and_then(|v| v.first()) {
            reads.push((now, e.plan, r.cranked.contains(&key)));
        }
        if landed_for(&r, ExitKind::Liquidate, &pos).next().is_some() {
            landed = Some(now);
            break;
        }
    }
    println!(
        "M7D-1 (b): outside cranks stopped at {stopped:?}; the program accepted at {accepted:?}; \
         landed {landed:?}; the keeper's reads (second, plan, cranked) {reads:?}"
    );
    assert_eq!(h.keeper.budget().tier(&key), Some(Tier::BelowFloor));
    let landed = landed.expect("M7D-1: never liquidated");
    // Every second the program accepted after the keeper's first read since the stop was the one
    // it liquidated at: none missed.
    let first_read = reads
        .iter()
        .map(|(t, _, _)| *t)
        .find(|t| *t > stopped.expect("stopped"))
        .expect("a read");
    let missed: Vec<i64> = accepted
        .iter()
        .copied()
        .filter(|t| *t >= first_read && *t < landed)
        .collect();
    assert!(missed.is_empty(), "accepted and missed: {missed:?}");
    assert_eq!(accepted.last(), Some(&landed));
}

/// ★M7D-1 (liquidation on the keeper's idle cranks alone): a ×2.3 jump, no other cranker. Below
/// the floor the EMA follows only the 30-s idle cranks, so the program first accepts minutes
/// later; the keeper's liquidation lands at that very second. As reviewed: never.
#[test]
fn m7d_1_c_a_below_floor_liquidation_lands_at_the_first_second_the_program_accepts() {
    let (first, landed, tier, reads) = jump(KeeperConfig::default(), Q / 10, 23, 1_200);
    let cranks = reads.iter().filter(|(_, _, c)| *c).count();
    println!(
        "M7D-1 (c): first accepted {first:?} s after the jump, landed {landed:?} s; {} reads, \
         {cranks} cranks",
        reads.len()
    );
    assert_eq!(tier, Some(Tier::BelowFloor));
    let first = first.expect("the program accepts within 1,200 s of idle cranks");
    assert_eq!(landed, Some(first));
    // The idle cadence: one crank per 30 s; at most two more reads after each.
    let span = reads.last().map_or(0, |r| r.0) - reads.first().map_or(0, |r| r.0);
    assert!(
        cranks as i64 <= span / 30 + 2,
        "{cranks} cranks in {span} s"
    );
    assert!(reads.len() <= 3 * cranks + 2, "{} reads", reads.len());
}

/// ★M7D-1 (a cascade whose `V` crosses the floor): a covered market — a 1.5-SOL position A
/// opened at `p₀`, two 0.12-SOL positions B opened after the price rose 25 % — in a ×2.6 pump over
/// 10 minutes. A is liquidated first, while the market is covered; with it gone `V` is below the
/// 1-SOL floor, and the B positions — liquidatable later in the pump — get the idle cadence only.
/// Each is liquidated at the first second the program accepts it. As reviewed, once a cascade
/// brought its market below the floor its remaining liquidations stopped.
#[test]
fn m7d_1_d_a_cascade_whose_value_at_risk_crosses_the_floor_is_liquidated() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let whale = h.with_env(|env| env.actor(mm, 0, 50_000_000_000_000));
    let (ba, na) = open(&h, mm, Q / 2);
    h.seconds(3);
    assert_eq!(h.keeper.budget().tier(&key), Some(Tier::Covered));
    h.with_env(|env| {
        let t = env.spot(mm) * 5 / 4;
        env.push_price(mm, &whale, t);
    });
    // The EMA follows (the market is covered: per second while off its EMA).
    h.seconds(60);
    let mut pos = vec![(ba.key(), na)];
    for _ in 0..2 {
        let (b, n) = open(&h, mm, Q / 25);
        pos.push((b.key(), n));
    }
    h.seconds(3);
    let oracle = Oracle::new(&h);
    let p1 = h.with_env(|env| env.spot(mm));
    let t0 = h.now();
    let mut first_ok: Vec<Option<i64>> = vec![None; pos.len()];
    let mut landed: Vec<Option<i64>> = vec![None; pos.len()];
    let mut classes: Vec<(i64, Option<Tier>, u64, usize)> = Vec::new();
    for s in 1..=1_200i64 {
        let f = (2.6f64.ln() * (s.min(600) as f64) / 600.0).exp();
        let target = (p1 as f64 * f) as u128;
        h.with_env(|env| {
            env.warp(1);
            if target > env.spot(mm) {
                env.push_price(mm, &whale, target);
            }
        });
        for (i, (b, n)) in pos.iter().enumerate() {
            if landed[i].is_none()
                && first_ok[i].is_none()
                && oracle
                    .accepts(&h, liquidate_ix(mm, &oracle.kp.pubkey(), b, *n))
                    .is_ok()
            {
                first_ok[i] = Some(s);
            }
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "{s}: {:?}", r.errors);
        for (i, (b, n)) in pos.iter().enumerate() {
            if landed_for(&r, ExitKind::Liquidate, &position_key(mm, b, *n))
                .next()
                .is_some()
            {
                landed[i] = Some(s);
            }
        }
        let open_n = landed.iter().filter(|l| l.is_none()).count();
        let class = h.keeper.budget().markets.get(&key).map(|m| (m.tier, m.var));
        if classes.last().map(|c| c.1) != Some(class.map(|c| c.0)) {
            classes.push((s, class.map(|c| c.0), class.map_or(0, |c| c.1), open_n));
        }
        if open_n == 0 {
            break;
        }
    }
    println!(
        "M7D-1 (d): from t0 = {t0}: first accepted {first_ok:?}, liquidated {landed:?}; classes \
         (second, class, V, positions open) {classes:?}"
    );
    // A, while covered; then the market below the floor with both B positions open.
    let a = landed[0].expect("A liquidated");
    assert!(
        classes.iter().any(|(t, c, v, n)| *t >= a
            && *c == Some(Tier::BelowFloor)
            && *v < 1_000_000_000
            && *n == 2),
        "the cascade crossed the floor with both B positions open: {classes:?}"
    );
    for i in 1..pos.len() {
        let first = first_ok[i].expect("the program accepts B's liquidation");
        assert!(first > a);
        // At the first second the program accepts it — or the next, when the other B's
        // liquidation went first in that second and its buyback moved the price above this one's
        // bound (`LiquidateWait`); its exit is then the next second's observation.
        let l = landed[i].expect("B liquidated");
        assert!(
            l == first || (l == first + 1 && landed.contains(&Some(first))),
            "B{i}: accepted at {first}, liquidated at {l}"
        );
    }
}

/// ★M7D-2: below the floor the idle crank an exit waits on is exit-critical. With the tail spent
/// (`tail_share_bps` 0) the expired position's `Observe` read cranks anyway — on the overdraft —
/// and the expiry lands the next second. As reviewed it was charged baseline and refused ("refused
/// by the global keeper budget (Baseline, rank None)"): the exit's reference never came.
#[test]
fn m7d_2_a_a_below_floor_exits_crank_is_exit_critical() {
    let mut cfg = KeeperConfig::default();
    cfg.budget.tail_share_bps = 0;
    let (mut h, mms) = setup(1, cfg);
    let mm = &mms[0];
    let key = mm.market();
    let (b, nonce) = open(&h, mm, Q / 10);
    let pos = position_key(mm, &b.key(), nonce);
    let deadline = h.with_env(|env| env.position(mm, &b.key(), nonce).deadline);
    h.seconds(3);
    let wait = deadline - 5 - h.now();
    h.idle(wait);
    let mut seen = None;
    let mut expired = None;
    for s in 0..90 {
        h.with_env(|env| env.warp(1));
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if landed_for(&r, ExitKind::Expire, &pos).next().is_some() {
            expired = Some(h.now() - deadline);
            break;
        }
        let Some(e) = r.evaluations.get(&key).and_then(|v| v.first()) else {
            continue;
        };
        if seen.is_none() && e.expired && e.plan == Plan::Observe {
            let refused = r
                .crank_failed
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, why)| why.clone());
            seen = Some((h.now() - deadline, refused, r.cranked.contains(&key)));
        }
    }
    let m = h.keeper.budget().markets[&key].clone();
    println!(
        "M7D-2 (a): the expired position's Observe read (s after the deadline, refusal, cranked) \
         {seen:?}; expired at +{expired:?} s; exit-critical spend {}, baseline {}; overdraft {}",
        m.spent[0],
        m.spent[1],
        h.keeper.budget().overdraft
    );
    let (at, refused, cranked) = seen.expect("an Observe read of the expired position");
    assert!(
        cranked && refused.is_none(),
        "the exit's crank refused: {refused:?}"
    );
    assert_eq!(expired, Some(at + 1));
    assert!(m.spent[0] > 0 && h.keeper.budget().overdraft >= m.spent[0]);
}

/// ★M7D-2: junk fills the tail, an honest small market's expiry still lands. One honest market
/// and ten junk markets, each with a 0.3-SOL position (below the floor), `G` 0.02 SOL with a
/// 0.25 % tail (50,000 lamports: ten cranks), one crank per transaction. The junk lists and opens
/// 400 s before the honest position's deadline and spends the tail; every later baseline idle
/// crank is refused (degraded: opens paused), the honest market's too. The honest position reaches
/// its deadline: its `Observe` crank is exit-critical — admitted on the overdraft —, and the expiry
/// lands the second after. No junk crank is exit-critical.
#[test]
fn m7d_2_b_junk_fills_the_tail_and_an_honest_expiry_still_lands() {
    const JUNK: usize = 10;
    let mut cfg = KeeperConfig {
        batch_v1: 1,
        batch_v0: 1,
        ..KeeperConfig::default()
    };
    cfg.budget.global_daily_lamports = 20_000_000;
    cfg.budget.tail_share_bps = 25;
    let (mut h, mms) = setup(1 + JUNK, cfg);
    let honest = &mms[0];
    let hk = honest.market();
    let (b, nonce) = open(&h, honest, Q / 10);
    let pos = position_key(honest, &b.key(), nonce);
    let deadline = h.with_env(|env| env.position(honest, &b.key(), nonce).deadline);
    h.seconds(3);
    let wait = deadline - 400 - h.now();
    h.idle(wait);
    assert_eq!(
        h.now().div_euclid(DAY_S),
        (deadline + 60).div_euclid(DAY_S),
        "one UTC day (one tail)"
    );
    for mm in &mms[1..] {
        let _ = open(&h, mm, Q / 10);
    }
    // The junk's first idle cranks fill the tail; its next ones are refused.
    h.seconds(40);
    let bg = h.keeper.budget();
    assert_eq!(bg.tail, bg.tail_cap, "the tail spent");
    for mm in &mms {
        assert_eq!(bg.tier(&mm.market()), Some(Tier::BelowFloor));
    }
    let wait = deadline - 5 - h.now();
    h.idle(wait);
    let o0 = h.keeper.budget().overdraft;
    let mut expired = None;
    for _ in 0..60 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if landed_for(&r, ExitKind::Expire, &pos).next().is_some() {
            expired = Some(h.now() - deadline);
            break;
        }
    }
    let bg = h.keeper.budget();
    let hm = &bg.markets[&hk];
    println!(
        "M7D-2 (b): tail {} of {}; the honest market expired at +{expired:?} s (exit-critical spend \
         {}, refused {}, overdraft +{}); junk: exit-critical spend {:?}, refused {:?}",
        bg.tail,
        bg.tail_cap,
        hm.spent[0],
        hm.refused,
        bg.overdraft - o0,
        mms[1..]
            .iter()
            .map(|m| bg.markets[&m.market()].spent[0])
            .collect::<Vec<_>>(),
        mms[1..]
            .iter()
            .map(|m| bg.markets[&m.market()].refused)
            .collect::<Vec<_>>()
    );
    assert!(
        expired.is_some_and(|t| t <= 32),
        "the honest expiry landed: {expired:?}"
    );
    assert_eq!(hm.spent[0], 5_000, "its exit's crank, exit-critical");
    assert_eq!(
        bg.overdraft - o0,
        5_000,
        "on the overdraft (the tail spent)"
    );
    assert!(
        hm.refused > 0,
        "its baseline idle cranks refused beyond the tail"
    );
    for mm in &mms[1..] {
        let m = &bg.markets[&mm.market()];
        assert_eq!(m.spent[0], 0, "junk never exit-critical");
        assert!(m.refused > 0, "junk refused beyond the tail");
    }
}

/// An attacker's reverted round trip on `mm` in one transaction: `[push +20 %, crank_price,
/// dump back]` (the duty-2 burst trigger, as in `budget`).
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

/// One run of M7D-4's burst on a near market under `G`: (slots, cranks, exit-critical charges,
/// discretionary charges, overdraft growth, seconds observed, charges refused, degraded at the
/// end).
fn near_burst(global: u64) -> (u32, u32, u64, u64, u64, usize, u64, bool) {
    let mut cfg = KeeperConfig {
        batch_v1: 1,
        batch_v0: 1,
        ..KeeperConfig::default()
    };
    cfg.budget.global_daily_lamports = global;
    let (mut h, mms) = setup(1, cfg);
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    let whale = h.with_env(|env| env.actor(mm, 0, 5_000_000_000_000));
    h.seconds(3);
    // Raise the price until the position is near its threshold (not liquidatable).
    let mut near = false;
    for _ in 0..120 {
        h.with_env(|env| {
            let t = env.spot(mm) * 102 / 100;
            env.push_price(mm, &whale, t);
        });
        let r = h.second().clone();
        if let Some(e) = r.evaluations.get(&key).and_then(|v| v.first())
            && e.near
            && e.plan == Plan::Watch
        {
            near = true;
            break;
        }
    }
    assert!(near, "the position near its threshold");
    for _ in 0..3 {
        h.second();
    }
    assert_eq!(h.keeper.budget().tier(&key), Some(Tier::Covered));
    let before = h.keeper.budget().markets[&key].clone();
    let o0 = h.keeper.budget().overdraft;
    let t0 = h.now();
    round_trip(&h, mm);
    let (mut slots, mut cranked) = (0u32, 0u32);
    let mut seconds = std::collections::BTreeSet::new();
    while h.now() < t0 + 20 {
        let r = h.slot().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if r.cranked.contains(&key) {
            cranked += 1;
            seconds.insert(h.now());
        }
        slots += 1;
        if slots.is_multiple_of(2) {
            h.with_env(|env| env.warp(1));
        }
    }
    let after = h.keeper.budget().markets[&key].clone();
    let degraded = h.keeper.track(&key).is_some_and(|t| t.state.is_degraded());
    (
        slots,
        cranked,
        (after.spent[0] - before.spent[0]) / 5_000,
        (after.spent[2] - before.spent[2]) / 5_000,
        h.keeper.budget().overdraft - o0,
        seconds.len(),
        after.refused - before.refused,
        degraded,
    )
}

/// ★M7D-4: a price burst on a covered market with a position near its threshold. Exit-critical
/// is per-second coverage: the first crank of each second; the burst's further per-slot cranks
/// are discretionary — drawn on the market's own entitlement and the free pool, and refused (the
/// market degraded: opens paused) once those are spent, while the per-second exit-critical crank
/// goes on. As reviewed all 40 of 20 s's slots were charged exit-critical and overdrew 200,000
/// lamports under `G` = 20,000.
#[test]
fn m7d_4_a_burst_on_a_near_market_charges_one_exit_critical_crank_a_second() {
    // Funded: the burst's further cranks are admitted, as discretionary.
    let (slots, cranked, exit, disc, over, secs, refused, degraded) = near_burst(10_000_000);
    println!(
        "M7D-4, G 0.01 SOL: {slots} slots, {cranked} cranks in {secs} seconds; exit-critical {exit}, \
         discretionary {disc}; overdraft +{over}; refused {refused}; degraded {degraded}"
    );
    assert!(
        exit <= 21 && exit + 1 >= secs as u64,
        "one exit-critical crank a second: {exit}"
    );
    assert!(
        disc >= 15,
        "the burst's further cranks discretionary: {disc}"
    );
    assert_eq!(exit + disc, u64::from(cranked));
    assert_eq!((over, refused), (0, 0));
    assert!(!degraded);
    // Spent: the further cranks are refused (degraded, opens paused), the per-second crank
    // overdraws.
    let (slots, cranked, exit, disc, over, secs, refused, degraded) = near_burst(20_000);
    println!(
        "M7D-4, G 20,000 lamports: {slots} slots, {cranked} cranks in {secs} seconds; exit-critical \
         {exit}, discretionary {disc}; overdraft +{over}; refused {refused}; degraded {degraded}"
    );
    assert!(exit <= 21, "{exit} exit-critical cranks in 20 s");
    assert!(secs >= 19, "every second still observed: {secs}");
    assert_eq!(disc, 0);
    assert!(
        refused >= 19,
        "the burst's further cranks refused: {refused}"
    );
    assert!(
        over <= 21 * 5_000,
        "the overdraft one crank a second: {over}"
    );
    assert!(
        degraded,
        "a refused charge degrades the market (opens paused)"
    );
}

/// ★M7D-5: a keeper start funds its covered markets at the batched estimate. Six covered markets
/// (one batch of six, `batch_v1` 6 by default) and `G` = 1.1 × their six prorated entitlements at
/// ⌈5,000 / 6⌉ = 834 lamports a crank: every market is funded at its first read and none is
/// refused or degraded; at the unbatched estimate as reviewed (5,000 a crank, 6× over) `G` would
/// have funded one market and refused the other five at their first idle crank. The batch's
/// landed charge (833) re-estimates each entitlement.
#[test]
fn m7d_5_a_keeper_start_funds_its_covered_markets_at_the_batched_estimate() {
    const N: usize = 6;
    let p = BudgetParams {
        global: 1,
        var_floor: 0,
        reserve_margin_bps: 10_000,
        tail_share_bps: 2_000,
        idle_interval_s: 30,
        first_charge: 834,
    };
    let mut env = Env::test_venue();
    let mms: Vec<MockMarket> = (0..N)
        .map(|_| env.mock_market(MarketSpec::default()))
        .collect();
    let first_tick = env.now() + N as i64 * 2 + 1;
    let sec = first_tick.rem_euclid(DAY_S);
    let need = BudgetParams::prorated(p.entitlement(834), sec);
    let unbatched = BudgetParams::prorated(p.entitlement(5_000), sec);
    let mut cfg = KeeperConfig::default();
    cfg.budget.global_daily_lamports = N as u64 * need * 11 / 10;
    let g = cfg.budget.global_daily_lamports;
    assert!(g < 2 * unbatched, "the unbatched estimate funds one market");
    let mut h = Harness::new(env, cfg);
    for mm in &mms {
        let _ = open(&h, mm, Q);
    }
    let mut refused = 0;
    let mut degraded = 0;
    for _ in 0..65 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        refused += r
            .crank_failed
            .iter()
            .filter(|(_, why)| why.contains("refused"))
            .count();
        degraded += r
            .alerts
            .iter()
            .filter(|a| matches!(a, Alert::Degraded { .. }))
            .count();
    }
    let bg = h.keeper.budget();
    let funded: Vec<u64> = mms.iter().map(|m| bg.markets[&m.market()].funded).collect();
    println!(
        "M7D-5: G {g}; funded {funded:?} (need {need} each at 834, {unbatched} at 5,000); \
         measured charge {:?}; refused {refused}, degraded {degraded}",
        bg.avg_charge
    );
    for mm in &mms {
        let m = &bg.markets[&mm.market()];
        assert_eq!(m.tier, Tier::Covered);
        assert!(m.funded > 0 && !m.estimated);
        assert_eq!(m.last_charge, Some(833));
        assert_eq!(m.spent[ChargeClass::Baseline as usize], m.spent_today());
    }
    assert_eq!((refused, degraded), (0, 0));
}
