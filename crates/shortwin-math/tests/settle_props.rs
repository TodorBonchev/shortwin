//! Properties of `settle` (ARCHITECTURE §3.8, §4.5, §8.1; ECONOMICS §5.6, §13): the box
//! holds the EMA point and collapses to it after `RING_LEN` seconds; every predicate reads
//! the corner least favourable to it, so a wider box never makes a decision easier; the
//! amount check; and, over random evaluation sequences, the arming invariants — only
//! box-robust evidence resets a live clock (I-E8, V2-4), S1/S2 need `settle_persist_n`
//! updates on one clock, S1 pays `V − K_s` uncapped and S2/S3 pay a box-consistent
//! `x(P_ema, L_ema)` (I-E5), S3 needs its timer (I-E9); and review F-S7's two fee views on
//! real quotes (a live fee above the hard cap is S3 evidence and the risk-fee refutation
//! still disarms).
#![allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]

mod common;

use proptest::prelude::*;
use shortwin_math::MathError;
use shortwin_math::observe::{Ema, Ring, RingEntry};
use shortwin_math::risk::{Budget, SettleKind};
use shortwin_math::settle::{
    ArmChange, ArmState, BoxQuotes, DisarmReason, S3Chain, S3Terms, SettleBox, SettleInputs,
    SettleTerms, amount_at, amount_ok, box_of, evaluate, not_liquidatable, pprime, refuted,
    s1_legs, s2_legs, s3_due,
};

/// A venue whose quotes rise with `P` and fall with `L`, like every constant-product quote
/// on `reprice(L, P)`: `B̂ = ⌊k·P/L⌋`, `σ = ⌊100·P/L⌋`; undefined when `L = 0`.
struct Mono {
    k: u128,
}

impl BoxQuotes for Mono {
    fn cost(&mut self, p: u128, l: u128) -> Result<Option<u64>, MathError> {
        Ok((p * self.k)
            .checked_div(l)
            .and_then(|c| u64::try_from(c).ok())
            .filter(|&c| c > 0))
    }

    fn sigma(&mut self, p: u128, l: u128) -> Result<u64, MathError> {
        Ok(u64::try_from(p * 100 / l.max(1)).unwrap_or(u64::MAX))
    }
}

fn a_box() -> impl Strategy<Value = SettleBox> {
    (
        1u128..1_000_000,
        0u128..100_000,
        1u128..1_000_000,
        0u128..100_000,
    )
        .prop_map(|(p, dp, l, dl)| SettleBox {
            p_lo: p,
            p_hi: p + dp,
            l_lo: l,
            l_hi: l + dl,
        })
}

/// A box that contains `inner`.
fn wider(inner: SettleBox, a: u128, b: u128, c: u128, d: u128) -> SettleBox {
    SettleBox {
        p_lo: inner.p_lo.saturating_sub(a).max(1),
        p_hi: inner.p_hi + b,
        l_lo: inner.l_lo.saturating_sub(c).max(1),
        l_hi: inner.l_hi + d,
    }
}

const S3_OFF: S3Chain = S3Chain {
    no_route_now: false,
    no_route_since: None,
    graduated_at: None,
    migrating: false,
    migration_seen_at: None,
};

