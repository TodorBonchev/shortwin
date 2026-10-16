//! ECON §12's attack patterns that apply to a real constant-product venue, reproduced against
//! the release short.win on the **dumped** AMM v4 program and Fartcoin's main pool (ARCH §8.2
//! "adversarial suite … on every real venue as it lands"; the mock versions, matched to ECON's
//! sims to the lamport, are `tests/attacks_exit_it.rs` and `tests/attacks_settle_it.rs`). Every
//! short.win instruction — including those inside an attacker's one-transaction bundle — is
//! checked by the differential oracle of `raydium_v4_support.rs`, and the invariant hooks run
//! after every instruction of every bundle.
//!
//! The market is Fartcoin at Tier C with the per-market sizing limits lifted within their
//! compiled bounds (`market_max_notional`, `wallet_cap_bps`), so the victim holds one position
//! at the SPEC per-position cap (`3·φ·X₀`, ≈ 0.75 % of the pool, ≈ 290 SOL of notional) — the
//! size at which ECON prices the attacks.
//!
//! | Test | ECON script | What holds on the real venue |
//! |---|---|---|
//! | `exit_sandwich_on_a_liquidation` | `sandwich.py`, `fixed_delta.py` (d) (atomic) | the keeper-attacker's best `[buy dX, liquidate, sell dX]` spends at most `⌊B̂_ref·(1 + δ)⌋`; lenders and protocol get what the honest liquidation gives them; the attacker's extra comes out of the borrower's remainder only; waiting 4 s does not widen the bound (fixed δ) |
//! | `exit_sandwich_on_an_expiry` | `sandwich.py` (expiry, atomic) | rent and the expiry fee are senior: paid exactly as in the honest expiry; the spend stays within `δ_exp` |
//! | `atomic_bundles_cannot_liquidate_a_fresh_position` | `ema_atomic.py` (atomic) | `[pump, crank_price, dump]` every second at ×1.25 … ×3 beside the honest keeper's crank never lifts the EMA (directional aggregation) and never makes the position liquidatable; `[pump, liquidate, dump]` is refused `NotLiquidatable` |
//! | `atomic_bundles_cannot_push_near_threshold_positions` | `ema_near.py` (atomic) | the same with the EMA caught up at `CR_ema` just above `L` |
//! | `atomic_bundles_cannot_force_s1` | `ema_atomic_multi.py` (b) (atomic) | a lender-keeper's `[pump past the S1 boundary, sol_settle, dump]` every second never arms the settlement clock |
//! | `held_pushes_on_a_fresh_position_do_not_pay` | `ema_atomic.py` (held) | holding the pool at ×2.5 until the position is liquidatable, then sandwiching the liquidation, costs the attacker more than it takes |
//! | `pushed_seconds_cannot_flip_s2_or_move_its_amount` | `fv_push.py` (T-BOX) | on the pool thinned into S2 by LP removal (`raydium_v4_e2e.rs`), one attacker-only push up or down before the honest S2 leaves it an S2 within `fv_dev_bps` of the honest amount; the flip bundle `[pump → sol_settle → dump]` cannot make it an uncapped S1 |
//! | `held_pushes_before_a_liquidation_take_from_the_borrower_only` (M10) | `sandwich.py` held, §12.6 | 1 s at +2.5 % and 3 s at +15 % held (attacker-only seconds) before the best sandwich: the spend within `⌊B̂_ref·(1 + δ)⌋` of the lifted reference, the lenders' and protocol's legs at least the honest ones, the attacker's extra out of the borrower's remainder only; the 3-s plan pays on this pool (O19's residual, scaled by `V`) |
//! | `atomic_bundles_cannot_force_the_oi_cap_basket` (M10) | `ema_atomic_multi.py` (a) (atomic) | the market filled to its OI cap (three positions); `[pump, crank_price, dump]` every second at ×1.5 / ×2 / ×3 for 60 s: no position becomes liquidatable, the final bundles are refused `NotLiquidatable` |
//! | `atomic_bundles_cannot_force_s1_on_the_oi_cap_basket` (M10) | `ema_atomic_multi.py` (b) (atomic) | the OI-cap basket; `[pump past the lowest S1 boundary, sol_settle × every position, dump]` every second for 60 s: the oracle sees nothing to arm, nothing arms or settles |
//! | `round_3_pushes_cannot_flip_the_thin_pool_s2` (M10) | `fv_push.py` (R3A-1, R4B-2, R4A-9) | on the thin pool one second before its S2: 2 pushed seconds, 1 push + 2 / 4 / 6 frozen seconds, 1–4 consecutive attacker-only seconds and 2–6 pushes with an honest second between, each followed by the flip bundle: no S1, any settlement within `fv_dev_bps`; 5–6 consecutive attacker-only seconds (the documented degraded-mode limit) reported |
//! | `round_3_settlement_attacks_delay_s1_by_seconds_only` (M10) | `arming.py`, `refute_push.py` (a), (b), (c) | the capped position 5 % beyond its S1 boundary after a genuine gap: honest S1 after 57 s (the mock's `RC` figure); a borrower bundle every 20 s changes nothing; a refutation push delays it ≤ 4 s, push-then-freeze ≤ 8 s, a spot-leg veto of 9–20 s ≤ 6 s; every settlement call the oracle's |

#![allow(clippy::result_large_err)]

#[path = "raydium_v4_support.rs"]
mod support;

use shortwin_math::settle::ArmChange;
use support::*;

/// Lent tokens: 2·10⁶ Fartcoin (≈ 8 % of the pool's tokens).
const LEND: u64 = 2_000_000_000_000;

/// The Fartcoin market with the sizing limits lifted and one position at the per-position cap.
fn capped_market() -> (Fart, Borrower, u64) {
    let mut f = Fart::new(fork_env_with(lift_notional), LEND);
    let q = max_open_q(&mut f);
    let (b, nonce, open) = f.open(q);
    let (x, y) = reserves(&f.env, &f.rm).unwrap();
    eprintln!(
        "victim: q = {q} ({:.3} % of X = {x}), N_open = {:.2} SOL, V = {:.2} SOL, pool {:.0} SOL",
        q as f64 * 100.0 / x as f64,
        open.n_open as f64 / 1e9,
        open.v as f64 / 1e9,
        y as f64 / 1e9
    );
    (f, b, nonce)
}

/// An attacker (or attacker-keeper) holding 60,000 SOL of WSOL and 4·10⁷ Fartcoin.
fn attacker(f: &mut Fart) -> Trader {
    actor(&mut f.env, &f.rm, 60_000 * SOL, 40_000_000_000_000)
}

