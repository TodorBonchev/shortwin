//! The PumpSwap clauses of I15 and I18 fire on what a program mutant would leave (M10, G-I15-PSwap
//! and G-I18-PSwap; ARCH §8.3; OWNER-M6D-1, M6R-1, M6E-1). Each case starts from a real, passing
//! instruction on the fork — a PumpSwap price start, a warm-up step, an expiry, an open — and
//! changes one thing: the market after it (its stored `L_ema`, `L_slow`, `pool_d`) or the event.
//! The real instruction passes the clause; each mutant fails it with the clause's own message.
//! Until M10 the evidence for these clauses was only that the pre-M6E-1 build failed
//! `lock::part_3a` / `part_3e` (no durable mutant).
//!
//! | Test | Clause |
//! |---|---|
//! | [`i18_a_a_price_start_anchors_l_ema_at_the_burned_depth`] | (a): a price start writes `L_ema = min(√(X·(Y + VQR)), D)` and `L_slow = min(L_ema, D)` — on `mature` (λ 92.98 %, `D < √k`) an anchor at `√k` fails, in `L_ema` or in `L_slow` |
//! | [`i18_f_the_warm_up_keeps_l_slow_at_or_below_d`] | (f): a warm-up integration leaving `L_slow > pool_d` fails; an instruction without an integration that moves `L_slow` other than to `min(L_slow, pool_d)` fails |
//! | [`i18_d_an_expiry_reference_is_capped_at_d`] | (d): with `L_ema`, `L_slow` at 3× the live depth (a held deposit's EMA) the real expiry's `B̂_ref` (cap `D`, so `L_b = L_live`) passes; the `B̂_ref` at row 57's cap `⌊D·10⁴/pool_lock_min_bps⌋` or uncapped fails |
//! | [`i15_an_open_is_sized_and_tiered_on_the_burned_lp`] | I15 (OWNER-M6D-1): with a third-party deposit of 3× the pool held, a day old, at a SOL/USD price where the live (deposit-inflated) WSOL vault earns Tier B and the burned LP's real SOL Tier C, the real open locks C; an `Open` sized on the live depth fails, and one at the live vault's Tier B fails |

use shortwin::events::{Expire, Open};
use shortwin::state::OracleCache;
use shortwin_litesvm_tests::fixtures;
use shortwin_litesvm_tests::invariants::confirm::ConfirmFacts;
use shortwin_litesvm_tests::invariants::pool_select::PoolFacts;
use shortwin_litesvm_tests::invariants::{self, IxEvents, TxSnapshot, Violation};
use shortwin_litesvm_tests::pamm::PammLp;
use shortwin_math::tier::{self, AGE_C_S, OraclePrice, Tier, TierInputs, TierVenue};
use shortwin_math::{bounds, caps, quote};

use crate::common::*;

/// I18 (the post-change confirmation part alone) on `ev` against `pre`.
fn i18(env: &Env, pre: &TxSnapshot, ev: &IxEvents) -> Result<(), Violation> {
    invariants::confirm::check_ix(
        &env.svm,
        &pre.positions,
        &ConfirmFacts {
            created: &ev.created,
            switched: &ev.switched,
            venue_changed: &ev.venue_changed,
            opens: &ev.opens,
            liquidations: &ev.liquidations,
            expiries: &ev.expiries,
            settlements: &ev.settlements,
            armed: &ev.armed,
            disarmed: &ev.disarmed,
        },
    )
}

/// I15 (the pool-selection part alone) on `ev` against `pre`.
fn i15(env: &Env, pre: &TxSnapshot, ev: &IxEvents) -> Result<(), Violation> {
    invariants::pool_select::check_ix(
        &env.svm,
        &pre.positions,
        &PoolFacts {
            created: &ev.created,
            opens: &ev.opens,
            switched: &ev.switched,
            proposed: &ev.proposed,
            cancelled: &ev.cancelled,
            venue_changed: &ev.venue_changed,
        },
    )
}

