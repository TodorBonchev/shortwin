//! Duty 5 and S3 (ARCH §7.1, §3.8 step 4, §5.3): the mock pool is disabled by its admin (a
//! halted venue: not executable, no observations, the S3 chain runs). The keeper starts the chain
//! at once and keeps it continuous with a crank at least every 30 s; once the S3 timer has run, an expired position
//! is settled in SOL (`sol_settle`, S3), and a healthy one stays frozen.
//!
//! AIR-19: with the global hard cap raised above a position's locked cap, a fee inside the band
//! runs the market's S3 chain (at the lowest cap an open position locked) and is "no route" for
//! that position alone: it settles in S3, while a position that locked the raised cap keeps its
//! route — no S3 timer, its expiry lands — and the fee is reported above the chain's hard cap.

use anchor_lang::prelude::Pubkey;
use shortwin::engine::arming;
use shortwin::events::SolSettle;
use shortwin_keeper::events;
use shortwin_keeper::monitor::AdminChange;
use shortwin_litesvm_tests::engine::sol_settle_ix;
use shortwin_litesvm_tests::{fixtures, pda};
use shortwin_math::settle;
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::common::*;

#[test]
fn an_expired_position_on_a_disabled_pool_settles_in_s3_and_a_healthy_one_stays() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let (b1, n1) = open(&h, mm, Q);
    let pos1 = position_key(mm, &b1.key(), n1);
    let deadline = h.with_env(|env| env.position(mm, &b1.key(), n1).deadline);
    h.seconds(2);
    // A second position, opened later: still healthy and far from its deadline at the end.
    h.idle(3_600);
    h.seconds(2);
    let (b2, n2) = open(&h, mm, Q / 3);
    h.seconds(2);

    // The keeper is idle until 700 s before the first deadline; then the pool is halted.
    let wait = deadline - 700 - h.now();
    h.idle(wait);
    h.seconds(5);
    h.with_env(|env| mock::update_knobs(env, &mm.pool, |k| k.disabled = true));
    let halted_at = h.now();

    let mut since = None;
    let mut last_crank = halted_at;
    let mut settled = None;
    for s in 0..1_000 {
        let r = h.second().clone();
        let now = h.now();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if r.cranked.contains(&key) {
            assert!(now - last_crank <= 30, "crank gap {} s", now - last_crank);
            last_crank = now;
        }
        let m = h.with_env(|env| env.market(&key));
        if m.no_route_since != 0 {
            // One continuous chain: it never restarts.
            assert_eq!(*since.get_or_insert(m.no_route_since), m.no_route_since);
        }
        if let Some(out) = landed_for(&r, ExitKind::SolSettle, &pos1).next() {
            let ev: Vec<SolSettle> = events::of(out);
            if let Some(e) = ev.first() {
                assert_eq!(e.condition, 3, "S3");
                assert_eq!(e.keeper, h.keeper.payer());
                settled = Some(now);
                break;
            }
        }
    }
    let since = since.expect("the S3 chain never started");
    // The keeper sees the outage at its next tick and observes at once: the chain starts then.
    assert_eq!(since, halted_at + 1);
    let settled = settled.expect("no S3 settlement");
    println!("halted at {halted_at}, chain since {since}, deadline {deadline}, S3 at {settled}");
    // Due once the position is expired and the chain has run s3_wait_s (600 s).
    assert!(settled >= deadline && settled >= since + 600, "{settled}");
    assert!(settled <= deadline.max(since + 600) + 1, "late: {settled}");
    assert!(!position_open(&h, mm, &b1.key(), n1));
    // The healthy position is frozen, not settled.
    assert!(position_open(&h, mm, &b2.key(), n2));
    assert_eq!(h.keeper.metrics.markets[&key].settlements, 1);
}

/// Governance's `venue_fee_hard_cap_bps = cap` as applied (TEST-ONLY: the state the timelocked
/// `ConfigChange::Params` leaves, written in place without its 48-h wait, so that the two
/// positions of the AIR-19 scenario expire seconds apart).
fn plant_hard_cap(h: &Harness, cap: u32) {
    h.with_env(|env| {
        let mut c = env.config();
        c.params.venue_fee_hard_cap_bps = cap;
        fixtures::set_zc(&mut env.svm, &pda::config(), &c, 0);
        assert_eq!(env.config().params.venue_fee_hard_cap_bps, cap);
    });
}

/// The condition `sol_settle` of `(borrower, nonce)` would settle at now (an outside simulation,
/// which must succeed; `None`: it settles nothing).
fn settles(h: &Harness, mm: &MockMarket, borrower: &Pubkey, nonce: u64) -> Option<u8> {
    h.with_env(|env| {
        let kp = env.funded_keypair();
        let ix = sol_settle_ix(env, mm, &kp.pubkey(), borrower, nonce);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&kp.pubkey()),
            &[&kp],
            env.svm.latest_blockhash(),
        );
        let info = env
            .svm
            .simulate_transaction(tx)
            .unwrap_or_else(|f| panic!("sol_settle: {:?}\n{}", f.err, f.meta.pretty_logs()));
        shortwin_litesvm_tests::events::of::<SolSettle>(&info.meta)
            .first()
            .map(|e| e.condition)
    })
}

