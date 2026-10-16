//! Fork E2E through a **real graduation** (M6; ARCH §2.5 "On → PumpSwap", §5.3, §5.4, §8.2
//! T-VENUE-SWITCH / T-STRADDLE, §8.4 "fork E2E", §9 M6 row; ECON §10.1–§10.3): the release
//! short.win on the **dumped** pump, pump-fees and PumpSwap ELFs, a curve market listed with
//! `create_market(Auto)` on ECON's fresh fixture curve (`Sim::d_market`), taken through the
//! curve's completion, pump's own `migrate_v2`, the keeper's crank moving it `→ PumpSwap`, the
//! post-change warm-up, pump's boost (the real `boost_buy_and_burn`, the boost authority's
//! signature forged: `graduation_support.rs`) and the keeper's exits on the pool — driven only by
//! real instructions, with the invariant hooks after every one (`Env::send`).
//!
//! | Test | Path |
//! |---|---|
//! | [`lifecycle_through_a_real_graduation`] | two lenders; A at 80 % (ECON's D@80 % position), C and B at 90 %; another user completes the curve → `PumpMigrating` (exits and opens `VenueMigrating`) → `migrate_v2` → `→ PumpSwap` (the pool cache, `graduated_at`, the ring at `lp_supply`, `(8, 1)`, tier C) → the warm-up: keeper liquidations `EmaWarmingUp`, `sol_settle` evaluates nothing, opens `EmaWarmingUp`, B's borrower closes on the pool at its quote → A liquidated on the pool at the gate, leg by leg against the pool's quote → opens `BoostInProgress` → the boost's push liquidates C (ECON §10.1) → the boost vault empties (its ATA closed), opens resume: D opens on the pool, is held to its deadline with hourly cranks and expired there → both lenders exit: tokens back, SOL income pro rata |
//! | [`t_venue_switch_the_graduation_rewrites_the_ring_and_the_box_is_narrow_from_the_gate`] | ★T-VENUE-SWITCH (graduation): every ring entry's `l`, `llo`, `lhi` = the pool's `lp_supply`, `(x, y) = reprice(lp_supply, p)`, prices and times kept, `L_slow ≤` the anchor, the bucket and both chains cleared; with the boost running and the keeper cranking every second, `L_ema` at the gate = the pool's `√(X·Y)` within 1 % and every second with the settlement box wider than 1 % in `L` lies inside the gate |
//! | [`the_atomic_self_financed_complete_and_migrate_is_liquidated_on_the_pool`] | M5-SEC-4's attack, end to end: on ECON's D@80 % market a borrower long `q` tokens (bought before its short) and short `q/2` sends `[buy_v2(rT), migrate_v2]` in one transaction (self-financed: it keeps the `rT` tokens); other users push the pool to ≥ 3.5× the last curve EMA before any observation; the keeper's first crank moves the market onto the pool with `P_ema` carried over, its liquidations wait for the eight integrations, and the position is liquidated on the pool at the pool's price — the lenders get `Q` back plus SOL (in M5 the market froze and S3 paid FV at the frozen EMA). The attacker's net is printed |
//! | [`m6_sec_1_a_low_recorded_in_the_warm_up_after_the_atomic_attack_is_bounded`] | M6-SEC-1, the security review's reproducer (★T-SEED-FALL; the owner's decision of 2026-09-30): the same attack, and in post-change second 1 / 4 / 8 another `[sell to 0.1× the pool, crank_price, buy back]` next to the keeper's crank; the warm-up seed falls at most one v0.4.1 step per integration (at the gate exactly the fall bound from the low's second, 0.128× / 0.312× / 0.819× the pool, above the low; as built the low itself, 0.100×), and the position is still liquidated on the pool, the lenders' `Q` back plus SOL, 23 / 17 / 12 s after the transition (undisturbed 9 s; as built, by the review's estimate `ln(P_pool/P_low)/ln(7/6)` s more, ≈ 24 s each) |
//! | [`econ_10_2_the_borrowers_close_of_a_straddle_completes_the_curve`] | ECON §10.2 "rt → Q/2, borrower closes 30 s later" (D@97 %) on pump's v3 (pump-final P2): the curve is bought down to `rT = Q/2`, `B̂_v3` keeps the position healthy, and the borrower's own close completes the curve in one instruction (synthetic migration; `PumpMigrating` in its events): `B − growth` = pump's charge `≤ B̂_v3 ≤` charge + 7, below the retired bundle's 1.9227 SOL; the borrower gets `V − B − R`. (The keeper's bundle for M5-SEC-4's self-made straddle is retired: `synthetic_exit_it`.) Since pump-final §12's sim work (P7) also the exact sim differential: `B`, the rent `R_min`, the payout and `pcB` / `pcQ` equal the normative sim's synthetic close to the lamport (1,883,736,744 / 720,630 / 4,603,772,566) |
//! | [`a_completing_close_then_the_graduation_carries_the_other_position`] | ECON §10.2 `q = rt` (D@97 %): other users buy the curve down to one borrower's `q`, its close buys exactly `rT` at pump's charge and completes the curve (`VenueStateChanged → PumpMigrating` in the close itself, ARCH §5.4 case 1), the other position's close pauses (`VenueMigrating`); `migrate_v2`, the transition, and the other position closes on the pool during the warm-up at the pool's quote; every lent token back |
//! | [`the_boost_gate_on_a_real_graduation`] | ARCH §5.4 case 3 (A13) on a pool `migrate_v2` just created: the whole boost (the sim's 40 chunks every 9 s) moves the price ×(1 + 0.2069)² = ×1.4566 exactly as ECON §10.1 says; opens refuse `BoostInProgress` while the vault holds more than `boost_dust_lamports` (343 refusals), resume once the last chunk has closed the vault's ATA (and the EMA has followed the push); a donor who re-creates the ATA and parks 1 SOL there blocks opens only until `graduated_at + boost_window_s` |

#![allow(clippy::result_large_err)]

#[path = "graduation_support.rs"]
mod support;

use shortwin_math::{quote, risk, settle};
use support::*;

const D80_SOLD: u64 = 634_480_000_000_000;
const D80_Q: u64 = 6_529_192_912_424;
const D90_Q: u64 = 5_348_333_909_677;
const D97_SOLD: u64 = 769_307_000_000_000;
const D97_Q: u64 = 4_521_732_607_755;

// ---- helpers --------------------------------------------------------------------------------------

