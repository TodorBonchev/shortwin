//! S3 on the mock venue (ARCH §3.8 step 4, §5.3; ECON §10.3; SPEC §5.3, §11 O6): T-S3-BROKEN
//! (the Emergency route-broken flag starts the S3 chain and changes nothing else), a disabled
//! venue and a venue-state layout change (not executable, A63: the chain starts by itself), the
//! S3 timer and its continuity, and the S3 amount (FV at the frozen EMA point, O15's expiry
//! fee for an expired position).

#![allow(clippy::result_large_err)]

#[path = "settle_support.rs"]
mod support;

use shortwin::errors::ShortwinError as E;
use shortwin_math::fees::CpmmFees;
use shortwin_math::quote::{cpmm, reprice};
use shortwin_math::rent;
use support::*;

/// `FV(P, L) = B̂(Q; reprice(L, P)) + ⌈p'·B̂/10⁴⌉ + R_due` at a one-way fee of `fee_ppm`
/// (ARCH §4.5), recomputed off-chain from the math crate.
fn fair_value(q: u64, p: u128, l: u128, fee_ppm: u64, pprime_bps: u16, r_due: u64) -> u64 {
    let (x, y) = reprice(l, p).expect("reprice");
    let fees = CpmmFees {
        trade: fee_ppm,
        ..mock::cpmm_fees(0)
    };
    let b = cpmm::buy_exact_out(q, y, x, &fees).expect("quote").cost;
    b + (u128::from(b) * u128::from(pprime_bps)).div_ceil(10_000) as u64 + r_due
}

/// Ages the market past `graduated_at + grad_grace_s` (the mock market's age counts from its
/// listing: no S3 can start earlier, ARCH §5.3), then moves the pool to `frac` of the S1
/// boundary (from the spot before the move) and lets the EMA follow: the position is then
/// CR-liquidatable on both legs but outside S1 (and, on a deep pool, S2): it would be
/// liquidated normally.
fn make_liquidatable(sc: &mut Sc, frac: f64) {
    let grace = i64::from(sc.pos().grad_grace_s);
    let age = sc.env.now() - sc.market().graduated_at;
    if age < grace {
        sc.env.warp(grace - age);
        sc.env.tick(&sc.mm, 5);
    }
    let p0 = sc.spot();
    let kb = sc.s1_boundary();
    sc.move_to(scale(p0, kb * frac));
    for _ in 0..20 {
        let out = sc.second();
        assert!(out.settled.is_none() && out.armed.is_none());
    }
}

mod t_s3_broken {
    use super::*;

    /// T-S3-BROKEN (ARCH §5.3, §8.2; V2-1, V1-7): an expired healthy position under the
    /// Emergency flag — market scope or the venue's bit — settles S3 once the continuous chain
    /// has run `s3_wait_s`; before its deadline the same position never settles (the flag never
    /// makes the spot leg true). The venue still executes, so the box is the ring's, and the
    /// amount is FV at the EMA with the **expiry fee** at both corners (O15), below `V − K_s`.
    #[test]
    fn a_flagged_route_settles_expired_positions_after_the_timer() {
        for venue_scope in [false, true] {
            let mut sc = Sc::default_market();
            sc.env.tick(&sc.mm, 5);
            // 700 s before the deadline the Emergency multisig flags the route.
            sc.warp_to_deadline(-700);
            if venue_scope {
                sc.set_venue_route_broken(true);
            } else {
                sc.set_route_broken(true);
            }
            // The keeper observes every 30 s (duty 5) and calls sol_settle: a healthy position
            // with the flag set is a no-op, however long the chain.
            for _ in 0..23 {
                sc.env.warp(30);
                sc.crank();
                let out = sc.settle_ok();
                assert!(out.settled.is_none() && out.armed.is_none());
            }
            let m = sc.market();
            assert!(m.no_route_since > 0 && sc.env.now() - m.no_route_since >= 600);
            assert_eq!(m.pool_fail_since, 0, "pool_fail never reads the flag");
            assert!(sc.env.now() < sc.pos().deadline);
            let pos = sc.pos();
            // At the deadline, per-second keeper calls: S3.
            sc.warp_to_deadline(0);
            let (_s, ev) = sc.run_until_settled(10).expect("S3");
            assert_eq!(ev.condition, 3);
            assert_eq!(
                (ev.pprime_lo, ev.pprime_hi),
                (pos.expiry_fee_bps, pos.expiry_fee_bps),
                "an expired position that is not CR-liquidatable pays the expiry fee (O15)"
            );
            let take = pos.v - k_s(&pos);
            assert!(ev.fv > 0 && ev.lender_amount == ev.fv.min(take) && ev.lender_amount < take);
            assert_eq!(ev.borrower_amount, take - ev.lender_amount);
            assert!(ev.ema_age_s <= 1, "the venue is observed: a fresh EMA");
            assert!(ev.p_lo <= ev.p_hi && ev.l_lo <= ev.l_hi);
        }
    }