/// Asserts `r` is a violation of `invariant` whose message contains `clause`.
#[track_caller]
fn fails(r: Result<(), Violation>, invariant: &str, clause: &str) {
    match r {
        Err(v) if v.invariant == invariant && v.detail.contains(clause) => {}
        other => panic!(
            "expected {invariant} ({clause:?}), got {:?}",
            other.map_err(|v| format!("{v}"))
        ),
    }
}

/// Rewrites the stored market `key` with `f` (a program mutant's state after the instruction).
fn rewrite(env: &mut Env, key: &Pubkey, f: impl FnOnce(&mut Market)) {
    let mut m = env.market(key);
    f(&mut m);
    let extra = lamports(env, key) - env.svm.minimum_balance_for_rent_exemption(Market::SPACE);
    fixtures::set_zc(&mut env.svm, key, &m, extra);
}

/// A copy of an event (Anchor events do not derive `Clone`): a Borsh round trip.
fn dup<T: anchor_lang::AnchorSerialize + anchor_lang::AnchorDeserialize>(e: &T) -> T {
    let mut bytes = Vec::new();
    e.serialize(&mut bytes).expect("serialize");
    T::try_from_slice(&bytes).expect("deserialize")
}

/// `mature` (λ 92.98 %: a third-party LP, so the burned depth `D` is below `√k`) listed with
/// `40·q` lent and **no** crank yet: the next crank is the market's PumpSwap price start.
fn unpriced_mature() -> (Env, PammMarket) {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let q = q_for(&env, &p);
    let curve = pda::bonding_curve(&p.mint);
    if env
        .svm
        .get_account(&curve)
        .is_some_and(|a| a.data.len() < vpump::CURVE_MIN_LEN)
    {
        let payer = env.payer.pubkey();
        ok(env.send(&[pamm::pump_extend_account_ix(&payer, &curve)], &[]));
    }
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, q.saturating_mul(40), 0);
    assert_eq!(market(&env, &pm).ring_count, 0, "no price yet");
    (env, pm)
}

/// One crank of `pm` (after `warp` seconds): the snapshot before it and its events.
fn crank_step(env: &mut Env, pm: &PammMarket, warp: i64) -> (TxSnapshot, IxEvents) {
    env.warp(warp);
    let pre = invariants::snapshot(&env.svm, &[pm.market()]);
    let meta = crank(env, pm);
    (pre, IxEvents::of(&meta, 0))
}

/// The pre-trade venue's `(X, Y + VQR)`, `√` of it and its locked depth `D` (the hooks' view).
fn venue_of(pre: &TxSnapshot) -> ((u64, u64), u128, u128) {
    let v = pre.positions.markets[0].venue.expect("a view");
    let (x, y) = v.reserves.expect("executable");
    let root = quote::liquidity(u128::from(x), u128::from(y)).expect("L");
    (
        (x, y),
        root,
        v.locked_depth.expect("PumpSwap: a locked depth"),
    )
}

/// I18 (a), M6R-1 / M6E-1: a PumpSwap price start anchors the EMA at the burned LP's depth.
#[test]
fn i18_a_a_price_start_anchors_l_ema_at_the_burned_depth() {
    let (mut env, pm) = unpriced_mature();
    let key = pm.market();
    let (pre, ev) = crank_step(&mut env, &pm, 1);
    assert_eq!(i18(&env, &pre, &ev).map_err(|v| v.to_string()), Ok(()));
    let m = market(&env, &pm);
    let (_, root, d) = venue_of(&pre);
    assert!(d < root, "mature: D {d} below √k {root}");
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1), "a restart");
    assert_eq!(
        (m.ema_liq.get(), m.liq_slow.get(), m.pool_d.get()),
        (d, d, d),
        "the anchor and L_slow at D"
    );
    let good = env.svm.get_account(&key).expect("market");
    // The anchor at √k (row 56's `⌊D·10⁴/pool_lock_min_bps⌋` exceeds √k here: the same value),
    // in L_ema, in L_slow, in both.
    type Mutant = fn(&mut Market, u128);
    let mutants: [(&str, Mutant); 3] = [
        ("L_ema at √k", |m, r| m.ema_liq.set(r)),
        ("L_slow at √k", |m, r| m.liq_slow.set(r)),
        ("both at √k", |m, r| {
            m.ema_liq.set(r);
            m.liq_slow.set(r);
        }),
    ];
    for (what, mutate) in mutants {
        rewrite(&mut env, &key, |m| mutate(m, root));
        let r = i18(&env, &pre, &ev);
        assert!(r.is_err(), "{what}");
        fails(r, "I18", "PumpSwap price start");
        env.svm.set_account(key, good.clone()).unwrap();
    }
    assert!(i18(&env, &pre, &ev).is_ok());
    assert!(invariants::check_ix(&env.svm, &pre, false, &ev).is_ok());
}