proptest! {
    #![proptest_config(common::proptest_config())]

    /// The box (ECON §5.6): contains `(P_ema, L_ema)`, is spanned by it and the ring's
    /// medians, and is the EMA point once more than `RING_LEN` seconds passed without an
    /// integration **on a venue that cannot execute** (never on an executable one: M2-SEC-1).
    #[test]
    fn box_holds_the_ema_point(entries in prop::collection::vec((1u128..10u128.pow(20), 1u128..10u128.pow(20),
                                                                  1u128..10u128.pow(12), 1u128..10u128.pow(12)), 1..=8),
                               p in 1u128..10u128.pow(20), l in 1u128..10u128.pow(12), age in -3i64..20,
                               exec in any::<bool>()) {
        let mut ring = Ring::default();
        for (i, &(a, b, c, d)) in entries.iter().enumerate() {
            ring.push(RingEntry { lo: a.min(b), hi: a.max(b), llo: c.min(d), lhi: c.max(d), ts: i as i64,
                                  ..RingEntry::default() }).unwrap();
        }
        let ema = Ema { price: p, liq: l, liq_slow: l, ts: 100, slow_ts: 100, count: 7, ..Ema::default() };
        let bx = box_of(&ema, &ring, 100 + age, exec).unwrap();
        prop_assert!(bx.contains(p, l));
        if age > 8 && !exec {
            prop_assert_eq!(bx, SettleBox::point(p, l));
        } else {
            let (pm, ph, lm, lh) = shortwin_math::observe::ring_medians(&ring).unwrap();
            prop_assert_eq!(bx, SettleBox { p_lo: p.min(pm), p_hi: p.max(ph), l_lo: l.min(lm), l_hi: l.max(lh) });
        }
    }

    /// "The same decision on the whole box" (ECON §5.6): with quotes monotone in (P, L), a
    /// wider box never makes S1, S2, a refutation or a "not liquidatable" disarm easier.
    #[test]
    fn wider_boxes_never_make_a_decision_easier(bx in a_box(), a in 0u128..1_000, b in 0u128..100_000,
                                                c in 0u128..1_000, d in 0u128..100_000, k in 1u128..1_000_000,
                                                v in 1u64..10u64.pow(9), ks in 0u64..10u64.pow(6), ert in 0u64..10u64.pow(6),
                                                smax in 0u32..50_000, cr in 10_500u32..40_000, p_rs in 1u128..1_000_000,
                                                l_live in 1u128..1_000_000, b_live in prop::option::of(0u64..10u64.pow(9))) {
        let wide = wider(bx, a, b, c, d);
        let budget = Budget { v: v.max(ks), k_s: ks, ert };
        let q = &mut Mono { k };
        if s1_legs(q, &budget, b_live, p_rs, l_live, &wide).unwrap() {
            prop_assert!(s1_legs(q, &budget, b_live, p_rs, l_live, &bx).unwrap());
        }
        if s2_legs(q, smax, u64::MAX, p_rs, l_live, &wide).unwrap() {
            prop_assert!(s2_legs(q, smax, u64::MAX, p_rs, l_live, &bx).unwrap());
        }
        if refuted(q, &budget, smax, l_live, &wide).unwrap() {
            prop_assert!(refuted(q, &budget, smax, l_live, &bx).unwrap());
        }
        if not_liquidatable(q, false, false, v, cr, &wide).unwrap() {
            prop_assert!(not_liquidatable(q, false, false, v, cr, &bx).unwrap());
        }
        // S1 at the box and a refutation at the same box exclude each other.
        let point = SettleBox::point(bx.p_lo, bx.l_hi);
        let s1 = s1_legs(q, &budget, None, bx.p_lo, bx.l_hi, &point).unwrap();
        prop_assert!(!(s1 && refuted(q, &budget, smax, bx.l_hi, &point).unwrap()));
    }

    /// FV's `p'` and the amount check (ECON §8.6; I-E5): the expiry fee exactly when expired
    /// and not below L at that state; a corner's amount never exceeds `V − K_s`; the check is
    /// exact and monotone in its tolerance.
    #[test]
    fn amounts(expired in any::<bool>(), v_eff in 0u64..10u64.pow(12), b in 1u64..10u64.pow(12), cr in 10_500u32..40_000,
               pen in 0u32..2_000, fee in 0u32..600, take in 0u64..10u64.pow(12), r in 0u64..10u64.pow(9),
               x_lo in 0u64..10u64.pow(12), dx in 0u64..10u64.pow(10), dev in 0u32..10_000, p in 1u128..10u128.pow(9),
               l in 1u128..10u128.pow(9), k in 1u128..10u128.pow(6)) {
        let pp = pprime(expired, v_eff, b, cr, pen, fee);
        let not_below = u128::from(v_eff) * 10_000 >= u128::from(cr) * u128::from(b);
        prop_assert_eq!(pp, if expired && not_below { fee } else { pen });
        let terms = SettleTerms { cr_liq_bps: cr, sigma_max_bps: 2_000, penalty_bps: pen, expiry_fee_bps: fee,
                                  fv_dev_bps: dev, persist_n: 40, arm_gap_s: 10, s3: S3Terms::DEFAULT };
        let a = amount_at(&mut Mono { k }, p, l, take, expired, v_eff, r, &terms).unwrap();
        prop_assert!(a.x <= take);
        if let Some(fv) = a.fv {
            prop_assert_eq!(a.x, fv.min(take));
            prop_assert!(fv >= r);
        } else {
            prop_assert_eq!(a.x, take);
        }
        let ok = amount_ok(x_lo, x_lo + dx, dev);
        prop_assert_eq!(ok, u128::from(dx) * 10_000 <= u128::from(dev) * u128::from(x_lo));
        if ok {
            prop_assert!(amount_ok(x_lo, x_lo + dx, dev.saturating_add(1)));
        }
        prop_assert!(amount_ok(x_lo + dx, x_lo, 0));
    }

    /// The S3 timer (ECON §10.3; I-E9) is monotone in time and never due without a
    /// no-route observation now, a chain, or liquidatable-or-expired.
    #[test]
    fn s3_timer_is_monotone(since in prop::option::of(0i64..10_000), grad in prop::option::of(0i64..10_000),
                            mig in any::<bool>(), seen in prop::option::of(0i64..10_000), now in 0i64..20_000,
                            later in 0i64..5_000, wait in 600u32..2_000, grace in 600u32..2_000, pause in 600u32..4_000) {
        let chain = S3Chain { no_route_now: true, no_route_since: since, graduated_at: grad, migrating: mig,
                              migration_seen_at: seen };
        let terms = S3Terms { s3_wait_s: wait, grad_grace_s: grace, max_migration_pause_s: pause };
        let due = s3_due(&chain, &terms, now, true);
        if due {
            prop_assert!(s3_due(&chain, &terms, now + later, true));
            prop_assert!(now >= since.unwrap() + i64::from(wait));
        }
        prop_assert!(!s3_due(&chain, &terms, now, false));
        let quiet = S3Chain { no_route_now: false, ..chain };
        prop_assert!(!s3_due(&quiet, &terms, now, true));
    }

    /// Arming over random evaluation sequences (ECON §5.6; ARCH §3.8; I-E5, I-E8, I-E9).
    #[test]
    fn arming_invariants(steps in prop::collection::vec((0i64..14, 0u64..3, prop::bool::weighted(0.8), 0u8..10,
                                                          prop::option::weighted(0.9, 1u128..3_000), 1u128..3_000,
                                                          0u8..8, 1u128..3_000, 1u128..3_000), 1..60),
                         k in 1u128..20_000, v in 1_000u64..50_000, persist in 0u16..6, gap in 2u16..20,
                         smax in 0u32..400, dev in 0u32..300) {
        let budget = Budget { v, k_s: v / 50, ert: v / 100 };
        let terms = SettleTerms { cr_liq_bps: 12_500, sigma_max_bps: smax, penalty_bps: 500, expiry_fee_bps: 200,
                                  fv_dev_bps: dev, persist_n: persist, arm_gap_s: gap, s3: S3Terms::DEFAULT };
        let mut st = ArmState::default();
        let (mut now, mut count) = (1_000i64, 100u64);
        for &(dt, dc, liq, venue, p_rs, pe, w, a, b) in &steps {
            // venue: 0 halted (no route), 1 executable with the route-broken flag, else normal
            let (exec, halted) = (venue != 0, venue <= 1);
            now += dt;
            count += dc;
            let bx = SettleBox { p_lo: pe.min(a), p_hi: pe.max(a), l_lo: 1_000u128.min(b), l_hi: 1_000u128.max(b) };
            let inp = SettleInputs {
                now, deadline: now + 1_000 - i64::from(w / 6) * 1_500, ema_count: count, p_ema: pe, l_ema: 1_000, bx,
                budget, r_due: 0, cr_liquidatable: liq, ema_leg: liq || w == 1, executable: exec, warmed: true, p_rs,
                l_live: 1_000, b_live: None, sigma_live: u64::MAX,
                s3: S3Chain { no_route_now: halted || !exec, no_route_since: Some(now - 700), ..S3_OFF },
            };
            let q = &mut Mono { k };
            let before = st;
            let e = evaluate(&mut st, &inp, &terms, q).unwrap();
            let live_before = before.is_live(now, gap);
            match e.arm {
                ArmChange::None => prop_assert_eq!(st, before),
                // AIR-16 (ECON §5.6): only a live clock keeps running; a lapsed one re-arms.
                ArmChange::Kept => prop_assert!(live_before),
                ArmChange::Armed => prop_assert!(!live_before && st.count == count && st.since == now),
                ArmChange::Disarmed(DisarmReason::Refuted) => {
                    prop_assert!(exec && before.armed == 1);
                    prop_assert!(refuted(q, &budget, smax, 1_000, &bx).unwrap());
                }
                ArmChange::Disarmed(DisarmReason::NotLiquidatable) => {
                    prop_assert!(!liq && !inp.expired() && before.armed == 1);
                }
            }
            // I-E8: a live clock is never restarted by an evaluation that does not disarm it.
            if live_before && !matches!(e.arm, ArmChange::Disarmed(_)) {
                prop_assert_eq!((st.count, st.since), (before.count, before.since));
            }
            if !(liq || inp.expired()) {
                prop_assert!(e.settlement.is_none() && !matches!(e.arm, ArmChange::Armed));
            }
            if let Some(s) = e.settlement {
                let take = v - budget.k_s;
                prop_assert_eq!(s.lenders + s.borrower, take);
                match s.kind {
                    SettleKind::S1 | SettleKind::S2 => {
                        prop_assert!(exec && st.armed == 1 && count - st.count >= u64::from(persist));
                        let s1 = s1_legs(q, &budget, None, p_rs.unwrap(), 1_000, &bx).unwrap();
                        prop_assert_eq!(s.kind == SettleKind::S1, s1);
                        if s.kind == SettleKind::S1 {
                            prop_assert_eq!((s.lenders, s.fv, s.corners), (take, None, None));
                        }
                    }
                    SettleKind::S3 => prop_assert!(s3_due(&inp.s3, &terms.s3, now, true)),
                }
                if let Some([lo, hi]) = s.corners {
                    prop_assert!(amount_ok(lo.x, hi.x, dev));
                    prop_assert_eq!(s.lenders, s.fv.map_or(take, |f| f.min(take)));
                }
                st = ArmState::default();
            }
        }
    }
}