    /// T-S3-BROKEN: with the flag set, the arming clock, S1's timing and amount, every close
    /// outcome and the `pool_fail` chain equal those of the unflagged run from the same state;
    /// the flag only adds the S3 chain.
    #[test]
    fn the_flag_changes_nothing_but_the_s3_chain() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        let saved = sc.env.svm.clone();
        let mut runs = Vec::new();
        for flag in [false, true] {
            sc.env.svm = saved.clone();
            if flag {
                sc.set_route_broken(true);
            }
            let kb = sc.s1_boundary();
            sc.move_by(kb * 1.05);
            let mut trace = Vec::new();
            let mut settled = None;
            for s in 1..=80u32 {
                let out = sc.second();
                let m = sc.market();
                trace.push((
                    out.armed.map(|a| a.s_count),
                    sc.pos_opt().map(|p| p.arm_state()),
                    m.pool_fail_since,
                    m.ema_count,
                ));
                if let Some(ev) = out.settled {
                    settled = Some((s, ev.condition, ev.lender_amount, ev.borrower_amount));
                    break;
                }
            }
            let m = sc.market();
            runs.push((trace, settled, m.no_route_since));
        }
        let (a, b) = (&runs[0], &runs[1]);
        assert_eq!(a.0, b.0, "arming, clock and pool_fail chain are identical");
        assert_eq!(a.1, b.1, "S1 at the same second with the same amount");
        assert_eq!(a.1.map(|s| s.1), Some(1));
        assert_eq!(a.2, 0);
        assert!(b.2 > 0, "only the flagged run has an S3 chain");
    }

    /// T-S3-BROKEN (the close side): under the flag a liquidatable position still closes, with
    /// the same buyback, rent and payout as without it (I10, V2-1); and a liquidatable position
    /// that nobody liquidates settles S3 at the timer under the flag only.
    #[test]
    fn closes_are_unchanged_and_liquidatable_positions_reach_s3() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        make_liquidatable(&mut sc, 0.8);
        let saved = sc.env.svm.clone();
        let (honest, _) = close_ok(&mut sc.env, &sc.mm, &sc.b, sc.nonce);
        sc.env.svm = saved.clone();
        sc.set_route_broken(true);
        let (flagged, _) = close_ok(&mut sc.env, &sc.mm, &sc.b, sc.nonce);
        assert_eq!(
            (flagged.buyback_cost, flagged.rent, flagged.borrower_payout),
            (honest.buyback_cost, honest.rent, honest.borrower_payout)
        );
        // Unflagged: the liquidatable position just waits (the liquidation track's exit).
        sc.env.svm = saved.clone();
        sc.crank_every(30, 21);
        assert!(sc.settle_ok().settled.is_none());
        // Flagged: S3 once the chain has run s3_wait_s, capped at FV with the penalty.
        sc.env.svm = saved;
        sc.set_route_broken(true);
        let pos = sc.pos();
        sc.crank_every(30, 20);
        assert!(
            sc.settle_ok().settled.is_none(),
            "the S3 timer is still running"
        );
        let (_s, ev) = sc.run_until_settled(40).expect("S3");
        assert_eq!(ev.condition, 3);
        assert_eq!(
            (ev.pprime_lo, ev.pprime_hi),
            (pos.penalty_bps, pos.penalty_bps)
        );
        let m = sc.market();
        assert!(sc.env.now() >= m.no_route_since + i64::from(pos.s3_wait_s));
    }
}

/// The keeper's exits of a liquidatable position under a price path, second by second: every
/// second the keeper cranks and tries `liquidate`; the refusal codes until it executes, and the
/// executed event's amounts.
type LiqRun = (Vec<Option<u32>>, Option<(u32, [u64; 10])>, i64, i64);

fn liquidation_run(sc: &mut Sc, flag: bool, keeper: &solana_keypair::Keypair) -> LiqRun {
    if flag {
        sc.set_route_broken(true);
    }
    liquidation_path(sc, keeper, 1.01, 90)
}

/// One keeper `liquidate` of `borrower`'s position `nonce` now: the executed event's amounts
/// (buyback, quotes, bound, rent, penalty and its keeper share, keeper top-up, borrower payout,
/// token surplus), or the refusal code.
fn liquidate_once(
    sc: &mut Sc,
    keeper: &solana_keypair::Keypair,
    borrower: &anchor_lang::prelude::Pubkey,
    nonce: u64,
) -> Result<[u64; 10], Option<u32>> {
    use solana_signer::Signer;
    let ix =
        shortwin_litesvm_tests::engine::liquidate_ix(&sc.mm, &keeper.pubkey(), borrower, nonce);
    let res = sc.env.send(&[ix], &[keeper]);
    match &res {
        Ok(meta) => {
            let e: shortwin::events::Liquidate = events::one(meta);
            Ok([
                e.buyback_cost,
                e.b_live,
                e.b_ref,
                e.max_in,
                e.rent,
                e.penalty,
                e.penalty_keeper,
                e.keeper_topup,
                e.borrower_payout,
                e.token_surplus,
            ])
        }
        Err(_) => Err(custom_code(&res)),
    }
}