/// `n` seconds, each: the boost chunks due, then the keeper's crank.
fn crank_secs(g: &mut Grad, n: i64) {
    for _ in 0..n {
        g.s.env.warp(1);
        g.tick_boost();
        g.crank();
    }
}

/// The keeper's liquidation of `p` on the market's current venue.
fn liquidate(g: &mut Grad, p: &Pos) -> TxResult {
    let kp = g.s.keeper.insecure_clone();
    let ix = match &g.pam {
        Some(pam) => liquidate_ix(pam, &kp.pubkey(), &p.key(), p.nonce),
        None => liquidate_ix(&g.s.pm, &kp.pubkey(), &p.key(), p.nonce),
    };
    g.s.env
        .send(&[pump::cu_limit_ix(POOL_KEEPER_EXIT_CU), ix], &[&kp])
}

/// The keeper's `sol_settle` of `p` on the pool: the `SolSettle` event if it settled.
fn settle_on_pool(g: &mut Grad, p: &Pos) -> Option<SolSettle> {
    let kp = g.s.keeper.insecure_clone();
    let ix = sol_settle_ix(&g.s.env, g.pam(), &kp.pubkey(), &p.key(), p.nonce);
    let meta = ok(g.s.env.send(&[ix], &[&kp]));
    events::of::<SolSettle>(&meta).pop()
}

/// The execution quote `B̂_live(q)` of the market's venue now (the harness's own view).
fn quote_buy_now(g: &Grad, q: u64) -> u64 {
    let m = market(&g.s.env, &g.s.pm);
    venue_view::view(&g.s.env.svm, &m)
        .expect("a view")
        .buy(q)
        .expect("a quote")
}

/// The exact-in sale quote `S(q)` of the market's venue now.
fn quote_sell_now(g: &Grad, q: u64) -> u64 {
    let m = market(&g.s.env, &g.s.pm);
    venue_view::view(&g.s.env.svm, &m)
        .expect("a view")
        .sell(q)
        .expect("a quote")
}

/// Liquidates `p` on the pool within `max_s` seconds of per-second cranks: every refusal before
/// it is the one the keeper's view predicts (`EmaWarmingUp` while warming up, else
/// `NotLiquidatable` while not CR-liquidatable); the liquidation is checked leg by leg against the
/// pool's quote. Returns the seconds waited and the event.
fn liquidate_on_pool_within(g: &mut Grad, p: &Pos, max_s: i64) -> (i64, Liquidate) {
    let (sec, ev, _) = liquidate_on_pool_within_low(g, p, max_s, None);
    (sec, ev)
}

/// What [`liquidate_on_pool_within_low`] saw of the warm-up seed: `P_ema` after the keeper's crank
/// in the low's second, and at the gate (the crank that left `ema_warm = 0`).
#[derive(Clone, Copy, Debug, Default)]
struct Seed {
    at_low: u128,
    at_gate: u128,
}

/// [`liquidate_on_pool_within`] with, optionally, M6-SEC-1's attack in second `k` after the
/// transition — `[sell to f_bps/10⁴ × the pool's spot, crank_price, buy back]` right after the
/// keeper's crank (`Grad::recorded_low`) — and the warm-up seed it saw.
fn liquidate_on_pool_within_low(
    g: &mut Grad,
    p: &Pos,
    max_s: i64,
    low: Option<(i64, u128)>,
) -> (i64, Liquidate, Seed) {
    let pos = g.s.position(p);
    let mut seed = Seed::default();
    for sec in 1..=max_s {
        g.s.env.warp(1);
        g.tick_boost();
        g.crank();
        let m = market(&g.s.env, &g.s.pm);
        if m.ema_warm == 0 && seed.at_gate == 0 {
            seed.at_gate = m.ema_price.get();
        }
        if let Some((k, f)) = low
            && sec == k
        {
            seed.at_low = m.ema_price.get();
            g.recorded_low(f);
        }
        let st = g.status(p);
        let warm = market(&g.s.env, &g.s.pm).ema_warm;
        let b_exec = st.b_live;
        let vault0 = tokens(&g.s.env, &g.s.pm.keys.pool_vault);
        let res = liquidate(g, p);
        if warm > 0 {
            assert_err(&res, E::EmaWarmingUp);
            continue;
        }
        if !st.cr_liq() {
            assert_err(&res, E::NotLiquidatable);
            continue;
        }
        if res.is_err() {
            // The fixed δ over the lagging reference: the live cost is above the bound while
            // the pool runs ahead of `P_ref` (the boost's push); the keeper retries next second.
            assert_err(&res, E::ExitBoundNotMet);
            continue;
        }
        let meta = ok(res);
        let ev: Liquidate = events::one(&meta);
        let back = tokens(&g.s.env, &g.s.pm.keys.pool_vault) - vault0;
        check_pool_liquidation(&ev, &pos, b_exec.expect("a quote"), back);
        return (sec, ev, seed);
    }
    panic!("not liquidated on the pool within {max_s} s");
}

/// A second lender holding `amount` tokens deposits them.
fn second_lender(g: &mut Grad, amount: u64) -> (Keypair, ix::LenderKeys) {
    let l2 = g.s.env.funded_keypair();
    let acc = fork::fund_token(
        &mut g.s.env,
        &l2.pubkey(),
        &g.s.pm.keys.mint,
        g.s.pm.curve.token_program,
        amount,
    );
    let keys = ix::LenderKeys {
        owner: l2.pubkey(),
        mint: g.s.pm.keys.mint,
        token_program: g.s.pm.keys.token_program,
        owner_token_account: acc,
    };
    ok(g.s.env.send(&[ix::deposit(&keys, amount, 0)], &[&l2]));
    (l2, keys)
}

/// A new borrower's open of `q` on the market's current venue.
fn try_open(g: &mut Grad, q: u64) -> TxResult {
    let mut b = g.s.env.borrower();
    match &g.pam {
        Some(pam) => g.s.env.open(pam, &mut b, q).0,
        None => g.s.env.open(&g.s.pm, &mut b, q).0,
    }
}

/// An open on the pool that must succeed.
fn open_on_pool(g: &mut Grad, q: u64) -> Pos {
    let mut b = g.s.env.borrower();
    let s_exp = quote_sell_now(g, q);
    let (res, nonce) = g.s.env.open(g.pam.as_ref().expect("graduated"), &mut b, q);
    let meta = ok(res);
    let open: Open = events::one(&meta);
    assert_eq!(open.venue, VenueKind::PumpSwap as u8);
    assert_eq!(open.s, s_exp, "S = the pool's sale quote");
    Pos { b, nonce, open }
}

