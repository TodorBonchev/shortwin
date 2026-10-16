//! Venue admin on the curve with a position open (M10 WP-8a; ARCH §4.3 "two fee views", §5.3,
//! §8.2; A43, A63, V1-8, V2-1, O15): what pump's admin and the Emergency multisig can do to an
//! open curve position, on the dumped pump and pump-fees ELFs.
//!
//! - **G-G4, pump `FeeConfig` hikes** (`fork::set_fee_config`, the bytes `update_fee_config` /
//!   `upsert_fee_tiers` leave): a fee between `venue_fee_cap_bps` and the hard cap refuses opens
//!   and is no S3 evidence, closes and liquidations buy back at the live fee; a fee above the hard cap starts the
//!   S3 chain while the curve is still observed, the risk view stays at the position's locked
//!   cap (a position the live fee would make CR-liquidatable is not liquidated), the borrower's
//!   close still executes at the live fee, and the expired position settles S3 with FV at the
//!   cap, never at the live fee.
//! - **G-H3, a curve layout change** (the bonding curve's discriminator, or a `bool` byte at 7):
//!   not executable — the crank and `sol_settle` succeed and record nothing, the S3 chain starts,
//!   opens and closes fail `NoRoute` — and S3 at the timer prices FV at the position's cap on the
//!   frozen EMA point (the adapter's cap-only view).
//! - **G-D6, the Emergency flag** (`set_route_broken`, the market or the curve's venue bit): the
//!   S3 chain only; S3 of an expired healthy position after `s3_wait_s` with the expiry fee and
//!   FV at the live fee; liquidations along a real pump identical second by second with and
//!   without it; and O15's race of that S3 against an honest `expire`.
//!
//! The curve has no `pool_fail` chain (Raydium's fast path only): it stays 0 throughout. Every
//! instruction goes through `Env::send`: the invariant hooks run after each one.

use shortwin::PauseScope;
use shortwin::constants::{PUMP_FEE_CONFIG, VENUE_BIT_PUMP_CURVE};
use shortwin::events::{Expire, Liquidate};
use shortwin_litesvm_tests::engine::{expire_ix, liquidate_ix, set_route_broken_ix};
use shortwin_math::fees::FeeBps;
use shortwin_math::quote::Pricing;
use shortwin_math::rent;
use solana_keypair::Keypair;

use crate::common::*;
use crate::curve_support::CurveCtx;

/// Anchor's `AccountNotInitialized`: a closed position account.
const ANCHOR_ACCOUNT_NOT_INITIALIZED: u32 = 3_012;

/// Rewrites pump's `FeeConfig` so every tier charges `protocol` bps (lp and creator kept).
fn set_protocol_fee(env: &mut Env, protocol: u64) {
    let (flat, mut tiers, stable, exotic) = fork::fee_config_specs(&env.svm, &PUMP_FEE_CONFIG);
    for t in &mut tiers {
        t.1[1] = protocol;
    }
    fork::set_fee_config(
        &mut env.svm,
        &PUMP_FEE_CONFIG,
        flat,
        &tiers,
        &stable,
        exotic,
    );
}

/// The curve's live one-way fee, bps.
fn live_bps(env: &Env, pm: &PumpMarket) -> u64 {
    pm.curve.fees(&env.svm).expect("fee tier").oneway_bps()
}

/// The Emergency multisig sets or clears the market's route-broken flag (`venue_scope =
/// false`) or the curve's bit of `venue_route_broken`.
fn set_flag(env: &mut Env, pm: &PumpMarket, venue_scope: bool, broken: bool) {
    let ea = env.pause_authority.insecure_clone();
    let ix = if venue_scope {
        let bit = VENUE_BIT_PUMP_CURVE.trailing_zeros() as u8;
        set_route_broken_ix(&ea.pubkey(), PauseScope::Global, bit, broken, None)
    } else {
        set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Market,
            0,
            broken,
            Some(pm.market()),
        )
    };
    ok(env.send(&[ix], &[&ea]));
}

/// Jumps the clock to `deadline + after` of `pos` (a new slot range) and cranks 3 s there.
fn jump_to_deadline(env: &mut Env, pm: &PumpMarket, pos: &ShortPosition, after: i64) {
    let slot = env.clock().slot;
    env.set_clock(pos.deadline + after, slot + 1_500_000);
    for _ in 0..3 {
        env.warp(1);
        crank(env, pm);
    }
}

