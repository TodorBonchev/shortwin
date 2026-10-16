//! ECON §12's settlement attacks reproduced against the program on the mock venue (ARCH §9 M2
//! exit criteria): `arming.py` (§12.10 i), `refute_push.py` (a) refutation push, (b) spot-leg
//! veto — `block_spot.py`'s one-low-observation primitive aimed at the settlement clock —,
//! (c) push-then-freeze (§12.10 ii–iv), and `fv_push.py` (§12.11): the amount pushes, the
//! S2 → S1 flip with 1–2 pushed seconds, push-then-freeze flips, `n` consecutive
//! attacker-only seconds (5 is the documented limit, R4A-9), the Emergency-flag S3 of an
//! expiring position (E), the `p'` veto (V, R4A-3) and (M10 G-E2) a contrary observation every
//! second deferring an S3 that is due (D, V2-14: deferral only).
//!
//! **Market.** The mock is a CPMM with the fee on the input, which is ECON's `RC` (a fresh pump
//! graduate on a 25 bps CPMM pool, 206.9 M tokens against 84.99 SOL, Tier C by age): every
//! scenario runs on that pool with one wallet's position of 0.4 % of the pool (`RC_Q`; one
//! wallet cannot reach the sims' `max_open_q`, the wallet cap binds). The scripts' timelines are
//! followed step by step (the keeper's `crank_price` + `sol_settle` each second = the sims'
//! `step_to(t)` + `try_exit(t, settle_only = True)`; an attacker bundle is one transaction
//! `[push → crank_price → trade back]`; an attacker-only second has no honest observation).
//!
//! **Expected outcomes.** The `ECON_*` constants are the scripts' results on exactly this
//! configuration (the sims' `RC` with `Q = RC_Q`, run from `docs/design/sim/attacks/*.py`'s
//! functions); they equal the published `RC` rows of ECON §12.10 (57 s, 60/64, 57/59, 55/57,
//! 60/68, 57/65) and the published `C` row of the spot-leg veto (57, 57, 57, 63), and the
//! `fv_push.py` outcome pattern of §12.11 (amount pushes settle 17 s / 14 s later within the
//! box, no flip with 1–2 pushes or push-then-freeze, S1 with 5 consecutive attacker-only
//! seconds only). Timings are asserted exactly; amounts against the program's own honest run.

#![allow(clippy::result_large_err)]

#[path = "settle_support.rs"]
mod support;

use shortwin_math::quote::{cpmm, liquidity, reprice};
use shortwin_math::rent;
use support::*;

/// The attack scripts' S1 set-up (`arming.py`, `refute_push.py`): an RC market, 120 s of honest
/// keeper cranks after the open, then a genuine gap to `beyond` × the S1 boundary. Returns the
/// scenario, the pre-gap spot and the boundary multiple.
fn s1_setup(beyond: f64) -> (Sc, u128, f64) {
    let mut sc = Sc::rc();
    sc.env.tick(&sc.mm, 120);
    let kb = sc.s1_boundary();
    let p0 = sc.spot();
    sc.move_to(scale(p0, kb * beyond));
    (sc, p0, kb)
}

/// `mature_soon` of `refute_push.py`: armed, and `lead` more EMA updates complete the clock.
fn mature_soon(sc: &Sc, lead: u64) -> bool {
    let Some(p) = sc.pos_opt() else { return false };
    p.s_armed == 1 && sc.market().ema_count + lead >= p.s_count + u64::from(p.settle_persist_n)
}

mod arming {
    use super::*;

    /// ECON §12.10 (i) values for this configuration: honest and attacked S1 after 57 s, two
    /// borrower bundles.
    const ECON_S1_AFTER: u32 = 57;

    /// `arming.py` (V2-4): every 20 s the borrower lands one atomic `[dump below the S1
    /// price → sol_settle → buy back]`; inside it the live S1 leg fails. Only box-robust
    /// evidence disarms, so the bundle neither disarms nor delays the settlement.
    #[test]
    fn a_borrower_bundle_every_20_s_does_not_delay_s1() {
        for attack in [false, true] {
            let (mut sc, p0, kb) = s1_setup(1.05);
            let mut bundles = 0;
            let mut settled = None;
            for s in 1..=600u32 {
                let out = sc.second();
                if let Some(ev) = out.settled {
                    settled = Some((s, ev));
                    break;
                }
                if attack && s % 20 == 0 {
                    let settle = sc.settle_ix();
                    let ixs = sc
                        .bundle_ixs(scale(p0, kb * 0.99), vec![settle])
                        .expect("a dump");
                    let meta = ok(sc.send_bundle(&ixs));
                    let o = Outcome::of(&meta);
                    assert!(
                        o.settled.is_none() && o.disarmed.is_none(),
                        "the bundle cannot disarm"
                    );
                    bundles += 1;
                }
            }
            let (s, ev) = settled.expect("S1");
            assert_eq!(ev.condition, 1);
            assert_eq!(s, ECON_S1_AFTER, "attack {attack}");
            assert_eq!(bundles, if attack { 2 } else { 0 });
        }
    }
}