/// The market state right before the transition crank and the pool's `lp_supply` after it.
struct Transition {
    before: Market,
    after: Market,
    anchor: u128,
}

/// `migrate_v2` by a third party and the keeper's crank that moves the market `→ PumpSwap`
/// (checked: the event, `graduated_at`, the pool cache, the restart, the chains, tier C).
fn migrate_and_transition(g: &mut Grad) -> Transition {
    g.migrate();
    let before = market(&g.s.env, &g.s.pm);
    let meta = g.crank();
    let from = before.venue_kind().expect("venue");
    venue_changed(&meta, from, VenueKind::PumpSwap);
    let after = market(&g.s.env, &g.s.pm);
    let pool = PammPool::of(&g.s.env.svm, &g.s.pm.keys.mint);
    let anchor = u128::from(pool.state(&g.s.env.svm).lp_supply);
    assert_eq!(
        after.graduated_at,
        g.s.env.now(),
        "graduated_at = now (O13)"
    );
    assert_eq!(
        (after.ema_warm, after.ema_guard),
        (8, 1),
        "the post-change restart"
    );
    assert_eq!(after.ema_liq.get(), anchor, "L_ema = lp_supply");
    assert_eq!(
        after.liq_slow.get(),
        before.liq_slow.get().min(anchor),
        "L_slow = min(L_slow, anchor)"
    );
    assert_eq!(
        (after.no_route_since, after.pool_fail_since),
        (0, 0),
        "the chains cleared"
    );
    assert_eq!(
        after.tier,
        shortwin_math::tier::Tier::C as u8,
        "tier C until the next crank"
    );
    assert!(
        after.pool_cache.contains(&pool.base_vault) && after.pool_cache.contains(&pool.quote_vault),
        "the pool cache names the vaults"
    );
    Transition {
        before,
        after,
        anchor,
    }
}

// ---- the lifecycle ------------------------------------------------------------------------------

/// The lifecycle through a real graduation (module docs).
#[test]
fn lifecycle_through_a_real_graduation() {
    let lend1 = 20_000_000_000_000u64;
    let mut g = Grad::d_market(D80_SOLD, lend1);
    let (l2, l2_keys) = second_lender(&mut g, lend1 / 4);

    // ---- positions on the curve: A at 80 %, C and B at 90 % ----
    let a = g.s.open(D80_Q);
    assert_eq!(a.open.venue, VenueKind::PumpCurve as u8);
    crank_secs(&mut g, 30);
    g.s.crowd_buy(SOLD_D90 - D80_SOLD);
    crank_secs(&mut g, 30);
    let c = g.s.open(D90_Q);
    let b = g.s.open(D90_Q / 4);
    crank_secs(&mut g, 10);
    assert!(!g.status(&a).cr_liq(), "A is healthy at 90 %");

    // ---- another user completes the curve: PumpMigrating, exits and opens pause ----
    g.s.env.warp(1);
    g.complete_curve();
    let meta = g.crank();
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpMigrating);
    assert_err(&g.s.env.close(&g.s.pm, &b.b, b.nonce), E::VenueMigrating);
    assert_err(&liquidate(&mut g, &a), E::VenueMigrating);
    assert_err(&try_open(&mut g, D90_Q / 10), E::VenueMigrating);
    crank_secs(&mut g, 1);

    // ---- migrate_v2 two seconds after the completion, the keeper's crank → PumpSwap ----
    g.s.env.warp(1);
    let tr = migrate_and_transition(&mut g);
    assert!(g.graduated(), "the driver follows the market onto its pool");
    let t_grad = g.s.env.now();
    eprintln!(
        "→ PumpSwap: anchor lp_supply {} (curve L_ema was {})",
        tr.anchor,
        tr.before.ema_liq.get()
    );

    // ---- the warm-up: keeper exits and settlements wait, opens wait, B's borrower closes ----
    let mut b_closed = None;
    while market(&g.s.env, &g.s.pm).ema_warm > 0 {
        g.s.env.warp(1);
        g.tick_boost();
        g.crank();
        if market(&g.s.env, &g.s.pm).ema_warm == 0 {
            break;
        }
        assert_err(&liquidate(&mut g, &a), E::EmaWarmingUp);
        assert!(settle_on_pool(&mut g, &a).is_none(), "nothing evaluated");
        assert_err(&try_open(&mut g, D90_Q / 10), E::EmaWarmingUp);
        if b_closed.is_none() {
            let b_exp = quote_buy_now(&g, b.open.q);
            let meta = ok(g
                .s
                .env
                .close(g.pam.as_ref().expect("graduated"), &b.b, b.nonce));
            let close: Close = events::one(&meta);
            assert_eq!(
                close.buyback_cost, b_exp,
                "B's close buys on the pool at its quote"
            );
            b_closed = Some(close);
        }
    }
    let close_b = b_closed.expect("B closed during the warm-up");
    assert!(
        g.s.env.now() - t_grad <= 9,
        "the gate opens with the eighth integration"
    );
    eprintln!(
        "B closed on the pool during the warm-up: B {:.4} SOL, payout {:.4}",
        close_b.buyback_cost as f64 / 1e9,
        close_b.borrower_payout as f64 / 1e9
    );

    // ---- A (the completion made it liquidatable) is liquidated on the pool at the gate ----
    let tokens_free0 = market(&g.s.env, &g.s.pm).tokens_free;
    let (sec, liq_a) = liquidate_on_pool_within(&mut g, &a, 30);
    eprintln!(
        "A liquidated on the pool {} s after the transition: B {:.4}, penalty {:.4}",
        g.s.env.now() - t_grad,
        liq_a.buyback_cost as f64 / 1e9,
        liq_a.penalty as f64 / 1e9
    );
    assert!(sec <= 5, "A goes at the gate");
    assert_eq!(
        market(&g.s.env, &g.s.pm).tokens_free,
        tokens_free0 + D80_Q,
        "A's tokens back to the lenders"
    );

    // ---- the boost is running: opens refuse BoostInProgress; its push liquidates C ----
    assert!(g.pam().pool.boost_amount(&g.s.env.svm) > g.s.env.config().params.boost_dust_lamports);
    assert_err(&try_open(&mut g, D90_Q / 10), E::BoostInProgress);
    let (_, liq_c) = liquidate_on_pool_within(&mut g, &c, 600);
    let t_c = g.s.env.now() - t_grad;
    eprintln!(
        "C liquidated by the boost push {t_c} s after the transition ({} boost calls so far)",
        g.boost.expect("boost").calls
    );
    assert!(t_c > 60, "C survived the completion and the gate");

    // ---- the boost ends: its vault's ATA is closed and opens resume ----
    while g.pam().pool.boost_amount(&g.s.env.svm) > 0 {
        crank_secs(&mut g, 1);
    }
    let boost_ata = g.pam().pool.boost_vault_ata;
    assert!(
        read::account(&g.s.env.svm, &boost_ata).is_none(),
        "the last chunk closes the boost vault's ATA"
    );
    assert!(
        g.s.env.now() < t_grad + i64::from(g.s.env.config().params.boost_window_s),
        "before the window's end"
    );
    crank_secs(&mut g, 60);
    let q_d = {
        let (x, y) = pool_xy(&g.s.env, &g.pam().pool);
        u64::try_from((x / 100).min(x * u128::from(5 * SOL) / y)).unwrap()
    };
    let d = open_on_pool(&mut g, q_d);
    eprintln!(
        "D opens on the pool {} s after the transition: tier {}, q {q_d}",
        g.s.env.now() - t_grad,
        d.open.tier
    );

    // ---- D is held to its deadline (hourly cranks) and expired on the pool ----
    let deadline = g.s.position(&d).deadline;
    while g.s.env.now() + 3_600 < deadline - 60 {
        g.s.env.warp(3_600);
        g.crank();
    }
    let rest = deadline - 60 - g.s.env.now();
    g.s.env.warp(rest);
    while g.s.env.now() < deadline {
        crank_secs(&mut g, 1);
    }
    let st = g.status(&d);
    assert!(st.expired && !st.cr_liq(), "expired, not CR-liquidatable");
    assert_err(&liquidate(&mut g, &d), E::UseExpire);
    let pos_d = g.s.position(&d);
    let b_exec = quote_buy_now(&g, pos_d.q);
    let kp = g.s.keeper.insecure_clone();
    let ix = expire_ix(g.pam(), &kp.pubkey(), &d.key(), d.nonce);
    let meta = ok(g
        .s
        .env
        .send(&[pump::cu_limit_ix(POOL_KEEPER_EXIT_CU), ix], &[&kp]));
    let e: Expire = events::one(&meta);
    assert_eq!(e.buyback_cost, b_exec, "B = the pool's quote");
    assert_eq!(e.rent, st.r_due, "the whole term's rent");
    let base = e.b_live_risk.min(e.b_ref);
    let left = pos_d.v - e.buyback_cost - e.rent;
    let fee = ((u128::from(base) * u128::from(g.s.env.config().params.expiry_fee_bps))
        .div_ceil(10_000) as u64)
        .min(left);
    assert_eq!(
        e.expiry_fee, fee,
        "the expiry fee on min(B̂_live,risk, B̂_ref)"
    );
    let k_e = risk::k_e(e.b_ref, &pos_d.expiry_keeper_terms()).unwrap();
    assert_eq!(e.keeper_reward, k_e.min(left - fee));
    assert_eq!(e.borrower_payout, left - fee - e.keeper_reward);
    let m = market(&g.s.env, &g.s.pm);
    assert_eq!((m.open_positions, m.borrowed_q), (0, 0), "nothing borrowed");

    // ---- both lenders exit once everything vested: tokens back, SOL income pro rata ----
    let params = g.s.env.config().params;
    g.s.env
        .warp(i64::from(params.lock_grace_s).max(i64::from(params.lump_vest_s)) + 86_400);
    g.crank();
    let market_key = g.s.pm.market();
    let l1 = g.s.pm.lender.insecure_clone();
    let l1_keys = g.s.pm.lender_keys();
    let (t1, sol1) = withdraw_all(&mut g.s.env, &market_key, &l1_keys, &l1);
    let (t2, sol2) = withdraw_all(&mut g.s.env, &market_key, &l2_keys, &l2);
    assert!(
        t1 + 2 >= lend1 && t2 + 2 >= lend1 / 4,
        "the tokens come back (dust aside): {t1} {t2}"
    );
    let income = liq_a.penalty_lender + liq_c.penalty_lender + e.expiry_fee;
    assert!(
        sol1 + sol2 > income,
        "rent, penalties and the expiry fee reach the lenders"
    );
    let ratio = sol1 as f64 / sol2 as f64;
    assert!((ratio - 4.0).abs() < 0.01, "pro rata to shares: {ratio}");
    eprintln!(
        "lenders: {t1} + {t2} tokens back, {:.6} + {:.6} SOL of income",
        sol1 as f64 / 1e9,
        sol2 as f64 / 1e9
    );
}

