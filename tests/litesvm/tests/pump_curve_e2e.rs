//! Fork E2E on the pump curve (M5; ARCH §8.2, §8.4 "fork E2E", §9 M5 row): the release
//! short.win against the **dumped** pump, pump-fees and PumpSwap programs and the pump coins of
//! `tests/fixtures/`, driven by real instructions — short.win's, and other users' `buy_v2` /
//! `sell_v2` on the curve — with the invariant hooks after every instruction (`Env::send`).
//! Every exit is checked against `shortwin-math` on the venue-state readers
//! (`pump_curve_support.rs`: pump's exact charge, the waterfall's legs, the keeper's view of
//! liquidatability agreeing with the program's every second, fair value).
//!
//! | Test | Path |
//! |---|---|
//! | [`lifecycle_on_a_live_curve`] | a live Token-2022 curve of the dump: `create_market(Auto)` (PumpCurve, Tier D, the trader's base ATA and pump UVA) → two lenders' deposits → an open refused in the warm-up → opens (`S`, `N_open`, the D4 margin and the fee recomputed) → other users' real sales, a crank every second → a profitable close at pump's charge → other users' real buys pump ×3 → two liquidations by normal buyback (every refusal before them is the one the keeper's view predicts) → an open held to its deadline with hourly cranks: `liquidate` refuses `UseExpire`, `expire` executes → an open and a close → both lenders exit with their tokens and their pro-rata SOL income |
//! | [`a_completion_with_positions_open_pauses_then_graduates_elsewhere`] | ECON's D@80% market: one position opened at 80 %, other users buy to 90 %, a second position there; another user's buy completes the curve → `PumpMigrating` (the next crank), pump refuses every curve trade, short.win's exits and opens pause (`VenueMigrating`); the keeper's crank and `sol_settle` every 30 s: the position the completion made liquidatable settles S3 exactly at `migration_seen_at + max_migration_pause_s + s3_wait_s` (70 min) at FV on the deterministic post-migration pool; the healthy one waits; then pump migrates the curve to another venue (no canonical pool): `PumpMigrating → None`, `graduated_elsewhere`, the detection set is the ReadSet; the healthy position stays frozen (no route) with the keeper cranking every 30 s until its deadline, then settles S3 at FV with the expiry fee; the lender takes back its tokens and the SOL |
//! | [`a_curve_seen_migrated_elsewhere_settles_its_liquidatable_position_in_s3`] | ECON's D@50% market: a position pumped past its liquidation threshold that nobody liquidates, a second opened at the new price; the curve completes and migrates elsewhere between two reads: `PumpCurve → None` directly; exits `NoRoute`; the liquidatable position settles S3 once the chain has run `s3_wait_s` (FV at the frozen EMA with the penalty), the healthy one is not settled; lenders can still withdraw from the list-only market |
//! | [`a_real_migrate_v2_moves_the_market_onto_its_pool_and_a_is_liquidated_there`] | M6: ECON's D@80% market with positions at 80 % and 90 %, the completion observed for 10 s, a real `migrate_v2`: the next crank moves the market onto its canonical pool (`PumpMigrating → PumpSwap`, the ring at the pool's `lp_supply`, the warm-up); keeper liquidations fail `EmaWarmingUp` for eight integrations, then the position the completion made liquidatable is liquidated on the pool and the healthy one closes there (M5 froze the market and settled S3 at 70 min) |
//! | [`a_fast_migrate_v2_is_priced_on_the_pool`] | M6: the same with `migrate_v2` the second after the completion: the EMA follows the pool (not the frozen pre-completion price of M5) and the position is liquidated on the pool, the lenders' tokens back |
//!
//! The only planted venue state is pump's own at graduation elsewhere (the curve's reserves
//! zeroed, as a migration to another venue leaves them, `fork::set_curve_reserves`; ARCH §8.2
//! scenario builders) and the D@x markets' undoing of the fixture's 1-lamport creation buy
//! (`Sim::d_market`).

#![allow(clippy::result_large_err)]

#[path = "pump_curve_support.rs"]
mod support;

use shortwin::events::MarketCreated;
use shortwin::state::MARKET_FLAG_GRADUATED_ELSEWHERE;
use shortwin_math::tier::Tier;
use shortwin_math::{caps, risk};
use support::*;

/// ECON's D@80% market (`int(PUMP_RT0·0.8)` tokens bought) and its position at the cap.
const D80_SOLD: u64 = 634_480_000_000_000;
const D80_Q: u64 = 6_529_192_912_424;
/// ECON's D@50% position at the cap.
const D50_Q: u64 = 10_071_769_920_663;