/// [`liquidation_run`]'s path from the state as it is: `n` seconds, each a move by `step`, a
/// crank and the keeper's `liquidate`.
fn liquidation_path(sc: &mut Sc, keeper: &solana_keypair::Keypair, step: f64, n: u32) -> LiqRun {
    let mut refusals = Vec::new();
    let mut done = None;
    for s in 1..=n {
        sc.move_by(step);
        sc.env.warp(1);
        sc.crank();
        let (b, nonce) = (sc.b.key(), sc.nonce);
        match liquidate_once(sc, keeper, &b, nonce) {
            Ok(amounts) => {
                done = Some((s, amounts));
                break;
            }
            Err(code) => refusals.push(code),
        }
    }
    let m = sc.market();
    (refusals, done, m.pool_fail_since, m.no_route_since)
}

mod t_s3_broken_exits {
    use super::*;
    use solana_signer::Signer;

    /// T-S3-BROKEN (the liquidation side; V2-1, I10): along the same rising path, with and
    /// without the route-broken flag, the keeper's `liquidate` is refused with the same codes
    /// in the same seconds, executes in the same second with identical amounts (buyback,
    /// quotes, bound, rent, penalty and its split, keeper top-up, borrower payout), and the
    /// `pool_fail` chain stays empty in both runs. The flag's only trace is its S3 chain.
    #[test]
    fn the_liquidatable_set_and_liquidations_equal_the_unflagged_run() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        let keeper = sc.env.funded_keypair();
        let saved = sc.env.svm.clone();
        let plain = liquidation_run(&mut sc, false, &keeper);
        sc.env.svm = saved;
        let flagged = liquidation_run(&mut sc, true, &keeper);
        assert!(
            plain.1.is_some(),
            "the path makes the position liquidatable"
        );
        assert!(!plain.0.is_empty(), "it was healthy first");
        assert_eq!(plain.0, flagged.0, "the same refusals, second by second");
        assert_eq!(plain.1, flagged.1, "the same liquidation, to the lamport");
        assert_eq!(
            (plain.2, flagged.2),
            (0, 0),
            "pool_fail never reads the flag"
        );
        assert_eq!(plain.3, 0);
        assert!(flagged.3 > 0, "only the flagged run has an S3 chain");
    }

    /// ECON §8.6's race under the Emergency flag (O15; ARCH §8.2 round 3 "Emergency-flag S3
    /// raced against an honest expiry"): at the deadline of a flagged, expired, healthy
    /// position whose S3 chain has run `s3_wait_s`, both the honest keeper's `expire()` and a
    /// lender-keeper's `sol_settle(S3)` are valid, and whichever lands first closes the
    /// position (the other then finds no position). The S3 amount is FV with the **expiry fee**
    /// (`p'` on both corners): the lenders get the tokens' executable value in SOL plus the fee
    /// and the rent — what the expiry pays them in tokens and SOL, within the EMA-to-spot
    /// difference — and the borrower loses about `K_s − K_e` more.
    #[test]
    fn an_emergency_flag_s3_races_an_honest_expiry() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        sc.warp_to_deadline(-700);
        sc.set_route_broken(true);
        for _ in 0..23 {
            sc.env.warp(30);
            sc.crank();
            assert!(sc.settle_ok().settled.is_none());
        }
        // The keeper covers the last seconds before a deadline every second (duty 1), so the
        // expiry has its fresh completed bucket.
        sc.warp_to_deadline(-5);
        sc.env.tick(&sc.mm, 5);
        assert_eq!(sc.env.now(), sc.pos().deadline);
        let pos = sc.pos();
        let honest = sc.env.funded_keypair();
        let saved = sc.env.svm.clone();

        // The honest keeper first: an ordinary expiry, the flag ignored (I10).
        let ix = shortwin_litesvm_tests::engine::expire_ix(
            &sc.mm,
            &honest.pubkey(),
            &sc.b.key(),
            sc.nonce,
        );
        let exp: shortwin::events::Expire = events::one(&ok(sc.env.send(&[ix], &[&honest])));
        assert!(sc.pos_opt().is_none(), "the expiry closed the position");
        assert!(
            sc.settle().is_err(),
            "the lender-keeper's S3 finds no position"
        );

        // The lender-keeper first: S3 at FV with the expiry fee.
        sc.env.svm = saved;
        let ev = sc.settle_ok().settled.expect("S3 at the deadline");
        assert_eq!(ev.condition, 3);
        assert_eq!(
            (ev.pprime_lo, ev.pprime_hi),
            (pos.expiry_fee_bps, pos.expiry_fee_bps),
            "O15: the expiry fee at both corners"
        );
        let ix = shortwin_litesvm_tests::engine::expire_ix(
            &sc.mm,
            &honest.pubkey(),
            &sc.b.key(),
            sc.nonce,
        );
        assert!(
            sc.env.send(&[ix], &[&honest]).is_err(),
            "the honest expiry finds no position"
        );

        // The two outcomes (ECON §8.6's table): the lenders' S3 take is the expiry's buyback,
        // fee and rent in SOL, within the EMA-to-spot difference of the quote ...
        let expiry_value = exp.buyback_cost + exp.expiry_fee + exp.rent;
        let diff = ev.lender_amount.abs_diff(expiry_value);
        assert!(
            diff * 1_000 <= expiry_value,
            "S3 take {} vs the expiry's B + fee + rent {expiry_value}",
            ev.lender_amount
        );
        assert_eq!(ev.lender_amount, ev.fv.min(pos.v - ev.k_s));
        // ... and the borrower loses K_s − K_e more (plus that same difference).
        let extra = i128::from(exp.borrower_payout) - i128::from(ev.borrower_amount);
        let keeper_gap = i128::from(ev.k_s) - i128::from(exp.keeper_reward);
        assert!(keeper_gap > 0, "K_s above K_e");
        assert!(
            (extra - keeper_gap).unsigned_abs() <= u128::from(diff),
            "borrower: expiry {} vs S3 {} (K_s {} / K_e {})",
            exp.borrower_payout,
            ev.borrower_amount,
            ev.k_s,
            exp.keeper_reward
        );
    }
}