// ---- T-VENUE-SWITCH (graduation) ------------------------------------------------------------

/// ★T-VENUE-SWITCH (ARCH §8.2) for the graduation, as built (module docs).
#[test]
fn t_venue_switch_the_graduation_rewrites_the_ring_and_the_box_is_narrow_from_the_gate() {
    let mut g = Grad::d_market(D80_SOLD, lend_for_80(D80_Q));
    let _a = g.s.open(D80_Q);
    crank_secs(&mut g, 30);
    g.s.env.warp(1);
    g.complete_curve();
    g.crank();
    crank_secs(&mut g, 1);
    g.s.env.warp(1);
    let tr = migrate_and_transition(&mut g);
    let (before, after, anchor) = (tr.before.observation(), tr.after.observation(), tr.anchor);
    // The transitioning crank rolled the curve's last bucket first (the prologue's order), then
    // rewrote every entry.
    assert_eq!(after.ring.len(), (before.ring.len() + 1).min(8));
    let old: Vec<_> = before.ring.iter().copied().collect();
    for (i, e) in after.ring.iter().enumerate() {
        assert_eq!(
            (e.l, e.llo, e.lhi),
            (anchor, anchor, anchor),
            "entry {i}: the liquidity rewritten to the anchor"
        );
        assert_eq!(
            (e.x, e.y),
            quote::reprice(anchor, e.p).unwrap_or((0, 0)),
            "entry {i}: (x, y) = reprice(anchor, p)"
        );
        if i > 0 {
            let o = &old[i - 1];
            assert_eq!(
                (e.ts, e.p, e.lo, e.hi),
                (o.ts, o.p, o.lo, o.hi),
                "entry {i}: price, bucket extremes and time kept"
            );
        }
    }
    assert_eq!(tr.after.b_sec, 0, "the open bucket cleared");
    assert_eq!(
        tr.after.ema_price.get(),
        after.ring.newest().unwrap().p,
        "P_ema carried over"
    );

    // The pool trades on (the boost), the keeper cranks every second.
    let t_grad = g.s.env.now();
    let mut gate_at = None;
    let mut wide_in_gate = 0;
    for sec in 1..=60 {
        crank_secs(&mut g, 1);
        let m = market(&g.s.env, &g.s.pm);
        let o = m.observation();
        let bx = settle::box_of(&o.ema, &o.ring, g.s.env.now(), true).unwrap();
        let wide = (bx.l_hi - bx.l_lo) * 10_000 > 100 * bx.l_lo;
        if gate_at.is_none() {
            if m.ema_warm == 0 {
                gate_at = Some(sec);
                let (x, y) = pool_xy(&g.s.env, &g.pam().pool);
                let l_pool = quote::liquidity(x, y).unwrap();
                let l = m.ema_liq.get();
                assert!(
                    l.abs_diff(l_pool) * 100 <= l_pool,
                    "L_ema at the gate = the pool's √(X·Y) within 1 %: {l} vs {l_pool}"
                );
            } else {
                wide_in_gate += u32::from(wide);
            }
        }
        if gate_at.is_some() {
            assert!(!wide, "+{sec} s: the box ≤ 1 % wide in L from the gate on");
        }
    }
    let gate_at = gate_at.expect("the gate opened");
    assert!(gate_at <= 9, "the gate at the eighth integration");
    eprintln!(
        "T-VENUE-SWITCH graduation: gate at +{gate_at} s (transition at {t_grad}), {wide_in_gate} wide second(s), all inside the gate"
    );
}