/// An open that must succeed, checked against `shortwin-math` on the readers: `S` = the sale
/// quote on the pre-trade curve, `N_open` = `B̂_spot` = the buy quote on the post-open curve,
/// `B̂_ema` = the risk quote on the newest ring entry, `M` = the D4 margin at Tier D's opening
/// CR, the open fee on `N_open`, and the lending vault's `−q`.
fn open_checked(s: &mut Sim, q: u64) -> Pos {
    let sale = s.pm.curve.quote_sell(&s.env.svm, q).expect("sale quote");
    let vault0 = tokens(&s.env, &s.pm.keys.pool_vault);
    let mut b = s.env.borrower();
    let (res, nonce) = s.env.open(&s.pm, &mut b, q);
    let meta = ok(res);
    let o: Open = events::one(&meta);
    assert_eq!(
        o.s, sale.proceeds,
        "S = the sale quote on the pre-trade curve"
    );
    let n_open = s.pm.curve.quote_buy(&s.env.svm, q).expect("buy quote").cost;
    assert_eq!(
        (o.n_open, o.b_spot),
        (n_open, n_open),
        "N_open = B̂ on the post-open curve"
    );
    let st = status(&s.env, &s.pm, &b.key(), nonce);
    assert_eq!(Some(o.b_ema), st.b_ema, "B̂_ema on the newest ring entry");
    let pos = s.env.position(&s.pm, &b.key(), nonce);
    let tier = s.env.config().tiers[Tier::D as usize];
    assert_eq!(
        o.m,
        risk::margin_d4(o.s, o.b_spot, Some(o.b_ema), tier.cr_open_bps).unwrap(),
        "the D4 margin"
    );
    assert_eq!(
        o.fee_open,
        risk::open_fee(o.n_open, 25).unwrap(),
        "the open fee"
    );
    assert_eq!((o.v, pos.v), (o.s + o.m, o.s + o.m));
    assert_eq!(o.tier, Tier::D as u8);
    assert_eq!(vault0 - tokens(&s.env, &s.pm.keys.pool_vault), q);
    Pos { b, nonce, open: o }
}

/// The keeper's liquidation attempt this second (after a crank): the oracle's view says whether
/// the program must refuse (`NotLiquidatable`, or `ExitBoundNotMet` while the live cost is above
/// the bound); an executed liquidation is checked leg by leg (pump's exact charge, the penalty
/// and its split, the keeper floor, `q` back). `Some` if it executed.
fn keeper_liquidates(s: &mut Sim, p: &Pos) -> Option<Liquidate> {
    let st = s.status(p);
    let pos = s.position(p);
    let charge = s.pm.curve.charge_buy(&s.env.svm, pos.q).map(|c| c.cost);
    let quote = s.pm.curve.quote_buy(&s.env.svm, pos.q).map(|c| c.cost);
    let vault0 = tokens(&s.env, &s.pm.keys.pool_vault);
    let kp = s.keeper.insecure_clone();
    let ix = liquidate_ix(&s.pm, &kp.pubkey(), &p.key(), p.nonce);
    let cu = pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU);
    let res = s.env.send(&[cu, ix], &[&kp]);
    if !st.cr_liq() {
        assert_err(&res, E::NotLiquidatable);
        return None;
    }
    let meta = match res {
        Ok(meta) => meta,
        Err(_) => {
            assert_err(&res, E::ExitBoundNotMet);
            return None;
        }
    };
    let ev: Liquidate = events::one(&meta);
    assert!(!ev.expired);
    assert_eq!(Some(ev.buyback_cost), charge, "B = pump's charge");
    assert_eq!(Some(ev.b_live), quote, "bounded by B̂_live");
    assert_eq!(Some(ev.b_live_risk), st.b_live_risk);
    let k_s = k_s(&pos);
    assert_eq!(
        ev.max_in,
        ((u128::from(ev.b_ref) * 10_200 / 10_000) as u64).min(pos.v - k_s - pos.ert),
        "max_in: the fixed 2 % over B̂_ref within V − K_s − ert"
    );
    assert!(ev.buyback_cost <= ev.max_in);
    let base = ev.b_live_risk.min(ev.b_ref);
    let rest = pos.v - ev.buyback_cost - ev.rent;
    let pen = (u128::from(base) * u128::from(pos.penalty_bps)).div_ceil(10_000) as u64;
    assert_eq!(
        ev.penalty,
        pen.min(rest),
        "penalty on min(B̂_live,risk, B̂_ref)"
    );
    assert_eq!(ev.penalty_keeper, ev.penalty * 3_000 / 10_000);
    assert_eq!(ev.penalty_protocol, ev.penalty * 2_000 / 10_000);
    assert_eq!(
        ev.penalty_lender,
        ev.penalty - ev.penalty_keeper - ev.penalty_protocol
    );
    if ev.borrower_payout > 0 {
        assert!(
            ev.penalty_keeper + ev.keeper_topup >= k_s,
            "the keeper floor (N22)"
        );
    }
    assert_eq!(
        ev.buyback_cost + ev.rent + ev.penalty + ev.keeper_topup + ev.borrower_payout,
        pos.v
    );
    assert!(ev.rent >= pos.r_min);
    assert_eq!(
        tokens(&s.env, &s.pm.keys.pool_vault) - vault0,
        pos.q,
        "q back"
    );
    Some(ev)
}

/// `sol_settle` of `p` by the keeper, with the curve's ReadSet or (`list_only`) the detection
/// set: the `SolSettle` event if it settled.
fn settle(s: &mut Sim, list_only: bool, p: &Pos) -> Option<SolSettle> {
    let kp = s.keeper.insecure_clone();
    let ix = if list_only {
        sol_settle_ix(&s.env, &AsNone(&s.pm), &kp.pubkey(), &p.key(), p.nonce)
    } else {
        sol_settle_ix(&s.env, &s.pm, &kp.pubkey(), &p.key(), p.nonce)
    };
    let meta = ok(s.env.send(&[ix], &[&kp]));
    events::of::<SolSettle>(&meta).pop()
}

/// `crank_price` of a list-only market (the detection set).
fn crank_none(s: &mut Sim) {
    let ix = crank_price_ix(&AsNone(&s.pm));
    ok(s.env.send(&[ix], &[]));
}