mod refute_push {
    use super::*;

    /// ECON §12.10 (ii), `RC` rows: (beyond, honest, attacked) seconds.
    const ECON_REFUTE: [(f64, u32, u32); 3] = [(1.03, 60, 64), (1.05, 57, 59), (1.08, 55, 57)];
    /// ECON §12.10 (iv), `RC` rows: (beyond, honest, attacked) seconds, 1 push + 4 frozen.
    const ECON_PTF: [(f64, u32, u32); 2] = [(1.03, 60, 68), (1.05, 57, 65)];
    /// ECON §12.10 (iii) (`C` row): S1 after (veto seconds, seconds).
    const ECON_VETO: [(u32, u32); 4] = [(0, 57), (9, 57), (12, 57), (20, 63)];

    /// (a) `refute_run`: one second before the clock would mature, the borrower's atomic
    /// `[dump to 0.7× → crank → buy back]` is that second's only observation, every cycle. The
    /// box's medians do not move with one attacker-only second: S1 lands 2–4 s later (v0.3:
    /// never within 600 s at 3 % beyond the boundary).
    #[test]
    fn a_refutation_push_delays_s1_by_seconds_only() {
        for (beyond, honest, attacked) in ECON_REFUTE {
            for attack in [false, true] {
                let (mut sc, _p0, _kb) = s1_setup(beyond);
                let mut pushes = 0;
                let mut last_push = None;
                let mut settled = None;
                for s in 1..=600u32 {
                    let since = sc.pos_opt().map(|p| p.s_since);
                    if attack && mature_soon(&sc, 1) && last_push != since {
                        last_push = since;
                        let target = scale(sc.spot(), 0.7);
                        sc.attacker_only_second(target);
                        pushes += 1;
                        continue;
                    }
                    if let Some(ev) = sc.second().settled {
                        settled = Some((s, ev));
                        break;
                    }
                }
                let (s, ev) = settled.expect("S1");
                assert_eq!(ev.condition, 1);
                let want = if attack { attacked } else { honest };
                assert_eq!(s, want, "x{beyond} attack {attack}");
                assert_eq!(pushes, u32::from(attack));
            }
        }
    }

    /// (c) `ptf_refute_run` (R4B-2): one attacker-only dump second (×0.7) three updates before
    /// maturity, then four seconds in which the honest keeper observes **and** the borrower adds
    /// an observation just below `P_ema` (the bucket straddles it, the EMA freezes), every
    /// cycle. The box on bucket extremes keeps the frozen seconds' honest highs: S1 lands 8 s
    /// later (v0.4's box on clamped observations: never within 600 s at 3 %).
    #[test]
    fn push_then_freeze_delays_s1_by_eight_seconds() {
        for (beyond, honest, attacked) in ECON_PTF {
            for attack in [false, true] {
                let (mut sc, _p0, _kb) = s1_setup(beyond);
                let (mut pushes, mut frozen, mut freeze_left) = (0, 0, 0);
                let mut last = None;
                let mut settled = None;
                for s in 1..=600u32 {
                    let since = sc.pos_opt().map(|p| p.s_since);
                    if attack && freeze_left == 0 && mature_soon(&sc, 3) && last != since {
                        last = since;
                        let target = scale(sc.spot(), 0.7);
                        sc.attacker_only_second(target);
                        pushes += 1;
                        freeze_left = 4;
                        continue;
                    }
                    sc.env.warp(1);
                    sc.crank();
                    if attack && freeze_left > 0 {
                        let ema = sc.market().ema_price.get();
                        sc.attacker_bundle(ema * 9_995 / 10_000);
                        frozen += 1;
                        freeze_left -= 1;
                    }
                    if let Some(ev) = sc.settle_ok().settled {
                        settled = Some((s, ev));
                        break;
                    }
                }
                let (s, ev) = settled.expect("S1");
                assert_eq!(ev.condition, 1);
                assert_eq!(
                    s,
                    if attack { attacked } else { honest },
                    "x{beyond} {attack}"
                );
                assert_eq!((pushes, frozen), if attack { (1, 4) } else { (0, 0) });
            }
        }
    }