// ---- M5-SEC-4: the atomic, self-financed complete + migrate ----------------------------------------

/// M5-SEC-4's first vector end to end (module docs): the attack no longer freezes the market.
#[test]
fn the_atomic_self_financed_complete_and_migrate_is_liquidated_on_the_pool() {
    atomic_complete_and_migrate(None);
}

/// What [`atomic_complete_and_migrate`] measured: the seconds from the transition to the
/// liquidation, the lenders' SOL, the pool over the carried `P_ema` before any observation, and
/// the warm-up seed.
#[derive(Clone, Copy, Debug)]
struct Atomic {
    waited: i64,
    lender_sol: u64,
    pool_over_ema: f64,
    p_pool: u128,
    seed: Seed,
}

/// The atomic, self-financed `[buy_v2(rT), migrate_v2]` on ECON's D@80 % market, end to end
/// ([`the_atomic_self_financed_complete_and_migrate_is_liquidated_on_the_pool`]), optionally with
/// M6-SEC-1's recorded low in a warm-up second (`low`, as [`liquidate_on_pool_within_low`]).
fn atomic_complete_and_migrate(low: Option<(i64, u128)>) -> Atomic {
    let mut g = Grad::d_market(D80_SOLD, lend_for_80(D80_Q));
    // The attacker: long `q` bought on the curve before its short (the M5 exploit's hedge).
    let attacker = CurveUser::new(&mut g.s.env, &g.s.pm.curve);
    let sol0 = lamports(&g.s.env, &attacker.kp.pubkey());
    ok(attacker.buy(&mut g.s.env, &g.s.pm.curve, D80_Q, u64::MAX));
    // Its short: ECON's D@80 % position, from the same wallet.
    let mut short = Borrower {
        kp: attacker.kp.insecure_clone(),
        next_nonce: 0,
    };
    crank_secs(&mut g, 60);
    let (res, nonce) = g.s.env.open(&g.s.pm, &mut short, D80_Q / 2);
    let open: Open = events::one(&ok(res));
    let p = Pos {
        b: short,
        nonce,
        open,
    };
    crank_secs(&mut g, 60);
    let p_ema0 = market(&g.s.env, &g.s.pm).ema_price.get();
    let lenders_free0 = market(&g.s.env, &g.s.pm).tokens_free;

    // One transaction: buy_v2(rT) + migrate_v2 by the attacker (it keeps the rT tokens).
    let rt = u64::try_from(curve(&g.s.env, &g.s.pm).rt).unwrap();
    g.s.env.warp(1);
    let fr = fork::pump_fee_recipient(&g.s.env.svm, 1);
    let bb = fork::pump_buyback_fee_recipient(&g.s.env.svm, 1);
    let buy = g.s.pm.curve.v2_ix(
        &g.s.env.svm,
        fork::PumpV2::Buy,
        &attacker.kp.pubkey(),
        &attacker.ata,
        &fr,
        &bb,
        rt,
        u64::MAX,
    );
    let mig = pump::migrate_v2_ix(&g.s.env.svm, &attacker.kp.pubkey(), &g.s.pm.curve);
    let meta = ok(g
        .s
        .env
        .send(&[pump::cu_limit_ix(1_400_000), buy, mig], &[&attacker.kp]));
    assert!(g.migrated(), "completed and migrated in one transaction");
    eprintln!(
        "attacker's complete + migrate: {} CU, {} inner instructions",
        meta.compute_units_consumed,
        meta.inner_instructions.iter().map(Vec::len).sum::<usize>()
    );
    assert_eq!(
        venue(&g.s.env, &g.s.pm),
        VenueKind::PumpCurve,
        "short.win never saw the completed curve"
    );

    // Other users push the pool before any observation (the boost's size of buy-and-burn).
    let np = pump::NewPool::of(&g.s.pm.curve);
    let r0 = np.reserves(&g.s.env.svm).unwrap();
    let buyer = g.s.env.funded_keypair();
    let payer = g.s.env.payer.pubkey();
    let program = g.s.pm.curve.token_program.id();
    ok(g.s.env.send(
        &[fork::create_ata_idempotent_ix(
            &payer,
            &buyer.pubkey(),
            &g.s.pm.keys.mint,
            &program,
        )],
        &[],
    ));
    let buyer_base = pda::ata(&buyer.pubkey(), &g.s.pm.keys.mint, &program);
    let wsol = fork::fund_wsol(&mut g.s.env, &buyer.pubkey(), 200 * SOL);
    let push = u64::try_from(u128::from(r0.base) - u128::from(r0.base) * 1000 / 1225).unwrap();
    ok(g.s.env.send(
        &[pump::pamm_buy_ix(
            &g.s.env.svm,
            &np,
            &buyer.pubkey(),
            &buyer_base,
            &wsol,
            push,
            200 * SOL,
        )],
        &[&buyer],
    ));
    let r1 = np.reserves(&g.s.env.svm).unwrap();
    let p_pool = r1.effective_quote * W / u128::from(r1.base);
    eprintln!(
        "pool / last curve EMA before any observation: {:.2}",
        p_pool as f64 / p_ema0 as f64
    );
    assert!(p_pool * 10 >= p_ema0 * 35, "≥ 3.5× the last curve EMA");

    // The keeper's next crank (the curve's ReadSet) moves the market onto the pool.
    g.s.env.warp(1);
    let meta = g.crank();
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpSwap);
    let m = market(&g.s.env, &g.s.pm);
    assert_eq!(
        m.ema_price.get(),
        m.observation().ring.newest().unwrap().p,
        "P_ema: the roll of the last curve bucket, then carried over"
    );
    assert!(
        m.ema_price.get().abs_diff(p_ema0) * 10_000 < p_ema0,
        "the completed curve was never observed"
    );
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1));

    // Liquidated on the pool once the warm-up has run; the lenders get Q back.
    let t_grad = g.s.env.now();
    let (_, ev, seed) = liquidate_on_pool_within_low(&mut g, &p, 120, low);
    let waited = g.s.env.now() - t_grad;
    assert!(waited >= 8, "never before the eight integrations");
    assert_eq!(
        market(&g.s.env, &g.s.pm).tokens_free,
        lenders_free0 + p.open.q,
        "the lenders' tokens back"
    );
    let lender_sol = ev.rent
        - ev.rent * u64::from(g.s.env.config().params.rent_protocol_bps) / 10_000
        + ev.penalty_lender;
    assert!(lender_sol > 0, "and SOL on top");
    // The buyback is priced at the pool, not at the frozen pre-completion EMA.
    let frozen_value = u128::from(p.open.q) * p_ema0 / W;
    assert!(
        u128::from(ev.buyback_cost) > frozen_value * 2,
        "B ({}) is at the pool's price, above twice Q at the frozen EMA ({frozen_value})",
        ev.buyback_cost
    );

    // The attacker's books: it holds its long q plus the rT it completed with; what selling them
    // into the pool pays now (the pool's own sale quote), against everything it spent.
    let held = tokens(&g.s.env, &attacker.ata);
    let sale = quote_sell_now(&g, held);
    let sol_now = lamports(&g.s.env, &attacker.kp.pubkey());
    let net = i128::from(sol_now) + i128::from(sale) - i128::from(sol0);
    eprintln!(
        "attacker: its short liquidated after {waited} s (B {:.4} SOL, penalty {:.4}); it holds {held} tokens that sell for {:.4} SOL now; net {} SOL (the lenders: Q back + {:.4} SOL)",
        ev.buyback_cost as f64 / 1e9,
        ev.penalty as f64 / 1e9,
        sale as f64 / 1e9,
        sol4(net),
        lender_sol as f64 / 1e9
    );
    Atomic {
        waited,
        lender_sol,
        pool_over_ema: p_pool as f64 / p_ema0 as f64,
        p_pool,
        seed,
    }
}