/// Real quotes for the fee-view test: the position's risk quote on `reprice(L, P)` at
/// `φ_risk = min(φ_live, cap)` and the fee-free σ of that state.
struct Priced {
    risk: shortwin_math::quote::Pricing,
    q: u64,
}

impl BoxQuotes for Priced {
    fn cost(&mut self, p: u128, l: u128) -> Result<Option<u64>, MathError> {
        Ok(self.risk.buy_repriced(self.q, l, p).map(|b| b.cost))
    }

    fn sigma(&mut self, p: u128, l: u128) -> Result<u64, MathError> {
        let Some((x, y)) = shortwin_math::quote::reprice(l, p) else {
            return Ok(u64::MAX);
        };
        let n = self.risk.buy(self.q, x, y).map(|b| b.net);
        Ok(shortwin_math::risk::sigma_xy(n, self.q, x, y).unwrap_or(u64::MAX))
    }
}

/// Review F-S7 (re-homed from M0's `risk_predicates.rs` onto `evaluate`): under ARCH §4.3's
/// two fee views, a live fee above `venue_fee_hard_cap_bps` makes the normal buyback
/// unaffordable at the live fee while no S1/S2 leg holds at the risk fee. v0.4.1 has no
/// "normal buyback allowed" step in `sol_settle`: the fee is S3 evidence (`s3_no_route`
/// with the global cap), the box-robust refutation at the risk fee still disarms a live
/// arm while the S3 timer runs, and S3 settles once it is due. At or below the cap the two
/// views coincide.
#[test]
fn fee_above_the_hard_cap_is_s3_evidence_and_refutes() {
    use shortwin_math::fees::{self, ray_v4};
    use shortwin_math::num::{Rounding, WAD, sqrt_u128};
    use shortwin_math::quote::Pricing;

    let (x, y, q) = (
        1_000_000_000_000_000u128,
        1_000_000_000_000u128,
        10_000_000_000_000u64,
    );
    let cap = 500u64;
    let live = Pricing::RayV4(ray_v4(600, 10_000).unwrap());
    let risk_view = live.capped(cap);
    assert!(fees::exceeds_cap(live.oneway_ppm(), 500));
    assert!(!fees::exceeds_cap(risk_view.oneway_ppm(), 500));

    let (l, p) = (sqrt_u128(x * y, Rounding::Floor), y * WAD / x);
    let mut quotes = Priced { risk: risk_view, q };
    let b_risk = quotes.cost(p, l).unwrap().unwrap();
    let b_live_exec = live.buy(q, x, y).unwrap().cost;
    // The vault covers the risk quote with 1,000 lamports to spare, not the live one.
    let k_s = 100_000_000;
    let budget = Budget {
        v: b_risk + 1_000 + k_s,
        k_s,
        ert: 0,
    };
    assert!(
        b_live_exec > budget.v - k_s,
        "the live-fee buyback is unaffordable"
    );
    let sigma_live = quotes.sigma(p, l).unwrap();
    assert!(sigma_live < 2_000);

    let terms = SettleTerms {
        cr_liq_bps: 12_500,
        sigma_max_bps: 2_000,
        penalty_bps: 500,
        expiry_fee_bps: 200,
        fv_dev_bps: 100,
        persist_n: 40,
        arm_gap_s: 10,
        s3: S3Terms::DEFAULT,
    };
    let now = 100_000;
    let mut inp = SettleInputs {
        now,
        deadline: now + 86_400,
        ema_count: 500,
        p_ema: p,
        l_ema: l,
        bx: SettleBox::point(p, l),
        budget,
        r_due: 0,
        cr_liquidatable: true,
        ema_leg: true,
        executable: true,
        warmed: true,
        p_rs: Some(p),
        l_live: l,
        b_live: live.capped(cap).buy(q, x, y).map(|b| b.cost),
        sigma_live,
        s3: S3Chain {
            no_route_now: fees::exceeds_cap(live.oneway_ppm(), cap),
            no_route_since: Some(now - 10),
            graduated_at: None,
            migrating: false,
            migration_seen_at: None,
        },
    };
    let mut arm = ArmState {
        count: 480,
        since: now - 20,
        last: now - 5,
        armed: 1,
        _pad: [0; 7],
    };
    // Neither S1 nor S2 holds at the risk fee; the refutation does, and disarms the live arm.
    assert!(!s1_legs(&mut quotes, &budget, inp.b_live, p, l, &inp.bx).unwrap());
    assert!(!s2_legs(&mut quotes, 2_000, sigma_live, p, l, &inp.bx).unwrap());
    assert!(refuted(&mut quotes, &budget, 2_000, l, &inp.bx).unwrap());
    let e = evaluate(&mut arm, &inp, &terms, &mut quotes).unwrap();
    assert_eq!(e.arm, ArmChange::Disarmed(DisarmReason::Refuted));
    assert_eq!((arm.armed, e.settlement), (0, None));
    // The S3 timer runs on the fee evidence alone; once due, S3 settles (point box).
    inp.now = now + 600;
    let e = evaluate(&mut arm, &inp, &terms, &mut quotes).unwrap();
    let s = e.settlement.expect("S3 due");
    assert_eq!(
        (s.kind, e.arm, arm.armed),
        (SettleKind::S3, ArmChange::None, 0)
    );
    assert!(s.lenders <= budget.v - k_s);

    // At or below the cap the two views coincide.
    let at_cap = Pricing::RayV4(ray_v4(25, 10_000).unwrap());
    assert_eq!(at_cap.capped(cap), at_cap);
    assert!(!fees::exceeds_cap(at_cap.oneway_ppm(), cap));
}

