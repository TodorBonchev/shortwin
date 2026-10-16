//! O17's automatic degraded mode — the **T-COVERAGE drill** (ARCH §7.1 duty 9, §8.2 T-COVERAGE;
//! ECON §5.8 "Degraded mode"; SPEC §14 O17, owner decision of 2026-09-30): the keeper instance
//! designated as the pauser (`[coverage_pause] enabled`) holds the `coverage_authority` key and
//! pauses opens on a market with open positions whose coverage lapses — a keeper outage (the
//! program's `CoverageGap`), consecutive missed seconds, or a charge the global O17 budget refused
//! (decision A of 2026-10-01) — with
//! `set_coverage_pause(market, true)`; exits, settlements, lender withdrawals and claims keep
//! working under the pause; after `recovery_s` (10 minutes) of restored coverage it sends
//! `set_coverage_pause(market, false)` and opens land again (`COVERAGE_CLEAR_DELAY_S` later,
//! M7-SEC-3).
//!
//! - **The drill** (`drill_*`): an outage of 45 s → `CoverageGap` → degraded → the pause lands in
//!   the same tick; an open is refused `CoveragePaused` while `close_short`, `add_margin`, an
//!   outside `sol_settle` and `crank_price`, the lender's `withdraw` and `claim_sol` land, and the
//!   keeper liquidates a pumped position; the pause lifts exactly `recovery_s` after the gap and an
//!   open lands. The budget path: the global budget exhausted → degraded and paused in the same
//!   tick (the pause is not charged to the budget), the keeper's liquidation lands under it (its
//!   cranks exit-critical: an overdraft, never throttled), the budget raised → the pause lifts
//!   600 s after the first landed observation.
//! - **A restart** with the flag set: the new instance starts the market `Recovering`
//!   (`CoveragePauseFound`), never unpauses at once, and unpauses after 600 s.
//! - **The role unset** (inert): the pauser alerts `CoveragePauseUnavailable{AuthorityUnset}` once
//!   and sends nothing — the `Degraded` alert's page to the Emergency multisig is the pause path;
//!   the key rotated away (`WrongKey`) and no key loaded (`NoKey`) the same; the program refuses
//!   the instruction while the role is unset.
//! - **Two instances** holding the key: exactly one pause and one unpause land; the second instance
//!   adopts the first one's pause (`Recovering`) instead of lifting it — and, being a designated
//!   pauser that saw the flag clear and then set without sending the set, pages
//!   `CoveragePauseForeign` (★OWNER-M7-3 (b)).
//! - **★The Emergency's clear** (OWNER-M7-3 (b), owner decision of 2026-10-01): a hijacked coverage
//!   key pauses a healthy market (`CoveragePauseForeign`, the market `Recovering`), the Emergency
//!   multisig clears it and the pauser leaves it clear (it never set it), opens resume after the
//!   clear delay; the Emergency clearing the pauser's **own** pause on a market it still holds
//!   degraded is set again (`CoveragePauseCleared`) — to override it, stop the pauser first.
//! - **Through the timelock**: the role enabled by a real `Authorities` change and revoked by
//!   rotating it away (the pauser then falls back to alerting).
//!
//! Runs on the test-venue build and the mock venue; needs a program build with
//! `set_coverage_pause` (PROGRAM's P1 of M7).

use anchor_lang::prelude::Pubkey;
use shortwin::errors::ShortwinError;
use shortwin::state::ConfigChange;
use shortwin::state::MARKET_FLAG_PAUSED_COVERAGE;
use shortwin_keeper::coverage::CoverageState;
use shortwin_keeper::{DegradedReason, ExitResult, Keeper, PauseUnavailable};
use shortwin_keeper_litesvm::{LiteSvmChain, block_on};
use shortwin_litesvm_tests::engine::{add_margin_ix, sol_settle_ix};
use shortwin_litesvm_tests::{errors, fixtures, ix, pda};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::common::*;

/// The designated pauser's configuration.
pub fn pauser_cfg() -> KeeperConfig {
    let mut cfg = KeeperConfig::default();
    cfg.coverage_pause.enabled = true;
    cfg
}