/// M6-SEC-1, the review's reproducer (ARCH §8.2 ★T-SEED-FALL; ECON §12.12 (12); the owner's decision
/// of 2026-09-30): the atomic, self-financed attack above, and in one warm-up second after the
/// transition another `[sell to 0.1× the pool, crank_price, buy back]` records a low far below the
/// pool next to the keeper's crank (the pool restored). As built the warm-up seed took that low
/// and kept it until the gate (the running minimum, bounded upward only), then climbed back at
/// most ×7/6 a second; now it falls by at most one v0.4.1 step per integration (×11/12 at `dt =
/// 1`): at the gate `P_ema ≥ P(the low's second)·(11/12)^(9 − k)` and above the low itself. No
/// exit is forced by the lagging seed and none is lost: the position is still liquidated on the
/// pool, the lenders get `Q` back plus SOL, within a few seconds of the undisturbed run. The
/// carried seed lies far below this pool (the attack pushed it ≈ 3.6× before any observation), so
/// the low reaches the seed only where it is deeper than the seed's own fall (the residual,
/// ECON §12.12 residual 17).
#[test]
fn m6_sec_1_a_low_recorded_in_the_warm_up_after_the_atomic_attack_is_bounded() {
    use shortwin_math::observe::price_down_bound;
    let base = atomic_complete_and_migrate(None);
    eprintln!(
        "M6-SEC-1 baseline: liquidated {} s after the transition, lenders Q back + {:.4} SOL \
         (pool {:.2}× the carried EMA; P_ema at the gate {:.3}× the pool)",
        base.waited,
        base.lender_sol as f64 / 1e9,
        base.pool_over_ema,
        base.seed.at_gate as f64 / base.p_pool as f64
    );
    // (post-change second of the low, the liquidation's seconds after the transition): the
    // undisturbed run is 9 s; as built each low set the seed to 0.1× the pool until the gate, and
    // its climb back at ×7/6 a second took ≈ 15 s more. AIR-22: with the sustained growth the lower
    // median of the newest steps, the 8th-second low's exit comes after 10 s (12 s under the
    // minimum); the 1st and 4th seconds' are unchanged.
    for (k, want) in [(1i64, 23i64), (4, 17), (8, 10)] {
        let a = atomic_complete_and_migrate(Some((k, 1_000)));
        // The low's bucket is integrated in second k + 1 and stays in the warm-up's running minimum
        // through the gate's integration in second 9: 9 − k integrations, the first with `dt = 2`
        // when k = 1 (the transition's roll was the last one; the first post-change crank only
        // records), else 1.
        let mut bound = a.seed.at_low;
        for i in k..9 {
            let dt = if i == 1 { 2 } else { 1 };
            bound = price_down_bound(bound, dt, 10_000).expect("down");
        }
        let low = a.p_pool / 10;
        eprintln!(
            "M6-SEC-1: a 0.1× low in post-change second {k}: P_ema at the low's second {:.3}×, at \
             the gate {:.3}× the pool (the fall bound {:.3}×; as built the low, 0.100×); \
             liquidated after {} s (baseline {}), lenders Q back + {:.4} SOL",
            a.seed.at_low as f64 / a.p_pool as f64,
            a.seed.at_gate as f64 / a.p_pool as f64,
            bound as f64 / a.p_pool as f64,
            a.waited,
            base.waited,
            a.lender_sol as f64 / 1e9
        );
        assert!(
            a.seed.at_gate >= bound,
            "second {k}: fell faster than the bound"
        );
        assert!(
            a.seed.at_gate > low,
            "second {k}: the seed stays above the recorded low"
        );
        assert!(a.lender_sol > 0, "the lenders whole");
        assert!(
            a.waited >= base.waited,
            "no exit earlier than the undisturbed run"
        );
        assert_eq!(a.waited, want, "second {k}");
    }
    assert_eq!(base.waited, 9);
}