mod fee_hike {
    use super::*;

    /// A venue fee above the global hard cap (ARCH §4.3 "two fee views", §5.3; A43, V2-1,
    /// I16): the pool still trades, so it is still observed, but it is `s3_no_route` evidence;
    /// an expired position settles S3 at the timer with FV at the position's locked cap, never
    /// at the live fee (an admin fee hike cannot inflate FV). A healthy position under the hike
    /// never settles.
    #[test]
    fn a_fee_above_the_hard_cap_is_s3_evidence_and_fv_stays_at_the_cap() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        sc.warp_to_deadline(-700);
        sc.knobs(|k| k.fee_ppm = 60_000);
        for _ in 0..23 {
            sc.env.warp(30);
            sc.crank();
            assert!(
                sc.settle_ok().settled.is_none(),
                "not expired, not liquidatable"
            );
        }
        let m = sc.market();
        assert!(m.no_route_since > 0 && m.pool_fail_since > 0 && m.b_lo.get() > 0);
        let pos = sc.pos();
        sc.warp_to_deadline(0);
        let (_s, ev) = sc.run_until_settled(10).expect("S3");
        assert_eq!(ev.condition, 3);
        let m = sc.market();
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let cap_ppm = u64::from(pos.venue_fee_hard_cap_bps) * 100;
        assert_eq!(
            ev.fv,
            fair_value(pos.q, p, l, cap_ppm, ev.pprime_lo, r_due),
            "FV at (P_ema, L_ema) and the locked cap"
        );
        assert_eq!(ev.pprime_lo, pos.expiry_fee_bps);
    }
}

mod not_executable {
    use super::*;