/// Cranks and calls `sol_settle` every `step` seconds, at most `n` times: the settlement.
fn run_to_s3(
    env: &mut Env,
    pm: &PumpMarket,
    kp: &Keypair,
    b: &Borrower,
    nonce: u64,
    step: i64,
    n: usize,
) -> Option<shortwin::events::SolSettle> {
    for _ in 0..n {
        env.warp(step);
        crank(env, pm);
        if let Some(ev) = settled(&ok(settle(env, pm, kp, &b.key(), nonce))) {
            return Some(ev);
        }
    }
    None
}

/// `liquidate` by `kp` with the keeper-exit CU limit.
fn liquidate(env: &mut Env, pm: &PumpMarket, kp: &Keypair, b: &Borrower, nonce: u64) -> TxResult {
    let ix = liquidate_ix(pm, &kp.pubkey(), &b.key(), nonce);
    env.send(&[pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU), ix], &[kp])
}

/// `expire` by `kp` with the keeper-exit CU limit.
fn expire(env: &mut Env, pm: &PumpMarket, kp: &Keypair, b: &Borrower, nonce: u64) -> TxResult {
    let ix = expire_ix(pm, &kp.pubkey(), &b.key(), nonce);
    env.send(&[pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU), ix], &[kp])
}

/// `FV = B̂ + ⌈p'·B̂/10⁴⌉ + R_due` for a risk quote `b`.
fn fv_of(b: u64, pprime_bps: u16, r_due: u64) -> u64 {
    b + (u128::from(b) * u128::from(pprime_bps)).div_ceil(10_000) as u64 + r_due
}

/// G-G4 (a): pump's fee raised between `venue_fee_cap_bps` (150) and the hard cap (500) with
/// a position open: opens refused `VenueFeeTooHigh`, no S3 evidence (the chain stays clear, the
/// curve observed), the borrower's close buys back at pump's live charge, and so does a keeper's
/// liquidation after a pump, its bound and risk quote at the live fee.
#[test]
fn a_fee_between_the_caps() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let base = live_bps(&env, &pm);
    set_protocol_fee(&mut env, 300);
    let fee = live_bps(&env, &pm);
    assert!(fee > 150 && fee <= 500 && fee > base, "{base} → {fee} bps");
    env.warp(1);
    crank(&mut env, &pm);
    let m = market(&env, &pm);
    assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0));
    assert!(m.b_lo.get() > 0, "observed");
    let mut b2 = env.borrower();
    assert_err(&env.open(&pm, &mut b2, q).0, E::VenueFeeTooHigh);
    // The borrower's close, on a clone: pump's live charge.
    let saved = env.svm.clone();
    env.warp(1);
    let charge = pm.curve.charge_buy(&env.svm, q).unwrap();
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.buyback_cost, charge.cost, "pump's live charge");
    assert_trader_clean(&env, &pm);
    // A keeper's liquidation after a real pump (a buyer takes 40 % of `vT`), the fee still
    // between the caps: only `NotLiquidatable` until it executes, and then the risk view and the
    // exit's bound are at the live fee, not at `venue_fee_cap_bps` — the buyback is pump's live
    // charge, `B̂_live` short.win's quote at the live fee, and the risk quote equals it
    // (`min(φ_live, cap) = φ_live`).
    env.svm = saved;
    let u = CurveUser::new(&mut env, &pm.curve);
    let vt = pm.curve.state(&env.svm).virtual_token_reserves;
    ok(u.buy(&mut env, &pm.curve, vt / 10 * 4, u64::MAX));
    let kp = keeper(&mut env);
    let mut done = None;
    for _ in 0..900 {
        env.warp(1);
        crank(&mut env, &pm);
        let charge = pm.curve.charge_buy(&env.svm, q).unwrap().cost;
        let quote = pm.curve.quote_buy(&env.svm, q).unwrap().cost;
        let ctx = CurveCtx::read(&env, &pm).expect("curve");
        let at_venue_cap = ctx.buy_live(q, Some(150)).unwrap().cost;
        let res = liquidate(&mut env, &pm, &kp, &b, nonce);
        match &res {
            Ok(meta) => {
                let ev = events::one::<Liquidate>(meta);
                done = Some((ev, charge, quote, at_venue_cap));
                break;
            }
            Err(_) => assert_eq!(
                custom_code(&res),
                Some(u32::from(E::NotLiquidatable)),
                "only not-yet"
            ),
        }
    }
    let (ev, charge, quote, at_venue_cap) = done.expect("liquidated within 15 min");
    let fee = live_bps(&env, &pm);
    assert!(fee > 150 && fee <= 500, "still between the caps: {fee} bps");
    assert!(!ev.expired && ev.penalty > 0);
    assert_eq!(ev.buyback_cost, charge, "pump's live charge");
    assert_eq!(ev.b_live, quote, "bounded by B̂_live(q) at the live fee");
    assert_eq!(ev.b_live_risk, ev.b_live, "the risk view at the live fee");
    assert!(
        at_venue_cap < ev.b_live_risk,
        "a risk view at 150 bps would quote less: {at_venue_cap} vs {}",
        ev.b_live_risk
    );
    assert_trader_clean(&env, &pm);
}