/// I18 (f), M6R-1: during a PumpSwap warm-up `L_slow ≤ pool_d`, and an instruction without an
/// integration moves `L_slow` only to `min(L_slow, pool_d)`.
#[test]
fn i18_f_the_warm_up_keeps_l_slow_at_or_below_d() {
    let (mut env, pm) = unpriced_mature();
    let key = pm.market();
    crank_step(&mut env, &pm, 1);
    // A warm-up integration (8 → 7).
    let (pre, ev) = crank_step(&mut env, &pm, 1);
    assert_eq!(i18(&env, &pre, &ev).map_err(|v| v.to_string()), Ok(()));
    let m = market(&env, &pm);
    assert_eq!(m.ema_warm, 7);
    assert!(m.liq_slow.get() <= m.pool_d.get());
    let good = env.svm.get_account(&key).expect("market");
    rewrite(&mut env, &key, |m| {
        let slow = m.liq_slow.get();
        m.pool_d.set(slow - 1);
    });
    fails(i18(&env, &pre, &ev), "I18", "above the locked depth");
    env.svm.set_account(key, good.clone()).unwrap();
    // A second crank in the same second: no integration.
    let (pre, ev) = crank_step(&mut env, &pm, 0);
    assert_eq!(
        market(&env, &pm).ema_count,
        pre.positions.markets[0].state.ema_count
    );
    assert_eq!(i18(&env, &pre, &ev).map_err(|v| v.to_string()), Ok(()));
    let good = env.svm.get_account(&key).expect("market");
    // A fall to anything but `pool_d` (a rise above it is the first clause's).
    rewrite(&mut env, &key, |m| {
        let slow = m.liq_slow.get();
        m.liq_slow.set(slow - 1);
    });
    fails(i18(&env, &pre, &ev), "I18", "without an integration");
    env.svm.set_account(key, good).unwrap();
    assert!(i18(&env, &pre, &ev).is_ok());
}