    /// A disabled venue (an admin halt; A64, V2-5): nothing is observed, the S3 chain starts
    /// by itself, closes fail with `NoRoute`, and a liquidatable position settles S3 at the
    /// timer with FV at the **frozen** EMA point — the box collapses to it after `RING_LEN`
    /// seconds without an integration — priced at the halted pool's own fee.
    #[test]
    fn a_disabled_venue_reaches_s3_after_the_timer() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        make_liquidatable(&mut sc, 0.8);
        let pos = sc.pos();
        sc.knobs(|k| k.disabled = true);
        sc.env.warp(1);
        let out = sc.settle_ok();
        assert!(out.settled.is_none());
        let m0 = sc.market();
        assert_eq!(
            m0.no_route_since,
            sc.env.now(),
            "the chain starts at the first call"
        );
        assert_eq!(m0.b_lo.get(), 0, "nothing recorded");
        assert_err(&sc.env.close(&sc.mm, &sc.b, sc.nonce), E::NoRoute);
        // A gap longer than S3_OBS_GAP_S (60 s) restarts the chain (C0-16).
        sc.env.warp(61);
        sc.crank();
        let since = sc.market().no_route_since;
        assert_eq!(since, sc.env.now());
        sc.crank_every(30, 19);
        assert!(sc.settle_ok().settled.is_none(), "not yet: 570 s of chain");
        let (_s, out) = sc.run_until_settled_out(40).expect("S3");
        eprintln!("sol_settle S3: {} trace entries, {} CU", out.trace, out.cu);
        assert!(out.trace == 2 && out.cu <= 150_000);
        let ev = out.settled.unwrap();
        assert_eq!(ev.condition, 3);
        assert!(sc.env.now() >= since + i64::from(pos.s3_wait_s));
        let m = sc.market();
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        assert_eq!(
            (p, l, m.ema_count),
            (m0.ema_price.get(), m0.ema_liq.get(), m0.ema_count),
            "the EMA is frozen while the venue cannot execute"
        );
        assert_eq!(
            (ev.p_lo, ev.p_hi, ev.l_lo, ev.l_hi),
            (p, p, l, l),
            "the EMA point"
        );
        assert!(ev.ema_age_s > 600, "the EMA is as old as the halt");
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let fv = fair_value(pos.q, p, l, u64::from(FEE_PPM), pos.penalty_bps, r_due);
        assert_eq!(ev.fv, fv, "FV at the pool's own fee");
        assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
    }

    /// A venue-state layout change (A63, V1-8: `layout_version = 1` moves the reserve counters,
    /// so the pool no longer parses): not executable — the crank and `sol_settle` succeed,
    /// record nothing, and start the chain; S3 settles at the timer, and with no readable fee
    /// FV is priced at the position's hard cap (`min(φ_live, cap)` with `φ_live` unknown).
    #[test]
    fn a_layout_change_is_not_executable_and_settles_s3_at_the_cap_fee() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        make_liquidatable(&mut sc, 0.8);
        let pos = sc.pos();
        sc.knobs(|k| k.layout_version = 1);
        sc.env.warp(1);
        sc.crank();
        let m = sc.market();
        assert_eq!((m.no_route_since, m.b_lo.get()), (sc.env.now(), 0));
        assert!(sc.settle_ok().settled.is_none());
        sc.crank_every(30, 19);
        let (_s, ev) = sc.run_until_settled(40).expect("S3");
        assert_eq!(ev.condition, 3);
        let m = sc.market();
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let cap_ppm = u64::from(pos.venue_fee_hard_cap_bps) * 100;
        let fv = fair_value(pos.q, p, l, cap_ppm, pos.penalty_bps, r_due);
        assert_eq!(ev.fv, fv, "FV at the hard cap");
        assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
    }

    /// V2-5 (disclosed): a healthy position on a halted venue stays frozen — no S3 (it is
    /// neither liquidatable nor expired; the frozen EMA cannot move), no close — until the venue
    /// returns; then the chain clears and everything works again.
    #[test]
    fn a_healthy_position_on_a_halted_venue_stays_frozen() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        sc.knobs(|k| k.disabled = true);
        sc.crank_every(30, 25);
        let out = sc.settle_ok();
        assert!(out.settled.is_none() && out.armed.is_none());
        assert!(sc.env.now() - sc.market().no_route_since >= 700);
        assert_err(&sc.env.close(&sc.mm, &sc.b, sc.nonce), E::NoRoute);
        sc.knobs(|k| k.disabled = false);
        sc.env.warp(1);
        sc.crank();
        assert_eq!(
            sc.market().no_route_since,
            0,
            "an observation with a route clears it"
        );
        sc.env.warp(1);
        close_ok(&mut sc.env, &sc.mm, &sc.b, sc.nonce);
    }
}

/// G-G3 (ARCH §4.3 "two fee views", §5.3; A26, V2-1, R3A-4 c): Governance moving the **global**
/// `venue_fee_hard_cap_bps` across the live fee while a position is open. The global cap is an
/// input of the market-level route predicates only (`s3_no_route`, `pool_fail`); every quote of
/// an open position — its risk view, its exit bounds, its S3 fair value — takes the cap the
/// position snapshotted at its open.
mod hard_cap {
    use super::*;
    use shortwin::state::ConfigChange;
    use shortwin_litesvm_tests::ix;
    use solana_signer::Signer;

    /// The live fee of these scenarios: 4 %, between the venue cap (150 bps) and the default
    /// hard cap (500 bps), so opens are refused and nothing else changes.
    const LIVE_FEE_PPM: u32 = 40_000;
    /// The lowered global hard cap: below the live fee, at its compiled floor.
    const LOWERED_BPS: u32 = 300;

    /// Governance proposes `venue_fee_hard_cap_bps = cap`, every other parameter kept.
    fn propose_hard_cap(sc: &mut Sc, cap: u32) {
        let c = sc.env.config();
        let mut params = c.params;
        params.venue_fee_hard_cap_bps = cap;
        let gov = sc.env.governance.insecure_clone();
        let change = ConfigChange::Params {
            params,
            tiers: c.tiers,
        };
        ok(sc
            .env
            .send(&[ix::propose_config(&gov.pubkey(), change)], &[&gov]));
    }

    /// Anyone applies the pending change.
    fn apply(sc: &mut Sc) {
        let ix = sc.env.apply_config_ix();
        ok(sc.env.send(&[ix], &[]));
    }

    /// The second position of the G-G3 scenarios: a borrower and the nonce of its position.
    struct Second(Borrower, u64);