/// Runs the real prologue over `seconds` (`roll`, then `record` of each observation of that
/// second) from a market created at `p0` and returns the post-roll state after the last.
fn run_path(
    p0: u128,
    l: u128,
    seconds: &[Vec<u128>],
) -> (shortwin_math::observe::Observation, i64) {
    use shortwin_math::observe::{EmaParams, Observation};
    let params = EmaParams::DEFAULT;
    let mut o = Observation::new(0, p0, l).unwrap();
    let mut t = 0i64;
    for obs in seconds {
        t += 1;
        o.roll(t, &params).unwrap();
        for &p in obs {
            o.record(t, p, l).unwrap();
        }
    }
    t += 1;
    o.roll(t, &params).unwrap();
    (o, t)
}

/// `flat` seconds at `p0`, then `ramp_s` seconds of a geometric ramp of `g_bps` per second,
/// each second observed `obs` times at rising prices from its opening price.
fn ramp(p0: u128, flat: usize, ramp_s: usize, g_bps: u128, obs: usize) -> Vec<Vec<u128>> {
    let mut p = p0;
    let mut out = vec![vec![p0]; flat];
    for _ in 0..ramp_s {
        let next = p * (10_000 + g_bps) / 10_000;
        out.push(
            (0..obs)
                .map(|k| p + (next - p) * k as u128 / obs as u128)
                .collect(),
        );
        p = next;
    }
    out
}