/// Checks an S3 settlement paid at the EMA point: `x = min(V − K_s, FV(P_ema, L_ema))` to the
/// lenders (the venue cannot execute, so all of `x` above `R_due` vests: AIR-09), the rest of
/// `V − K_s` to the borrower, the position closed; the settlement box (ARCH §3.8) is the EMA
/// point once nothing was observed for more than `RING_LEN` seconds on a venue that cannot
/// execute (a list-only market), else the EMA widened by the ring's medians (a migrating curve
/// is still observed).
fn assert_s3(s: &Sim, ev: &SolSettle, pos: &ShortPosition, r_due: u64, fv: u64) {
    let m = market(&s.env, &s.pm);
    let obs = m.observation();
    let (p, l) = (m.ema_price.get(), m.ema_liq.get());
    assert_eq!(ev.condition, 3, "S3");
    let stale = s.env.now() - obs.ema.ts > shortwin_math::consts::RING_LEN as i64;
    let want = if stale {
        (p, p, l, l)
    } else {
        let (pmlo, pmhi, lmlo, lmhi) = shortwin_math::observe::ring_medians(&obs.ring).unwrap();
        (p.min(pmlo), p.max(pmhi), l.min(lmlo), l.max(lmhi))
    };
    assert_eq!(
        (ev.p_lo, ev.p_hi, ev.l_lo, ev.l_hi),
        want,
        "the settlement box"
    );
    assert_eq!(ev.fv, fv, "FV at (P_ema, L_ema)");
    let k = k_s(pos);
    assert_eq!(ev.k_s, k);
    let x = fv.min(pos.v - k);
    assert_eq!(ev.lender_amount, x, "lenders: min(V − K_s, FV)");
    assert_eq!(ev.borrower_amount, pos.v - k - x);
    // A migrating curve and a list-only market cannot execute: their `P_ema` is no market price
    // the immediate part could pay at, so only `R_due` is immediate (AIR-09).
    assert_eq!(ev.lump, x - x.min(r_due), "all of x above R_due vests");
    assert!(
        read::account(
            &s.env.svm,
            &pda::position(&s.pm.market(), &pos.borrower, pos.nonce).0
        )
        .is_none(),
        "the position is closed to its borrower"
    );
}

// ---- the lifecycle ----------------------------------------------------------------------------