    /// Two positions open at the default caps — the scenario's [`Q`] and, opened after the pool
    /// rose 5 % and the EMA followed, a second one of `Q / 2` (it becomes liquidatable later on
    /// a rising path) — the pool's fee raised to [`LIVE_FEE_PPM`] by its admin, and a hard-cap
    /// change to `cap` proposed and timelocked (the keeper cranking every 30 s through the last
    /// 10 minutes, then every second for 5 s): the change is ready to apply.
    fn ready(cap: u32) -> (Sc, Second) {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        sc.move_by(1.05);
        sc.crank_every(30, 20);
        let mut b2 = sc.env.borrower();
        let (_open, n2, _) = open_ok(&mut sc.env, &sc.mm, &mut b2, Q / 2);
        sc.knobs(|k| k.fee_ppm = LIVE_FEE_PPM);
        propose_hard_cap(&mut sc, cap);
        sc.env.warp(i64::from(sc.env.config().timelock_s) - 600);
        sc.crank_every(30, 20);
        sc.env.tick(&sc.mm, 5);
        assert_eq!(sc.pos().venue_fee_hard_cap_bps, 500);
        assert_eq!(
            sc.env
                .position(&sc.mm, &b2.key(), n2)
                .venue_fee_hard_cap_bps,
            500
        );
        (sc, Second(b2, n2))
    }

    /// One keeper `liquidate` attempt (its amounts or refusal code).
    type Attempt = Result<[u64; 10], Option<u32>>;

    /// A G-G3 run from the state as it is: each second the pool moves by `step`, the keeper
    /// cranks and tries `liquidate` on each position still open (`(second, position, attempt)`,
    /// position 0 the scenario's, 1 the [`Second`]), until both are liquidated or `n` seconds
    /// pass; then the market's `pool_fail_since` and `no_route_since`.
    type PairRun = (Vec<(u32, usize, Attempt)>, i64, i64);

    fn pair_path(
        sc: &mut Sc,
        keeper: &solana_keypair::Keypair,
        second: &Second,
        step: f64,
        n: u32,
    ) -> PairRun {
        let owners = [(sc.b.key(), sc.nonce), (second.0.key(), second.1)];
        let mut open = [true; 2];
        let mut attempts = Vec::new();
        for s in 1..=n {
            sc.move_by(step);
            sc.env.warp(1);
            sc.crank();
            for (i, (b, nonce)) in owners.iter().enumerate() {
                if open[i] {
                    let a = liquidate_once(sc, keeper, b, *nonce);
                    open[i] = a.is_err();
                    attempts.push((s, i, a));
                }
            }
            if !open.iter().any(|o| *o) {
                break;
            }
        }
        let m = sc.market();
        (attempts, m.pool_fail_since, m.no_route_since)
    }

    /// The second each position was liquidated in.
    fn liquidated_at(run: &PairRun) -> [Option<u32>; 2] {
        let mut at = [None; 2];
        for (s, i, a) in &run.0 {
            if a.is_ok() {
                at[*i] = Some(*s);
            }
        }
        at
    }

    /// The borrowers' closes of both positions now, each on its own clone of the state: the
    /// `Close` amounts (buyback, rent and its protocol share, payout, `B̂_live`, token surplus).
    fn closes(sc: &mut Sc, second: &Second) -> [[u64; 6]; 2] {
        let saved = sc.env.svm.clone();
        let amounts = |c: shortwin::events::Close| {
            [
                c.buyback_cost,
                c.rent,
                c.rent_protocol,
                c.borrower_payout,
                c.b_live,
                c.token_surplus,
            ]
        };
        let (first, _) = close_ok(&mut sc.env, &sc.mm, &sc.b, sc.nonce);
        sc.env.svm = saved.clone();
        let (other, _) = close_ok(&mut sc.env, &sc.mm, &second.0, second.1);
        sc.env.svm = saved;
        [amounts(first), amounts(other)]
    }