proptest! {
    #![proptest_config(common::proptest_config())]

    /// ECON §5.6, ARCH §4.5 (§8.1 "in a monotone uptrend the S1 corner is `P_ema`"): in a
    /// sustained ramp the EMA (τ = `EMA_TAU_S`) lags the lower median of the last 8 bucket
    /// lows, so the box's `P_lo`, the S1 corner's price, is `P_ema` itself and honest S1
    /// timing is unchanged. Ramps of 0.05–5 %/s (ECON §5.7's lag-study pumps run
    /// 1.8–3.9 %/s) running for at least two ring lengths: the EMA starts the ramp at the
    /// ramp's own opening price, ≈ 5 steps ahead of its steady lag, and needs ≈ 9–11 steps to
    /// fall behind the median (`uptrend_claim_needs_a_sustained_ramp`).
    #[test]
    fn uptrend_s1_corner_is_the_ema(
        g_bps in 5u128..=500,
        flat in 0usize..20,
        ramp_s in 16usize..90,
        obs in 1usize..=3,
        p0 in 1_000_000_000_000u128..100_000_000_000_000_000_000,
        l in 1_000_000_000u128..1_000_000_000_000_000,
    ) {
        let (o, now) = run_path(p0, l, &ramp(p0, flat, ramp_s, g_bps, obs));
        let bx = box_of(&o.ema, &o.ring, now, true).unwrap();
        prop_assert_eq!(bx.p_lo, o.ema.price);
        prop_assert!(!bx.is_point());
    }
}