/// G-G4 (b): pump's fee raised far above the hard cap (every tier's protocol fee at 200 %) with
/// a position open. The curve is still observed, but the fee is S3 evidence: the chain starts
/// (no `pool_fail` on the curve). Opens are refused. The risk view stays at the position's
/// 500 bps: at the live fee the position would be CR-liquidatable, yet `liquidate` refuses
/// `NotLiquidatable` (I16's risk-quote clause). The borrower's own close executes at pump's
/// live charge. At the deadline, after `s3_wait_s` of chain, S3 pays FV at the cap — the live
/// fee's FV is far above it.
#[test]
fn a_fee_above_the_hard_cap() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let pos = env.position(&pm, &b.key(), nonce);
    let cap = u64::from(pos.venue_fee_hard_cap_bps);
    for _ in 0..3 {
        env.warp(1);
        crank(&mut env, &pm);
    }
    set_protocol_fee(&mut env, 20_000);
    assert!(live_bps(&env, &pm) > cap);
    env.warp(1);
    crank(&mut env, &pm);
    let m = market(&env, &pm);
    assert_eq!(m.no_route_since, env.now(), "S3 evidence");
    assert_eq!(m.pool_fail_since, 0, "the curve has no fast-path chain");
    assert!(
        m.b_lo.get() > 0,
        "a fee above the cap does not stop observation"
    );
    let mut b2 = env.borrower();
    assert_err(&env.open(&pm, &mut b2, q).0, E::VenueFeeTooHigh);
    // At the live fee the position would be CR-liquidatable; at its cap it is not.
    let ctx = CurveCtx::read(&env, &pm).expect("curve");
    let live = ctx.buy_live(q, None).unwrap().cost;
    let capped = ctx.buy_live(q, Some(cap)).unwrap().cost;
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let v_eff = u128::from(pos.v - r_due);
    assert!(v_eff * 10_000 < u128::from(live) * u128::from(pos.cr_liq_bps));
    assert!(v_eff * 10_000 > u128::from(capped) * u128::from(pos.cr_liq_bps));
    let kp = keeper(&mut env);
    for _ in 0..5 {
        env.warp(1);
        crank(&mut env, &pm);
        assert_err(
            &liquidate(&mut env, &pm, &kp, &b, nonce),
            E::NotLiquidatable,
        );
    }
    // The borrower's close, on a clone: pump's live charge.
    let saved = env.svm.clone();
    env.warp(1);
    let charge = pm.curve.charge_buy(&env.svm, q).unwrap();
    let (close, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.buyback_cost, charge.cost, "the live 200 %+ fee");
    env.svm = saved;
    // S3 at the deadline, FV at the locked cap.
    jump_to_deadline(&mut env, &pm, &pos, -5);
    let since = market(&env, &pm).no_route_since;
    let ev = run_to_s3(&mut env, &pm, &kp, &b, nonce, 30, 30).expect("S3");
    assert_eq!(ev.condition, 3);
    assert!(env.now() >= since + i64::from(pos.s3_wait_s) && env.now() < since + 630);
    assert_eq!(ev.pprime_lo, pos.expiry_fee_bps, "O15");
    let m = market(&env, &pm);
    let (p, l) = (m.ema_price.get(), m.ema_liq.get());
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let ctx = CurveCtx::read(&env, &pm).expect("curve");
    let at_cap = ctx.buy_repriced(pos.q, l, p, cap).unwrap().cost;
    let fv = fv_of(at_cap, ev.pprime_lo, r_due);
    assert_eq!(ev.fv, fv, "FV at (P_ema, L_ema) and the locked cap");
    let at_live = ctx.pricing().buy_repriced(pos.q, l, p).unwrap().cost;
    assert!(
        fv_of(at_live, ev.pprime_lo, r_due) > 2 * fv,
        "never the live fee"
    );
    assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
}