/// Writes `key` as `GlobalConfig.coverage_authority` (the state an applied `Authorities` change
/// leaves; `enabled_through_the_timelock_and_revoked_by_rotation` runs the real change).
pub fn set_coverage_authority(env: &mut Env, key: Pubkey) {
    let mut cfg = env.config();
    cfg.coverage_authority = key;
    let lamports = env
        .svm
        .get_account(&pda::config())
        .expect("config")
        .lamports;
    fixtures::set_zc(&mut env.svm, &pda::config(), &cfg, 0);
    let mut a = env.svm.get_account(&pda::config()).expect("config");
    a.lamports = lamports;
    env.svm.set_account(pda::config(), a).expect("config");
}

/// A pauser keeper on `n` mock markets, the role enabled with a fresh key the keeper holds.
pub fn pauser_setup(n: usize, cfg: KeeperConfig) -> (Harness, Vec<MockMarket>, Keypair) {
    let (mut h, mms) = setup(n, cfg);
    let auth = Keypair::new();
    h.with_env(|env| set_coverage_authority(env, auth.pubkey()));
    h.keeper.set_coverage_authority(Some(auth.insecure_clone()));
    (h, mms, auth)
}

/// The market's on-chain `paused_coverage` flag.
pub fn paused(h: &Harness, market: &Pubkey) -> bool {
    h.with_env(|env| env.market(market).has_flag(MARKET_FLAG_PAUSED_COVERAGE))
}

/// Whether `r` reports the keeper's `set_coverage_pause(market, paused)` landed.
pub fn pause_sent(r: &TickReport, market: &Pubkey, paused: bool) -> bool {
    r.alerts.iter().any(|a| {
        matches!(a, Alert::CoveragePauseSent { market: m, paused: p } if m == market && *p == paused)
    })
}

/// A new borrower's open of `Q` on `mm`: `Ok` if it lands, else the program's error code.
pub fn try_open(h: &Harness, mm: &MockMarket) -> Result<(), Option<u32>> {
    h.with_env(|env| {
        let mut b = env.borrower();
        let (res, _) = env.open(mm, &mut b, Q);
        match res {
            Ok(_) => Ok(()),
            Err(f) => Err(errors::custom_code(&Err(f))),
        }
    })
}

fn code(e: ShortwinError) -> Option<u32> {
    Some(u32::from(e))
}

/// Ticks one second at a time until `f(report)` holds (at most `max` seconds); returns the
/// number of seconds ticked.
fn until(h: &mut Harness, max: u32, f: impl Fn(&TickReport) -> bool) -> Option<u32> {
    for s in 1..=max {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if f(&r) {
            return Some(s);
        }
    }
    None
}

/// Pumps `mm` by 4 % per second (a whale's buy before each keeper tick) until the keeper's
/// liquidation of `(b, nonce)` lands, then the whale sells back to the starting price; returns
/// the report of the liquidation's tick.
fn pump_until_liquidated(h: &mut Harness, mm: &MockMarket, b: &Borrower, nonce: u64) -> TickReport {
    let pos = position_key(mm, &b.key(), nonce);
    let whale = h.with_env(|env| env.actor(mm, 0, 5_000_000_000_000));
    let spot0 = h.with_env(|env| env.spot(mm));
    for s in 0..120 {
        h.with_env(|env| {
            env.warp(1);
            let target = env.spot(mm) * 104 / 100;
            env.push_price(mm, &whale, target);
        });
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        if landed_for(&r, ExitKind::Liquidate, &pos).next().is_some() {
            h.with_env(|env| {
                env.fund_base(mm, &whale, 500_000_000_000_000);
                env.push_price(mm, &whale, spot0);
            });
            return r;
        }
    }
    panic!("no liquidation");
}