    /// G-G3, the differential (two positions): from the same state, with the global hard cap
    /// kept at 500 bps and lowered to 300 bps (below the live 4 %), the borrowers' closes of
    /// both positions have identical amounts — buyback, rent, payout and the leg's bound
    /// `B̂_live` (a bound at 300 bps would sit below the 4 % charge) — and the keeper's
    /// `liquidate` along the same rising path refuses with the same codes in the same seconds
    /// and executes in the same seconds with identical amounts (`B̂_ref`, `B̂_live`, `max_in`,
    /// penalty, payout) for both positions: the liquidatable set is the same; both positions
    /// keep their 500 bps throughout. Only the lowered run's route chains (`s3_no_route`,
    /// `pool_fail`) start. Control: the same path at a live fee of 300 bps — the risk view the
    /// positions would get if the lowered global cap leaked into their quotes — liquidates them
    /// in later seconds, so the comparison has the resolution to see such a leak.
    #[test]
    fn a_hard_cap_below_the_live_fee_moves_only_the_route_chains() {
        let (mut sc, second) = ready(LOWERED_BPS);
        let keeper = sc.env.funded_keypair();
        let saved = sc.env.svm.clone();
        let kept_closes = closes(&mut sc, &second);
        let kept = pair_path(&mut sc, &keeper, &second, 1.004, 300);
        let m = sc.market();
        assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0));

        sc.env.svm = saved.clone();
        apply(&mut sc);
        assert_eq!(sc.env.config().params.venue_fee_hard_cap_bps, LOWERED_BPS);
        assert_eq!(
            sc.pos().venue_fee_hard_cap_bps,
            500,
            "the position keeps its cap"
        );
        let lowered_closes = closes(&mut sc, &second);
        let lowered = pair_path(&mut sc, &keeper, &second, 1.004, 300);

        assert_eq!(
            kept_closes, lowered_closes,
            "the same closes, to the lamport"
        );
        let at = liquidated_at(&kept);
        assert!(
            at[0].is_some() && at[1].is_some(),
            "the path makes both positions liquidatable: {at:?}"
        );
        assert!(at[0] < at[1], "the second position later: {at:?}");
        for (i, liquidated) in at.iter().enumerate() {
            assert!(
                kept.0
                    .iter()
                    .any(|(s, j, _)| *j == i && Some(*s) < *liquidated),
                "position {i} was healthy first"
            );
        }
        assert!(
            kept.0
                .iter()
                .all(|(_, _, a)| a.is_ok() || *a == Err(Some(code(E::NotLiquidatable)))),
            "only not-yet before each liquidation"
        );
        assert_eq!(
            kept.0, lowered.0,
            "the same refusals and liquidations, second by second, to the lamport"
        );
        assert_eq!((kept.1, kept.2), (0, 0), "no chain under the kept cap");
        assert!(
            lowered.1 > 0 && lowered.2 > 0,
            "the lowered cap starts both chains: pool_fail {}, no_route {}",
            lowered.1,
            lowered.2
        );

        // Control: the risk view at 300 bps.
        sc.env.svm = saved;
        sc.knobs(|k| k.fee_ppm = LOWERED_BPS * 100);
        let leaked = pair_path(&mut sc, &keeper, &second, 1.004, 300);
        let leaked_at = liquidated_at(&leaked);
        eprintln!(
            "G-G3: liquidated at seconds {at:?} under both caps, at {leaked_at:?} with a \
             300 bps risk view"
        );
        for (i, (leaked, kept)) in leaked_at.iter().zip(at).enumerate() {
            assert!(
                leaked.is_some_and(|s| Some(s) > kept),
                "a 300 bps risk view makes position {i} liquidatable later: {leaked_at:?} vs {at:?}"
            );
        }
    }

    /// G-G3, S3 and the way back: under the lowered global cap an expired healthy position
    /// settles S3 once the chain has run `s3_wait_s` — never before its deadline — and its FV is
    /// priced at `min(φ_live, its own 500 bps)` = the live 4 %, not at the lowered 300 bps (an
    /// FV at 300 bps differs). Raising the cap back above the live fee clears both chains at
    /// the next observation.
    #[test]
    fn the_lowered_cap_is_s3_evidence_and_fv_keeps_the_positions_cap() {
        let (mut sc, _second) = ready(LOWERED_BPS);
        apply(&mut sc);
        let back = sc.env.svm.clone();
        sc.warp_to_deadline(-700);
        for _ in 0..23 {
            sc.env.warp(30);
            sc.crank();
            assert!(
                sc.settle_ok().settled.is_none(),
                "not expired, not liquidatable"
            );
        }
        let m = sc.market();
        assert!(m.no_route_since > 0 && m.pool_fail_since > 0 && m.b_lo.get() > 0);
        assert!(sc.env.now() - m.no_route_since >= i64::from(sc.pos().s3_wait_s));
        let pos = sc.pos();
        sc.warp_to_deadline(0);
        let (_s, ev) = sc.run_until_settled(10).expect("S3");
        assert_eq!(ev.condition, 3);
        assert_eq!(ev.pprime_lo, pos.expiry_fee_bps, "O15");
        let m = sc.market();
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let fv = fair_value(pos.q, p, l, u64::from(LIVE_FEE_PPM), ev.pprime_lo, r_due);
        assert_eq!(ev.fv, fv, "FV at the live fee under the position's own cap");
        let at_global = fair_value(
            pos.q,
            p,
            l,
            u64::from(LOWERED_BPS) * 100,
            ev.pprime_lo,
            r_due,
        );
        assert!(
            at_global < fv,
            "the lowered global cap would price FV lower"
        );

        // The way back.
        sc.env.svm = back;
        sc.env.warp(1);
        sc.crank();
        let m = sc.market();
        assert!(m.no_route_since > 0 && m.pool_fail_since > 0);
        propose_hard_cap(&mut sc, 500);
        sc.env.warp(i64::from(sc.env.config().timelock_s));
        sc.crank();
        assert!(
            sc.market().no_route_since > 0,
            "still above the lowered cap"
        );
        apply(&mut sc);
        sc.env.warp(1);
        sc.crank();
        let m = sc.market();
        assert_eq!(
            (m.no_route_since, m.pool_fail_since),
            (0, 0),
            "an observation under the restored cap clears both chains"
        );
        assert!(sc.settle_ok().settled.is_none());
    }

    /// The raised global hard cap of the AIR-19 scenario, and the live fee inside the band
    /// between it and the 500 bps the first position locked.
    const RAISED_BPS: u32 = 1_000;
    const BAND_FEE_PPM: u32 = 80_000;

    /// **AIR-19** (the regression): Governance raises the global hard cap from 500 to 1,000 bps
    /// with a position open at its locked 500 bps; a second position opens under the raised cap
    /// (it locks 1,000 bps); then the pool's admin raises the fee to 8 %, inside the band
    /// between the two caps. The first position's keeper exits judge the buyback at the live fee
    /// but bound it at its locked cap, so its `expire` refuses `ExitBoundNotMet`. Before the fix
    /// the S3 chain compared the fee with the global cap only, saw a route, and the expired
    /// position had no exit at all. Now the market's chain runs at the lowest cap an open
    /// position locked, and a fee above a position's own cap is "no route" for that position:
    /// the first position settles S3 once the chain has run `s3_wait_s`, FV at its own cap; the
    /// second keeps its route — `sol_settle` leaves it alone at its deadline (the chain still
    /// runs: the mark is the lowest cap locked since the market last emptied) and its `expire`
    /// lands; with no position open the mark resets and the next observation clears the chain.
    #[test]
    fn air_19_a_fee_above_the_locked_cap_is_no_route_for_that_position() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        propose_hard_cap(&mut sc, RAISED_BPS);
        sc.env.warp(i64::from(sc.env.config().timelock_s) - 600);
        sc.crank_every(30, 20);
        sc.env.tick(&sc.mm, 5);
        apply(&mut sc);
        assert_eq!(sc.env.config().params.venue_fee_hard_cap_bps, RAISED_BPS);
        let mut b2 = sc.env.borrower();
        let (_open, n2, _) = open_ok(&mut sc.env, &sc.mm, &mut b2, Q / 2);
        let p2 = sc.env.position(&sc.mm, &b2.key(), n2);
        assert_eq!(
            (sc.pos().venue_fee_hard_cap_bps, p2.venue_fee_hard_cap_bps),
            (500, 1_000),
            "each position keeps the cap of its open"
        );
        sc.knobs(|k| k.fee_ppm = BAND_FEE_PPM);

        // The first position: the chain runs at its 500 bps, S3 at the timer.
        sc.warp_to_deadline(-700);
        for _ in 0..23 {
            sc.env.warp(30);
            sc.crank();
            assert!(
                sc.settle_ok().settled.is_none(),
                "not expired, not liquidatable"
            );
        }
        let m = sc.market();
        assert!(
            m.no_route_since > 0,
            "a fee above an open position's locked cap starts the S3 chain"
        );
        assert!(sc.env.now() - m.no_route_since >= i64::from(sc.pos().s3_wait_s));
        assert_eq!(m.pool_fail_since, 0, "pool_fail keeps the global cap");
        sc.warp_to_deadline(-5);
        sc.env.tick(&sc.mm, 5);
        let pos = sc.pos();
        let keeper = sc.env.funded_keypair();
        let expire = |sc: &mut Sc, b: &anchor_lang::prelude::Pubkey, nonce: u64| {
            let ix = shortwin_litesvm_tests::engine::expire_ix(&sc.mm, &keeper.pubkey(), b, nonce);
            sc.env.send(&[ix], &[&keeper])
        };
        let (b1, n1) = (sc.b.key(), sc.nonce);
        assert_err(&expire(&mut sc, &b1, n1), E::ExitBoundNotMet);
        let (_s, ev) = sc.run_until_settled(10).expect("S3");
        assert_eq!(ev.condition, 3);
        assert_eq!(ev.pprime_lo, pos.expiry_fee_bps, "O15");
        let m = sc.market();
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let fv = fair_value(pos.q, p, l, 50_000, ev.pprime_lo, r_due);
        assert_eq!(ev.fv, fv, "FV at the position's own 500 bps cap");
        assert_eq!(m.open_positions, 1);
        assert_eq!(
            m.min_open_hard_cap_bps, 500,
            "the mark keeps the lowest cap locked since the market last emptied"
        );

        // The second position keeps its route under its own 1,000 bps cap.
        (sc.b, sc.nonce, sc.q) = (b2, n2, Q / 2);
        sc.warp_to_deadline(-700);
        sc.crank_every(30, 23);
        let m = sc.market();
        assert!(
            m.no_route_since > 0 && sc.env.now() - m.no_route_since >= 600,
            "the chain still runs at the lowest cap locked since the market emptied"
        );
        sc.warp_to_deadline(-5);
        sc.env.tick(&sc.mm, 5);
        assert_eq!(sc.env.now(), sc.pos().deadline);
        assert!(
            sc.settle_ok().settled.is_none(),
            "a fee under the position's own cap is a route: no S3"
        );
        let (b2, n2) = (sc.b.key(), sc.nonce);
        let exp: shortwin::events::Expire = events::one(&ok(expire(&mut sc, &b2, n2)));
        assert!(exp.buyback_cost > 0 && sc.pos_opt().is_none());
        assert_eq!(
            sc.market().min_open_hard_cap_bps,
            0,
            "the exit that empties the market resets the mark"
        );

        // No position open: the next observation, at the global cap, clears the chain.
        sc.env.warp(1);
        sc.crank();
        assert_eq!(sc.market().no_route_since, 0, "the chain clears");
    }
}