/// G-H3 on the curve (A63, V1-8): the bonding curve's layout changes with a position open — its
/// discriminator, or a `bool` byte at 7 (the reader takes 0 and 1 only). Not executable: the
/// crank and `sol_settle` succeed and record nothing (`b_lo` 0, the EMA frozen), the S3 chain
/// starts, the market stays `PumpCurve`, opens and closes fail `NoRoute`. The expired position
/// settles S3 once the chain has run `s3_wait_s`, at the frozen EMA point, priced by the cap-only
/// view at the position's hard cap (the curve's fee is unreadable), not at the curve's last live
/// fee (that FV differs). The old bytes back, the chain clears.
#[test]
fn a_curve_layout_change_with_a_position_open() {
    let patches: [fn(&mut Vec<u8>); 2] = [
        |d| d[0] ^= 0xff,
        |d| d[shortwin_venue_state::pump::OFF_COMPLETE] = 7,
    ];
    for patch in patches {
        let (mut env, pm, q) = setup(PumpCoin::T22_115);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
        let pos = env.position(&pm, &b.key(), nonce);
        for _ in 0..3 {
            env.warp(1);
            crank(&mut env, &pm);
        }
        let last_live = CurveCtx::read(&env, &pm).expect("curve");
        let original = env.svm.get_account(&pm.curve.curve).unwrap();
        fork::patch(&mut env.svm, &pm.curve.curve, patch);
        env.warp(1);
        let meta = crank(&mut env, &pm);
        no_venue_change(&meta);
        let m = market(&env, &pm);
        assert_eq!(m.venue_kind(), Some(VenueKind::PumpCurve));
        assert_eq!(m.no_route_since, env.now(), "the chain starts");
        assert_eq!(m.b_lo.get(), 0, "nothing recorded");
        // The hooks' own view (I13 / I16) sees the venue as not executable too, so a recorded
        // observation or a missing chain here would fail every instruction's hooks.
        let view = shortwin_litesvm_tests::invariants::venue_view::view(&env.svm, &m);
        assert!(!view.expect("a venue view").executable);
        let kp = keeper(&mut env);
        assert!(settled(&ok(settle(&mut env, &pm, &kp, &b.key(), nonce))).is_none());
        assert_err(&env.close(&pm, &b, nonce), E::NoRoute);
        let mut b2 = env.borrower();
        assert_err(&env.open(&pm, &mut b2, q).0, E::NoRoute);
        let frozen = market(&env, &pm);
        jump_to_deadline(&mut env, &pm, &pos, -5);
        let since = market(&env, &pm).no_route_since;
        let ev = run_to_s3(&mut env, &pm, &kp, &b, nonce, 30, 30).expect("S3");
        assert_eq!(ev.condition, 3);
        assert!(env.now() >= since + i64::from(pos.s3_wait_s) && env.now() < since + 630);
        let m = market(&env, &pm);
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        assert_eq!(
            (p, l),
            (frozen.ema_price.get(), frozen.ema_liq.get()),
            "the EMA is frozen"
        );
        assert_eq!((ev.p_lo, ev.p_hi, ev.l_lo, ev.l_hi), (p, p, l, l));
        assert_eq!(ev.pprime_lo, pos.expiry_fee_bps);
        let cap = u64::from(pos.venue_fee_hard_cap_bps);
        let b_fv = Pricing::PumpSwap(FeeBps::new(0, cap, 0))
            .buy_repriced(pos.q, l, p)
            .unwrap()
            .cost;
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let fv = fv_of(b_fv, ev.pprime_lo, r_due);
        assert_eq!(ev.fv, fv, "FV at the position's hard cap");
        // Control: FV at the curve's last live fee (its fee schedule before the patch, under the
        // cap) is another amount, so the assertion above tells the two apart.
        let at_last = last_live.buy_repriced(pos.q, l, p, cap).unwrap().cost;
        assert_ne!(
            ev.fv,
            fv_of(at_last, ev.pprime_lo, r_due),
            "not at the last live fee"
        );
        assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
        // The old bytes back.
        env.svm.set_account(pm.curve.curve, original).unwrap();
        env.warp(1);
        crank(&mut env, &pm);
        assert_eq!(market(&env, &pm).no_route_since, 0);
    }
}