/// ★ **The drill, outage path** (K7 (1)–(3)).
#[test]
fn drill_an_outage_pauses_opens_exits_continue_and_recovery_resumes_them() {
    let (mut h, mms, auth) = pauser_setup(1, pauser_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let (b1, n1) = open(&h, mm, Q);
    let (b2, n2) = open(&h, mm, Q);
    let (b3, n3) = open(&h, mm, Q);
    // b2 is made safe: it stays open through the pump below (and keeps the market covered).
    h.with_env(|env| {
        let kp = b2.kp.insecure_clone();
        ok(env.send(&[add_margin_ix(mm, &b2.key(), n2, 20_000_000_000)], &[&kp]));
    });
    for _ in 0..5 {
        let r = h.second().clone();
        assert!(r.coverage_pauses.is_empty() && r.alerts.is_empty(), "{r:?}");
    }
    assert!(!paused(&h, &key));

    // (1) The keeper is down for 45 s: its first integration after it reports the gap, the
    // market is degraded and the pause lands in the same tick.
    h.idle(45);
    let r = h.second().clone();
    let t_gap = h.now();
    assert!(
        r.alerts.iter().any(|a| matches!(
            a,
            Alert::Degraded {
                market,
                reason: DegradedReason::CoverageGap { gap_s: 46.., open_positions: 3 }
            } if *market == key
        )),
        "{:?}",
        r.alerts
    );
    assert!(pause_sent(&r, &key, true), "{:?}", r.alerts);
    assert!(paused(&h, &key));
    let rec = &r.coverage_pauses[0];
    assert!(rec.paused && rec.market == key);
    let ExitResult::Landed {
        outcome,
        unit_limit,
        bytes,
        ..
    } = &rec.result
    else {
        panic!("{rec:?}")
    };
    println!(
        "T-COVERAGE: the keeper's set_coverage_pause: {} CU (limit {unit_limit}), {bytes} B, fee {} \
         lamports (two signatures)",
        outcome.units_consumed, outcome.fee
    );
    assert!(outcome.fee >= 10_000, "fee payer + coverage_authority");
    let m = &h.keeper.metrics.markets[&key];
    assert_eq!((m.coverage_pauses, m.coverage_unpauses), (1, 0));
    assert_eq!(
        h.keeper.track(&key).unwrap().state,
        CoverageState::Recovering {
            degraded_since: t_gap,
            since: t_gap
        }
    );
    assert_eq!(h.keeper.coverage_authority(), Some(auth.pubkey()));

    // (2) Paused: opens are refused; exits, settlements, lender flows and cranks land.
    assert_eq!(try_open(&h, mm), Err(code(ShortwinError::CoveragePaused)));
    h.with_env(|env| {
        ok(env.close(mm, &b3, n3));
        let kp = b1.kp.insecure_clone();
        ok(env.send(&[add_margin_ix(mm, &b1.key(), n1, 1_000_000)], &[&kp]));
        let outsider = env.funded_keypair();
        let settle = sol_settle_ix(env, mm, &outsider.pubkey(), &b1.key(), n1);
        ok(env.send(&[settle], &[&outsider]));
        env.crank(mm);
        let lender = mm.lender.insecure_clone();
        let shares = env.lender(&key, &lender.pubkey()).expect("lender").shares / 10;
        ok(env.send(&[ix::withdraw(&mm.lender_keys(), shares, 0)], &[&lender]));
        ok(env.send(&[ix::claim_sol(&lender.pubkey(), &key)], &[&lender]));
    });
    let r = pump_until_liquidated(&mut h, mm, &b1, n1);
    assert!(paused(&h, &key), "still paused at the liquidation");
    let liq_at = h.now();
    // U28: the liquidation's `B̂_live/B̂_ref` at the keeper's decision is recorded, within the
    // bound it met (`B̂_live ≤ ⌊B̂_ref·(10⁴ + δ_liq)/10⁴⌋`).
    let hist = &h.keeper.metrics.b_ratio_landed;
    assert_eq!(hist.count, 1, "{hist:?}");
    let delta = h.with_env(|env| u64::from(env.config().params.delta_liq_bps));
    assert!(hist.sum <= 10_000 + delta, "{hist:?}");
    println!(
        "T-COVERAGE: under the pause the keeper liquidated the pumped position at +{} s; {} \
         exits in the report",
        liq_at - t_gap,
        r.exits.len()
    );

    // (3) The pause lifts `recovery_s` after the gap ended and an open lands.
    let waited = until(&mut h, 700, |r| pause_sent(r, &key, false)).expect("unpaused");
    let lifted = h.now();
    assert_eq!(
        lifted - t_gap,
        601,
        "restored at +600 s, lifted in that tick"
    );
    assert!(!paused(&h, &key));
    assert!(
        h.reports
            .iter()
            .rev()
            .take(2)
            .flat_map(|r| &r.alerts)
            .any(|a| matches!(a, Alert::CoverageRestored { market, .. } if *market == key))
    );
    assert_eq!(h.keeper.track(&key).unwrap().state, CoverageState::Healthy);
    // M7-SEC-3: opens resume `COVERAGE_CLEAR_DELAY_S` after the unpause.
    assert_eq!(try_open(&h, mm), Err(code(ShortwinError::CoveragePaused)));
    h.seconds(u32::try_from(shortwin::constants::COVERAGE_CLEAR_DELAY_S).unwrap());
    assert_eq!(try_open(&h, mm), Ok(()));
    let m = &h.keeper.metrics.markets[&key];
    assert_eq!((m.coverage_pauses, m.coverage_unpauses), (1, 1));
    assert_eq!(h.keeper.metrics.coverage_pause_failures, 0);
    // Nothing else: no second pause once the fresh position is covered.
    for _ in 0..40 {
        let r = h.second().clone();
        assert!(r.coverage_pauses.is_empty(), "{r:?}");
    }
    println!(
        "T-COVERAGE drill: paused at the gap (+0 s), liquidated under the pause at +{} s, \
         unpaused at +{} s ({waited} ticks after the liquidation)",
        liq_at - t_gap,
        lifted - t_gap
    );
}

/// ★ **The drill, budget path** (K7 (4); O17: the budget's exhaustion is degraded mode at once;
/// the global budget of decision A, 2026-10-01).
#[test]
fn drill_an_exhausted_budget_pauses_opens_and_restored_coverage_resumes_them() {
    let mut cfg = pauser_cfg();
    // Two cranks' worth of fees per UTC day (G), all of it the one covered market's entitlement.
    cfg.budget.global_daily_lamports = 10_000;
    let (mut h, mms, _auth) = pauser_setup(1, cfg);
    let mm = &mms[0];
    let key = mm.market();
    let (b1, n1) = open(&h, mm, Q);
    let (b2, n2) = open(&h, mm, Q);
    // b2 is made safe: only b1 is liquidated by the pump below.
    h.with_env(|env| {
        let kp = b2.kp.insecure_clone();
        ok(env.send(&[add_margin_ix(mm, &b2.key(), n2, 20_000_000_000)], &[&kp]));
    });
    let secs = until(&mut h, 70, |r| {
        r.alerts
            .iter()
            .any(|a| matches!(a, Alert::BudgetExhausted { market, .. } if *market == key))
    })
    .expect("budget exhausted");
    let r = h.reports.last().unwrap().clone();
    assert!(
        r.alerts.iter().any(|a| matches!(
            a,
            Alert::Degraded {
                market,
                reason: DegradedReason::Budget { spent: 10_000, budget: 10_000, rank: Some(1) }
            } if *market == key
        )),
        "{:?}",
        r.alerts
    );
    assert!(
        pause_sent(&r, &key, true),
        "paused in the same tick: {:?}",
        r.alerts
    );
    assert!(paused(&h, &key));
    // The pause is outside the budget: today's spend is the two cranks only.
    assert_eq!(h.keeper.track(&key).unwrap().spent_today, 10_000);
    assert_eq!(h.keeper.metrics.markets[&key].budget_degraded, 1);
    println!("T-COVERAGE budget drill: exhausted and paused at +{secs} s");

    assert_eq!(try_open(&h, mm), Err(code(ShortwinError::CoveragePaused)));
    // Exits are never budgeted: the keeper liquidates the pumped position under the pause (the
    // per-second cranks of the near position are exit-critical: booked to the overdraft).
    let r = pump_until_liquidated(&mut h, mm, &b1, n1);
    assert!(
        h.keeper.budget().overdraft > 0,
        "exit-critical cranks overdrew"
    );
    assert!(r.crank_failed.is_empty() || paused(&h, &key));
    assert!(paused(&h, &key));
    // Still exhausted, still degraded: the idle observations of b2 are refused.
    for _ in 0..40 {
        h.second();
    }
    assert!(h.keeper.track(&key).unwrap().state.is_degraded());
    assert!(paused(&h, &key));

    // The owner raises the budget (the same as the UTC day rolling over): the next observation
    // lands, and the pause lifts 600 s later.
    h.keeper.config_mut().budget.global_daily_lamports = 30_000_000;
    let r = h.second().clone();
    assert!(r.cranked.contains(&key), "{r:?}");
    let resumed = h.now();
    until(&mut h, 700, |r| pause_sent(r, &key, false)).expect("unpaused");
    assert_eq!(h.now() - resumed, 601);
    assert!(!paused(&h, &key));
    // M7-SEC-3: the clear delay, then opens land.
    assert_eq!(try_open(&h, mm), Err(code(ShortwinError::CoveragePaused)));
    h.seconds(u32::try_from(shortwin::constants::COVERAGE_CLEAR_DELAY_S).unwrap());
    assert_eq!(try_open(&h, mm), Ok(()));
}

/// K7 (5): a restart finds the flag set — recovering from its first sight, unpaused after 600 s,
/// never at once.
#[test]
fn a_restarted_pauser_keeps_the_pause_until_its_own_coverage_recovers() {
    let (mut h, mms, auth) = pauser_setup(1, pauser_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(3);
    h.idle(40);
    let r = h.second().clone();
    assert!(pause_sent(&r, &key, true), "{:?}", r.alerts);
    // A crash 10 s later; the new process starts 20 s after that.
    h.seconds(10);
    h.idle(20);
    h.restart(pauser_cfg());
    h.keeper.set_coverage_authority(Some(auth.insecure_clone()));
    let r = h.second().clone();
    let found = h.now();
    assert!(
        r.alerts
            .iter()
            .any(|a| matches!(a, Alert::CoveragePauseFound { market } if *market == key)),
        "{:?}",
        r.alerts
    );
    assert!(r.coverage_pauses.is_empty(), "no unpause at once");
    assert!(paused(&h, &key));
    until(&mut h, 700, |r| pause_sent(r, &key, false)).expect("unpaused");
    // The recovery counts from the restart's first second (the flag found set), not from the
    // first instance's gap.
    assert_eq!(h.now() - found, 601);
    assert!(!paused(&h, &key));
}

/// K7 (6): the role unset — inert: the pauser alerts once and sends nothing (the `Degraded` alert
/// pages the Emergency multisig); the program refuses the instruction; a rotated key and a
/// missing key alert the same way.
#[test]
fn an_unset_or_rotated_role_falls_back_to_the_page() {
    let (mut h, mms) = setup(1, pauser_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let held = Keypair::new();
    h.keeper.set_coverage_authority(Some(held.insecure_clone()));
    let _ = open(&h, mm, Q);
    h.seconds(3);
    h.idle(40);
    let mut unavailable = Vec::new();
    for _ in 0..5 {
        let r = h.second().clone();
        assert!(r.coverage_pauses.is_empty(), "nothing sent: {r:?}");
        unavailable.extend(r.alerts.iter().filter_map(|a| match a {
            Alert::CoveragePauseUnavailable {
                market,
                paused: true,
                reason,
            } if *market == key => Some(reason.clone()),
            _ => None,
        }));
    }
    assert_eq!(unavailable, vec![PauseUnavailable::AuthorityUnset], "once");
    assert!(h.keeper.track(&key).unwrap().state.is_degraded());
    assert!(!paused(&h, &key));
    assert_eq!(
        try_open(&h, mm),
        Ok(()),
        "not paused: the page is the pause path"
    );
    // The program refuses the instruction while the role is unset, whoever signs.
    h.with_env(|env| {
        let ix = shortwin_keeper::ixs::set_coverage_pause(&held.pubkey(), &key, true);
        let res = env.send(&[ix], &[&held]);
        errors::assert_err(&res, ShortwinError::CoverageAuthorityDisabled);
        let gov = env.governance.insecure_clone();
        let ix = shortwin_keeper::ixs::set_coverage_pause(&gov.pubkey(), &key, true);
        let res = env.send(&[ix], &[&gov]);
        errors::assert_err(&res, ShortwinError::CoverageAuthorityDisabled);
    });
    // The role set to another key: `WrongKey`; the instruction signed by the held key is refused.
    let other = Keypair::new();
    h.with_env(|env| set_coverage_authority(env, other.pubkey()));
    let r = h.second().clone();
    assert!(
        r.alerts.iter().any(|a| matches!(
            a,
            Alert::CoveragePauseUnavailable { reason: PauseUnavailable::WrongKey { configured, held: k }, .. }
                if *configured == other.pubkey() && *k == held.pubkey()
        )),
        "{:?}",
        r.alerts
    );
    h.with_env(|env| {
        let ix = shortwin_keeper::ixs::set_coverage_pause(&held.pubkey(), &key, true);
        errors::assert_err(&env.send(&[ix], &[&held]), ShortwinError::Unauthorized);
    });
    // No key loaded: `NoKey`.
    h.keeper.set_coverage_authority(None);
    let r = h.second().clone();
    assert!(
        r.alerts.iter().any(|a| matches!(
            a,
            Alert::CoveragePauseUnavailable {
                reason: PauseUnavailable::NoKey,
                ..
            }
        )),
        "{:?}",
        r.alerts
    );
    assert!(r.coverage_pauses.is_empty());
    // A keeper that is not the designated pauser sends nothing and raises none of these.
    let (mut h2, mms2) = setup(1, KeeperConfig::default());
    let auth = Keypair::new();
    h2.with_env(|env| set_coverage_authority(env, auth.pubkey()));
    h2.keeper
        .set_coverage_authority(Some(auth.insecure_clone()));
    let _ = open(&h2, &mms2[0], Q);
    h2.seconds(3);
    h2.idle(40);
    let r = h2.second().clone();
    assert!(r.alerts.iter().any(|a| matches!(a, Alert::Degraded { .. })));
    assert!(r.coverage_pauses.is_empty());
    assert!(
        !r.alerts
            .iter()
            .any(|a| matches!(a, Alert::CoveragePauseUnavailable { .. }))
    );
    assert!(!paused(&h2, &mms2[0].market()));
}

fn tick_other(k: &mut Keeper<LiteSvmChain>) -> TickReport {
    block_on(k.tick()).expect("keeper tick")
}

/// The Emergency multisig's `set_coverage_pause(false)` (★OWNER-M7-3 (b): clear only).
fn emergency_clear(h: &Harness, market: &Pubkey) {
    h.with_env(|env| ok(env.emergency_clear_coverage(market)));
}

/// ★OWNER-M7-3 (b) (owner decision of 2026-10-01; ARCH §7.1 duty 9's runbook): the coverage key
/// is hijacked — someone outside the designated pauser sets the flag on a market it holds
/// healthy. The pauser pages `CoveragePauseForeign` (it never sent that set) and adopts the pause
/// (`Recovering`), sending nothing; the Emergency multisig clears the flag at once and the pauser
/// leaves it clear (`CoveragePauseCleared` raised; the adopted recovery ends), opens resume
/// `COVERAGE_CLEAR_DELAY_S` after the clear; a second set by the thief is paged and undone the
/// same way.
#[test]
fn the_emergency_clears_a_hijacked_keys_pause_and_the_pauser_leaves_it_clear() {
    let (mut h, mms, auth) = pauser_setup(1, pauser_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(5);
    for round in 0..2 {
        // The thief's set, signed by the hijacked key outside the pauser.
        let thief = auth.insecure_clone();
        h.with_env(|env| {
            let ix = shortwin_keeper::ixs::set_coverage_pause(&thief.pubkey(), &key, true);
            ok(env.send(&[ix], &[&thief]));
        });
        let r = h.second().clone();
        assert!(
            r.alerts
                .iter()
                .any(|a| matches!(a, Alert::CoveragePauseForeign { market } if *market == key)),
            "round {round}: {:?}",
            r.alerts
        );
        assert!(r.coverage_pauses.is_empty(), "the pauser sends nothing");
        assert!(matches!(
            h.keeper.track(&key).unwrap().state,
            CoverageState::Recovering { .. }
        ));
        assert!(!h.keeper.track(&key).unwrap().own_degraded);
        assert_eq!(try_open(&h, mm), Err(code(ShortwinError::CoveragePaused)));
        // The Emergency clears it; the pauser leaves it clear.
        emergency_clear(&h, &key);
        let r = h.second().clone();
        assert!(
            r.alerts
                .iter()
                .any(|a| matches!(a, Alert::CoveragePauseCleared { market } if *market == key)),
            "{:?}",
            r.alerts
        );
        assert!(r.coverage_pauses.is_empty(), "not set again: {r:?}");
        assert_eq!(h.keeper.track(&key).unwrap().state, CoverageState::Healthy);
        assert!(!paused(&h, &key));
        // Opens wait out the clear delay, then land.
        let delay = u32::try_from(shortwin::constants::COVERAGE_CLEAR_DELAY_S).unwrap();
        h.seconds(delay);
        assert_eq!(try_open(&h, mm), Ok(()));
        for _ in 0..40 {
            let r = h.second().clone();
            assert!(r.coverage_pauses.is_empty(), "{r:?}");
            assert!(!paused(&h, &key));
        }
    }
    println!(
        "OWNER-M7-3 (b): a hijacked key's pause paged (CoveragePauseForeign), cleared by the \
         Emergency and left clear by the pauser, twice"
    );
}

/// ★OWNER-M7-3 (b): the Emergency clearing the pauser's **own** pause (a market it degraded and
/// still holds degraded) is set again in the same tick (`CoveragePauseCleared`); to override it,
/// the runbook stops the pauser first (`[coverage_pause] enabled = false`), after which the
/// Emergency's clear stays.
#[test]
fn the_emergency_clearing_the_pausers_own_pause_is_set_again_until_the_pauser_is_stopped() {
    let (mut h, mms, _auth) = pauser_setup(1, pauser_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(3);
    h.idle(45);
    let r = h.second().clone();
    assert!(pause_sent(&r, &key, true), "{:?}", r.alerts);
    assert!(h.keeper.track(&key).unwrap().own_degraded);
    emergency_clear(&h, &key);
    let r = h.second().clone();
    assert!(
        r.alerts
            .iter()
            .any(|a| matches!(a, Alert::CoveragePauseCleared { market } if *market == key))
    );
    assert!(pause_sent(&r, &key, true), "set again: {:?}", r.alerts);
    assert!(paused(&h, &key));
    // Stop the pauser, then clear: the clear stays (the keeper still holds the market degraded
    // and alerts; the page to the Emergency is the path).
    h.keeper.config_mut().coverage_pause.enabled = false;
    emergency_clear(&h, &key);
    for _ in 0..20 {
        let r = h.second().clone();
        assert!(r.coverage_pauses.is_empty(), "{r:?}");
        assert!(!paused(&h, &key));
    }
    assert!(h.keeper.track(&key).unwrap().state.is_degraded());
}

/// K7 (7): two instances (two regions), both holding the key and enabled — a misconfiguration
/// the design allows for: exactly one pause and one unpause land; the second instance adopts the
/// first one's pause (`Recovering`) instead of lifting it, and pages it as a set it did not send
/// (`CoveragePauseForeign`: a second coverage key, ★OWNER-M7-3 (b)).
#[test]
fn two_instances_never_flip_flop_the_flag() {
    let (mut h, mms, auth) = pauser_setup(1, pauser_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let mut b = h.another_keeper(pauser_cfg());
    b.set_coverage_authority(Some(auth.insecure_clone()));
    let _ = open(&h, mm, Q);
    let mut sent: Vec<(i64, char, bool)> = Vec::new();
    let mut found_b = false;
    let mut run = |h: &mut Harness,
                   b: &mut Keeper<LiteSvmChain>,
                   secs: u32,
                   sent: &mut Vec<(i64, char, bool)>| {
        for _ in 0..secs {
            let ra = h.second().clone();
            let rb = tick_other(b);
            for (who, r) in [('A', &ra), ('B', &rb)] {
                for p in &r.coverage_pauses {
                    if p.result.landed().is_some() {
                        sent.push((h.now(), who, p.paused));
                    }
                }
            }
            found_b |= rb
                .alerts
                .iter()
                .any(|a| matches!(a, Alert::CoveragePauseForeign { .. }));
        }
    };
    run(&mut h, &mut b, 3, &mut sent);
    h.idle(40);
    run(&mut h, &mut b, 640, &mut sent);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[0].2 && !sent[1].2, "{sent:?}");
    assert_eq!(sent[1].0 - sent[0].0, 601, "{sent:?}");
    assert!(found_b, "B adopted A's pause and paged it as foreign");
    assert!(!paused(&h, &key));
    println!("two instances: {sent:?}");
}

/// The role through the real timelock: an `Authorities` change enables it, and rotating it away
/// revokes the keeper's power (the pauser falls back to alerting `WrongKey`).
#[test]
fn enabled_through_the_timelock_and_revoked_by_rotation() {
    let auth = Keypair::new();
    let mut env = Env::test_venue();
    let authorities = |cov: Pubkey, env: &Env| {
        let c = env.config();
        ConfigChange::Authorities {
            governance: c.governance,
            pause_authority: c.pause_authority,
            coverage_authority: cov,
            fee_recipient: c.fee_recipient,
        }
    };
    let apply = |env: &mut Env, change: ConfigChange| {
        let gov = env.governance.insecure_clone();
        ok(env.send(
            &[ix::propose_config(&gov.pubkey(), change.clone())],
            &[&gov],
        ));
        env.warp(i64::from(env.config().timelock_s));
        ok(env.send(&[ix::apply_config_for(&change)], &[]));
    };
    let change = authorities(auth.pubkey(), &env);
    apply(&mut env, change);
    assert_eq!(env.config().coverage_authority, auth.pubkey());
    let mm = env.mock_market(MarketSpec::default());
    let key = mm.market();
    let mut h = Harness::new(env, pauser_cfg());
    h.keeper.set_coverage_authority(Some(auth.insecure_clone()));
    let _ = open(&h, &mm, Q);
    h.seconds(3);
    h.idle(40);
    let r = h.second().clone();
    assert!(pause_sent(&r, &key, true), "{:?}", r.alerts);
    // Governance rotates the role away (a lost or compromised key; the keeper is down through the
    // 48-h timelock here): the keeper's key no longer works, so once its coverage has recovered it
    // cannot lift the pause it set — it alerts `WrongKey` (page the new key's holder, or the
    // Emergency multisig), once; the flag stays set.
    let new = Keypair::new();
    h.with_env(|env| {
        let change = authorities(new.pubkey(), env);
        apply(env, change);
    });
    let mut wrong = 0;
    for _ in 0..700 {
        let r = h.second().clone();
        assert!(r.coverage_pauses.is_empty(), "{r:?}");
        wrong += r
            .alerts
            .iter()
            .filter(|a| {
                matches!(
                    a,
                    Alert::CoveragePauseUnavailable {
                        paused: false,
                        reason: PauseUnavailable::WrongKey { .. },
                        ..
                    }
                )
            })
            .count();
    }
    assert_eq!(wrong, 1);
    assert_eq!(h.keeper.track(&key).unwrap().state, CoverageState::Healthy);
    assert!(paused(&h, &key));
    h.with_env(|env| {
        let ix = shortwin_keeper::ixs::set_coverage_pause(&auth.pubkey(), &key, false);
        errors::assert_err(&env.send(&[ix], &[&auth]), ShortwinError::Unauthorized);
        let ix = shortwin_keeper::ixs::set_coverage_pause(&new.pubkey(), &key, false);
        ok(env.send(&[ix], &[&new]));
    });
    assert!(!paused(&h, &key));
}
