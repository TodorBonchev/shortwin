//! Duty 3 (ARCH §7.1, §3.6, §5.1): the keeper liquidates at the **first second** the program
//! accepts a liquidation (the fixed δ bound; retried every second on `ExitBoundNotMet`),
//! observes every second before it, and sends no separate crank in the second it acts
//! (duty 7). Checked against an outside oracle that simulates the same liquidation every
//! second.

use shortwin::events::Liquidate;
use shortwin_keeper::decide::Plan;
use shortwin_keeper::events;
use shortwin_litesvm_tests::engine::liquidate_ix;
use solana_signer::Signer;

use crate::common::*;

/// Drives `mm`'s price with `path(second, spot) -> target` (a whale's trade before each
/// keeper tick) until the keeper's liquidation of `(b, nonce)` lands; returns (first second
/// the oracle accepted a liquidation, second it landed, plans seen before it).
fn run(path: impl Fn(u32, u128) -> u128) -> (i64, i64, Vec<Plan>) {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let (b, nonce) = open(&h, mm, Q);
    let pos = position_key(mm, &b.key(), nonce);
    let oracle = Oracle::new(&h);
    let keeper = h.keeper.payer();
    let whale = h.with_env(|env| env.actor(mm, 0, 5_000_000_000_000));
    h.seconds(5);
    let mut first_ok = None;
    let mut plans = Vec::new();
    for s in 0..120 {
        h.with_env(|env| {
            env.warp(1);
            let spot = env.spot(mm);
            let target = path(s, spot);
            if target.abs_diff(spot) * 10_000 > spot {
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
        let ev = r.evaluations.get(&key).and_then(|v| v.first()).cloned();
        if let Some(out) = landed_for(&r, ExitKind::Liquidate, &pos).next() {
            let l: Liquidate = events::of(out).into_iter().next().expect("Liquidate event");
            assert_eq!((l.keeper, l.position), (keeper, pos));
            // The liquidation is the second's observation: no separate crank (duty 7).
            assert!(!r.cranked.contains(&key));
            assert!(r.observed(&key));
            assert!(!position_open(&h, mm, &b.key(), nonce));
            assert_eq!(h.keeper.metrics.markets[&key].liquidations, 1);
            let first = first_ok.expect("landed before the oracle accepted");
            println!(
                "liquidated {} s into the path (first accepted at the same second: {}), plans before: {:?}",
                s + 1,
                first == now,
                plans
            );
            return (first, now, plans);
        }
        if let Some(e) = &ev {
            plans.push(e.plan);
            // A near position is observed every second (duty 1).
            if e.near {
                assert!(r.observed(&key), "second {s}: near but not observed: {e:?}");
            }
        }
    }
    panic!("no liquidation (first accepted: {first_ok:?})");
}

#[test]
fn a_trend_is_liquidated_at_the_first_executable_second() {
    // +4 % per second.
    let (first, landed, _) = run(|_, spot| spot * 104 / 100);
    assert_eq!(landed, first);
}

#[test]
fn a_jump_is_liquidated_at_the_first_executable_second() {
    // ×2.3 at once, then flat: the EMA leg holds a few seconds later.
    let (first, landed, _) = run(|s, spot| if s == 0 { spot * 23 / 10 } else { spot });
    assert_eq!(landed, first);
}

/// `ExitBoundNotMet` (ARCH §3.6 step 6): in the second the position first becomes
/// liquidatable, a whale lifts the live price 5 % above the last completed bucket — beyond
/// the fixed 2 % δ of a flat reference. The keeper's projection sees the same bound
/// (`LiquidateWait`), observes that second, and liquidates the next one, when the spike has
/// become the reference.
#[test]
fn a_spike_above_the_bound_is_waited_out_and_liquidated_the_next_second() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let (b, nonce) = open(&h, mm, Q);
    let pos = position_key(mm, &b.key(), nonce);
    let oracle = Oracle::new(&h);
    let whale = h.with_env(|env| env.actor(mm, 0, 5_000_000_000_000));
    h.seconds(5);
    h.with_env(|env| {
        let target = env.spot(mm) * 23 / 10;
        env.push_price(mm, &whale, target);
    });
    let liq = || liquidate_ix(mm, &oracle.kp.pubkey(), &b.key(), nonce);
    let mut spiked_at = None;
    for s in 0..60 {
        h.with_env(|env| env.warp(1));
        let now = h.now();
        if spiked_at.is_none() && oracle.accepts(&h, liq()).is_ok() {
            h.with_env(|env| {
                let target = env.spot(mm) * 105 / 100;
                env.push_price(mm, &whale, target);
            });
            let code = oracle.accepts(&h, liq()).unwrap_err();
            assert_eq!(
                code,
                Some(u32::from(shortwin::errors::ShortwinError::ExitBoundNotMet))
            );
            spiked_at = Some(now);
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if Some(now) == spiked_at {
            let e = &r.evaluations[&key][0];
            assert_eq!(e.plan, Plan::LiquidateWait, "{e:?}");
            assert!(e.b_live > e.max_in, "{e:?}");
            assert!(r.cranked.contains(&key), "the waiting second is observed");
            assert!(landed_for(&r, ExitKind::Liquidate, &pos).next().is_none());
        }
        if landed_for(&r, ExitKind::Liquidate, &pos).next().is_some() {
            assert_eq!(
                Some(now - 1),
                spiked_at,
                "liquidated the second after the spike"
            );
            return;
        }
    }
    panic!("no liquidation (spike at {spiked_at:?})");
}