// ---- ECON §10.2: a straddle's close completes the curve (pump-final P2) -------------------------

/// The sim's D@97 % close through the retired bundle path (`Market._buy_back` with `synthetic`
/// off: `pool.buy(Q)` right after `migrate()`): what the borrower paid before pump's synthetic
/// migration (ECON 1.9227 SOL).
const D97_STR_B: u64 = 1_922_674_204;
const D97_V: u64 = 6_488_229_940;
/// The sim's D@97 % close on pump's v3 (pump-final §12, P7; `shortwin_sim.py`'s default
/// `synthetic` switch, ECON §10.2's "synthetic v3 (one transaction)" row): `PumpCurve.buy(Q)` past
/// `rT = Q/2`, pump's synthetic migration — `⌊rT·vQ/(vT − rT)⌋ + 1` on the curve, `⌈Y_s·q₂/(X_s −
/// q₂)⌉` on the would-be pool, each part's fees ceil'ed —, its rent at the program's `r_min` (4 h at
/// the pre-open 0 %, the market's first position) and the borrower's `V − B − R`.
const D97_V3_B: u64 = 1_883_736_744;
const D97_V3_RENT: u64 = 720_630;
const D97_V3_PAYOUT: u64 = 4_603_772_566;
/// The sim's post-complete fields after that close (`pcB = Q − rT`, `pcQ = n₂`).
const D97_V3_PCB: u64 = 2_260_866_303_878;
const D97_V3_PCQ: u64 = 938_978_951;

/// ECON §10.2 "rt → Q/2, borrower closes 30 s later" (D@97 %) on pump's v3 (pump-final §5.2; owner
/// D1 (a), D3): the curve is bought down to `rT = Q/2`; the straddle's quote `B̂_v3` keeps the
/// position healthy; at `TE + 30` the borrower's own `close_short` completes the curve in one
/// instruction — `rT` from the curve, `Q − rT` from the would-be pool, the market `PumpMigrating`
/// in its own events. `B − growth` is pump's charge, `≤ B̂_v3 ≤` charge + 7, below the retired
/// bundle's 1.9227 SOL; the borrower gets `V − B − R`. (M5-SEC-4's self-made straddle, which this
/// file drove through the keeper's bundle, is `synthetic_exit_it::a_self_made_straddle_is_
/// liquidated_at_once`.) **The sim differential** (pump-final §12, P7): `B` is the normative sim's
/// own close of the same position (`shortwin_sim.py`, synthetic mode) to the lamport, as are the
/// rent (`R_min` at the pre-open utilisation), the payout and the curve's `pcB` / `pcQ`.
#[test]
fn econ_10_2_the_borrowers_close_of_a_straddle_completes_the_curve() {
    let mut g = Grad::d_market(D97_SOLD, lend_for_80(D97_Q));
    let p = g.s.open(D97_Q);
    assert_eq!(p.open.v, D97_V);
    // Cranks up to TE as the sim's keeper (every 30 s, then every second from TE − 5).
    let mut t = 0;
    while t < TE - 5 {
        t = (t + 30).min(TE - 5);
        g.s.step_to(t);
    }
    while t < TE {
        t += 1;
        g.s.step_to(t);
    }
    // At TE other users buy the curve down to rT = Q/2 (`leave_real_reserves(0.5)`).
    let rt = u64::try_from(curve(&g.s.env, &g.s.pm).rt).unwrap();
    g.s.crowd_buy(rt - D97_Q / 2);
    let rt = u64::try_from(curve(&g.s.env, &g.s.pm).rt).unwrap();
    assert_eq!(rt, D97_Q / 2);
    while t < TE + 30 {
        t += 1;
        g.s.step_to(t);
        assert!(
            !g.status(&p).cr_liq(),
            "+{} s: the straddle quote keeps it healthy",
            t - TE
        );
    }
    let b_v3 = g.status(&p).b_live.expect("B̂_v3");
    let pos = g.s.position(&p);
    let growth = g.s.pm.curve.growth_top_up(&g.s.env.svm);
    let meta = ok(g.s.env.close(&g.s.pm, &p.b, p.nonce));
    let ev: Close = events::one(&meta);
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpMigrating);
    let curve_part = fork::pump_trade_events(&meta);
    let pool_part = fork::pump_post_complete_buy_events(&meta);
    assert_eq!(
        (curve_part.len(), pool_part.len()),
        (1, 1),
        "one buy_v3: a curve part and a pool part"
    );
    assert_eq!(
        (curve_part[0].token_amount, pool_part[0].base_out),
        (rt, D97_Q - rt)
    );
    let charge = curve_part[0].sol_amount
        + curve_part[0].fee
        + curve_part[0].creator_fee
        + pool_part[0].quote_in
        + pool_part[0].fee
        + pool_part[0].creator_fee;
    assert_eq!(ev.b_live, b_v3, "the keeper's view prices what executes");
    assert_eq!(
        ev.buyback_cost - growth,
        charge,
        "B − growth = pump's charge"
    );
    assert!(b_v3 >= charge && b_v3 - charge <= 7);
    assert!(ev.buyback_cost < D97_STR_B, "below the retired bundle's B");
    // The sim differential (P7): the sim's synthetic close, to the lamport.
    assert_eq!(
        charge, D97_V3_B,
        "pump's charge = the sim's PumpCurve.buy(Q)"
    );
    assert_eq!(
        ev.buyback_cost,
        D97_V3_B + growth,
        "B = the sim's (+ the curve's growth)"
    );
    assert_eq!(ev.rent, D97_V3_RENT, "R_min (4 h at the pre-open 0 %)");
    assert_eq!(ev.borrower_payout, pos.v - ev.buyback_cost - ev.rent);
    assert_eq!(
        ev.borrower_payout + growth,
        D97_V3_PAYOUT,
        "the sim's V − B − R"
    );
    let c = g.s.pm.curve.state(&g.s.env.svm);
    assert!(c.complete && c.post_complete_base_out == D97_Q - rt);
    assert_eq!(
        (c.post_complete_base_out, c.post_complete_quote_in),
        (D97_V3_PCB, D97_V3_PCQ),
        "the sim's pcB, pcQ"
    );
    eprintln!(
        "ECON §10.2 on v3: close +30 s completes the curve, B {:.4} SOL (the bundle's 1.9227), \
         payout {:.4} SOL (borrower P&L {})",
        ev.buyback_cost as f64 / 1e9,
        ev.borrower_payout as f64 / 1e9,
        sol4(i128::from(ev.borrower_payout) - i128::from(pos.m) - i128::from(pos.fee_open))
    );
    assert_eq!(market(&g.s.env, &g.s.pm).open_positions, 0);
}
// ---- the boost gate -------------------------------------------------------------------------------