/// The lifecycle on a live curve of the dump (module docs).
#[test]
fn lifecycle_on_a_live_curve() {
    let (mut env, c) = fork_env_with(PumpCoin::T22_115, |_| {});
    // Other users: one holds a bag bought before the listing (it sells into the market later).
    let crowd = CurveUser::new(&mut env, &c);
    let vt0 = c.state(&env.svm).virtual_token_reserves;
    ok(crowd.buy(&mut env, &c, vt0 / 10, u64::MAX));
    let vt = c.state(&env.svm).virtual_token_reserves;
    let q = vt / 200;

    // ---- listing: create_market(Auto) classifies a live SOL curve ----
    let meta = ok(env.list_pump(&c));
    let created: MarketCreated = events::one(&meta);
    assert_eq!(created.venue, VenueKind::PumpCurve as u8);
    assert_eq!(created.tier, Tier::D as u8);
    assert_eq!(created.bonding_curve, c.curve);
    let spot0 = {
        let s = c.curve_state(&env.svm);
        s.vq * W / s.vt
    };
    let pm = env.pump_market_listed(
        PumpSpec {
            coin: PumpCoin::T22_115,
            lend: 40 * q,
            warmup: 0,
        },
        c,
    );
    let m = market(&env, &pm);
    assert_eq!(
        m.ema_price.get(),
        spot0,
        "the price starts at the curve's spot"
    );
    assert_eq!(m.curve_base_ata, pm.curve.curve_base_ata);
    assert!(
        read::account(&env.svm, &pm.trader_base_ata).is_some(),
        "trader base ATA"
    );
    assert!(
        read::account(&env.svm, &fork::pump_user_volume_accumulator(&pm.trader())).is_some(),
        "the trader's pump UVA"
    );
    // A second lender.
    let l2 = env.funded_keypair();
    let l2_acc = fork::fund_token(
        &mut env,
        &l2.pubkey(),
        &pm.keys.mint,
        pm.curve.token_program,
        10 * q,
    );
    let l2_keys = ix::LenderKeys {
        owner: l2.pubkey(),
        mint: pm.keys.mint,
        token_program: pm.keys.token_program,
        owner_token_account: l2_acc,
    };
    ok(env.send(&[ix::deposit(&l2_keys, 10 * q, 0)], &[&l2]));
    let keeper = env.funded_keypair();
    let t0 = env.now();
    let mut s = Sim {
        env,
        pm,
        crowd,
        keeper,
        t0,
    };

    // ---- the warm-up ----
    let mut early = s.env.borrower();
    assert_err(&s.env.open(&s.pm, &mut early, q).0, E::MarketWarmingUp);
    s.env.warp(i64::from(s.env.config().params.market_warmup_s));
    s.crank();

    // ---- two opens at the listing price ----
    let a = open_checked(&mut s, q);
    let b = open_checked(&mut s, q);
    for _ in 0..5 {
        s.env.warp(1);
        s.crank();
    }

    // ---- other users sell the price 15 % down over 3 s; the keeper cranks every second ----
    let p0 = s.spot();
    for k in 1..=3u32 {
        s.env.warp(1);
        assert!(s.exo_move(p0 * u128::from(100 - 5 * k) / 100).is_none());
        s.crank();
    }
    for _ in 0..3 {
        s.env.warp(1);
        s.crank();
    }
    // A closes in profit, at pump's exact charge.
    let charge = s.pm.curve.charge_buy(&s.env.svm, q).unwrap().cost;
    let meta = ok(s.env.close(&s.pm, &a.b, a.nonce));
    let close_a: Close = events::one(&meta);
    assert_eq!(close_a.buyback_cost, charge, "B = pump's charge");
    assert!(
        close_a.buyback_cost < a.open.s,
        "a short gains when the price falls"
    );
    assert!(
        close_a.borrower_payout > a.open.m,
        "A takes back more than its margin"
    );

    // C opens at the lower price.
    let cpos = open_checked(&mut s, q);
    for _ in 0..3 {
        s.env.warp(1);
        s.crank();
    }

    // ---- other users buy the price ×3 over 5 s; B and C cross their liquidation CR ----
    let p1 = s.spot();
    for k in 1..=5 {
        s.env.warp(1);
        assert!(
            s.exo_move((p1 as f64 * 3f64.powf(f64::from(k) / 5.0)) as u128)
                .is_none()
        );
        s.crank();
    }
    let mut open = vec![b, cpos];
    let mut liquidated = Vec::new();
    for _ in 0..60 {
        if open.is_empty() {
            break;
        }
        s.env.warp(1);
        s.crank();
        let mut keep = Vec::new();
        for p in open {
            match keeper_liquidates(&mut s, &p) {
                Some(ev) => liquidated.push(ev),
                None => keep.push(p),
            }
        }
        open = keep;
    }
    assert!(open.is_empty(), "both liquidated within a minute");
    assert_eq!(liquidated.len(), 2);
    for l in &liquidated {
        eprintln!(
            "liquidation: B {:.4} SOL (B̂_ref {:.4}), rent {:.6}, penalty {:.4} (lenders {:.4}, \
             keeper {:.4}), borrower {:.4}",
            l.buyback_cost as f64 / 1e9,
            l.b_ref as f64 / 1e9,
            l.rent as f64 / 1e9,
            l.penalty as f64 / 1e9,
            l.penalty_lender as f64 / 1e9,
            l.penalty_keeper as f64 / 1e9,
            l.borrower_payout as f64 / 1e9
        );
    }

    // ---- D opens once the EMA caught up, then the price stays until its deadline ----
    for _ in 0..30 {
        s.env.warp(1);
        s.crank();
    }
    let d = open_checked(&mut s, q);
    let deadline = s.position(&d).deadline;
    while s.env.now() + 3_600 < deadline - 60 {
        s.env.warp(3_600);
        s.crank();
    }
    s.env.warp(deadline - 60 - s.env.now());
    while s.env.now() < deadline {
        s.env.warp(1);
        s.crank();
    }
    let st = s.status(&d);
    assert!(st.expired && !st.cr_liq(), "expired, not CR-liquidatable");
    let kp = s.keeper.insecure_clone();
    let cu = || pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU);
    let ix = liquidate_ix(&s.pm, &kp.pubkey(), &d.key(), d.nonce);
    assert_err(&s.env.send(&[cu(), ix], &[&kp]), E::UseExpire);
    let pos_d = s.position(&d);
    let charge = s.pm.curve.charge_buy(&s.env.svm, q).unwrap().cost;
    let ix = expire_ix(&s.pm, &kp.pubkey(), &d.key(), d.nonce);
    let meta = ok(s.env.send(&[cu(), ix], &[&kp]));
    let e: Expire = events::one(&meta);
    assert_eq!(e.buyback_cost, charge, "B = pump's charge");
    assert!(e.rent >= pos_d.r_min && e.rent == st.r_due, "72 h of rent");
    let base = e.b_live_risk.min(e.b_ref);
    let rest = pos_d.v - e.buyback_cost - e.rent;
    let fee = ((u128::from(base) * 200).div_ceil(10_000) as u64).min(rest);
    assert_eq!(e.expiry_fee, fee, "2 % on min(B̂_live,risk, B̂_ref)");
    let k_e = risk::k_e(e.b_ref, &pos_d.expiry_keeper_terms()).unwrap();
    assert_eq!(e.keeper_reward, k_e.min(rest - fee));
    assert_eq!(e.borrower_payout, rest - fee - e.keeper_reward);

    // ---- E opens and closes ----
    let epos = open_checked(&mut s, q);
    s.env.warp(10);
    s.crank();
    let charge = s.pm.curve.charge_buy(&s.env.svm, q).unwrap().cost;
    let meta = ok(s.env.close(&s.pm, &epos.b, epos.nonce));
    let close_e: Close = events::one(&meta);
    assert_eq!(close_e.buyback_cost, charge);
    let m = market(&s.env, &s.pm);
    assert_eq!((m.open_positions, m.borrowed_q), (0, 0), "nothing borrowed");

    // ---- both lenders exit once the lumps vested: tokens back pro rata, SOL income ----
    let lock_grace_s = s.env.config().params.lock_grace_s;
    let vest = i64::from(s.env.config().params.lump_vest_s);
    s.env.warp(i64::from(lock_grace_s).max(vest) + 86_400);
    s.crank();
    let market_key = s.pm.market();
    let l1 = s.pm.lender.insecure_clone();
    let l1_keys = s.pm.lender_keys();
    let (t1, sol1) = withdraw_all(&mut s.env, &market_key, &l1_keys, &l1);
    let (t2, sol2) = withdraw_all(&mut s.env, &market_key, &l2_keys, &l2);
    assert!(
        t1 + 2 >= 40 * q && t2 + 2 >= 10 * q,
        "the tokens come back (dust aside)"
    );
    let income: u64 = liquidated.iter().map(|l| l.penalty_lender).sum::<u64>() + e.expiry_fee;
    assert!(
        sol1 + sol2 > income,
        "rent, penalties and the expiry fee reach the lenders"
    );
    let ratio = sol1 as f64 / sol2 as f64;
    assert!(
        (ratio - 4.0).abs() < 0.01,
        "SOL income pro rata to shares: {ratio}"
    );
    eprintln!(
        "lenders: {t1} + {t2} tokens back, {:.6} + {:.6} SOL of income",
        sol1 as f64 / 1e9,
        sol2 as f64 / 1e9
    );
}

