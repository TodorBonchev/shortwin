//! Degraded mode (ARCH §7.1 duty 9; ECON §5.8): a keeper that cannot meet its coverage on a
//! market says so — the program's `CoverageGap` after an outage (a gap above `COVERAGE_GAP_S`,
//! 40 s since OWNER-M7-12), or a charge the global O17 budget refused — and the alert lifts after
//! 10 minutes of restored coverage.

use shortwin_keeper::DegradedReason;
use shortwin_keeper::coverage::CoverageState;

use crate::common::*;

#[test]
fn an_outage_is_reported_through_coverage_gap_and_recovers_after_ten_minutes() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let key = mms[0].market();
    let _ = open(&h, &mms[0], Q);
    let r = h.second().clone();
    assert!(r.alerts.is_empty(), "no gap after the open: {:?}", r.alerts);
    // The keeper is down for 45 s: the next integration follows a 46 s gap.
    h.idle(45);
    let r = h.second().clone();
    let gap = r.alerts.iter().find_map(|a| match a {
        Alert::Degraded {
            market,
            reason:
                DegradedReason::CoverageGap {
                    gap_s,
                    open_positions,
                },
        } if *market == key => Some((*gap_s, *open_positions)),
        _ => None,
    });
    let (gap_s, open) = gap.unwrap_or_else(|| panic!("{:?}", r.alerts));
    assert_eq!(gap_s, 46);
    assert_eq!(open, 1);
    assert!(h.keeper.track(&key).unwrap().state.is_degraded());
    assert_eq!(h.keeper.metrics.markets[&key].coverage_gaps, 1);
    // Ten minutes of met coverage lift it.
    let mut restored = None;
    for i in 0..610 {
        let r = h.second();
        if r.alerts
            .iter()
            .any(|a| matches!(a, Alert::CoverageRestored { market, .. } if *market == key))
        {
            restored = Some(i);
            break;
        }
    }
    assert!(restored.is_some_and(|i| i >= 599), "{restored:?}");
    assert_eq!(h.keeper.track(&key).unwrap().state, CoverageState::Healthy);
    // A second outage pages again.
    h.idle(40);
    let r = h.second().clone();
    assert!(
        r.alerts
            .iter()
            .any(|a| matches!(a, Alert::Degraded { market, .. } if *market == key)),
        "{:?}",
        r.alerts
    );
}

/// O17 (owner decision of 2026-09-30; the global budget of decision A, 2026-10-01): a charge the
/// budget refuses on a market with open positions is degraded mode **at once**
/// (`DegradedReason::Budget`, at the first observation the budget refuses) — before the M7 change
/// it waited for `degraded_after_misses` (3) consecutive misses, which on the 30-s idle cadence
/// took 90 s. The skipped observations still count as misses (and escalate the price for when the
/// budget returns).
#[test]
fn an_exhausted_budget_misses_observations_and_degrades_the_market() {
    let mut cfg = KeeperConfig::default();
    // Two cranks' worth of fees per day, all of it the one covered market's entitlement.
    cfg.budget.global_daily_lamports = 10_000;
    let (mut h, mms) = setup(1, cfg);
    let key = mms[0].market();
    let _ = open(&h, &mms[0], Q);
    let mut alerts = Vec::new();
    let mut degraded_at = None;
    let mut exhausted_at = None;
    for s in 0..70 {
        let r = h.second();
        for a in &r.alerts {
            match a {
                Alert::BudgetExhausted { market, .. } if *market == key => {
                    exhausted_at.get_or_insert(s);
                }
                Alert::Degraded { market, .. } if *market == key => {
                    degraded_at.get_or_insert(s);
                }
                _ => {}
            }
        }
        alerts.extend(r.alerts.clone());
    }
    assert!(exhausted_at.is_some(), "{alerts:?}");
    assert!(
        alerts.iter().any(|a| matches!(
            a,
            Alert::Degraded {
                market,
                reason: DegradedReason::Budget {
                    spent: 10_000,
                    budget: 10_000,
                    rank: Some(1)
                }
            } if *market == key
        )),
        "{alerts:?}"
    );
    assert_eq!(
        degraded_at, exhausted_at,
        "degraded in the tick the budget ran out"
    );
    let m = &h.keeper.metrics.markets[&key];
    assert_eq!(m.cranks, 2);
    assert_eq!(m.budget_degraded, 1);
    assert!(m.misses >= 1 && m.degraded);
    // Lifting the budget restores coverage at once.
    h.keeper.config_mut().budget.global_daily_lamports = 0;
    let r = h.second();
    assert!(r.cranked.contains(&key));
    h.second();
    assert!(matches!(
        h.keeper.track(&key).unwrap().state,
        CoverageState::Recovering { .. }
    ));
}