/// The limits of the uptrend claim (a doc precision, recorded in the crate README): the
/// ramp's first 8 seconds, a step that fills 4 of the 8 entries, and a steady ramp between
/// ≈ 9.5 %/s (where the EMA's steady lag `(1 + g)/(1 + 6g)` exceeds `(1 + g)^−4`) and
/// ≈ 16.7 %/s (above it the default ±100 % clamp caps the EMA's growth at 1/6 per second and
/// it falls behind again) leave `P_ema` above the lower median, so the S1 corner is `P_mlo`
/// there.
#[test]
fn uptrend_claim_needs_a_sustained_ramp() {
    let (p0, l) = (1_000_000_000_000_000u128, 1_000_000_000_000u128);
    let cases = [
        ("the ramp's first 8 s", ramp(p0, 20, 8, 50, 1)),
        ("12 %/s", ramp(p0, 20, 60, 1_200, 1)),
        ("a step held 4 s", {
            let mut s = vec![vec![p0]; 20];
            s.extend(std::iter::repeat_n(vec![2 * p0], 4));
            s
        }),
    ];
    for (name, path) in cases {
        let (o, now) = run_path(p0, l, &path);
        let bx = box_of(&o.ema, &o.ring, now, true).unwrap();
        assert!(bx.p_lo < o.ema.price, "{name}: {bx:?} vs {}", o.ema.price);
    }
    // A 5 %/s ramp after 16 s, and a 25 %/s ramp (clamped EMA): the EMA lags the median.
    for (g, secs) in [(500, 16), (2_500, 30)] {
        let (o, now) = run_path(p0, l, &ramp(p0, 20, secs, g, 1));
        assert_eq!(
            box_of(&o.ema, &o.ring, now, true).unwrap().p_lo,
            o.ema.price,
            "{g} bps/s"
        );
    }
}