/// I18 (d), M6E-1: a PumpSwap exit's `B̂_ref` reads `L_b = max(L_live, min(max(L_ema', L_slow),
/// D))` — `L_live` on a canonical pool — however high the EMA terms stand. A real expiry on
/// `mature` passes; with `L_ema` and `L_slow` rewritten to 3× the live depth (what a held deposit
/// of twice the pool leaves, part 3 (f)) the same event still passes, and an event whose `B̂_ref`
/// took row 57's cap `⌊D·10⁴/pool_lock_min_bps⌋` (2D at the default 5,000) or none fails.
#[test]
fn i18_d_an_expiry_reference_is_capped_at_d() {
    let (mut env, pm, q) = setup(PammCoin::Mature);
    let key = pm.market();
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let pos = env.position(&pm, &b.key(), nonce);
    env.warp(pos.deadline - env.now());
    crank(&mut env, &pm);
    env.warp(1);
    crank(&mut env, &pm);
    let pre = invariants::snapshot(&env.svm, &[key]);
    let kp = keeper(&mut env);
    let meta = ok(expire(&mut env, &pm, &kp, &b.key(), nonce));
    let ev = IxEvents::of(&meta, 1);
    assert_eq!(ev.expiries.len(), 1);
    assert_eq!(i18(&env, &pre, &ev).map_err(|v| v.to_string()), Ok(()));
    let (_, l_live, d) = venue_of(&pre);
    // The EMA terms a held deposit leaves: 3× the live depth.
    rewrite(&mut env, &key, |m| {
        m.ema_liq.set(3 * l_live);
        m.liq_slow.set(3 * l_live);
    });
    assert_eq!(
        i18(&env, &pre, &ev).map_err(|v| v.to_string()),
        Ok(()),
        "the cap D: L_b = L_live"
    );
    let post = market(&env, &pm);
    let obs = post.observation();
    let p_ref = bounds::expiry_reference(
        &obs.ema,
        &obs.ring,
        &obs.last,
        env.now(),
        &pos.exit_params(),
    )
    .expect("P_ref")
    .expect("a fresh bucket");
    let pricing = pre.positions.markets[0]
        .venue
        .and_then(|v| v.pricing)
        .expect("pricing")
        .capped(u64::from(pos.venue_fee_hard_cap_bps));
    let b_ref_at = |cap: Option<u128>| {
        let l_b = bounds::ref_liquidity(l_live, &obs.ema, &obs.ring, cap).expect("L_b");
        pricing.buy_repriced(pos.q, l_b, p_ref).expect("B_ref").cost
    };
    let real = ev.expiries[0].b_ref;
    assert_eq!(
        b_ref_at(Some(d)),
        real,
        "the program's B_ref is the cap-D one"
    );
    let lam = u128::from(env.config().params.pool_lock_min_bps);
    for (what, cap) in [
        (
            "row 57's cap ⌊D·10⁴/pool_lock_min_bps⌋",
            Some(d * 10_000 / lam),
        ),
        ("no cap", None),
    ] {
        let b_ref = b_ref_at(cap);
        assert!(
            b_ref < real,
            "{what}: a deeper reference quotes less ({b_ref} vs {real})"
        );
        let mut e: Expire = dup(&ev.expiries[0]);
        e.b_ref = b_ref;
        let mutant = IxEvents {
            expiries: vec![e],
            ..IxEvents::default()
        };
        fails(i18(&env, &pre, &mutant), "I18", "Expire");
    }
}

/// The SOL/USD cache at `price` (Pyth units, `expo = −8`, no confidence), published now — what
/// `crank_sol_usd` writes (as `Env::fresh_oracle`, at a chosen price).
fn cache_usd(env: &mut Env, price: i64) {
    let mut c = env.oracle_cache();
    c.price = price;
    c.conf = 0;
    c.expo = -8;
    c.publish_time = env.now();
    c.updated_slot = env.clock().slot;
    fixtures::set_borsh(
        &mut env.svm,
        &pda::oracle_cache(),
        &c,
        OracleCache::SPACE,
        0,
    );
}