    /// The largest spot at which the spot leg still fails (`block_spot.liq_price_target`):
    /// `CR(V_eff, B̂(Q; reprice(L_live, P))) ≥ L`, by bisection on `P`.
    fn liq_price_target(sc: &Sc) -> u128 {
        let p = sc.pos();
        let m = sc.market();
        let r_due = rent::due(&p.pos_rent(), &m.basis()).unwrap();
        let v_eff = u128::from(p.v.saturating_sub(r_due));
        let r = mock::reserves(&sc.env.svm, sc.mm.mock());
        let l_live = liquidity(u128::from(r.base), u128::from(r.quote)).unwrap();
        let fees = mock::cpmm_fees(FEE_PPM);
        let healthy = |price: u128| {
            reprice(l_live, price)
                .and_then(|(x, y)| cpmm::buy_exact_out(p.q, y, x, &fees))
                .is_some_and(|b| v_eff * 10_000 / u128::from(b.cost) >= u128::from(p.cr_liq_bps))
        };
        let spot = sc.spot();
        let (mut lo, mut hi) = (spot / 4, spot);
        for _ in 0..80 {
            let mid = (lo + hi) / 2;
            if healthy(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// (b) `spotblock_run` (R3A-6 b; `block_spot.py`'s primitive on the settlement clock): from
    /// 20 s after arming, for `w` seconds, one low observation per second (below the
    /// liquidation price, in the same bucket as the keeper's) fails the robust-spot leg. It
    /// keeps the clock running instead of disarming it: S1 is unchanged for vetoes shorter than
    /// the time to maturity and at most 6 s late otherwise (v0.3: +34–42 s).
    #[test]
    fn a_spot_leg_veto_keeps_the_clock() {
        for (w, want) in ECON_VETO {
            let (mut sc, _p0, _kb) = s1_setup(1.05);
            let (mut armed_at, mut blocked) = (None, 0);
            let mut settled = None;
            for s in 1..=400u32 {
                if armed_at.is_none() && sc.pos_opt().is_some_and(|p| p.s_armed == 1) {
                    armed_at = Some(s);
                }
                if armed_at.is_some_and(|a| s - a >= 20) && blocked < w {
                    sc.env.warp(1);
                    sc.crank();
                    let target = liq_price_target(&sc) * 995 / 1_000;
                    sc.attacker_bundle(target);
                    let out = sc.settle_ok();
                    assert!(out.settled.is_none() && out.disarmed.is_none());
                    blocked += 1;
                    continue;
                }
                if let Some(ev) = sc.second().settled {
                    settled = Some((s, ev));
                    break;
                }
            }
            let (s, ev) = settled.expect("S1");
            assert_eq!(ev.condition, 1);
            assert_eq!(s, want, "veto of {w} s");
        }
    }
}

mod fv_push {
    use super::*;
    use shortwin::events::SolSettle;

    /// `fv_push.py`'s (M) model case on this configuration: 97 % of the LP pulled, then a ×1.8
    /// move: S2 (the vault covers the buyback, its slippage is above σ_max), capped at FV.
    const DRAIN_BPS: u64 = 9_700;
    const MOVE: f64 = 1.8;
    /// The sims' timings on this configuration: a lender's one-second push up settles 17 s
    /// later, a borrower's push down 14 s later (§12.11: 17–18 s and 13–16 s).
    const ECON_UP_S: u32 = 17;
    const ECON_DOWN_S: u32 = 14;
    /// The sims' lenders' takes on this configuration (lamports): honest, after the push up,
    /// after the push down. The program's differ only by the rent due (the sims hold
    /// utilisation at 80 % for rent, the program uses the pool's own: `r_min` differs by
    /// ≈ 4·10⁻⁵ SOL), so they are compared within 2 bps of `V − K_s`.
    const SIM_TAKES: [u64; 3] = [800_282_406, 809_914_621, 795_154_127];

    /// `ready_model`: the state one second before the honest keeper's S2 would execute, and
    /// that honest settlement.
    fn ready_model() -> (Sc, SolSettle) {
        fv_model_ready(DRAIN_BPS, MOVE)
    }

    /// `fv_push.py` (M), (V2-9, R3A-1, R4B-2, R4A-9) on one saved state: the honest S2; a
    /// lender's push up and a borrower's push down move the amount by at most `fv_dev_bps`
    /// and delay it by the sims' 17 s / 14 s; the S2 → S1 flip fails with 1–2 pushed seconds,
    /// with one push and 0–10 frozen seconds, and with 1–4 consecutive attacker-only seconds or
    /// pushes interleaved with honest seconds; 5 or 6 consecutive attacker-only seconds (a
    /// censored keeper, degraded mode) flip it — the documented limit.
    #[test]
    fn amount_pushes_and_flips() {
        let (mut sc, honest) = ready_model();
        assert_eq!(honest.condition, 2, "the model case is an S2");
        let take = sc.take();
        assert!(honest.lender_amount < take, "capped at FV");
        let dev = u64::from(sc.pos().fv_dev_bps);
        let within =
            |x: u64| x.abs_diff(honest.lender_amount) * 10_000 <= 2 * dev * honest.lender_amount;
        let near_sim = |x: u64, i: usize| x.abs_diff(SIM_TAKES[i]) * 10_000 <= 2 * take;
        assert!(
            near_sim(honest.lender_amount, 0),
            "{}",
            honest.lender_amount
        );
        let saved = sc.env.svm.clone();

        // A lender pushes up in one attacker-only second, then calls sol_settle every second.
        push(&mut sc, 3.0);
        let mut up = None;
        for s in 1..=60u32 {
            sc.env.warp(1);
            if let Some(ev) = sc.settle_ok().settled {
                up = Some((s, ev));
                break;
            }
        }
        let (s, ev) = up.expect("S2 after the push");
        eprintln!(
            "fv_push (M): honest S2 {} lamports (V − K_s {take}); push up: {} after {s} s",
            honest.lender_amount, ev.lender_amount
        );
        assert_eq!((ev.condition, s), (2, ECON_UP_S));
        assert!(ev.lender_amount > honest.lender_amount && within(ev.lender_amount));
        assert!(near_sim(ev.lender_amount, 1));

        // A borrower pushes down in one attacker-only second; the keeper goes on.
        sc.env.svm = saved.clone();
        push(&mut sc, 0.33);
        let (s, ev) = sc.run_until_settled(120).expect("S2 after the push");
        eprintln!("fv_push (M): push down: {} after {s} s", ev.lender_amount);
        assert_eq!((ev.condition, s), (2, ECON_DOWN_S));
        assert!(ev.lender_amount < honest.lender_amount && within(ev.lender_amount));
        assert!(near_sim(ev.lender_amount, 2));

        // S2 → S1 flips.
        for n in [1, 2] {
            sc.env.svm = saved.clone();
            for _ in 0..n {
                push(&mut sc, 3.0);
            }
            assert!(
                flip_bundle(&mut sc, 3.0).is_none(),
                "{n} pushed seconds cannot flip"
            );
        }
        for k in [0, 2, 3, 4, 5, 6, 8, 10] {
            sc.env.svm = saved.clone();
            push(&mut sc, 3.0);
            for _ in 0..k {
                sc.env.warp(1);
                sc.crank();
                let ema = sc.market().ema_price.get();
                sc.attacker_bundle(ema * 10_005 / 10_000);
            }
            assert!(
                flip_bundle(&mut sc, 3.0).is_none(),
                "1 push + {k} frozen seconds"
            );
        }
        for n in 1..=6u32 {
            for gap in [0u32, 1] {
                if n == 1 && gap == 1 {
                    continue;
                }
                sc.env.svm = saved.clone();
                let mut honest_first = false;
                for i in 0..n {
                    push(&mut sc, 3.0);
                    if i + 1 < n {
                        for _ in 0..gap {
                            honest_first |= sc.second().settled.is_some();
                        }
                    }
                }
                assert!(!honest_first);
                let flip = flip_bundle(&mut sc, 3.0);
                if n >= 5 && gap == 0 {
                    let ev = flip.expect("5 consecutive attacker-only seconds flip S2 into S1");
                    assert_eq!((ev.condition, ev.lender_amount), (1, take));
                } else {
                    assert!(
                        flip.is_none(),
                        "{n} pushes, {gap} honest s between: no flip"
                    );
                }
            }
        }
    }

    /// (E) the realistic case: the Emergency flag is set 30 min before a healthy position's
    /// deadline; at the deadline the keeper's S3 is capped at FV with the expiry fee. A lender's
    /// push up or a borrower's push down moves it by at most `fv_dev_bps`, 17 s / 13 s later
    /// (the sims' values); the S3 → S1 flip with 1–2 pushed seconds fails.
    #[test]
    fn emergency_flag_s3_amount_pushes() {
        const ECON_E_UP_S: u32 = 17;
        const ECON_E_DOWN_S: u32 = 13;
        let mut sc = Sc::rc();
        sc.env.tick(&sc.mm, 5);
        sc.warp_to_deadline(-1800);
        sc.set_route_broken(true);
        sc.crank_every(30, 58);
        let deadline = sc.pos().deadline;
        let now = sc.env.now();
        sc.env
            .tick(&sc.mm, u32::try_from(deadline - 1 - now).unwrap());
        let dev = u64::from(sc.pos().fv_dev_bps);
        let saved = sc.env.svm.clone();
        let honest = sc
            .second()
            .settled
            .expect("the honest keeper's S3 at the deadline");
        assert_eq!(honest.condition, 3);
        let within =
            |x: u64| x.abs_diff(honest.lender_amount) * 10_000 <= 2 * dev * honest.lender_amount;

        sc.env.svm = saved.clone();
        push(&mut sc, 3.0);
        let mut up = None;
        for s in 1..=60u32 {
            sc.env.warp(1);
            if let Some(ev) = sc.settle_ok().settled {
                up = Some((s, ev));
                break;
            }
        }
        let (s, ev) = up.expect("S3 after the push");
        assert_eq!((ev.condition, s), (3, ECON_E_UP_S));
        assert!(ev.lender_amount > honest.lender_amount && within(ev.lender_amount));

        sc.env.svm = saved.clone();
        push(&mut sc, 0.33);
        let (s, ev) = sc.run_until_settled(120).expect("S3 after the push");
        assert_eq!((ev.condition, s), (3, ECON_E_DOWN_S));
        assert!(ev.lender_amount < honest.lender_amount && within(ev.lender_amount));

        for n in [1, 2] {
            sc.env.svm = saved.clone();
            for _ in 0..n {
                push(&mut sc, 3.0);
            }
            assert!(
                flip_bundle(&mut sc, 3.0).is_none(),
                "S3 → S1 flip with {n} pushes"
            );
        }
    }

    /// (D) V2-14 / `refute_push.py` (d) (M10 G-E2): the borrower's contrary observation every
    /// second against an S3 that is due. The Emergency flag is set 30 min before a healthy
    /// position's deadline; from 11 s before the deadline (the sim's `lead` 12) the borrower
    /// lands one atomic `[dump 1 % or 3 %, crank_price, buy back]` after the keeper's crank each
    /// second for `w` = 20 and 60 s, the keeper calling `sol_settle` every second. What ECON
    /// claims is a deferral only: whatever lands is the S3, its amount within `2·fv_dev_bps` of
    /// the honest S3's (as `amount_pushes_and_flips`; the rent accrues meanwhile). Pinned: on
    /// this configuration (a 0.4 % position) a −1 % round trip does not widen the box enough to
    /// defer at all — the S3 in the honest second, at the deadline; a −3 % one defers it for as
    /// long as it is paid, and the S3 lands exactly 6 s after the last contrary second (14 / 54 s
    /// after the deadline, the seconds of `refute_push.widen_s3` on ECON's `RC`, whose
    /// max-size position the sim defers at −1 % too: `python3 tests/litesvm/sim/s3_widen.py`).
    /// `-- --nocapture` prints the seconds and amounts.
    #[test]
    fn a_contrary_observation_every_second_defers_s3_only() {
        let mut sc = Sc::rc();
        sc.env.tick(&sc.mm, 5);
        sc.warp_to_deadline(-1800);
        sc.set_route_broken(true);
        sc.crank_every(30, 58);
        let deadline = sc.pos().deadline;
        let now = sc.env.now();
        sc.env
            .tick(&sc.mm, u32::try_from(deadline - 12 - now).unwrap());
        let dev = u64::from(sc.pos().fv_dev_bps);
        let saved = sc.env.svm.clone();
        let (hs, honest) = sc.run_until_settled(60).expect("the honest S3");
        assert_eq!(honest.condition, 3);
        assert_eq!(sc.env.now(), deadline, "the honest S3 at the deadline");
        for pct in [100u128, 300] {
            for w in [20u32, 60] {
                sc.env.svm = saved.clone();
                let mut cost = 0i128;
                let mut got = None;
                for s in 1..=200u32 {
                    sc.env.warp(1);
                    sc.crank();
                    if s <= w {
                        let target = sc.spot() * (10_000 - pct) / 10_000;
                        cost += sc.attacker_bundle(target);
                    }
                    if let Some(ev) = sc.settle_ok().settled {
                        got = Some((s, ev));
                        break;
                    }
                }
                let (s, ev) = got.expect("S3");
                eprintln!(
                    "contrary observation −{} % for {w} s (cost {:.4} SOL): S3 at {s} s (honest \
                     {hs} s), {} vs honest {} lamports",
                    pct / 100,
                    cost as f64 / 1e9,
                    ev.lender_amount,
                    honest.lender_amount
                );
                assert_eq!(ev.condition, 3, "S3, nothing else");
                if pct == 100 {
                    assert_eq!(s, hs, "−1 % for {w} s: no deferral on this configuration");
                } else {
                    assert!(s > hs, "−3 % for {w} s: the S3 is deferred");
                    assert_eq!(
                        s,
                        w + 6,
                        "−3 % for {w} s: the S3 6 s after the last contrary second"
                    );
                }
                assert!(
                    ev.lender_amount.abs_diff(honest.lender_amount) * 10_000
                        <= 2 * dev * honest.lender_amount,
                    "−{pct} bps for {w} s: the honest S3's amount"
                );
            }
        }
    }

    /// (V) the `p'` veto (R4A-3): the Emergency flag is set and a genuine ×2.3 move makes the
    /// position expired **and** CR-liquidatable at its deadline; in the second before the
    /// keeper's `sol_settle(S3)` the borrower lands one contrary low observation (it fails the
    /// robust-spot leg). `p'` is decided on each box corner, not on the raw legs: the penalty
    /// applies at both corners and the lenders' take, lump and the borrower's remainder are
    /// unchanged (v0.4: −2.9 % to −7.3 % for lenders).
    #[test]
    fn a_contrary_observation_does_not_change_pprime() {
        let mut sc = Sc::rc();
        sc.env.tick(&sc.mm, 5);
        sc.warp_to_deadline(-1800);
        sc.set_route_broken(true);
        sc.move_by(2.3);
        sc.crank_every(30, 58);
        let deadline = sc.pos().deadline;
        let now = sc.env.now();
        sc.env
            .tick(&sc.mm, u32::try_from(deadline - 1 - now).unwrap());
        let pos = sc.pos();
        let saved = sc.env.svm.clone();
        let mut runs = Vec::new();
        for veto in [false, true] {
            sc.env.svm = saved.clone();
            sc.env.warp(1);
            sc.crank();
            if veto {
                let p = sc.pos();
                let m = sc.market();
                let r_due = rent::due(&p.pos_rent(), &m.basis()).unwrap();
                let v_eff = u128::from(p.v - r_due);
                let r = mock::reserves(&sc.env.svm, sc.mm.mock());
                let l_live = liquidity(u128::from(r.base), u128::from(r.quote)).unwrap();
                let fees = mock::cpmm_fees(FEE_PPM);
                let spot = sc.spot();
                let (mut lo, mut hi) = (spot / 4, spot);
                for _ in 0..80 {
                    let mid = (lo + hi) / 2;
                    let healthy = reprice(l_live, mid)
                        .and_then(|(x, y)| cpmm::buy_exact_out(p.q, y, x, &fees))
                        .is_some_and(|b| {
                            v_eff * 10_000 / u128::from(b.cost) >= u128::from(p.cr_liq_bps)
                        });
                    if healthy {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                sc.attacker_bundle(lo * 99 / 100);
            }
            sc.env.warp(1);
            sc.crank();
            let ev = sc.settle_ok().settled.expect("S3");
            runs.push(ev);
        }
        let (h, v) = (&runs[0], &runs[1]);
        assert_eq!((h.condition, v.condition), (3, 3));
        assert_eq!(
            (h.pprime_lo, h.pprime_hi),
            (pos.penalty_bps, pos.penalty_bps)
        );
        assert_eq!(
            (v.pprime_lo, v.pprime_hi),
            (pos.penalty_bps, pos.penalty_bps)
        );
        assert_eq!(
            (h.lender_amount, h.lump, h.borrower_amount),
            (v.lender_amount, v.lump, v.borrower_amount),
            "the veto changes nothing"
        );
    }
}