/// `sandwich.setup_liq` on the real pool: 120 s of honest cranks, a quick honest rise to ×1.8
/// with the EMA settled, then +0.1 %/s (a real buy and the keeper's crank every second) until
/// the first second the position is liquidatable on both legs.
fn setup_liq() -> (Fart, Borrower, u64) {
    let (mut f, b, nonce) = capped_market();
    f.tick(120);
    let p0 = f.spot();
    for k in 1..=5 {
        f.second_at(scale(p0, 1.8f64.powf(f64::from(k) / 5.0)));
    }
    f.tick(60);
    assert!(!f.predict(&b.key(), nonce).cr_liquidatable());
    let mut k = 1.0f64;
    let p1 = f.spot();
    for _ in 0..1_000 {
        k *= 1.001;
        f.second_at(scale(p1, k));
        if f.predict(&b.key(), nonce).cr_liquidatable() {
            return (f, b, nonce);
        }
    }
    panic!("never liquidatable");
}

/// The largest front-run `dX` (tokens bought exact-out) after which `which` executes: the
/// attack scripts' best in-transaction pump, found with the oracle on a clone. Where the exit
/// already executes without a front-run the window starts at 0; after held pushes it starts
/// where the live spot reaches the pushed bucket's low (`would_liq`). The window's upper edge
/// is located on a 0.5 % grid of the token reserve, then by bisection.
fn best_dx(f: &mut Fart, a: &Trader, which: Which, b: &Pubkey, nonce: u64) -> u64 {
    let (x, _) = reserves(&f.env, &f.rm).unwrap();
    let ok_at = |f: &mut Fart, dx: u64| {
        let front = buy_ix(&f.rm, a, dx);
        let v = view_after(&mut f.env, &f.rm, b, nonce, &[front], &[&a.owner]);
        predict(&v).decision(which).is_ok()
    };
    let step = x / 200;
    let mut lo = if ok_at(f, 1) {
        1
    } else {
        let mut d = step;
        while !ok_at(f, d) {
            d += step;
            assert!(d < x / 10 * 7, "no front-run makes the exit execute");
        }
        d
    };
    let mut hi = lo + step;
    while ok_at(f, hi) {
        lo = hi;
        hi += step;
    }
    while hi - lo > x / 1_000_000 {
        let mid = lo + (hi - lo) / 2;
        if ok_at(f, mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The attacker-keeper's `[buy dX, exit, sell dX]` (one transaction), checked by the oracle.
/// Returns the exit and the attacker's P&L: WSOL change plus the keeper legs (the back-run
/// sells exactly the tokens the front-run bought).
fn sandwich(
    f: &mut Fart,
    a: &Trader,
    which: Which,
    b: &Pubkey,
    nonce: u64,
    dx: u64,
) -> (Exit, i128) {
    let w0 = wsol_of(&f.env, a);
    let l0 = i128::from(lamports(&f.env, &a.owner.pubkey()));
    let t0 = tokens(&f.env, &a.token);
    let front = buy_ix(&f.rm, a, dx);
    let back = sell_ix(&f.rm, a, dx);
    let kp = a.owner.insecure_clone();
    let ex = exit_checked(
        &mut f.env,
        &f.rm,
        which,
        &kp,
        b,
        nonce,
        &[front],
        &[back],
        &[],
    );
    assert_eq!(
        tokens(&f.env, &a.token),
        t0,
        "the back-run sells what the front-run bought"
    );
    let pnl = wsol_of(&f.env, a) - w0 + i128::from(lamports(&f.env, &a.owner.pubkey())) - l0;
    (ex, pnl)
}

/// Lenders' and protocol's take of a liquidation: rent and their penalty shares.
fn lenders_take(e: &Liquidate) -> u64 {
    e.rent + e.penalty_lender + e.penalty_protocol
}

/// The lenders' and protocol's legs of an attacked liquidation (`ev`) are the honest one's (`h`)
/// up to the penalty (`penalty_bps`) on a drop of its base `min(B̂_live,risk, B̂_ref)` of at most
/// 1 ppm (the front-run's own fees stay in the pool and move the reference by lamports) plus 2
/// lamports of flooring (`venue_attacks_support::assert_lenders_keep`).
fn assert_lenders_keep(h: &Liquidate, ev: &Liquidate, penalty_bps: u16, what: &str) {
    let base = |e: &Liquidate| e.b_live_risk.min(e.b_ref);
    let (hl, al) = (lenders_take(h), lenders_take(ev));
    let drop = base(h).saturating_sub(base(ev));
    assert!(
        u128::from(drop) * 1_000_000 <= u128::from(base(h)),
        "{what}: the penalty base falls by at most 1 ppm: {} → {}",
        base(h),
        base(ev)
    );
    let allowance = (u128::from(drop) * u128::from(penalty_bps)).div_ceil(10_000) as u64 + 2;
    assert!(
        al + allowance >= hl,
        "{what}: lenders and protocol keep the honest legs: {hl} vs {al} lamports (allowance \
         {allowance})"
    );
}

/// `sandwich.py` / `fixed_delta.py` (d), liquidation, atomic model, on the real pool.
#[test]
fn exit_sandwich_on_a_liquidation() {
    let (mut f, b, nonce) = setup_liq();
    let a = attacker(&mut f);
    let saved = f.env.svm.clone();

    // The honest keeper at the first liquidatable second.
    let h = clone_event(f.exit(Which::Liquidate, &b.key(), nonce).liquidated());
    let honest_keeper = i128::from(h.penalty_keeper + h.keeper_topup);

    // The attacker-keeper's best sandwich in the same second.
    f.env.svm = saved.clone();
    let dx = best_dx(&mut f, &a, Which::Liquidate, &b.key(), nonce);
    let (ex, pnl) = sandwich(&mut f, &a, Which::Liquidate, &b.key(), nonce, dx);
    let ev = clone_event(ex.liquidated());
    let delta = u128::from(ex.predicted().view.pos.delta_liq_bps);
    assert!(
        u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * (10_000 + delta),
        "the spend stays within the fixed δ over the reference"
    );
    let extra = pnl - honest_keeper;
    let loss = i128::from(h.borrower_payout) - i128::from(ev.borrower_payout);
    eprintln!(
        "sandwich at t: dX = {dx}, B {:.4} → {:.4} SOL (B̂_ref {:.4}), attacker extra {:+.4} SOL, \
         borrower loss {:.4} SOL, lenders+protocol {} → {} lamports",
        h.buyback_cost as f64 / 1e9,
        ev.buyback_cost as f64 / 1e9,
        ev.b_ref as f64 / 1e9,
        extra as f64 / 1e9,
        loss as f64 / 1e9,
        lenders_take(&h),
        lenders_take(&ev)
    );
    assert!(
        loss > 0 && extra < loss,
        "the attacker's extra comes out of the borrower only"
    );
    assert!(
        u128::try_from(loss).unwrap() * 10_000 <= u128::from(ev.b_ref) * delta,
        "the borrower loses at most δ·B̂_ref"
    );
    let penalty = ex.predicted().view.pos.penalty_bps;
    assert_lenders_keep(&h, &ev, penalty, "the atomic sandwich");

    // After 4 s of waiting (the price held, nobody exits): the same fixed δ.
    f.env.svm = saved;
    f.tick(4);
    let dx4 = best_dx(&mut f, &a, Which::Liquidate, &b.key(), nonce);
    let (ex4, pnl4) = sandwich(&mut f, &a, Which::Liquidate, &b.key(), nonce, dx4);
    let ev4 = clone_event(ex4.liquidated());
    assert_eq!(ev4.delta_bps, ev.delta_bps);
    let extra4 = pnl4 - honest_keeper;
    let loss4 = i128::from(h.borrower_payout) - i128::from(ev4.borrower_payout);
    eprintln!(
        "sandwich after 4 s: attacker extra {:+.4} SOL, borrower loss {:.4} SOL",
        extra4 as f64 / 1e9,
        loss4 as f64 / 1e9
    );
    assert!(u128::from(ev4.buyback_cost) * 10_000 <= u128::from(ev4.b_ref) * (10_000 + delta));
    assert!(
        loss4 * 100 <= loss * 125 + 1_000_000,
        "waiting does not widen the bound: loss {loss} → {loss4}"
    );
}

/// `sandwich.py`, expiry, atomic model, on the real pool: the victim held to its deadline at a
/// flat price.
#[test]
fn exit_sandwich_on_an_expiry() {
    let (mut f, b, nonce) = capped_market();
    let pos = f.pos(&b, nonce);
    while f.env.now() + 3_600 < pos.deadline - 120 {
        f.env.warp(3_600);
        crank_checked(&mut f.env, &f.rm);
    }
    f.env.warp(pos.deadline - 120 - f.env.now());
    f.tick(120);
    assert_eq!(f.env.now(), pos.deadline);
    let a = attacker(&mut f);
    let saved = f.env.svm.clone();
    let h = clone_event(f.exit(Which::Expire, &b.key(), nonce).expired());
    f.env.svm = saved;
    let dx = best_dx(&mut f, &a, Which::Expire, &b.key(), nonce);
    let (ex, pnl) = sandwich(&mut f, &a, Which::Expire, &b.key(), nonce, dx);
    let ev = clone_event(ex.expired());
    let extra = pnl - i128::from(h.keeper_reward);
    let loss = i128::from(h.borrower_payout) - i128::from(ev.borrower_payout);
    eprintln!(
        "expiry sandwich: dX = {dx}, B {:.4} → {:.4} SOL (B̂_ref {:.4}), rent {:.6} / {:.6}, \
         fee {:.6} / {:.6}, attacker extra {:+.4} SOL, borrower loss {:.4} SOL",
        h.buyback_cost as f64 / 1e9,
        ev.buyback_cost as f64 / 1e9,
        ev.b_ref as f64 / 1e9,
        h.rent as f64 / 1e9,
        ev.rent as f64 / 1e9,
        h.expiry_fee as f64 / 1e9,
        ev.expiry_fee as f64 / 1e9,
        extra as f64 / 1e9,
        loss as f64 / 1e9
    );
    assert_eq!(ev.rent, h.rent, "rent is senior");
    assert!(
        h.expiry_fee.abs_diff(ev.expiry_fee) * 1_000 <= h.expiry_fee,
        "the expiry fee is senior (its base moves only by the front-run's liquidity)"
    );
    let delta = u128::from(pos.delta_exp_bps);
    assert!(u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * (10_000 + delta));
    assert!(
        loss > 0 && extra < loss,
        "the attacker's extra comes out of the borrower only"
    );
}

/// One second of `ema_atomic.py`'s atomic model: the honest keeper cranks at the true price,
/// then the attacker's one-transaction `[buy d, crank_price, sell d]` shows the pool at
/// `target` to the same second's bucket. Returns the attacker's WSOL cost.
fn atomic_second(f: &mut Fart, a: &Trader, target: u128) -> i128 {
    f.second();
    let d = move_size(&f.env, &f.rm, target);
    if d <= 0 {
        return 0;
    }
    let d = u64::try_from(d).unwrap();
    let w0 = wsol_of(&f.env, a);
    let ixs = [
        buy_ix(&f.rm, a, d),
        crank_price_ix(&f.rm),
        sell_ix(&f.rm, a, d),
    ];
    let kp = a.owner.insecure_clone();
    ok(f.env.send(&ixs, &[&kp]));
    w0 - wsol_of(&f.env, a)
}

/// The attacker's final `[buy d, liquidate, sell d]` (it pumps the live spot inside its own
/// transaction), checked by the oracle: refused `NotLiquidatable`.
fn final_bundle_refused(f: &mut Fart, a: &Trader, b: &Pubkey, nonce: u64, target: u128) {
    let d = u64::try_from(move_size(&f.env, &f.rm, target).max(1)).unwrap();
    let kp = a.owner.insecure_clone();
    let front = buy_ix(&f.rm, a, d);
    let back = sell_ix(&f.rm, a, d);
    let ex = exit_checked(
        &mut f.env,
        &f.rm,
        Which::Liquidate,
        &kp,
        b,
        nonce,
        &[front],
        &[back],
        &[],
    );
    assert_eq!(ex.refused(), E::NotLiquidatable);
}

/// `ema_atomic.py`, atomic model: a fresh position (60 s after the open) under atomic bundles
/// every second at ×1.25 … ×3 for 30 s. The honest keeper's observation in each second keeps
/// the bucket's low at the true price, so the EMA never rises above the honest evidence and
/// the robust spot leg reads the honest low.
#[test]
fn atomic_bundles_cannot_liquidate_a_fresh_position() {
    let (mut f, b, nonce) = capped_market();
    f.tick(60);
    let a = attacker(&mut f);
    let p_true = f.spot();
    let ema0 = f.market().ema_price.get();
    let saved = f.env.svm.clone();
    for mult in [1.25, 1.5, 2.0, 3.0] {
        f.env.svm = saved.clone();
        let target = scale(p_true, mult);
        let mut honest_max = p_true;
        let mut cost = 0i128;
        for n in 1..=30 {
            honest_max = honest_max.max(f.spot());
            cost += atomic_second(&mut f, &a, target);
            let p = predict(&read_view(&f.env, &f.rm, &b.key(), nonce));
            assert!(!p.would_liq() && !p.ema_leg, "×{mult}, second {n}");
            assert!(
                p.view.market().ema_price.get() <= ema0.max(honest_max),
                "×{mult}, second {n}: the pushes do not lift the EMA"
            );
            if n % 10 == 0 {
                final_bundle_refused(&mut f, &a, &b.key(), nonce, target);
            }
        }
        eprintln!(
            "×{mult}: 30 atomic seconds cost the attacker {:.3} SOL",
            cost as f64 / 1e9
        );
    }
}

/// `ema_near.py`, atomic model: after an honest rise with the EMA caught up (`CR_ema` 1.05 …
/// 1.25 × `L`), 20 s of atomic bundles at ×1.05 … ×2 never make the position liquidatable.
#[test]
fn atomic_bundles_cannot_push_near_threshold_positions() {
    for k0 in [1.6, 1.75, 1.9] {
        let (mut f, b, nonce) = capped_market();
        f.tick(60);
        let p = scale(f.spot(), k0);
        f.second_at(p);
        f.tick(300);
        let st = f.predict(&b.key(), nonce);
        assert!(!st.cr_liquidatable(), "×{k0}: not liquidatable honestly");
        let over = st.cr_ema_bps as f64 / f64::from(st.view.pos.cr_liq_bps);
        eprintln!("×{k0}: CR_ema / L = {over:.3}");
        assert!(
            over > 1.0 && over < 1.35,
            "×{k0}: near the threshold ({over:.3})"
        );
        let a = attacker(&mut f);
        let p_true = f.spot();
        let saved = f.env.svm.clone();
        for mult in [1.05, 1.1, 1.2, 1.5, 2.0] {
            f.env.svm = saved.clone();
            let target = scale(p_true, mult);
            for n in 1..=20 {
                atomic_second(&mut f, &a, target);
                let pr = predict(&read_view(&f.env, &f.rm, &b.key(), nonce));
                assert!(!pr.would_liq(), "×{k0} push ×{mult}, second {n}");
            }
            final_bundle_refused(&mut f, &a, &b.key(), nonce, target);
        }
    }
}

/// `ema_atomic_multi.py` (b), atomic model: a lender-keeper pushes the pool past the position's
/// S1 boundary and calls `sol_settle` inside its own pumped transaction every second for 60 s
/// (the honest keeper cranks first in each second): the robust spot leg reads the honest bucket
/// low, so the S1 legs never hold and the clock never arms. The oracle predicts every one of
/// those settlement calls.
#[test]
fn atomic_bundles_cannot_force_s1() {
    let (mut f, b, nonce) = capped_market();
    f.tick(60);
    let pos = f.pos(&b, nonce);
    let kb = s1_boundary(&f.env, &f.rm, &pos);
    let a = attacker(&mut f);
    let target = scale(f.spot(), kb * 1.2);
    let kp = a.owner.insecure_clone();
    for n in 1..=60 {
        f.second();
        let d = u64::try_from(move_size(&f.env, &f.rm, target)).unwrap();
        let front = buy_ix(&f.rm, &a, d);
        let pred = predict_settle(&view_after(
            &mut f.env,
            &f.rm,
            &b.key(),
            nonce,
            std::slice::from_ref(&front),
            &[&kp],
        ));
        assert!(
            pred.eval.settlement.is_none() && pred.eval.arm == ArmChange::None,
            "second {n}: the oracle sees nothing to arm: {:?}",
            pred.eval
        );
        let settle = sol_settle_ix(&f.env, &f.rm, &kp.pubkey(), &b.key(), nonce);
        let ixs = [front, settle, sell_ix(&f.rm, &a, d)];
        let meta = ok(f.env.send(&ixs, &[&kp]));
        assert!(
            events::of::<SolSettle>(&meta).is_empty()
                && events::of::<SettleArmed>(&meta).is_empty(),
            "second {n}: nothing arms or settles"
        );
        assert_eq!(f.pos(&b, nonce).s_armed, 0);
    }
}

/// An attacker-only second (the attack scripts' `push`, degraded mode): the clock advances
/// and the attacker's one-transaction `[move the pool to target, crank_price, move it back]` is
/// the second's only observation (no honest crank). A push up buys `d` tokens exact-out and
/// sells them back; a push down sells `d` and buys them back exact-out. Returns the
/// attacker's WSOL cost.
fn attacker_only_second(f: &mut Fart, a: &Trader, target: u128) -> i128 {
    f.env.warp(1);
    let d = move_size(&f.env, &f.rm, target);
    let crank = crank_price_ix(&f.rm);
    let ixs = match d {
        0 => vec![crank],
        d if d > 0 => {
            let d = u64::try_from(d).unwrap();
            vec![buy_ix(&f.rm, a, d), crank, sell_ix(&f.rm, a, d)]
        }
        d => {
            let d = u64::try_from(-d).unwrap();
            vec![sell_ix(&f.rm, a, d), crank, buy_ix(&f.rm, a, d)]
        }
    };
    let w0 = wsol_of(&f.env, a);
    let kp = a.owner.insecure_clone();
    ok(f.env.send(&ixs, &[&kp]));
    w0 - wsol_of(&f.env, a)
}

/// One held second (`common.push_second(.., "held")`): the attacker holds the pool at `target`
/// for the whole second — its `[buy d, crank_price, sell d]` is the second's only observation
/// (no honest crank: a degraded keeper) — and pays the round trip. Returns its cost.
fn held_second(f: &mut Fart, a: &Trader, target: u128) -> i128 {
    attacker_only_second(f, a, target)
}

/// `ema_atomic.py`, held model: forcing the liquidation of a fresh position by holding the pool
/// at ×2.5 until it is liquidatable, then sandwiching the liquidation the next second. The
/// borrower loses its margin, but on the real pool the held pushes cost the attacker far more
/// than the sandwich and the keeper legs bring.
#[test]
fn held_pushes_on_a_fresh_position_do_not_pay() {
    let (mut f, b, nonce) = capped_market();
    f.tick(60);
    let a = attacker(&mut f);
    let target = scale(f.spot(), 2.5);
    let mut paid = 0i128;
    let mut secs = 0;
    loop {
        secs += 1;
        paid += held_second(&mut f, &a, target);
        if predict(&read_view(&f.env, &f.rm, &b.key(), nonce)).would_liq() {
            break;
        }
        assert!(secs < 60, "never liquidatable");
    }
    f.env.warp(1);
    let dx = best_dx(&mut f, &a, Which::Liquidate, &b.key(), nonce);
    let (ex, pnl) = sandwich(&mut f, &a, Which::Liquidate, &b.key(), nonce, dx);
    let ev = clone_event(ex.liquidated());
    let net = pnl - paid;
    eprintln!(
        "held ×2.5 for {secs} s: push cost {:.3} SOL, sandwich + keeper legs {:+.3} SOL, net {:+.3} \
         SOL; borrower payout {:.3} SOL",
        paid as f64 / 1e9,
        pnl as f64 / 1e9,
        net as f64 / 1e9,
        ev.borrower_payout as f64 / 1e9
    );
    assert!(net < 0, "the held attack loses money: {net}");
}

// ---- fv_push.py: pushed seconds around an S2 on the real venue --------------------------------

/// Keeper seconds (a checked crank and a checked `sol_settle` each) until the first second
/// whose `sol_settle` settles, peeked on a clone: the VM just before that second and the
/// honest settlement.
fn ready_to_settle(f: &mut Fart, b: &Pubkey, nonce: u64) -> (litesvm::LiteSVM, SolSettle) {
    for _ in 0..200 {
        let saved = f.env.svm.clone();
        f.second();
        if let Some(ev) = f.settle(b, nonce).ev {
            f.env.svm = saved.clone();
            return (saved, ev);
        }
    }
    panic!("never settles");
}

/// ECON §12.11 `fv_push.py` on the thin real pool of `raydium_v4_e2e.rs`'s S2 path (T-BOX on a
/// real venue), one second before the honest S2: (a) one attacker-only second pushing the pool
/// ×1.3 or ×0.8 — the keeper's next calls still settle S2, never S1, within `fv_dev_bps` of
/// the honest amount (the box holds the honest ring medians); (b) the flip bundle `[pump past
/// the S1 boundary → sol_settle → dump]` — the robust spot leg reads the honest bucket low, so
/// no S1. Every settlement call, in the bundle too, is the oracle's.
#[test]
fn pushed_seconds_cannot_flip_s2_or_move_its_amount() {
    let ThinPool {
        mut f,
        b,
        nonce,
        pos,
        ..
    } = thin_pool_market();
    let a = attacker(&mut f);
    let (saved, honest) = ready_to_settle(&mut f, &b.key(), nonce);
    assert_eq!(honest.condition, 2, "the honest outcome is S2");
    let x_h = honest.lender_amount;
    let fv_dev = u128::from(pos.fv_dev_bps);
    for mult in [1.3, 0.8] {
        f.env.svm = saved.clone();
        let target = scale(f.spot(), mult);
        let cost = attacker_only_second(&mut f, &a, target);
        let mut got = None;
        for s in 1..=30 {
            f.second();
            if let Some(ev) = f.settle(&b.key(), nonce).ev {
                got = Some((s, ev));
                break;
            }
        }
        let (s, ev) = got.expect("settles");
        eprintln!(
            "push ×{mult} (cost {:.3} SOL): S{} {s} s later, {:.6} SOL vs honest {:.6} SOL",
            cost as f64 / 1e9,
            ev.condition,
            ev.lender_amount as f64 / 1e9,
            x_h as f64 / 1e9
        );
        assert_eq!(ev.condition, 2, "×{mult}: S2, never S1");
        assert!(
            u128::from(ev.lender_amount.abs_diff(x_h)) * 10_000 <= fv_dev * u128::from(x_h),
            "×{mult}: the amount moves by at most fv_dev_bps"
        );
    }
    // (b) The flip bundle.
    f.env.svm = saved;
    f.env.warp(1);
    let kb = s1_boundary(&f.env, &f.rm, &pos);
    let target = scale(f.spot(), kb * 1.1);
    let d = u64::try_from(move_size(&f.env, &f.rm, target)).unwrap();
    let kp = a.owner.insecure_clone();
    let front = buy_ix(&f.rm, &a, d);
    let pred = predict_settle(&view_after(
        &mut f.env,
        &f.rm,
        &b.key(),
        nonce,
        std::slice::from_ref(&front),
        &[&kp],
    ));
    let settle = sol_settle_ix(&f.env, &f.rm, &kp.pubkey(), &b.key(), nonce);
    let ixs = [front, settle, sell_ix(&f.rm, &a, d)];
    let meta = ok(f.env.send(&ixs, &[&kp]));
    let evs = events::of::<SolSettle>(&meta);
    match (pred.eval.settlement, evs.first()) {
        (None, None) => {}
        (Some(s), Some(ev)) => {
            assert_eq!(ev.condition, s.kind as u8);
            assert_eq!(ev.lender_amount, s.lenders);
            assert_eq!(ev.borrower_amount, s.borrower);
        }
        (p, e) => panic!(
            "the oracle ({p:?}) and the program ({:?}) disagree",
            e.map(|e| e.condition)
        ),
    }
    eprintln!(
        "flip bundle (pump ×{:.2}): {}",
        kb * 1.1,
        evs.first().map_or("no settlement".to_owned(), |e| format!(
            "S{} of {:.6} SOL",
            e.condition,
            e.lender_amount as f64 / 1e9
        ))
    );
    assert!(
        evs.iter().all(|e| e.condition != 1),
        "the flip bundle cannot turn the capped S2 into an uncapped S1"
    );
}

// ---- M10: the §12.6 held model, the OI-cap basket, round 3 on the real pool --------------------

/// `sandwich.attack(m, pos, t, ("liquidation",), "held")` on the real pool (ECON §12.6, the
/// held-model residual, owner item O19): before its sandwich the attacker-keeper holds the pool
/// at `P_ema × (1 + x)` for `h` whole seconds (attacker-only seconds: every observation of each
/// second sees it, no honest crank — the degraded-mode exposure) to raise the reference the fixed
/// bound is computed from, then lands its best `[buy dX, liquidate, sell dX]` the next second,
/// every short.win instruction checked by the oracle. Plans 1 s at +2.5 % and 3 s at +15 % (the
/// sim's RA / A plan). What holds, as on ECON's markets: the spend stays within
/// `⌊B̂_ref·(1 + δ)⌋` of the lifted reference (I11); while the borrower keeps a remainder the
/// lenders' and protocol's legs are at least the honest ones (the penalty grows with the
/// reference); the attacker's extra over an honest keeper — net of its held pushes — comes out of
/// the borrower's remainder only. What the real pool shows: at the per-position cap (`V` ≈ 869
/// SOL) the 3-s plan **pays** — it beats the atomic sandwich by tens of SOL (`-- --nocapture`;
/// `tests/litesvm/README.md`), the residual ECON §12.6 discloses (there: RA +1.52 SOL over an
/// honest keeper on a 37.9-SOL vault), scaled by the position.
#[test]
fn held_pushes_before_a_liquidation_take_from_the_borrower_only() {
    let (mut f, b, nonce) = setup_liq();
    let a = attacker(&mut f);
    let saved = f.env.svm.clone();
    let h = clone_event(f.exit(Which::Liquidate, &b.key(), nonce).liquidated());
    let honest_keeper = i128::from(h.penalty_keeper + h.keeper_topup);
    f.env.svm = saved.clone();
    let dx = best_dx(&mut f, &a, Which::Liquidate, &b.key(), nonce);
    let (_, atomic) = sandwich(&mut f, &a, Which::Liquidate, &b.key(), nonce, dx);
    for (secs, x_bps) in [(1u32, 250u128), (3, 1_500)] {
        f.env.svm = saved.clone();
        let mut pre = 0i128;
        for _ in 0..secs {
            let target = f.market().ema_price.get() * (10_000 + x_bps) / 10_000;
            pre += held_second(&mut f, &a, target);
        }
        f.env.warp(1);
        let dx = best_dx(&mut f, &a, Which::Liquidate, &b.key(), nonce);
        let (ex, pnl) = sandwich(&mut f, &a, Which::Liquidate, &b.key(), nonce, dx);
        let ev = clone_event(ex.liquidated());
        let delta = u128::from(ex.predicted().view.pos.delta_liq_bps);
        assert!(
            u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * (10_000 + delta),
            "I11: the spend within the fixed δ over the reference"
        );
        let (hl, al) = (lenders_take(&h), lenders_take(&ev));
        assert!(
            ev.borrower_payout > 0,
            "the vault still covers the waterfall"
        );
        let penalty = ex.predicted().view.pos.penalty_bps;
        assert_lenders_keep(&h, &ev, penalty, &format!("{secs} s at +{x_bps} bps"));
        let net = pnl - pre;
        let extra = net - honest_keeper;
        let loss = i128::from(h.borrower_payout) - i128::from(ev.borrower_payout);
        eprintln!(
            "held {secs} s at +{:.1} %: push {:.4} SOL, sandwich + keeper {:+.4} SOL, net {:+.4} SOL \
             (atomic sandwich {:+.4}), extra over an honest keeper {:+.4} SOL; B̂_ref {:.4} → {:.4} \
             SOL, lenders + protocol {} → {} lamports, borrower {:.4} → {:.4} SOL",
            x_bps as f64 / 100.0,
            pre as f64 / 1e9,
            pnl as f64 / 1e9,
            net as f64 / 1e9,
            atomic as f64 / 1e9,
            extra as f64 / 1e9,
            h.b_ref as f64 / 1e9,
            ev.b_ref as f64 / 1e9,
            hl,
            al,
            h.borrower_payout as f64 / 1e9,
            ev.borrower_payout as f64 / 1e9
        );
        assert!(
            loss > 0 && extra < loss,
            "the attacker's extra comes out of the borrower's remainder only"
        );
    }
}

/// `ema_atomic_multi.fill` on the real pool: positions at the largest size the caps admit, one a
/// second (a checked open and a keeper second each), until the per-market OI cap leaves no room
/// for another (a further open is refused `CapMarketOi`).
fn oi_basket() -> (Fart, Vec<(Borrower, u64)>) {
    let mut f = Fart::new(fork_env_with(lift_notional), LEND);
    let (x, _) = reserves(&f.env, &f.rm).unwrap();
    let mut basket = Vec::new();
    loop {
        let q = max_open_q(&mut f);
        if q <= x / 50_000 {
            break;
        }
        let (b, nonce, _) = f.open(q);
        basket.push((b, nonce));
        f.second();
        assert!(basket.len() < 20, "the OI cap binds");
    }
    let saved = f.env.svm.clone();
    let mut extra = f.env.borrower();
    let first = f.pos(&basket[0].0, basket[0].1).q;
    let (res, _) = f.env.open(&f.rm, &mut extra, first / 2);
    assert_err(&res, E::CapMarketOi);
    f.env.svm = saved;
    let sum: u64 = basket.iter().map(|(b, n)| f.pos(b, *n).q).sum();
    eprintln!(
        "OI basket: {} positions, ΣQ {:.3} % of X",
        basket.len(),
        sum as f64 * 100.0 / x as f64
    );
    f.tick(60);
    (f, basket)
}

/// `ema_atomic_multi.py` (a), model atomic, on the real pool: the market filled to its OI cap
/// ([`oi_basket`]), then at ×1.5 / ×2 / ×3 the attacker's `[pump, crank_price, dump]` every second
/// next to the honest keeper's crank for 60 s: no basket position is ever liquidatable to the
/// attacker's own final transaction (`would_liq`, the oracle's), and every 20 s its `[pump,
/// liquidate, dump]` on the first and the last position is refused `NotLiquidatable` (oracle-
/// checked).
#[test]
fn atomic_bundles_cannot_force_the_oi_cap_basket() {
    let (mut f, basket) = oi_basket();
    assert!(basket.len() >= 2, "a basket");
    let a = attacker(&mut f);
    let p_true = f.spot();
    let saved = f.env.svm.clone();
    for mult in [1.5, 2.0, 3.0] {
        f.env.svm = saved.clone();
        let target = scale(p_true, mult);
        let mut cost = 0i128;
        for n in 1..=60 {
            cost += atomic_second(&mut f, &a, target);
            for (b, nonce) in &basket {
                let p = predict(&read_view(&f.env, &f.rm, &b.key(), *nonce));
                assert!(
                    !p.would_liq() && !p.cr_liquidatable(),
                    "×{mult}, second {n}"
                );
            }
            if n % 20 == 0 {
                for (b, nonce) in [&basket[0], basket.last().unwrap()] {
                    final_bundle_refused(&mut f, &a, &b.key(), *nonce, target);
                }
            }
        }
        eprintln!(
            "OI basket ×{mult}: 60 atomic seconds cost the attacker {:.3} SOL, nothing forced",
            cost as f64 / 1e9
        );
    }
}

/// `ema_atomic_multi.py` (b), model atomic, on the real pool: a lender-keeper pushes the pool past
/// the basket's lowest S1 boundary (×1.1 / ×1.3 / ×1.6 of it) and calls `sol_settle` on every
/// basket position inside its own pumped transaction each second for 60 s (the honest keeper
/// cranks first in each second): the oracle sees nothing to arm on any position, and nothing arms
/// or settles.
#[test]
fn atomic_bundles_cannot_force_s1_on_the_oi_cap_basket() {
    let (mut f, basket) = oi_basket();
    let kb = basket
        .iter()
        .map(|(b, n)| s1_boundary(&f.env, &f.rm, &f.pos(b, *n)))
        .fold(f64::MAX, f64::min);
    let a = attacker(&mut f);
    let kp = a.owner.insecure_clone();
    let p_true = f.spot();
    let saved = f.env.svm.clone();
    for m in [1.1, 1.3, 1.6] {
        f.env.svm = saved.clone();
        let target = scale(p_true, kb * m);
        for n in 1..=60 {
            f.second();
            let d = u64::try_from(move_size(&f.env, &f.rm, target)).unwrap();
            let front = buy_ix(&f.rm, &a, d);
            let mut ixs = vec![front.clone()];
            for (b, nonce) in &basket {
                let pred = predict_settle(&view_after(
                    &mut f.env,
                    &f.rm,
                    &b.key(),
                    *nonce,
                    std::slice::from_ref(&front),
                    &[&kp],
                ));
                assert!(
                    pred.eval.settlement.is_none() && pred.eval.arm == ArmChange::None,
                    "×{:.2}, second {n}: the oracle sees nothing to arm",
                    kb * m
                );
                ixs.push(sol_settle_ix(&f.env, &f.rm, &kp.pubkey(), &b.key(), *nonce));
            }
            ixs.push(sell_ix(&f.rm, &a, d));
            let meta = ok(f.env.send(&ixs, &[&kp]));
            assert!(
                events::of::<SolSettle>(&meta).is_empty()
                    && events::of::<SettleArmed>(&meta).is_empty(),
                "×{:.2}, second {n}: nothing arms or settles",
                kb * m
            );
        }
        for (b, nonce) in &basket {
            assert_eq!(f.pos(b, *nonce).s_armed, 0);
        }
    }
}

// ---- round 3 (ECON §12.10, §12.11) on the real pool ---------------------------------------------

/// The flip bundle `[pump to mult × spot → sol_settle → dump]` one second later, its settlement
/// predicted by the oracle on the pumped state (checked against the event). Returns the
/// settlement, if any.
fn flip_bundle(f: &mut Fart, a: &Trader, b: &Pubkey, nonce: u64, mult: f64) -> Option<SolSettle> {
    f.env.warp(1);
    let target = scale(f.spot(), mult);
    let d = u64::try_from(move_size(&f.env, &f.rm, target)).unwrap();
    let kp = a.owner.insecure_clone();
    let front = buy_ix(&f.rm, a, d);
    let pred = predict_settle(&view_after(
        &mut f.env,
        &f.rm,
        b,
        nonce,
        std::slice::from_ref(&front),
        &[&kp],
    ));
    let settle = sol_settle_ix(&f.env, &f.rm, &kp.pubkey(), b, nonce);
    let ixs = [front, settle, sell_ix(&f.rm, a, d)];
    let meta = ok(f.env.send(&ixs, &[&kp]));
    let ev = events::of::<SolSettle>(&meta).pop();
    match (pred.eval.settlement, &ev) {
        (None, None) => {}
        (Some(s), Some(ev)) => {
            assert_eq!(ev.condition, s.kind as u8);
            assert_eq!(ev.lender_amount, s.lenders);
            assert_eq!(ev.borrower_amount, s.borrower);
        }
        (p, e) => panic!(
            "the oracle ({p:?}) and the program ({:?}) disagree",
            e.as_ref().map(|e| e.condition)
        ),
    }
    ev
}

/// A frozen second (`refute_push.py` (c), `fv_push.py`'s freeze): the honest keeper cranks, then
/// the attacker's `[move to target, crank_price, move back]` adds an observation at `target`
/// (just above or below `P_ema`: the bucket straddles it). Returns the attacker's cost.
fn frozen_second(f: &mut Fart, a: &Trader, target: u128) -> i128 {
    f.second();
    let d = move_size(&f.env, &f.rm, target);
    let crank = crank_price_ix(&f.rm);
    let ixs = match d {
        0 => vec![crank],
        d if d > 0 => {
            let d = u64::try_from(d).unwrap();
            vec![buy_ix(&f.rm, a, d), crank, sell_ix(&f.rm, a, d)]
        }
        d => {
            let d = u64::try_from(-d).unwrap();
            vec![sell_ix(&f.rm, a, d), crank, buy_ix(&f.rm, a, d)]
        }
    };
    let w0 = wsol_of(&f.env, a);
    let kp = a.owner.insecure_clone();
    ok(f.env.send(&ixs, &[&kp]));
    w0 - wsol_of(&f.env, a)
}

/// `fv_push.py` (R3A-1, R4B-2, R4A-9) on the thin real pool of `raydium_v4_e2e.rs`'s S2 path, one
/// second before the honest S2 (every settlement call the oracle's): the flip bundle fails to
/// make the S2 an uncapped S1 after 2 pushed seconds; after 1 push and 2 / 4 / 6 frozen seconds
/// (push-then-freeze); after 1–4 consecutive attacker-only seconds, and after 2–6 pushes with an
/// honest second between each. Five or six consecutive attacker-only seconds — a censored
/// keeper, degraded mode, the documented limit (R4A-9) — are reported, not required to fail.
/// Whatever settles stays within `fv_dev_bps` of the honest S2.
#[test]
fn round_3_pushes_cannot_flip_the_thin_pool_s2() {
    let ThinPool {
        mut f,
        b,
        nonce,
        pos,
        ..
    } = thin_pool_market();
    let a = attacker(&mut f);
    let (saved, honest) = ready_to_settle(&mut f, &b.key(), nonce);
    assert_eq!(honest.condition, 2);
    let x_h = honest.lender_amount;
    let fv_dev = u128::from(pos.fv_dev_bps);
    let kb = s1_boundary(&f.env, &f.rm, &pos);
    let check = |what: &str, ev: Option<SolSettle>| {
        if let Some(ev) = &ev {
            assert_ne!(ev.condition, 1, "{what}: no S1");
            assert!(
                u128::from(ev.lender_amount.abs_diff(x_h)) * 10_000 <= fv_dev * u128::from(x_h),
                "{what}: within fv_dev_bps"
            );
        }
        eprintln!(
            "{what}: {}",
            ev.map_or("no settlement".to_owned(), |e| format!(
                "S{} of {:.6} SOL (honest {:.6})",
                e.condition,
                e.lender_amount as f64 / 1e9,
                x_h as f64 / 1e9
            ))
        );
    };
    // Two pushed seconds, then the flip bundle.
    f.env.svm = saved.clone();
    for _ in 0..2 {
        let t = scale(f.spot(), 1.3);
        attacker_only_second(&mut f, &a, t);
    }
    let ev = flip_bundle(&mut f, &a, &b.key(), nonce, kb * 1.1);
    check("2 pushed seconds + flip", ev);
    // Push then freeze.
    for k in [2, 4, 6] {
        f.env.svm = saved.clone();
        let t = scale(f.spot(), 1.3);
        attacker_only_second(&mut f, &a, t);
        for _ in 0..k {
            let ema = f.market().ema_price.get();
            frozen_second(&mut f, &a, ema * 10_005 / 10_000);
        }
        let ev = flip_bundle(&mut f, &a, &b.key(), nonce, kb * 1.1);
        check(&format!("1 push + {k} frozen seconds + flip"), ev);
    }
    // n attacker-only seconds, consecutive or with an honest second between each.
    for n in 1..=6u32 {
        for gap in [0u32, 1] {
            if n == 1 && gap == 1 {
                continue;
            }
            f.env.svm = saved.clone();
            for i in 0..n {
                let t = scale(f.spot(), 1.3);
                attacker_only_second(&mut f, &a, t);
                if i + 1 < n {
                    for _ in 0..gap {
                        f.second();
                    }
                }
            }
            let ev = flip_bundle(&mut f, &a, &b.key(), nonce, kb * 1.1);
            let what = format!("{n} pushes, {gap} honest s between");
            if n >= 5 && gap == 0 {
                eprintln!(
                    "{what} (the documented limit): {}",
                    ev.as_ref()
                        .map_or("no settlement".to_owned(), |e| format!("S{}", e.condition))
                );
            } else {
                check(&what, ev);
            }
        }
    }
}

/// The attack scripts' S1 set-up on the real pool (`arming.py`, `refute_push.py`): the capped
/// position, 120 s of honest keeper cranks, then a genuine gap (the whale's real buy) to
/// `beyond` × the S1 boundary. Returns the market, the position and the pre-gap spot and
/// boundary multiple.
fn s1_setup(beyond: f64) -> (Fart, Borrower, u64, u128, f64) {
    let (mut f, b, nonce) = capped_market();
    f.tick(120);
    let kb = s1_boundary(&f.env, &f.rm, &f.pos(&b, nonce));
    let p0 = f.spot();
    f.move_to(scale(p0, kb * beyond));
    (f, b, nonce, p0, kb)
}

/// `mature_soon` of `refute_push.py`: armed, and `lead` more EMA updates complete the clock.
fn mature_soon(f: &Fart, b: &Borrower, nonce: u64, lead: u64) -> bool {
    let p = f.pos(b, nonce);
    p.s_armed == 1 && f.market().ema_count + lead >= p.s_count + u64::from(p.settle_persist_n)
}

/// One keeper second (checked crank and checked `sol_settle`); the settlement, if any.
fn keeper_second(f: &mut Fart, b: &Borrower, nonce: u64) -> Option<SolSettle> {
    f.second();
    f.settle(&b.key(), nonce).ev
}

/// ECON §12.10 on the real pool (`arming.py`, `refute_push.py` (a), (b), (c)), the capped
/// Fartcoin position 5 % beyond its S1 boundary after a genuine gap, the keeper's checked crank
/// and `sol_settle` every second (the oracle predicts the arming clock every second):
/// - honest: S1 after `s_h` seconds;
/// - (V2-4) the borrower's `[dump below the S1 price → sol_settle → buy back]` every 20 s neither
///   disarms nor delays it;
/// - (a) a refutation push — one attacker-only second at ×0.7 one EMA update before maturity,
///   every cycle — delays S1 by at most 4 s (the mock: 2–4 s);
/// - (c) push-then-freeze — one attacker-only ×0.7 second three updates before maturity, then 4
///   frozen seconds just below `P_ema` — by at most 8 s;
/// - (b) a spot-leg veto — from 20 s after arming, one low observation per second below the
///   liquidation price for 9 / 12 / 20 s — keeps the clock running: at most 6 s late.
#[test]
fn round_3_settlement_attacks_delay_s1_by_seconds_only() {
    let honest = {
        let (mut f, b, nonce, ..) = s1_setup(1.05);
        let mut got = None;
        for s in 1..=600u32 {
            if let Some(ev) = keeper_second(&mut f, &b, nonce) {
                got = Some((s, ev));
                break;
            }
        }
        let (s, ev) = got.expect("S1");
        assert_eq!(ev.condition, 1, "the gap settles S1");
        s
    };
    eprintln!("honest S1 after {honest} s");

    // arming.py: a borrower bundle every 20 s.
    {
        let (mut f, b, nonce, p0, kb) = s1_setup(1.05);
        let a = attacker(&mut f);
        let kp = a.owner.insecure_clone();
        let mut bundles = 0;
        let mut got = None;
        for s in 1..=600u32 {
            if let Some(ev) = keeper_second(&mut f, &b, nonce) {
                got = Some((s, ev));
                break;
            }
            if s % 20 == 0 {
                let target = scale(p0, kb * 0.99);
                let d = u64::try_from(-move_size(&f.env, &f.rm, target)).unwrap();
                let settle = sol_settle_ix(&f.env, &f.rm, &kp.pubkey(), &b.key(), nonce);
                let ixs = [sell_ix(&f.rm, &a, d), settle, buy_ix(&f.rm, &a, d)];
                let meta = ok(f.env.send(&ixs, &[&kp]));
                assert!(
                    events::of::<SolSettle>(&meta).is_empty()
                        && events::of::<shortwin::events::SettleDisarmed>(&meta).is_empty(),
                    "the bundle cannot disarm"
                );
                bundles += 1;
            }
        }
        let (s, ev) = got.expect("S1");
        eprintln!("borrower bundle every 20 s ({bundles}): S1 after {s} s");
        assert_eq!((ev.condition, s), (1, honest), "no delay");
    }

    // (a) refutation push and (c) push-then-freeze.
    for (lead, freeze, max_delay) in [(1u64, 0u32, 4u32), (3, 4, 8)] {
        let (mut f, b, nonce, ..) = s1_setup(1.05);
        let a = attacker(&mut f);
        let (mut pushes, mut frozen, mut freeze_left) = (0, 0, 0);
        let mut last = None;
        let mut got = None;
        for s in 1..=600u32 {
            let since = f.pos(&b, nonce).s_since;
            if freeze_left == 0 && mature_soon(&f, &b, nonce, lead) && last != Some(since) {
                last = Some(since);
                let t = scale(f.spot(), 0.7);
                attacker_only_second(&mut f, &a, t);
                pushes += 1;
                freeze_left = freeze;
                continue;
            }
            if freeze_left > 0 {
                let ema = f.market().ema_price.get();
                frozen_second(&mut f, &a, ema * 9_995 / 10_000);
                frozen += 1;
                freeze_left -= 1;
                if let Some(ev) = f.settle(&b.key(), nonce).ev {
                    got = Some((s, ev));
                    break;
                }
                continue;
            }
            if let Some(ev) = keeper_second(&mut f, &b, nonce) {
                got = Some((s, ev));
                break;
            }
        }
        let (s, ev) = got.expect("S1");
        eprintln!(
            "{} ({pushes} attacker-only, {frozen} frozen seconds): S1 after {s} s (honest {honest})",
            if freeze == 0 {
                "refutation push"
            } else {
                "push-then-freeze"
            }
        );
        assert_eq!(ev.condition, 1);
        assert!(
            s <= honest + max_delay,
            "delayed by {} s (at most {max_delay})",
            s - honest
        );
    }

    // (b) the spot-leg veto.
    for w in [9u32, 12, 20] {
        let (mut f, b, nonce, ..) = s1_setup(1.05);
        let a = attacker(&mut f);
        let (mut armed_at, mut blocked) = (None, 0);
        let mut got = None;
        for s in 1..=600u32 {
            if armed_at.is_none() && f.pos(&b, nonce).s_armed == 1 {
                armed_at = Some(s);
            }
            if armed_at.is_some_and(|t| s - t >= 20) && blocked < w {
                f.second();
                let pr = f.predict(&b.key(), nonce);
                let target = liq_price_target(&pr) * 995 / 1_000;
                let d = move_size(&f.env, &f.rm, target);
                assert!(d < 0, "a low observation needs a sale");
                let d = u64::try_from(-d).unwrap();
                let kp = a.owner.insecure_clone();
                let ixs = [
                    sell_ix(&f.rm, &a, d),
                    crank_price_ix(&f.rm),
                    buy_ix(&f.rm, &a, d),
                ];
                ok(f.env.send(&ixs, &[&kp]));
                let out = f.settle(&b.key(), nonce);
                assert!(
                    out.ev.is_none() && out.disarmed.is_none(),
                    "a vetoed second neither settles nor disarms"
                );
                blocked += 1;
                continue;
            }
            if let Some(ev) = keeper_second(&mut f, &b, nonce) {
                got = Some((s, ev));
                break;
            }
        }
        let (s, ev) = got.expect("S1");
        eprintln!("spot-leg veto of {w} s: S1 after {s} s (honest {honest})");
        assert_eq!(ev.condition, 1);
        assert!(
            s <= honest + 6,
            "veto of {w} s: delayed by {} s",
            s - honest
        );
    }
}

/// `block_spot.liq_price_target` on the real pool: the largest spot at which the liquidation spot
/// leg still fails — `CR(V_eff, B̂(q; reprice(L_live, P))) ≥ L` at the position's risk view — by
/// bisection on `P`.
fn liq_price_target(p: &Predicted) -> u128 {
    let pos = &p.view.pos;
    let (x, y) = p.view.xy();
    let l = shortwin_math::quote::liquidity(x, y).unwrap();
    let healthy = |price: u128| {
        p.view
            .risky()
            .buy_repriced(pos.q, l, price)
            .is_some_and(|b| {
                u128::from(p.v_eff) * 10_000 / u128::from(b.cost) >= u128::from(pos.cr_liq_bps)
            })
    };
    let spot = shortwin_math::risk::price_wad(x, y).unwrap();
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