// ---- graduation with positions open --------------------------------------------------------------

/// A curve completes with positions open (module docs): the migration pause, S3 at 70 min, then
/// pump migrates it elsewhere and the healthy position settles S3 at its deadline.
#[test]
fn a_completion_with_positions_open_pauses_then_graduates_elsewhere() {
    let mut s = Sim::d_market(D80_SOLD, lend_for_80(D80_Q + 5_400_000_000_000));
    // A at 80 % (ECON's D@80% position at the cap).
    let a = open_checked(&mut s, D80_Q);
    for _ in 0..30 {
        s.env.warp(1);
        s.crank();
    }
    // Other users buy the curve from 80 % to 90 % progress; B opens there at the cap.
    s.crowd_buy(SOLD_D90 - D80_SOLD);
    for _ in 0..30 {
        s.env.warp(1);
        s.crank();
    }
    let vt = u64::try_from(curve(&s.env, &s.pm).vt).unwrap();
    let qb = caps::max_q_position(vt, 300).unwrap();
    let b = open_checked(&mut s, qb);
    for _ in 0..10 {
        s.env.warp(1);
        s.crank();
    }
    assert!(
        !s.status(&a).cr_liq() && !s.status(&b).cr_liq(),
        "both healthy at 90 %"
    );

    // ---- another user completes the curve; the next crank sees it ----
    let rt = u64::try_from(curve(&s.env, &s.pm).rt).unwrap();
    s.env.warp(1);
    s.crowd_buy(rt);
    let done = curve(&s.env, &s.pm);
    assert!(done.complete && done.rt == 0 && done.vt > 0);
    assert_eq!(
        venue(&s.env, &s.pm),
        VenueKind::PumpCurve,
        "nothing read the curve yet"
    );
    let meta = s.crank();
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpMigrating);
    let t_mig = s.env.now();
    let m = market(&s.env, &s.pm);
    assert_eq!((m.migration_seen_at, m.no_route_since), (t_mig, t_mig));
    // Pump itself refuses every trade on a complete curve.
    let res = s.crowd.buy(&mut s.env, &s.pm.curve, 1_000_000, u64::MAX);
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_BONDING_CURVE_COMPLETE)
    );
    // short.win pauses exits and opens (SPEC §5.3).
    s.env.warp(1);
    assert_err(&s.env.close(&s.pm, &b.b, b.nonce), E::VenueMigrating);
    let kp = s.keeper.insecure_clone();
    let ix = liquidate_ix(&s.pm, &kp.pubkey(), &a.key(), a.nonce);
    assert_err(&s.env.send(&[ix], &[&kp]), E::VenueMigrating);
    let mut late = s.env.borrower();
    assert_err(
        &s.env.open(&s.pm, &mut late, 1_000_000_000).0,
        E::VenueMigrating,
    );
    // The migrating curve is still observed: the EMA integrates the completed price, and after
    // `liq_persist_n` ring entries at it A's EMA leg holds (its spot leg is true: no route).
    s.crank();
    assert!(
        !s.status(&a).cr_liq(),
        "the EMA leg needs liq_persist_n entries"
    );
    for _ in 0..30 {
        s.env.warp(1);
        s.crank();
    }
    let st_a = s.status(&a);
    assert!(
        st_a.cr_liq(),
        "the completion made A liquidatable (no route, EMA leg on B̂_str)"
    );
    assert!(!s.status(&b).cr_liq(), "B stays healthy");

    // ---- the keeper: a crank and sol_settle every 30 s; S3 of A at 70 min ----
    let pos_a = s.position(&a);
    let due = t_mig + i64::from(pos_a.max_migration_pause_s) + i64::from(pos_a.s3_wait_s);
    assert_eq!(due - t_mig, 4_200, "70 min with the default timers");
    let ev = loop {
        s.env.warp(30);
        s.crank();
        let st = s.status(&a);
        if let Some(ev) = settle(&mut s, false, &a) {
            // FV on the deterministic post-migration pool at P_ema, the penalty (liquidatable).
            let m = market(&s.env, &s.pm);
            let b_fv = b_fv_migrating(&s.env, &s.pm, &pos_a, m.ema_price.get());
            let fv = fair_value(&pos_a, s.env.now(), st.r_due, b_fv);
            assert_eq!(
                (ev.pprime_lo, ev.pprime_hi),
                (pos_a.penalty_bps, pos_a.penalty_bps)
            );
            assert_s3(&s, &ev, &pos_a, st.r_due, fv);
            break ev;
        }
        assert!(s.env.now() < due, "S3 is due at migration_seen_at + 70 min");
        assert!(
            settle(&mut s, false, &b).is_none(),
            "B is neither liquidatable nor expired"
        );
    };
    assert!(s.env.now() >= due && s.env.now() < due + 30, "S3 at 70 min");
    let ev_a = ev;
    eprintln!(
        "stuck migration: A settled S3 at +{} s, FV {:.4} SOL, lenders {:.4}, borrower {:.4}",
        s.env.now() - t_mig,
        ev_a.fv as f64 / 1e9,
        ev_a.lender_amount as f64 / 1e9,
        ev_a.borrower_amount as f64 / 1e9
    );
    assert!(settle(&mut s, false, &b).is_none(), "B still waits");
    assert_eq!(venue(&s.env, &s.pm), VenueKind::PumpMigrating);

    // ---- pump migrates the curve to another venue: PumpMigrating → None ----
    fork::set_curve_reserves(&mut s.env.svm, &s.pm.curve.curve, 0, 0, 0, 0);
    s.env.warp(1);
    let meta = s.crank();
    venue_changed(&meta, VenueKind::PumpMigrating, VenueKind::None);
    let m = market(&s.env, &s.pm);
    assert!(m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
    s.env.warp(1);
    assert_err(
        &s.env.send(&[crank_price_ix(&s.pm)], &[]),
        E::VenueAccountMismatch,
    );
    crank_none(&mut s);
    let ix = expire_ix(&s.pm, &kp.pubkey(), &b.key(), b.nonce);
    assert_err(&s.env.send(&[ix], &[&kp]), E::NoRoute);
    assert_err(&s.env.close(&s.pm, &b.b, b.nonce), E::NoRoute);
    let frozen = (m.ema_price.get(), m.ema_liq.get());

    // ---- B is frozen until its deadline; the keeper keeps the chain alive every 30 s ----
    let pos_b = s.position(&b);
    let ev = loop {
        s.env.warp(30);
        crank_none(&mut s);
        if s.env.now() < pos_b.deadline {
            continue;
        }
        let m = market(&s.env, &s.pm);
        assert_eq!(
            (m.ema_price.get(), m.ema_liq.get()),
            frozen,
            "nothing observed"
        );
        let r_due = rent_due(&s.env, &s.pm, &pos_b);
        let b_fv = b_fv_none(&pos_b, frozen.1, frozen.0);
        let fv = fair_value(&pos_b, s.env.now(), r_due, b_fv);
        let ev = settle(&mut s, true, &b).expect("S3 at the deadline");
        assert_eq!(
            (ev.pprime_lo, ev.pprime_hi),
            (pos_b.expiry_fee_bps, pos_b.expiry_fee_bps),
            "expired and healthy: the expiry fee (O15)"
        );
        assert_s3(&s, &ev, &pos_b, r_due, fv);
        break ev;
    };
    assert!(s.env.now() - pos_b.deadline < 30, "S3 at B's deadline");
    eprintln!(
        "graduated elsewhere: B settled S3 at its deadline, FV {:.4} SOL, lenders {:.4}, borrower {:.4}",
        ev.fv as f64 / 1e9,
        ev.lender_amount as f64 / 1e9,
        ev.borrower_amount as f64 / 1e9
    );

    // ---- the market holds no position; the lender takes back its tokens and the SOL ----
    let m = market(&s.env, &s.pm);
    assert_eq!((m.open_positions, m.borrowed_q), (0, 0));
    let lend = lend_for_80(D80_Q + 5_400_000_000_000);
    assert_eq!(
        m.tokens_free,
        lend - D80_Q - qb,
        "the settled debts are extinguished"
    );
    let lock_grace_s = s.env.config().params.lock_grace_s;
    let vest = i64::from(s.env.config().params.lump_vest_s);
    s.env.warp(i64::from(lock_grace_s).max(vest) + 86_400);
    crank_none(&mut s);
    let market_key = s.pm.market();
    let lender = s.pm.lender.insecure_clone();
    let keys = s.pm.lender_keys();
    let (tokens_back, sol) = withdraw_all(&mut s.env, &market_key, &keys, &lender);
    assert!(tokens_back + 2 >= lend - D80_Q - qb);
    let takes = ev_a.lender_amount + ev.lender_amount;
    eprintln!("lender: {tokens_back} tokens back, {sol} lamports (S3 takes {takes})");
    assert!(
        sol <= takes && takes - sol < 10_000,
        "the lone lender receives both S3 takes (share-accounting dust aside)"
    );
}

