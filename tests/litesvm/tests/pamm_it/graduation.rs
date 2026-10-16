//! Graduation (ARCH §2.5 "On → PumpSwap", §3.6 precondition 1b, §3.8 step 1b, §5.3, §5.4; M6):
//! a curve market with open positions through its completion (`→ PumpMigrating`, exits pause),
//! a real `migrate_v2` on the dumped pump ELF, the `→ PumpSwap` transition in the next prologue
//! (the pool cache, `graduated_at`, the ring rewritten to the pool's `lp_supply` anchor and the
//! post-change confirmation warm-up, exactly as a Raydium adoption restarts), keeper exits and
//! `sol_settle` gated during the warm-up, the borrower's own close on the pool at any time, and
//! exits on the pool after it. The harness hooks check the transition's effects after every
//! instruction (`invariants::positions`, T-VENUE-SWITCH; I16's cleared chains; I18's restart).

use shortwin_litesvm_tests::pump::{CurveUser, PumpMarket, PumpSpec};

use crate::common::*;

/// A curve market on the near-complete fixture curve with `40·q` lent (`q = vT/200`), warmed up.
fn near_market() -> (Env, PumpMarket, u64) {
    let (mut env, c) = Env::fork_pump_coin(fork::PumpCoin::Near);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(fork::PumpCoin::Near, q * 40), c);
    (env, pm, q)
}

/// An external user buys the curve down to `rt_left` real tokens (0 completes it).
fn buy_down_to(env: &mut Env, pm: &PumpMarket, rt_left: u64) -> CurveUser {
    let u = CurveUser::new(env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(env, &pm.curve, rt - rt_left, u64::MAX));
    u
}

/// `migrate_v2` by a funded user (top-level; ARCH §5.4 tx2).
fn migrate(env: &mut Env, pm: &PumpMarket) -> TransactionMetadata {
    let migrator = env.funded_keypair();
    let ix = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
    ok(env.send(&[cu_limit_ix(600_000), ix], &[&migrator]))
}

/// The venue kind stored on the market.
fn venue(env: &Env, mk: &Pubkey) -> VenueKind {
    env.market(mk).venue_kind().expect("venue")
}