/// G-D6 on the curve (T-S3-BROKEN; ARCH §5.3, V2-1, O15): the Emergency flag — the market's or
/// the curve's venue bit — on an executable curve starts the S3 chain at the next observation
/// and nothing else (opens `RouteBroken`; the curve still observed). Flagged 300 s before the
/// deadline, the healthy position never settles before `max(deadline, since + s3_wait_s)` and
/// settles S3 at the first keeper call after it, with the expiry fee at both corners and FV at
/// the EMA point at the live fee (under the cap). Clearing the flag clears the chain.
#[test]
fn an_emergency_flag_settles_expired_positions_after_the_timer() {
    for venue_scope in [false, true] {
        let (mut env, pm, q) = setup(PumpCoin::T22_115);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
        let pos = env.position(&pm, &b.key(), nonce);
        jump_to_deadline(&mut env, &pm, &pos, -300);
        set_flag(&mut env, &pm, venue_scope, true);
        let mut b2 = env.borrower();
        assert_err(&env.open(&pm, &mut b2, q).0, E::RouteBroken);
        env.warp(1);
        crank(&mut env, &pm);
        let m = market(&env, &pm);
        let since = m.no_route_since;
        assert_eq!(
            since,
            env.now(),
            "venue scope {venue_scope}: the chain starts"
        );
        assert!(m.b_lo.get() > 0, "the curve is still observed");
        let kp = keeper(&mut env);
        let due = (since + i64::from(pos.s3_wait_s)).max(pos.deadline);
        let ev = loop {
            env.warp(30);
            crank(&mut env, &pm);
            if let Some(ev) = settled(&ok(settle(&mut env, &pm, &kp, &b.key(), nonce))) {
                break ev;
            }
            assert!(env.now() < due, "S3 is due at {due}");
        };
        assert!(env.now() >= due && env.now() < due + 30);
        assert_eq!(ev.condition, 3);
        assert_eq!(
            (ev.pprime_lo, ev.pprime_hi),
            (pos.expiry_fee_bps, pos.expiry_fee_bps)
        );
        let m = market(&env, &pm);
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let ctx = CurveCtx::read(&env, &pm).expect("curve");
        let cap = u64::from(pos.venue_fee_hard_cap_bps);
        let fv = fv_of(
            ctx.buy_repriced(pos.q, l, p, cap).unwrap().cost,
            ev.pprime_lo,
            r_due,
        );
        assert_eq!(ev.fv, fv, "FV at the EMA point, the live fee");
        let take = pos.v - k_s(&pos);
        assert_eq!(ev.lender_amount, fv.min(take));
        assert_eq!(ev.borrower_amount, take - ev.lender_amount);
        set_flag(&mut env, &pm, venue_scope, false);
        env.warp(1);
        crank(&mut env, &pm);
        assert_eq!(market(&env, &pm).no_route_since, 0);
    }
}