/// A curve that completes and migrates elsewhere between two reads, with a liquidatable and a
/// healthy position open (module docs).
#[test]
fn a_curve_seen_migrated_elsewhere_settles_its_liquidatable_position_in_s3() {
    let lend = lend_for_80(2 * D50_Q);
    let mut s = Sim::d_market(SOLD_D50, lend);
    let p = open_checked(&mut s, D50_Q);
    for _ in 0..60 {
        s.env.warp(1);
        s.crank();
    }
    // Other users pump ×3 over 30 s; an observer cranks every second, nobody liquidates.
    let p1 = s.spot();
    for k in 1..=30 {
        s.env.warp(1);
        assert!(
            s.exo_move((p1 as f64 * 3f64.powf(f64::from(k) / 30.0)) as u128)
                .is_none()
        );
        s.crank();
    }
    for _ in 0..30 {
        s.env.warp(1);
        s.crank();
    }
    assert!(s.status(&p).cr_liq(), "P is liquidatable");
    // H opens at the new price, at the cap.
    let vt = u64::try_from(curve(&s.env, &s.pm).vt).unwrap();
    let h = open_checked(&mut s, caps::max_q_position(vt, 300).unwrap());
    for _ in 0..10 {
        s.env.warp(1);
        s.crank();
    }
    assert!(!s.status(&h).cr_liq());

    // ---- between two reads: the curve completes and pump migrates it to another venue ----
    let rt = u64::try_from(curve(&s.env, &s.pm).rt).unwrap();
    s.env.warp(1);
    s.crowd_buy(rt);
    fork::set_curve_reserves(&mut s.env.svm, &s.pm.curve.curve, 0, 0, 0, 0);
    s.env.warp(1);
    let meta = s.crank();
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::None);
    let t_none = s.env.now();
    let m = market(&s.env, &s.pm);
    assert!(m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
    assert_eq!(
        m.no_route_since, t_none,
        "the S3 chain starts at the transition"
    );
    let frozen = (m.ema_price.get(), m.ema_liq.get());
    // No route: exits refuse, opens are refused.
    let kp = s.keeper.insecure_clone();
    s.env.warp(1);
    let ix = liquidate_ix(&s.pm, &kp.pubkey(), &p.key(), p.nonce);
    assert_err(&s.env.send(&[ix], &[&kp]), E::NoRoute);
    assert_err(&s.env.close(&s.pm, &h.b, h.nonce), E::NoRoute);
    let mut late = s.env.borrower();
    let (res, _) = s.env.open(&s.pm, &mut late, 1_000_000_000);
    assert!(res.is_err(), "no opens on a list-only market");

    // ---- the keeper: crank and sol_settle every 30 s; S3 once the chain ran s3_wait_s ----
    let pos_p = s.position(&p);
    let pos_h = s.position(&h);
    let due = t_none + i64::from(pos_p.s3_wait_s);
    let ev = loop {
        s.env.warp(30);
        crank_none(&mut s);
        let r_due = rent_due(&s.env, &s.pm, &pos_p);
        let b_fv = b_fv_none(&pos_p, frozen.1, frozen.0);
        let fv = fair_value(&pos_p, s.env.now(), r_due, b_fv);
        if let Some(ev) = settle(&mut s, true, &p) {
            assert_eq!(
                (ev.pprime_lo, ev.pprime_hi),
                (pos_p.penalty_bps, pos_p.penalty_bps),
                "liquidatable: the penalty"
            );
            assert_s3(&s, &ev, &pos_p, r_due, fv);
            break ev;
        }
        assert!(
            s.env.now() < due,
            "S3 is due s3_wait_s after the chain started"
        );
        assert!(settle(&mut s, true, &h).is_none(), "H is healthy: no S3");
    };
    assert!(s.env.now() >= due && s.env.now() < due + 30);
    assert!(settle(&mut s, true, &h).is_none(), "H stays frozen");
    assert_eq!(market(&s.env, &s.pm).open_positions, 1);
    eprintln!(
        "graduated elsewhere: P settled S3 {} s after the transition, FV {:.4}, lenders {:.4}, \
         borrower {:.4}; H ({} tokens) frozen until its deadline",
        s.env.now() - t_none,
        ev.fv as f64 / 1e9,
        ev.lender_amount as f64 / 1e9,
        ev.borrower_amount as f64 / 1e9,
        pos_h.q
    );

    // ---- the list-only market still serves its lenders ----
    let keys = s.pm.lender_keys();
    let lender = s.pm.lender.insecure_clone();
    let lp: shortwin::state::LenderPosition =
        read::borsh(&s.env.svm, &pda::lender(&s.pm.market(), &lender.pubkey()).0).unwrap();
    let before = tokens(&s.env, &keys.owner_token_account);
    ok(s.env
        .send(&[ix::withdraw(&keys, lp.shares / 10, 0)], &[&lender]));
    assert!(
        tokens(&s.env, &keys.owner_token_account) > before,
        "a withdrawal goes through"
    );
}