/// ARCH §5.4 case 3 on a real graduation (module docs).
#[test]
fn the_boost_gate_on_a_real_graduation() {
    let mut g = Grad::d_market(D80_SOLD, lend_for_80(D80_Q));
    g.s.env.warp(1);
    g.crank();
    g.complete_curve();
    g.crank();
    crank_secs(&mut g, 1);
    g.s.env.warp(1);
    migrate_and_transition(&mut g);
    let pool = g.pam().pool;
    let t_grad = g.s.env.now();
    let p0 = pool_spot(&g.s.env, &pool);
    let params = g.s.env.config().params;
    // Through the warm-up and the whole boost (40 chunks, one every 9 s).
    let mut refused = 0;
    while pool.boost_amount(&g.s.env.svm) > 0 {
        crank_secs(&mut g, 1);
        if market(&g.s.env, &g.s.pm).ema_warm == 0
            && pool.boost_amount(&g.s.env.svm) > params.boost_dust_lamports
        {
            assert_err(&try_open(&mut g, D80_Q / 10), E::BoostInProgress);
            refused += 1;
        }
    }
    let p1 = pool_spot(&g.s.env, &pool);
    let push = p1 as f64 / p0 as f64;
    eprintln!(
        "the boost: {} calls over {} s, price ×{push:.4} (ECON ×1.4566), {refused} opens refused BoostInProgress",
        g.boost.unwrap().calls,
        g.s.env.now() - t_grad
    );
    assert_eq!(g.boost.unwrap().calls, 40);
    assert!((push - 1.2069f64.powi(2)).abs() < 1e-4, "×(1 + 0.2069)²");
    assert!(read::account(&g.s.env.svm, &pool.boost_vault_ata).is_none());
    // The vault is empty: opens resume once the EMA has caught up with the push.
    crank_secs(&mut g, 60);
    let q = D80_Q / 10;
    let d = open_on_pool(&mut g, q);
    assert!(g.s.env.now() < t_grad + i64::from(params.boost_window_s));
    eprintln!(
        "an open {} s after the graduation (tier {})",
        g.s.env.now() - t_grad,
        d.open.tier
    );

    // A donor re-creates the closed ATA and parks 1 SOL there (anyone can; R-VEN §1.4 item 5):
    // opens refuse again, but only until graduated_at + boost_window_s. (Whether pump's boost
    // bot would spend a donation is UNVERIFIED, R-VEN §1.4; here nobody does.)
    g.run_boost = false;
    let donor = g.s.env.funded_keypair();
    ok(g.s.env.send(
        &[fork::create_ata_idempotent_ix(
            &donor.pubkey(),
            &pool.boost_vault,
            &WSOL_MINT,
            &TOKEN_PROGRAM_ID,
        )],
        &[&donor],
    ));
    fork::wrap(&mut g.s.env, &pool.boost_vault_ata, SOL);
    assert_eq!(pool.boost_amount(&g.s.env.svm), SOL);
    crank_secs(&mut g, 1);
    assert_err(&try_open(&mut g, q), E::BoostInProgress);
    let end = t_grad + i64::from(params.boost_window_s);
    while g.s.env.now() < end - 1 {
        g.s.env.warp(30.min(end - 1 - g.s.env.now()).max(1));
        g.crank();
    }
    assert_err(&try_open(&mut g, q), E::BoostInProgress);
    crank_secs(&mut g, 1);
    assert_eq!(g.s.env.now(), end);
    let e = open_on_pool(&mut g, q);
    assert_eq!(e.open.q, q, "the window bounds the grief");
}

// ---- a completing buyback, then the graduation ----------------------------------------------------

/// ECON §10.2 `q = rt` then the graduation: a borrower's close that buys exactly the curve's `rT`
/// completes it (the market `PumpMigrating` after the leg, ARCH §5.4 case 1); `migrate_v2` and the
/// keeper's crank move the market onto its pool with the other position open, and that one closes
/// there during the warm-up at the pool's quote; every lent token is back.
#[test]
fn a_completing_close_then_the_graduation_carries_the_other_position() {
    let q1 = D97_Q / 2;
    let q2 = D97_Q / 4;
    let mut g = Grad::d_market(D97_SOLD, lend_for_80(q1 + q2));
    let lent = market(&g.s.env, &g.s.pm).tokens_free;
    let p2 = g.s.open(q2);
    let p1 = g.s.open(q1);
    crank_secs(&mut g, 30);
    // Other users buy the curve down to rT = q1.
    let rt = u64::try_from(curve(&g.s.env, &g.s.pm).rt).unwrap();
    g.s.crowd_buy(rt - q1);
    crank_secs(&mut g, 1);
    let charge =
        g.s.pm
            .curve
            .charge_buy(&g.s.env.svm, q1)
            .expect("q1 ≤ rT")
            .cost;
    let meta = ok(g.s.env.close(&g.s.pm, &p1.b, p1.nonce));
    let close1: Close = events::one(&meta);
    assert_eq!(
        close1.buyback_cost, charge,
        "pump's charge for the whole rT"
    );
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpMigrating);
    let c = curve(&g.s.env, &g.s.pm);
    assert!(c.complete && c.rt == 0, "the close completed the curve");
    assert_err(&g.s.env.close(&g.s.pm, &p2.b, p2.nonce), E::VenueMigrating);
    crank_secs(&mut g, 1);
    g.s.env.warp(1);
    migrate_and_transition(&mut g);
    crank_secs(&mut g, 2);
    assert!(market(&g.s.env, &g.s.pm).ema_warm > 0, "still warming up");
    let b_exp = quote_buy_now(&g, q2);
    let close2: Close = events::one(&ok(g.s.env.close(
        g.pam.as_ref().expect("graduated"),
        &p2.b,
        p2.nonce,
    )));
    assert_eq!(
        close2.buyback_cost, b_exp,
        "the other position closes on the pool"
    );
    let m = market(&g.s.env, &g.s.pm);
    assert_eq!(
        (m.open_positions, m.tokens_free),
        (0, lent),
        "every lent token back"
    );
}