/// G-D6, the liquidation side on the curve (V2-1, I10): from the same state, along the same
/// path of real `buy_v2`s by another user (0.75 % of `vT` a second), with and without the
/// Emergency flag the keeper's `liquidate` refuses with the same codes in the same seconds and
/// executes in the same second with identical amounts; only the flagged run has an S3 chain.
#[test]
fn liquidations_are_identical_with_and_without_the_flag() {
    let (mut env, pm, q) = setup(PumpCoin::Fresh);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    let kp = keeper(&mut env);
    let u = CurveUser::new(&mut env, &pm.curve);
    let step = pm.curve.state(&env.svm).virtual_token_reserves / 400 * 3;
    let saved = env.svm.clone();
    let mut runs = Vec::new();
    for flag in [false, true] {
        env.svm = saved.clone();
        if flag {
            set_flag(&mut env, &pm, false, true);
        }
        let mut trace: Vec<Result<[u64; 10], Option<u32>>> = Vec::new();
        for _ in 0..90 {
            ok(u.buy(&mut env, &pm.curve, step, u64::MAX));
            env.warp(1);
            crank(&mut env, &pm);
            let res = liquidate(&mut env, &pm, &kp, &b, nonce);
            let done = res.is_ok();
            trace.push(match &res {
                Ok(meta) => {
                    let e: Liquidate = events::one(meta);
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
            });
            if done {
                break;
            }
        }
        runs.push((trace, market(&env, &pm).no_route_since));
    }
    let (plain, flagged) = (&runs[0], &runs[1]);
    assert!(
        plain.0.last().is_some_and(Result::is_ok),
        "the path liquidates"
    );
    assert!(plain.0.len() > 1, "healthy first");
    assert_eq!(plain.0, flagged.0, "the same attempts, second by second");
    assert_eq!(plain.1, 0);
    assert!(flagged.1 > 0, "only the flagged run has an S3 chain");
}

/// O15's race on the curve under the Emergency flag (ARCH §8.2 round 3): at the deadline of a
/// flagged, expired, healthy position whose chain has run `s3_wait_s`, the honest `expire()` and
/// a lender-keeper's `sol_settle(S3)` are both valid; whichever lands first closes the position
/// and the other finds none (`AccountNotInitialized`). The S3 take equals the expiry's buyback,
/// fee and rent within 0.1 %, and the borrower's difference is `K_s − K_e` within that margin.
#[test]
fn an_emergency_flag_s3_races_an_honest_expiry() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let pos = env.position(&pm, &b.key(), nonce);
    jump_to_deadline(&mut env, &pm, &pos, -700);
    set_flag(&mut env, &pm, false, true);
    let kp = keeper(&mut env);
    for _ in 0..23 {
        env.warp(30);
        crank(&mut env, &pm);
        assert!(settled(&ok(settle(&mut env, &pm, &kp, &b.key(), nonce))).is_none());
    }
    let now = env.now();
    env.warp(pos.deadline - 5 - now);
    for _ in 0..5 {
        env.warp(1);
        crank(&mut env, &pm);
    }
    assert_eq!(env.now(), pos.deadline);
    let honest = keeper(&mut env);
    let saved = env.svm.clone();

    let exp: Expire = events::one(&ok(expire(&mut env, &pm, &honest, &b, nonce)));
    assert_eq!(
        custom_code(&settle(&mut env, &pm, &kp, &b.key(), nonce)),
        Some(ANCHOR_ACCOUNT_NOT_INITIALIZED),
        "the lender-keeper's S3 finds no position"
    );

    env.svm = saved;
    let ev = settled(&ok(settle(&mut env, &pm, &kp, &b.key(), nonce))).expect("S3");
    assert_eq!(ev.condition, 3);
    assert_eq!(
        (ev.pprime_lo, ev.pprime_hi),
        (pos.expiry_fee_bps, pos.expiry_fee_bps)
    );
    assert_eq!(
        custom_code(&expire(&mut env, &pm, &honest, &b, nonce)),
        Some(ANCHOR_ACCOUNT_NOT_INITIALIZED),
        "the honest expiry finds no position"
    );
    let expiry_value = exp.buyback_cost + exp.expiry_fee + exp.rent;
    let diff = ev.lender_amount.abs_diff(expiry_value);
    assert!(
        diff * 1_000 <= expiry_value,
        "S3 take {} vs the expiry's B + fee + rent {expiry_value}",
        ev.lender_amount
    );
    let extra = i128::from(exp.borrower_payout) - i128::from(ev.borrower_amount);
    let keeper_gap = i128::from(ev.k_s) - i128::from(exp.keeper_reward);
    assert!(
        (extra - keeper_gap).unsigned_abs() <= u128::from(diff),
        "borrower: expiry {} vs S3 {} (K_s {} / K_e {})",
        exp.borrower_payout,
        ev.borrower_amount,
        ev.k_s,
        exp.keeper_reward
    );
}