// ---- a real migrate_v2 (M6: the → PumpSwap transition) -------------------------------------------

/// ECON's D@80% market with A at 80 % and B at 90 %, the curve completed by another user and
/// observed by the keeper's crank every second for `observed_s` seconds, then `migrate_v2` on the
/// real pump ELF (anyone may call it): the canonical PumpSwap pool exists and trades, and the next
/// crank moves the market onto it (`PumpMigrating → PumpSwap`, M6: the ring rewritten to the
/// pool's `lp_supply`, the post-change warm-up). Returns the market, A, B, the second the
/// migration was seen and the market on its pool (the existence requirement's accounts created,
/// ARCH §5.4 tx3).
fn migrated_after(observed_s: i64) -> (Sim, Pos, Pos, i64, PammMarket) {
    let mut s = Sim::d_market(D80_SOLD, lend_for_80(D80_Q + 5_400_000_000_000));
    let a = open_checked(&mut s, D80_Q);
    for _ in 0..30 {
        s.env.warp(1);
        s.crank();
    }
    s.crowd_buy(SOLD_D90 - D80_SOLD);
    for _ in 0..30 {
        s.env.warp(1);
        s.crank();
    }
    let vt = u64::try_from(curve(&s.env, &s.pm).vt).unwrap();
    let b = open_checked(&mut s, caps::max_q_position(vt, 300).unwrap());
    for _ in 0..10 {
        s.env.warp(1);
        s.crank();
    }
    let rt = u64::try_from(curve(&s.env, &s.pm).rt).unwrap();
    s.env.warp(1);
    s.crowd_buy(rt);
    let meta = s.crank();
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpMigrating);
    let t_mig = s.env.now();
    for _ in 0..observed_s {
        s.env.warp(1);
        s.crank();
    }
    let migrator = s.env.funded_keypair();
    let ix = pump::migrate_v2_ix(&s.env.svm, &migrator.pubkey(), &s.pm.curve);
    ok(s.env.send(&[pump::cu_limit_ix(600_000), ix], &[&migrator]));
    let c = curve(&s.env, &s.pm);
    assert_eq!((c.vt, c.vq, c.rt, c.rq), (0, 0, 0, 0), "migrated");
    s.env.warp(1);
    let meta = s.crank();
    venue_changed(&meta, VenueKind::PumpMigrating, VenueKind::PumpSwap);
    let m = market(&s.env, &s.pm);
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1), "the post-change warm-up");
    let pam = PammMarket::graduated(&s.env.svm, &s.pm);
    let payer = s.env.payer.pubkey();
    let ixs = pamm::venue_account_ixs(&s.env.svm, &payer, &pam);
    ok(s.env.send(&ixs, &[]));
    (s, a, b, t_mig, pam)
}