/// The whole graduation with two curve positions open: complete → `PumpMigrating` (exits
/// refuse `VenueMigrating`) → `migrate_v2` → the next crank moves the market onto its canonical
/// pool (`VenueStateChanged PumpMigrating → PumpSwap`, the pool cache, `graduated_at = now`, the
/// ring at the pool's `lp_supply`, `ema_warm = 8`, `ema_guard = 1`) → during the warm-up a keeper's
/// liquidation and expiry fail `EmaWarmingUp`, `sol_settle` evaluates nothing, an open fails
/// `EmaWarmingUp`, while the borrower's own close executes on the pool at the pool's quote; after
/// eight integrations of the pool's own buckets the market is confirmed (`ema_warm = 0`), opens
/// wait for the boost (`BoostInProgress`), and the second position closes on the pool.
#[test]
fn a_curve_market_graduates_onto_its_pool_and_exits_resume_after_the_warm_up() {
    let (mut env, pm, q) = near_market();
    let mut b1 = env.borrower();
    let (res, n1) = env.open(&pm, &mut b1, q);
    ok(res);
    let mut b2 = env.borrower();
    let (res, n2) = env.open(&pm, &mut b2, q / 2);
    ok(res);
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    venue_changed(
        &ok(env.send(&[crank_price_ix(&pm)], &[])),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    env.warp(1);
    assert_err(&env.close(&pm, &b1, n1), E::VenueMigrating);
    migrate(&mut env, &pm);
    let pool = pda::canonical_pool(&pm.keys.mint);
    let lp_supply = vamm::read_pool(
        &vamm::PROGRAM,
        &env.svm.get_account(&pool).expect("pool").data,
    )
    .expect("pool")
    .lp_supply;
    env.warp(1);
    let meta = ok(env.send(&[crank_price_ix(&pm)], &[]));
    let ev = venue_changed(&meta, VenueKind::PumpMigrating, VenueKind::PumpSwap);
    assert_eq!(ev.pool, pool);
    let m = env.market(&pm.market());
    assert_eq!(venue(&env, &pm.market()), VenueKind::PumpSwap);
    assert_eq!(m.graduated_at, env.now());
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1));
    assert_eq!(
        m.ema_liq.get(),
        u128::from(lp_supply),
        "L_ema = the pool's lp_supply"
    );
    assert!(
        m.liq_slow.get() <= u128::from(lp_supply),
        "L_slow never raised"
    );
    assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0));

    // The existence requirement's accounts (the keeper's graduation duty, ARCH §5.4 tx3).
    let pam = PammMarket::graduated(&env.svm, &pm);
    let payer = env.payer.pubkey();
    let ixs = pamm::venue_account_ixs(&env.svm, &payer, &pam);
    ok(env.send(&ixs, &[]));

    // The warm-up: keeper exits and settlements wait; the borrower's close does not.
    env.warp(1);
    crank(&mut env, &pam);
    let kp = keeper(&mut env);
    assert_err(
        &liquidate(&mut env, &pam, &kp, &b1.key(), n1),
        E::EmaWarmingUp,
    );
    let meta = ok(settle(&mut env, &pam, &kp, &b1.key(), n1));
    assert!(
        events::of::<SolSettle>(&meta).is_empty(),
        "no settlement while warming up"
    );
    let mut b3 = env.borrower();
    let (res, _) = env.open(&pam, &mut b3, q / 4);
    assert_err(&res, E::EmaWarmingUp);
    let b_exp = quote_buy(&env, &pam.pool, q / 2).expect("quote");
    let (close2, _) = close_ok(&mut env, &pam, &b2, n2);
    assert_eq!(
        close2.b_live, b_exp,
        "the close buys on the pool at its quote"
    );
    assert_trader_clean(&env, &pam);

    // Eight integrations of the pool's own buckets confirm the state.
    let mut secs = 0;
    while env.market(&pm.market()).ema_warm > 0 {
        env.warp(1);
        crank(&mut env, &pam);
        secs += 1;
        assert!(secs <= 8, "the warm-up ends after 8 integrations");
    }
    let m = env.market(&pm.market());
    assert_eq!(m.ema_warm, 0);
    // Opens wait for the boost (A13).
    let (res, _) = env.open(&pam, &mut b3, q / 4);
    assert_err(&res, E::BoostInProgress);
    // The curve position exits on the pool.
    env.warp(1);
    crank(&mut env, &pam);
    let b_exp = quote_buy(&env, &pam.pool, q).expect("quote");
    let (close1, _) = close_ok(&mut env, &pam, &b1, n1);
    assert_eq!(close1.b_live, b_exp);
    assert_eq!(env.market(&pm.market()).open_positions, 0);
}

/// A graduation with no position open, then a normal PumpSwap market: once the boost is spent
/// (the vault at dust) an open and a close run on the pool to the lamport.
#[test]
fn after_graduation_the_market_trades_on_its_pool() {
    let (mut env, pm, q) = near_market();
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    migrate(&mut env, &pm);
    env.warp(1);
    let meta = ok(env.send(&[crank_price_ix(&pm)], &[]));
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpSwap);
    let pam = PammMarket::graduated(&env.svm, &pm);
    let payer = env.payer.pubkey();
    let ixs = pamm::venue_account_ixs(&env.svm, &payer, &pam);
    ok(env.send(&ixs, &[]));
    crank_secs(&mut env, &pam, 9);
    assert_eq!(env.market(&pm.market()).ema_warm, 0);
    wait_out_boost(&mut env, &pam);
    let q = q.min(q_for(&env, &pam.pool));
    let s_exp = quote_sell(&env, &pam.pool, q).expect("quote");
    let mut b = env.borrower();
    let (open, nonce, _) = open_ok(&mut env, &pam, &mut b, q);
    assert_eq!(open.s, s_exp);
    assert_eq!(open.venue, VenueKind::PumpSwap as u8);
    env.warp(1);
    crank(&mut env, &pam);
    close_ok(&mut env, &pam, &b, nonce);
    assert_trader_clean(&env, &pam);
}