/// I15, OWNER-M6D-1 (ARCH row 57, A82; ECON #106): a PumpSwap open is sized and tiered on the
/// burned LP alone. With a third-party deposit of 3× the pool held (λ falls to ≈ 23 %, `D`
/// stays), a market a day old and the SOL/USD price cached a little above `θ_B`'s threshold for
/// the live (deposit-inflated) WSOL vault — so the live vault earns Tier B and the burned LP's real
/// SOL `max(⌊D·isqrt(P_ema)/10⁹⌋ − VQR⁺, 0)` Tier C —, a real open passes at Tier C; an `Open`
/// sized at the per-position cap on the live `X`, and one at the live vault's Tier B, fail.
#[test]
fn i15_an_open_is_sized_and_tiered_on_the_burned_lp() {
    let (mut env, pm, q) = setup(PammCoin::Mature);
    let key = pm.market();
    let lp = PammLp::funded_for(&mut env, &pm.pool, 30_000);
    lp.deposit(&mut env, &pm.pool, 30_000);
    // Age the market past Tier C's day (O13: its age runs from the listing).
    let listed = market(&env, &pm).graduated_at;
    env.warp(listed + AGE_C_S + 60 - env.now());
    crank(&mut env, &pm);
    crank_secs(&mut env, &pm, 2);
    let mut b = env.borrower();
    // The oracle: 5 % above the least price at which the live vault's `Λ = 2·R_now` reaches θ_B.
    let r_live = pm.pool.reserves(&env.svm).expect("reserves").real_quote;
    let params = env.config().params;
    let lambda_live = 2 * u128::from(r_live);
    let p_b = u128::from(params.theta_b_usd_e6) * 100_000_000_000;
    let price = i64::try_from(p_b.div_ceil(lambda_live) * 21 / 20).expect("price");
    cache_usd(&mut env, price);
    let p_ema = market(&env, &pm).ema_price.get();
    let pre = invariants::snapshot(&env.svm, &[key]);
    let (open, _, meta) = open_ok(&mut env, &pm, &mut b, q);
    let last = meta.inner_instructions.len() - 1;
    let ev = IxEvents::of(&meta, last);
    assert_eq!(ev.opens.len(), 1);
    assert_eq!(i15(&env, &pre, &ev).map_err(|v| v.to_string()), Ok(()));
    assert_eq!(market(&env, &pm).open_positions, 1, "the open landed");
    // Sized on the live depth: the per-position cap on X_now.
    let v = pre.positions.markets[0].venue.expect("a view");
    let (x, _) = v.reserves.expect("executable");
    let d = v.locked_depth.expect("D");
    let config = env.config();
    let tp = config.tiers[usize::from(open.tier)];
    let x0 = caps::conservative_x0(
        x,
        market(&env, &pm).liq_slow.get(),
        p_ema,
        Some((d, 10_000)),
    )
    .expect("X0");
    assert!(
        x0.saturating_mul(3) < x,
        "the deposit inflates X: X0 {x0}, X {x}"
    );
    let cap_on = |x: u64| {
        let impact = caps::max_q_position(x, tp.impact_pos_bps).expect("impact");
        let fee = caps::fee_size_cap(x, v.cap_fee_ppm, config.params.fee_size_cap_x100)
            .expect("fee cap")
            .unwrap_or(u64::MAX);
        impact.min(fee)
    };
    let live = cap_on(x);
    assert!(
        live > cap_on(x0) && open.q <= cap_on(x0),
        "live cap {live}, burned {}",
        cap_on(x0)
    );
    let mut e: Open = dup(&open);
    e.q = live;
    let sized = IxEvents {
        opens: vec![e],
        ..IxEvents::default()
    };
    fails(i15(&env, &pre, &sized), "I15", "per-position cap");
    // The tier: the live vault earns B at this price, the burned LP's real SOL C — and the real
    // open locked C.
    let vqr = u128::try_from(pm.pool.state(&env.svm).virtual_quote_reserves.max(0)).expect("VQR");
    let burned = (d * p_ema.isqrt() / 1_000_000_000).saturating_sub(vqr);
    let tier_of = |r: u128| {
        let r = u64::try_from(r).expect("u64");
        tier::assign(&TierInputs {
            venue: TierVenue::Unlocked,
            r_now: r,
            y_slow: u128::from(r),
            vqr: 0,
            p_ema,
            now: env.now(),
            graduated_at: listed,
            oracle: OraclePrice {
                price,
                conf: 0,
                expo: -8,
                publish_time: env.now(),
            },
            oracle_stale_s: params.oracle_stale_s,
            theta_a_usd_e6: params.theta_a_usd_e6,
            theta_b_usd_e6: params.theta_b_usd_e6,
        })
    };
    assert_eq!(
        (tier_of(u128::from(r_live)), tier_of(burned)),
        (Tier::B, Tier::C),
        "at ${price}e-8: the live vault {r_live} B, the burned LP's real SOL {burned} C"
    );
    assert_eq!(
        open.tier,
        Tier::C as u8,
        "the open is tiered on the burned LP"
    );
    let mut e: Open = dup(&open);
    e.tier = Tier::B as u8;
    let tiered = IxEvents {
        opens: vec![e],
        ..IxEvents::default()
    };
    fails(i15(&env, &pre, &tiered), "I15", "better than the burned LP");
    eprintln!(
        "I15 PumpSwap: R_now {r_live} lamports (Tier B), the burned LP's real SOL {burned} (Tier \
         C) at {price}·10⁻⁸ USD/SOL"
    );
}