/// AIR-19 (the TODO.md keeper item on the S3-chain predictions). A position opens at the default
/// 500-bps hard cap; Governance raises the global cap to 1,000 bps; a second position opens under
/// it (it locks 1,000), so the market's mark `min_open_hard_cap_bps` is 500. 700 s before the
/// first deadline the pool's fee goes to 8 %, inside the band. The keeper reports the fee above
/// the market's S3-chain hard cap (the mark; the global cap alone would not) and starts the chain
/// at once (the prologue's `s3_no_route`). The first position's own route is gone: once expired
/// with the chain run `s3_wait_s` it is S3-due and settles in S3. The second's is not: the
/// keeper's evaluation never makes it S3-due — at its deadline, with the chain running 700 s, as
/// `sol_settle` agrees (it would settle nothing) — no `sol_settle` is sent for it, and its expiry
/// lands. Before this change the keeper judged every position on the market's chain predicate
/// and made the second S3-due at its deadline.
#[test]
fn air19_a_fee_above_a_positions_locked_cap_is_no_route_for_it_alone() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = &mms[0];
    let key = mm.market();
    let (b1, n1) = open(&h, mm, Q);
    let pos1 = position_key(mm, &b1.key(), n1);
    h.seconds(2);
    plant_hard_cap(&h, 1_000);
    let (b2, n2) = open(&h, mm, Q / 2);
    let pos2 = position_key(mm, &b2.key(), n2);
    let (p1, p2) = h.with_env(|env| {
        (
            env.position(mm, &b1.key(), n1),
            env.position(mm, &b2.key(), n2),
        )
    });
    assert_eq!(
        (p1.venue_fee_hard_cap_bps, p2.venue_fee_hard_cap_bps),
        (500, 1_000),
        "each position keeps the cap of its open"
    );
    assert_eq!(
        h.with_env(|env| env.market(&key)).min_open_hard_cap_bps,
        500
    );
    assert!(p2.deadline > p1.deadline);
    h.seconds(2);

    let wait = p1.deadline - 700 - h.now();
    h.idle(wait);
    h.seconds(5);
    h.with_env(|env| mock::update_knobs(env, &mm.pool, |k| k.fee_ppm = 80_000));
    let fee_at = h.now();
    let mut settled = None;
    let mut expired = None;
    let mut p1_due = None;
    let mut p2_checked = false;
    for s in 0..1_000 {
        h.idle(1);
        let now = h.now();
        // The keeper's own evaluations on the state its tick reads (its report keeps the ones
        // after its exits, which close the positions).
        if settled.is_none()
            && let Some((e, _, _)) = keeper_eval(&h, key, pos1)
            && e.s3_due
        {
            p1_due.get_or_insert(now);
        }
        if let Some((e, proj, _)) = keeper_eval(&h, key, pos2) {
            assert!(
                !e.s3_due,
                "second {s}: the second position's route stands: {e:?}"
            );
            if e.expired && !p2_checked {
                p2_checked = true;
                // On the market's chain predicate — the keeper's input before AIR-19 — the
                // expired second position would be S3-due: the chain has run `s3_wait_s`.
                let chain = arming::s3_chain(&proj.market, proj.s3_no_route);
                assert!(proj.s3_no_route && settle::s3_due(&chain, &p2.s3_terms(), proj.now, true));
                // The program's own answer: `sol_settle` settles nothing for it.
                assert_eq!(settles(&h, mm, &b2.key(), n2), None);
                println!(
                    "second position expired at {now}: chain since {:?}, its own route stands",
                    chain.no_route_since
                );
            }
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if s == 0 {
            let fees: Vec<&AdminChange> = r
                .alerts
                .iter()
                .filter_map(|a| match a {
                    Alert::VenueAdmin { market, change, .. } if *market == key => Some(change),
                    _ => None,
                })
                .collect();
            assert!(
                matches!(
                    fees.as_slice(),
                    [AdminChange::FeeChanged {
                        to_ppm: 80_000,
                        above_hard_cap: true,
                        ..
                    }]
                ),
                "{fees:?}"
            );
            assert!(r.needs[&key].no_route && r.needs[&key].start_chain);
            assert_eq!(h.with_env(|env| env.market(&key)).no_route_since, now);
        }
        assert!(
            !r.exits
                .iter()
                .any(|e| e.position == pos2 && e.kind == ExitKind::SolSettle),
            "second {s}: no sol_settle for the second position"
        );
        if let Some(out) = landed_for(&r, ExitKind::SolSettle, &pos1).next()
            && let Some(e) = events::of::<SolSettle>(out).first()
        {
            assert_eq!(e.condition, 3, "S3");
            settled = Some(now);
        }
        if landed_for(&r, ExitKind::Expire, &pos2).next().is_some() {
            expired = Some(now);
            break;
        }
    }
    let settled = settled.expect("no S3 settlement of the first position");
    let expired = expired.expect("the second position was not expired");
    println!(
        "fee in the band at {fee_at}; first: deadline {}, S3-due at {p1_due:?}, S3 at {settled}; \
         second: deadline {}, expired at {expired}",
        p1.deadline, p2.deadline
    );
    assert_eq!(p1_due, Some(settled), "S3-due in the second it settled");
    assert!(p2_checked, "the expired second position was evaluated");
    assert!(settled >= p1.deadline && settled >= fee_at + 1 + 600);
    assert!(expired >= p2.deadline && expired <= p2.deadline + 2);
    assert!(!position_open(&h, mm, &b1.key(), n1));
    assert!(!position_open(&h, mm, &b2.key(), n2));
}