/// The keeper's cranks on the pool every second until `pos` is liquidated there (at most
/// `max_s` seconds): every liquidation before the warm-up ends fails `EmaWarmingUp`. Returns
/// the second after the graduation and the event.
fn liquidate_on_the_pool(s: &mut Sim, pam: &PammMarket, pos: &Pos, max_s: i64) -> (i64, Liquidate) {
    let t0 = s.env.now();
    let kp = s.keeper.insecure_clone();
    loop {
        s.env.warp(1);
        ok(s.env.send(&[crank_price_ix(pam)], &[]));
        let warm = market(&s.env, &s.pm).ema_warm;
        let ix = liquidate_ix(pam, &kp.pubkey(), &pos.key(), pos.nonce);
        let res = s.env.send(&[pump::cu_limit_ix(400_000), ix], &[&kp]);
        if warm > 0 {
            assert_err(&res, ShortwinError::EmaWarmingUp);
        } else if let Ok(meta) = res {
            return (s.env.now() - t0, events::one(&meta));
        }
        assert!(
            s.env.now() - t0 < max_s,
            "liquidated on the pool within {max_s} s"
        );
    }
}

/// `migrate_v2` 10 s after the completion (the keeper observed the completed curve meanwhile):
/// the next crank moves the market onto its pool (M6; in M5 it stayed `PumpMigrating` with a
/// frozen EMA and settled A S3 at 70 min on the cap-only view). A — made liquidatable by the
/// completion — is liquidated **on the pool** once the warm-up is over; B, healthy, closes there.
#[test]
fn a_real_migrate_v2_moves_the_market_onto_its_pool_and_a_is_liquidated_there() {
    let (mut s, a, b, _t_mig, pam) = migrated_after(10);
    let (sec, ev) = liquidate_on_the_pool(&mut s, &pam, &a, 120);
    assert!(sec >= 8, "never before the warm-up's eight integrations");
    eprintln!(
        "migrate_v2 after 10 s: A liquidated on the pool at +{sec} s (buyback {:.4} SOL, penalty {:.4})",
        ev.buyback_cost as f64 / 1e9,
        ev.penalty as f64 / 1e9
    );
    s.env.warp(1);
    ok(s.env.send(&[crank_price_ix(&pam)], &[]));
    ok(s.env.close(&pam, &b.b, b.nonce));
    assert_eq!(market(&s.env, &s.pm).open_positions, 0);
}

/// `migrate_v2` right after the completion (mainnet's usual 0–6 slots; here the keeper's first
/// crank saw the completed curve and the migration landed the next second): in M5 the EMA froze
/// one integration into the completion, A was not liquidatable on it and settled S3 only at its
/// deadline, the lenders paid at the frozen pre-completion price (the recorded M5 exposure). M6's
/// transition observes the pool: the EMA follows the pool's price (by at most one v0.4.1 step per
/// second, re-seeded from the pool's own buckets during the warm-up), A becomes liquidatable on
/// it and is liquidated on the pool — the lenders get `Q` back.
#[test]
fn a_fast_migrate_v2_is_priced_on_the_pool() {
    let (mut s, a, _b, _t_mig, pam) = migrated_after(0);
    let pool_before = shortwin_litesvm_tests::pamm::PammPool::of(&s.env.svm, &s.pm.keys.mint);
    let p_pool = pool_before.spot_wad(&s.env.svm);
    let tokens_free = market(&s.env, &s.pm).tokens_free;
    let q_a = s.position(&a).q;
    let (sec, ev) = liquidate_on_the_pool(&mut s, &pam, &a, 120);
    let p_ema = market(&s.env, &s.pm).ema_price.get();
    eprintln!(
        "fast migrate_v2: A liquidated on the pool at +{sec} s; P_ema / P_pool = {:.3}",
        p_ema as f64 / p_pool as f64
    );
    assert!(p_ema * 10 > p_pool * 7, "the EMA followed the pool");
    assert_eq!(
        market(&s.env, &s.pm).tokens_free,
        tokens_free + q_a,
        "the lenders' tokens are back"
    );
    assert!(ev.buyback_cost > 0);
}
