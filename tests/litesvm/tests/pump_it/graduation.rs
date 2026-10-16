//! Graduation on the curve (ARCH §2.5 `sync_venue`, §3.8 step 4, §5.3, §5.4; M5): the venue
//! state machine `PumpCurve → PumpMigrating` (a completing buyback, an external completion seen
//! by a crank), the migration pause (exits refuse with `VenueMigrating`, S3 only after
//! `max_migration_pause_s + s3_wait_s` — the stuck migration at 70 min, FV on the deterministic
//! post-migration pool), a real `migrate_v2` after which the next crank moves the market onto its
//! canonical pool (M6's `→ PumpSwap`; `pamm_it::graduation` runs the exits there), a curve that
//! graduated elsewhere (`→ None`, the `graduated_elsewhere` flag, the
//! detection set as ReadSet, S3 after the chain: M2-SEC-5 on a real transition), and the
//! `None → PumpCurve` re-detection by `crank_price` (the curve cache, the price start, the
//! collection window cleared). M4's two carry-overs (ARCH §9 M4 → M5): a curve that graduated
//! elsewhere **with a position open** adopts its Raydium pool only through M4's collection window
//! (switch only if all fit; the deepest candidate; the position carried and closed on the new
//! pool), and the `None →` pump re-detection clears a running window (`fp_*`) and the route chains.
//! The M6 security review's curve-side findings: M6-SEC-5 (a full wipe settled during the
//! migration pause waits for the curve-era cashback) and M6-SEC-3 (decided by the owner on
//! 2026-09-30: curve opens refuse `GraduationHalted` exactly while PumpSwap's `create_pool` halt is
//! set; a position opened before it keeps the stalled-graduation path to S3). M10 G-A2: a
//! stranger's real `create_pool(index = 1)` of the coin after the completion never graduates the
//! market (nor do its bytes at the canonical address); the real `migrate_v2` does. pump-final
//! P2: the wipe gate's regressions on a non-cashback curve with lamports seeded into the trader's
//! pump UVA (T-WIPE-GATE: pump's v3 trades refuse a live cashback coin, owner D1 (a)); and a
//! `PumpMigrating` market after pump's synthetic migration — priced on the pool `migrate_v2`
//! creates (the live `curve_base_ata`, `pcQ`; FV ρ-scaled), donations included, no FV when the
//! post-complete fields are not known, a live curve carrying them not executable, a complete
//! cashback curve observed (T-MIG-*, T-LIVE-PC-GUARD, T-LISTING).
//!
//! Every instruction goes through `Env::send`: the invariant hooks run after each one. The only
//! planted state is the venue's (pump's curve account: reserves zeroed as by a migration to
//! another venue; a curve account removed before a listing and restored after it; a completed
//! curve made live again, the one state a re-detection with a window open needs; Raydium CPMM
//! pools of the pump mint planted from the dumped USELESS pool, `pool_select::plant_cpmm`).

use shortwin::events::TraderSwept;
use shortwin::state::{
    MARKET_FLAG_GRADUATED_ELSEWHERE, MARKET_FLAG_WIPE_LIVE, MARKET_FLAG_WIPE_REWARDS_CLAIMED,
    MARKET_FLAG_WIPE_TRADER_SWEPT, Market,
};
use shortwin_litesvm_tests::engine::expire_ix;
use shortwin_litesvm_tests::{ix, pamm};
use shortwin_math::fees::{self, FeeBps, PammFeeSchedule};
use shortwin_math::pool::{WIPE_REWARDS_CLAIMED, WIPE_TRADER_SWEPT};
use shortwin_math::quote::{self as mq, MigrationParams, sol_leg};
use shortwin_math::rent;
use shortwin_venue_state::{pump as vpump, pump_amm as vamm};

use crate::common::*;

/// The venue kind stored on the market.
fn venue(env: &Env, pm: &PumpMarket) -> VenueKind {
    market(env, pm).venue_kind().expect("venue")
}

/// `B̂_FV(q; P)` of a `PumpMigrating` market (ARCH §4.5, `Quoter::fv_repriced`; pump-final §5.4),
/// recomputed from the math crate and the dumped accounts: the pool `migrate_v2` builds from the
/// complete curve — the standard 206.9 M tokens and `rQ − fee`, or after a synthetic pool part the
/// live `curve_base_ata` and `rQ − fee + pcQ` — repriced at `P` (ρ-scaled after a pool part) with
/// its own liquidity, at its PumpSwap tier with the position's hard cap (the risk view).
fn migrating_fv_quote(env: &Env, pm: &PumpMarket, q: u64, p: u128, cap_bps: u64) -> u64 {
    let live = pm.curve.curve_state(&env.svm);
    assert!(live.complete && live.vt > 0, "a stuck migration");
    let c = pm.curve.state(&env.svm);
    let (pool_tokens, post_complete_quote_in) =
        if c.post_complete_base_out != 0 || c.post_complete_quote_in != 0 {
            (
                u128::from(tokens(env, &pm.curve.curve_base_ata)),
                c.post_complete_quote_in,
            )
        } else {
            (shortwin::venues::STANDARD_POOL_TOKENS, 0)
        };
    let g = env
        .svm
        .get_account(&shortwin::constants::PUMP_GLOBAL)
        .unwrap();
    let global = vpump::read_global(&g.owner.to_bytes(), &g.data).unwrap();
    let pfc = env
        .svm
        .get_account(&shortwin::constants::PUMP_AMM_FEE_CONFIG)
        .unwrap();
    let tiers: Vec<fees::FeeTier> = vpump::read_fee_config(&pfc.owner.to_bytes(), &pfc.data)
        .unwrap()
        .fee_tiers
        .iter()
        .map(|t| fees::FeeTier {
            threshold: t.threshold,
            fees: FeeBps::new(t.fees.lp, t.fees.protocol, t.fees.creator),
        })
        .collect();
    let gc = env
        .svm
        .get_account(&shortwin::constants::PUMP_AMM_GLOBAL_CONFIG)
        .unwrap();
    let gcfg = vamm::read_global_config(&gc.owner.to_bytes(), &gc.data).unwrap();
    let m = MigrationParams {
        migration_fee: global.pool_migration_fee(),
        pool_tokens,
        post_complete_quote_in,
    };
    let sched = PammFeeSchedule {
        tiers: &tiers,
        base_supply: 1_000_000_000_000_000,
        creator_fee_configurable: gcfg.creator_fee_configurable(),
        pool_creator_fee_bps: 0,
        coin_creator_is_default: c.creator_is_default(),
        cap_bps: Some(cap_bps),
    };
    let fv = mq::migrating_fv(q, p, &live, &m, &sched).unwrap().cost;
    if post_complete_quote_in == 0 {
        // Without a pool part: the pre-v3 FV on the deterministic pool, to the lamport.
        let (xp, yp) = mq::migrated_pool(&live, &m).unwrap();
        let f = fees::pump_amm(yp, xp, &sched).unwrap();
        let l_pool = mq::liquidity(xp, yp).unwrap();
        let (x, y) = mq::reprice(l_pool, p).unwrap();
        assert_eq!(fv, sol_leg::buy_exact_out(q, x, y, false, &f).unwrap().cost);
    }
    fv
}

/// A buyback of exactly `rT` completes the curve: the close succeeds (pump's charge, `q` back in
/// the lending vault) and the venue sync moves the market to `PumpMigrating` in the same
/// instruction (`VenueStateChanged` before `Close`, `migration_seen_at = now`); the next open
/// is refused with `VenueMigrating`.
#[test]
fn a_completing_buyback_moves_the_market_to_pump_migrating() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(5);
    buy_down_to(&mut env, &pm, q);
    env.warp(1);
    let charge = pm
        .curve
        .charge_buy(&env.svm, q)
        .expect("the completing charge");
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let (close, meta) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.buyback_cost, charge.cost, "B = pump's charge for rT");
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, q);
    let c = pm.curve.state(&env.svm);
    assert!(
        c.complete && c.real_token_reserves == 0,
        "the curve completed"
    );
    let ev = venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpMigrating);
    assert_eq!(ev.market, pm.market());
    assert_eq!(ev.pool, pda::canonical_pool(&pm.keys.mint));
    assert_eq!(ev.ts, env.now());
    let m = market(&env, &pm);
    assert_eq!(venue(&env, &pm), VenueKind::PumpMigrating);
    assert_eq!(m.migration_seen_at, env.now());
    assert!(!m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
    assert_trader_clean(&env, &pm);
    // The event precedes `Close` in the trace.
    let raw = events::raw(&meta);
    let pos = |d: &[u8]| raw.iter().position(|p| p.starts_with(d)).unwrap();
    use anchor_lang::Discriminator;
    assert!(
        pos(shortwin::events::VenueStateChanged::DISCRIMINATOR) < pos(Close::DISCRIMINATOR),
        "VenueStateChanged is emitted before Close"
    );
    env.warp(1);
    let mut b2 = env.borrower();
    let (res, _) = env.open(&pm, &mut b2, q);
    assert_err(&res, E::VenueMigrating);
    // A crank on the migrating curve changes nothing more.
    env.warp(1);
    no_venue_change(&crank(&mut env, &pm));
}

/// A stuck migration (ARCH §5.3, §5.4; the M5 test "stuck-migration S3 at 70 min"): a third
/// party completes the curve 30 min before the position's deadline; the next crank moves the
/// market to `PumpMigrating`; opens, the close, the expiry and a liquidation are refused with
/// `VenueMigrating` (exits pause); the keeper's `sol_settle` does nothing until
/// `migration_seen_at + max_migration_pause_s + s3_wait_s` (70 min with the defaults) on a
/// continuous chain, then settles S3 with FV priced on the deterministic post-migration pool
/// at `P_ema` (the risk view: the position's hard cap), the expiry fee on top (O15).
#[test]
fn a_stuck_migration_pauses_exits_and_settles_s3_after_the_pause() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let kp = keeper(&mut env);
    warp_to_deadline(&mut env, &pm, &b.key(), nonce, -1_800);
    crank(&mut env, &pm);
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    // The completing trade ran outside short.win: the market is still `PumpCurve` until an
    // instruction reads the curve.
    assert_eq!(venue(&env, &pm), VenueKind::PumpCurve);
    env.warp(1);
    let meta = crank(&mut env, &pm);
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpMigrating);
    let t0 = env.now();
    let m = market(&env, &pm);
    assert_eq!((m.migration_seen_at, m.no_route_since), (t0, t0));
    let pos = env.position(&pm, &b.key(), nonce);
    assert_eq!(
        (pos.max_migration_pause_s, pos.s3_wait_s),
        (3_600, 600),
        "the default timers"
    );
    // Exits pause; opens are refused.
    env.warp(1);
    assert_err(&env.close(&pm, &b, nonce), E::VenueMigrating);
    let mut b2 = env.borrower();
    assert_err(&env.open(&pm, &mut b2, q).0, E::VenueMigrating);
    let liq = shortwin_litesvm_tests::engine::liquidate_ix(&pm, &kp.pubkey(), &b.key(), nonce);
    assert_err(&env.send(&[liq], &[&kp]), E::VenueMigrating);
    // The keeper: a crank and a `sol_settle` every 30 s (ECON §5.8 duty 5).
    let due = t0 + i64::from(pos.max_migration_pause_s) + i64::from(pos.s3_wait_s);
    let mut expired_checked = false;
    let ev = loop {
        env.warp(30);
        crank(&mut env, &pm);
        if !expired_checked && env.now() > pos.deadline {
            let ix = expire_ix(&pm, &kp.pubkey(), &b.key(), nonce);
            assert_err(&env.send(&[ix], &[&kp]), E::VenueMigrating);
            expired_checked = true;
        }
        let meta = ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
        if let Some(ev) = settled(&meta) {
            break ev;
        }
        assert!(env.now() < due, "S3 is due at migration_seen_at + 70 min");
    };
    assert!(expired_checked);
    assert!(env.now() >= due && env.now() < due + 30, "S3 at 70 min");
    assert_eq!(ev.condition, 3);
    assert_eq!(venue(&env, &pm), VenueKind::PumpMigrating);
    assert_eq!(
        (ev.pprime_lo, ev.pprime_hi),
        (pos.expiry_fee_bps, pos.expiry_fee_bps)
    );
    let m = market(&env, &pm);
    let b_fv = migrating_fv_quote(
        &env,
        &pm,
        pos.q,
        m.ema_price.get(),
        u64::from(pos.venue_fee_hard_cap_bps),
    );
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let fv =
        b_fv + (u128::from(b_fv) * u128::from(pos.expiry_fee_bps)).div_ceil(10_000) as u64 + r_due;
    assert_eq!(ev.fv, fv, "FV on the post-migration pool at P_ema");
    let take = pos.v - k_s(&pos);
    assert_eq!(ev.lender_amount, fv.min(take));
    assert!(read::account(&env.svm, &pda::position(&pm.market(), &b.key(), nonce).0).is_none());
}

/// `migrate_v2` on the real pump ELF (anyone may call it) after the completion: the canonical
/// pool appears with exactly the deterministic reserves short.win prices on a complete curve
/// (`X_p` = the curve's 206.9 M pool tokens, effective quote `Y_p = rQ − pool_migration_fee`)
/// and the boost of `⌊0.2069·Y⌋`; the curve's reserves are zeroed. (Priced before the
/// completion, `Y_p` is one lamport above:
/// `straddle::b_v3_prices_the_live_exit_and_b_str_the_post_migration_pool`.) The next crank moves
/// the market onto the pool (M6: `PumpMigrating → PumpSwap`, the ring rewritten to the pool's
/// `lp_supply`, the post-change warm-up; in M5 it stayed `PumpMigrating`); the curve's account
/// lists no longer fit the market (`VenueAccountMismatch`).
#[test]
fn migrate_v2_on_the_real_elf_graduates_the_market_onto_its_pool() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    let live = pm.curve.curve_state(&env.svm);
    let g = env
        .svm
        .get_account(&shortwin::constants::PUMP_GLOBAL)
        .unwrap();
    let fee = vpump::read_global(&g.owner.to_bytes(), &g.data)
        .unwrap()
        .pool_migration_fee();
    let pool_tokens = u128::from(tokens(&env, &pm.curve.curve_base_ata));
    let (xp, yp) = mq::migrated_pool(
        &live,
        &MigrationParams {
            migration_fee: fee,
            pool_tokens,
            post_complete_quote_in: 0,
        },
    )
    .unwrap();
    let migrator = env.funded_keypair();
    let ix = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
    let meta = ok(env.send(&[pump::cu_limit_ix(600_000), ix], &[&migrator]));
    eprintln!(
        "migrate_v2: {} CU, {} inner instructions",
        meta.compute_units_consumed,
        meta.inner_instructions.iter().map(Vec::len).sum::<usize>()
    );
    let np = pump::NewPool::of(&pm.curve);
    let acc = env.svm.get_account(&np.pool).expect("the canonical pool");
    assert_eq!(
        acc.data.len(),
        shortwin_litesvm_tests::pamm::POOL_LEN_2026_10,
        "a new pool is 287 bytes (271 before the 2026-10-02 redeploy)"
    );
    let r = np.reserves(&env.svm).expect("reserves");
    assert_eq!(u128::from(r.base), xp, "X_p");
    assert_eq!(pool_tokens, shortwin::venues::STANDARD_POOL_TOKENS);
    assert_eq!(
        r.effective_quote, yp,
        "Y_p of the complete curve (rQ − migration fee) is the pool's effective quote"
    );
    let st = np.state(&env.svm).unwrap();
    assert_eq!(
        st.virtual_quote_reserves,
        i128::try_from(r.effective_quote * 2_069 / 10_000).unwrap(),
        "the boost"
    );
    let c = pm.curve.state(&env.svm);
    assert_eq!(
        (c.virtual_token_reserves, c.real_token_reserves),
        (0, 0),
        "migrated"
    );
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpMigrating,
        VenueKind::PumpSwap,
    );
    assert_eq!(venue(&env, &pm), VenueKind::PumpSwap, "M6 moves it on");
    let m = market(&env, &pm);
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1));
    assert_eq!(m.ema_liq.get(), u128::from(st.lp_supply));
    env.warp(1);
    assert_err(&env.close(&pm, &b, nonce), E::VenueAccountMismatch);
}

/// A curve that completed and then migrated **elsewhere** (reserves zeroed, no canonical pool;
/// planted on pump's account) with a position open: the next crank moves `PumpMigrating →
/// None` and sets `graduated_elsewhere`; from then on the market's ReadSet is the detection set
/// (the curve's own set is refused), exits have no route (`NoRoute`), and an expired position
/// settles S3 once the chain has run `s3_wait_s` (M2-SEC-5 on a real transition): FV at the
/// cap-only risk view at the frozen EMA point.
#[test]
fn a_curve_that_graduates_elsewhere_goes_list_only_and_settles_s3() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let kp = keeper(&mut env);
    warp_to_deadline(&mut env, &pm, &b.key(), nonce, 1);
    crank(&mut env, &pm);
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    // Pump migrates it to another venue: reserves zeroed, no canonical pool.
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    env.warp(1);
    let meta = crank(&mut env, &pm);
    let ev = venue_changed(&meta, VenueKind::PumpMigrating, VenueKind::None);
    assert_eq!(ev.pool, pda::canonical_pool(&pm.keys.mint));
    let m = market(&env, &pm);
    assert_eq!(venue(&env, &pm), VenueKind::None);
    assert!(m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
    // The detection set is the ReadSet now.
    env.warp(1);
    assert_err(
        &env.send(&[crank_price_ix(&pm)], &[]),
        E::VenueAccountMismatch,
    );
    let none = AsNone(&pm);
    crank(&mut env, &none);
    let pos = env.position(&pm, &b.key(), nonce);
    let ix = expire_ix(&pm, &kp.pubkey(), &b.key(), nonce);
    assert_err(&env.send(&[ix], &[&kp]), E::NoRoute);
    let m0 = market(&env, &pm);
    let since = m0.no_route_since;
    assert!(since > 0);
    let ev = loop {
        env.warp(30);
        crank(&mut env, &none);
        let meta = ok(settle(&mut env, &none, &kp, &b.key(), nonce));
        if let Some(ev) = settled(&meta) {
            break ev;
        }
        assert!(env.now() < since + i64::from(pos.s3_wait_s));
    };
    assert_eq!(ev.condition, 3);
    assert!(env.now() >= since + i64::from(pos.s3_wait_s));
    let m = market(&env, &pm);
    let (p, l) = (m.ema_price.get(), m.ema_liq.get());
    assert_eq!((p, l), (m0.ema_price.get(), m0.ema_liq.get()), "frozen");
    assert_eq!((ev.p_lo, ev.p_hi, ev.l_lo, ev.l_hi), (p, p, l, l));
    let cap = shortwin_math::quote::Pricing::PumpSwap(FeeBps::new(
        0,
        u64::from(pos.venue_fee_hard_cap_bps),
        0,
    ));
    let bq = cap.buy_repriced(pos.q, l, p).unwrap().cost;
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let fv = bq + (u128::from(bq) * u128::from(pos.expiry_fee_bps)).div_ceil(10_000) as u64 + r_due;
    assert_eq!(ev.fv, fv, "FV at the cap-only risk view");
    assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
}

/// A live curve whose completion and migration elsewhere both happened between two reads:
/// `PumpCurve → None` directly (complete, `vT = 0`, no canonical pool).
#[test]
fn a_live_curve_seen_migrated_elsewhere_goes_straight_to_none() {
    let (mut env, pm, _q) = setup(PumpCoin::T22_115);
    fork::set_curve_complete(&mut env.svm, &pm.curve.curve, true);
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    env.warp(1);
    let meta = crank(&mut env, &pm);
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::None);
    assert!(market(&env, &pm).has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
}

/// `None → PumpCurve` by `crank_price` (ARCH §2.5 `None` row): a mint listed before its curve
/// was readable (the curve account removed for the listing, restored after) is re-detected by
/// the next crank on its detection set: `VenueStateChanged(None → PumpCurve)`, the curve cache
/// written (as a listing does), the price state started at the curve's spot, Tier D, the S3
/// chain and the collection window cleared. The crank creates no account: the trader's base
/// ATA and pump UVA are pre-created by anyone (here the payer) before the first open, which is
/// then exact.
#[test]
fn a_list_only_market_is_redetected_as_a_curve() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
    let saved = env.svm.get_account(&c.curve).unwrap();
    env.svm
        .set_account(c.curve, solana_account::Account::default())
        .unwrap();
    // A list-only listing takes no init set.
    let creator = env.funded_keypair();
    let list = shortwin_litesvm_tests::ix::create_market(
        &creator.pubkey(),
        &c.mint,
        &c.token_program.id(),
        shortwin::instructions::create_market::VENUE_HINT_AUTO,
        &pda::canonical_pool(&c.mint),
    );
    ok(env.send(&[list], &[&creator]));
    let market_key = pda::market(&c.mint).0;
    assert_eq!(
        env.market(&market_key).venue_kind(),
        Some(VenueKind::None),
        "no curve: list-only"
    );
    env.svm.set_account(c.curve, saved).unwrap();
    let vt = c.state(&env.svm).virtual_token_reserves;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::T22_115, (vt / 200) * 40), c);
    let meta = crank(&mut env, &AsNone(&pm));
    let ev = venue_changed(&meta, VenueKind::None, VenueKind::PumpCurve);
    assert_eq!(ev.ts, env.now());
    let m: Market = market(&env, &pm);
    assert_eq!(venue(&env, &pm), VenueKind::PumpCurve);
    assert_eq!(m.tier, shortwin_math::tier::Tier::D as u8);
    assert_eq!(m.curve_base_ata, pm.curve.curve_base_ata);
    assert_eq!(m.cached_creator_vault, pm.creator_vault);
    let s = pm.curve.state(&env.svm);
    let (vt, vq) = (
        u128::from(s.virtual_token_reserves),
        u128::from(s.virtual_quote_reserves),
    );
    assert_eq!(m.ema_price.get(), vq * shortwin_math::num::WAD / vt);
    assert_eq!(m.fp_t0, 0, "no collection window");
    assert_eq!(m.fp_d.get(), 0);
    assert_eq!(m.no_route_since, 0);
    // The crank created nothing: the first open needs the trader's accounts.
    let trader = pm.trader();
    assert!(read::account(&env.svm, &pm.trader_base_ata).is_none());
    let payer = env.payer.pubkey();
    let program = pm.curve.token_program.id();
    let ixs = [
        fork::create_ata_idempotent_ix(&payer, &trader, &pm.keys.mint, &program),
        fork::pump_init_uva_ix(&payer, &trader),
    ];
    ok(env.send(&ixs, &[]));
    env.warp(i64::from(env.config().params.market_warmup_s));
    let q = q_for(&env, &pm);
    crate::flows::round_trip(&mut env, &pm, q);
}

// ---- M4's carry-overs (ARCH §2.5 `None` row, §9 M4 / M5) ------------------------------------------

/// A CPMM pool for `pm`'s mint planted at the curve's spot now (WSOL token 0, `λ` 100 %, the
/// USELESS template's config), `tokens` deep: a Raydium pool the coin could have graduated to.
fn plant_at_spot(env: &mut Env, pm: &PumpMarket, tokens: u64) -> fork::AnyPool {
    use shortwin_litesvm_tests::pool_select::{PlantSpec, plant_cpmm};
    let s = pm.curve.state(&env.svm);
    let sol = (u128::from(tokens) * u128::from(s.virtual_quote_reserves)
        / u128::from(s.virtual_token_reserves)) as u64;
    fork::AnyPool::Cpmm(plant_cpmm(
        env,
        fork::CpmmFixture::Useless,
        PlantSpec {
            mint: pm.keys.mint,
            program: pm.curve.token_program,
            sol_first: true,
            sol,
            tokens,
            lp_counter: 1_000_000_000_000,
            lambda_bps: 10_000,
        },
    ))
}

/// The curve completes with a position open, then pump migrates it **elsewhere** (reserves zeroed,
/// no canonical pool; planted on pump's account): `PumpCurve → PumpMigrating → None` with the
/// `graduated_elsewhere` flag. Returns the market, the borrower, the position's nonce and `q`.
fn graduated_elsewhere_with_a_position() -> (Env, PumpMarket, Borrower, u64, u64) {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    crank(&mut env, &pm);
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    // Returned before pump zeroes the curve: a test plants its pools at the completed spot first.
    (env, pm, b, nonce, q)
}

/// Pump migrates the completed curve elsewhere (reserves zeroed, no canonical pool) and a crank
/// sees it: `PumpMigrating → None`, `graduated_elsewhere`.
fn migrate_elsewhere(env: &mut Env, pm: &PumpMarket) {
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    env.warp(1);
    venue_changed(&crank(env, pm), VenueKind::PumpMigrating, VenueKind::None);
    let m = market(env, pm);
    assert_eq!(m.venue_kind(), Some(VenueKind::None));
    assert!(m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
    assert_eq!(m.open_positions, 1);
}

/// **M4 carry-over** (ARCH §2.5 `None` row and footnote *, M4-SEC-4; §9 M5): a curve market whose
/// curve graduates **elsewhere** with a position open becomes list-only (`→ None`) with the
/// position open, and its Raydium pool is adopted only through M4's collection window, on the real
/// transition (M4's `m4_sec_4_*` simulated it by rewriting the venue byte): a pool the position
/// does not fit is refused outright (switch only if all fit, `CarriedPositionsTooLarge`); a fitting
/// one opens the window (`PoolProposed`, kind fast — no first-come adoption); a deeper one replaces
/// it inside the window and the first can no longer come back (`PoolNotDominant`); the execution
/// before the window ends is refused (`PoolSwitchNotDue`); after `pool_fast_window_s` the deepest
/// is adopted (`PoolSwitched`, kind fast) with the position carried (its locked terms unchanged),
/// the market leaves the no-route chain, and the borrower closes on the new pool. The pools are
/// planted CPMM pools of the pump mint at the completed curve's price (the dumped ELF trades them).
#[test]
fn a_curve_that_graduated_elsewhere_adopts_its_raydium_pool_through_the_window() {
    use shortwin::events::{POOL_KIND_FAST, PoolProposed, PoolSwitched};
    use shortwin_litesvm_tests::pool_select::{carried_caps, fixture_of, pool_key};
    use shortwin_litesvm_tests::raydium::RayMarket;

    let (mut env, pm, b, nonce, q) = graduated_elsewhere_with_a_position();
    // Candidates priced at the completed curve's spot (where the EMA stands).
    let thin = plant_at_spot(&mut env, &pm, 100 * q);
    let fits = plant_at_spot(&mut env, &pm, 400 * q);
    let deep = plant_at_spot(&mut env, &pm, 1_200 * q);
    migrate_elsewhere(&mut env, &pm);
    let pos_key = pda::position(&pm.market(), &b.key(), nonce).0;
    let pos0 = env.svm.get_account(&pos_key).unwrap().data;
    let since = market(&env, &pm).no_route_since;
    assert!(since > 0, "no route: the S3 chain runs");
    let (cap_thin, _) = carried_caps(&env, &thin);
    let (cap_fits, _) = carried_caps(&env, &fits);
    assert!(
        cap_thin < q && q <= cap_fits,
        "the thin pool's cap {cap_thin} < q {q} ≤ the fitting pool's {cap_fits}"
    );

    // Switch only if all fit: the thin pool cannot open the window.
    assert_err(&env.propose(None, &thin), E::CarriedPositionsTooLarge);
    assert_eq!(market(&env, &pm).fp_t0, 0, "no window");
    // A fitting pool opens the window; nothing is adopted first-come.
    let meta = ok(env.propose(None, &fits));
    assert!(
        events::of::<PoolSwitched>(&meta).is_empty(),
        "no first-come adoption"
    );
    let w: PoolProposed = events::one(&meta);
    assert_eq!((w.kind, w.candidate), (POOL_KIND_FAST, pool_key(&fits)));
    assert_eq!(market(&env, &pm).venue_kind(), Some(VenueKind::None));
    // A deeper pool replaces the window's candidate; the shallower cannot come back.
    env.warp(30);
    let w2: PoolProposed = events::one(&ok(env.propose(None, &deep)));
    assert_eq!(w2.candidate, pool_key(&deep));
    assert_eq!(w2.window_ends, w.window_ends, "the window is not extended");
    assert_err(&env.propose(None, &fits), E::PoolNotDominant);
    assert_err(&env.execute_switch_list_only(&deep), E::PoolSwitchNotDue);
    // After the window: the deepest is adopted with the position carried.
    let now = env.now();
    env.warp(w.window_ends - now);
    let meta = ok(env.execute_switch_list_only(&deep));
    let e: PoolSwitched = events::one(&meta);
    assert_eq!((e.kind, e.candidate), (POOL_KIND_FAST, pool_key(&deep)));
    let m = market(&env, &pm);
    assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumCpmm));
    assert_eq!(m.open_positions, 1);
    assert_eq!(
        (m.fp_t0, m.no_route_since),
        (0, 0),
        "window and chain cleared"
    );
    assert_eq!(
        env.svm.get_account(&pos_key).unwrap().data,
        pos0,
        "the position (its locked terms) is carried byte for byte"
    );
    // The position closes on the new pool.
    let amm_config = match &deep {
        fork::AnyPool::Cpmm(p) => Some(p.amm_config),
        fork::AnyPool::V4(_) => None,
    };
    let rm = RayMarket {
        fixture: fixture_of(&deep),
        pool: deep,
        keys: pm.keys,
        trader_wsol: pda::trader_wsol(&pm.market()).0,
        lender: pm.lender.insecure_clone(),
        lender_account: pm.lender_account,
        amm_config,
    };
    env.warp(1);
    crank(&mut env, &rm);
    let meta = ok(env.close(&rm, &b, nonce));
    let c: Close = events::one(&meta);
    assert_eq!(c.q, q);
    assert_eq!(market(&env, &pm).open_positions, 0);
    println!(
        "graduated elsewhere → None with a position (q {q}): a pool of 100·q tokens (above the window floor) refused (its cap {cap_thin} < q), one of 400·q (cap {cap_fits}) opened the window, one of 1,200·q replaced it and was adopted {} s after the transition; the position closed on it (buyback {} lamports)",
        w.window_ends - since,
        c.buyback_cost,
    );
}

/// **M4 carry-over** (ARCH §2.5 "Precedence", D1; §9 M4 → M5): `crank_price`'s `None →` pump
/// re-detection **clears a collection window**. A list-only market with a position open (a curve
/// that graduated elsewhere) has a Raydium window running; its detection set then shows a live
/// pump curve again (planted on pump's account: a state mainnet cannot reach — a migration is
/// final — and the only way to reach this branch with a window open, as M4's `pump_appears`
/// variant planted it) and the next crank moves the market onto the curve (`VenueStateChanged
/// None → PumpCurve`), clearing `fp_*` and the no-route chain; the window's execution then has
/// nothing to resolve, and the position closes on the curve.
#[test]
fn a_none_to_pump_redetection_clears_the_collection_window() {
    use shortwin::events::PoolProposed;

    let (mut env, pm, b, nonce, q) = graduated_elsewhere_with_a_position();
    let saved = env.svm.get_account(&pm.curve.curve).unwrap();
    let fits = plant_at_spot(&mut env, &pm, 1_200 * q);
    migrate_elsewhere(&mut env, &pm);
    let _: PoolProposed = events::one(&ok(env.propose(None, &fits)));
    let m = market(&env, &pm);
    assert!(m.fp_t0 != 0 && m.fp_d.get() > 0, "a window runs");
    // The detection set shows a live curve again (the saved completed curve, un-completed, with a
    // third of its tokens back: a live SOL curve).
    env.svm.set_account(pm.curve.curve, saved).unwrap();
    fork::set_curve_complete(&mut env.svm, &pm.curve.curve, false);
    let s = pm.curve.state(&env.svm);
    fork::set_curve_reserves(
        &mut env.svm,
        &pm.curve.curve,
        s.virtual_token_reserves + 3 * q,
        s.virtual_quote_reserves,
        3 * q,
        s.real_quote_reserves,
    );
    let base_ata = pm.curve.curve_base_ata;
    let held = tokens(&env, &base_ata);
    fork::set_token_amount(&mut env.svm, &base_ata, held + 3 * q);
    env.warp(1);
    let meta = crank(&mut env, &AsNone(&pm));
    venue_changed(&meta, VenueKind::None, VenueKind::PumpCurve);
    let m = market(&env, &pm);
    assert_eq!(m.venue_kind(), Some(VenueKind::PumpCurve));
    assert_eq!(
        (m.fp_t0, m.fp_d.get(), m.fp_keys, m.no_route_since),
        (0, 0, [Pubkey::default(); 6], 0),
        "the collection window and the no-route chain are cleared"
    );
    // The window has nothing left to resolve; the position closes on the curve.
    assert_err(&env.execute_switch_list_only(&fits), E::NoPendingSwitch);
    env.warp(1);
    crank(&mut env, &pm);
    let (c, _) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(c.q, q);
}

// ---- the M6 security review (curve side) --------------------------------------------------------

/// The seeding lender withdraws every free token (every share whose tokens `T` covers).
fn withdraw_all_free(env: &mut Env, pm: &PumpMarket) {
    let m = market(env, pm);
    let pool = m.pool();
    let want = m.tokens_free;
    let lender = env
        .lender(&pm.market(), &pm.lender.pubkey())
        .expect("lender");
    let (mut lo, mut hi) = (0u128, lender.shares);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if shortwin_math::pool::tokens_for_shares(&pool, mid).unwrap() <= u128::from(want) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let kp = pm.lender.insecure_clone();
    ok(env.send(&[ix::withdraw(&pm.lender_keys(), lo, want)], &[&kp]));
    assert_eq!(market(env, pm).tokens_free, 0);
}

/// The cashback pump's `claim_cashback` pays `user` now: its pump UVA's lamports above the
/// account's rent-exempt minimum (pump's `PUMP_CASHBACK_README`).
fn claimable_cashback(env: &Env, user: &Pubkey) -> u64 {
    let uva = fork::pump_user_volume_accumulator(user);
    let acc = env.svm.get_account(&uva).expect("the trader's pump UVA");
    acc.lamports - env.svm.minimum_balance_for_rent_exemption(acc.data.len())
}

/// T-WIPE-GATE (pump-final §14, C6): the lamports seeded into the trader's pump UVA where a cashback
/// coin's trades used to pay its cashback — pump's v3 trades refuse a live cashback coin and
/// short.win no longer lists one (owner D1 (a)), but `claim_cashback` pays any balance above the
/// UVA's rent, donated lamports included, so the wipe gate (`engine::lender::rewards_required`,
/// `sweep_trader`'s pump-UVA leg) stays needed and reachable. Above a 0-byte account's rent-exempt
/// minimum, as keeper-litesvm's `wipe` scenario needs.
const SEEDED_CASHBACK: u64 = 1_000_000;

/// Seeds [`SEEDED_CASHBACK`] into the trader's pump UVA (a System transfer anyone can make) and
/// returns what `claim_cashback` would pay now.
fn seed_uva(env: &mut Env, pm: &PumpMarket) -> u64 {
    assert!(
        !pm.curve.state(&env.svm).is_cashback_coin,
        "a non-cashback curve (D1 (a))"
    );
    let trader = pm.trader();
    let before = claimable_cashback(env, &trader);
    env.fund(
        &fork::pump_user_volume_accumulator(&trader),
        SEEDED_CASHBACK,
    );
    let after = claimable_cashback(env, &trader);
    assert_eq!(after - before, SEEDED_CASHBACK);
    after
}

/// The keeper's chain through a stuck migration (ARCH §5.3): a crank and a `sol_settle` every
/// 30 s until the position settles; returns the event and the second it settled at.
fn settle_through_the_pause(
    env: &mut Env,
    pm: &PumpMarket,
    kp: &solana_keypair::Keypair,
    b: &Pubkey,
    nonce: u64,
    mut each: impl FnMut(&mut Env),
) -> (shortwin::events::SolSettle, i64) {
    for _ in 0..200 {
        env.warp(30);
        crank(env, pm);
        each(env);
        let meta = ok(settle(env, pm, kp, b, nonce));
        if let Some(ev) = settled(&meta) {
            return (ev, env.now());
        }
    }
    panic!("never settled");
}

/// ★ M6-SEC-5. `engine::lender::rewards_required` left out `PumpMigrating`, so a full wipe that
/// settled during the migration pause (S3 is the only settlement there) let a deposit end the wipe
/// record without `wipe_rewards_claimed`: a legacy cashback curve's pre-wipe cashback, still in the
/// trader's pump UVA, then went to the next epoch's lenders (R4A-7). Reproduced on the pre-fix build:
/// the deposit below landed right after the S3 wipe, with the cashback unclaimed. Now the deposit
/// waits (`WipeSweepPending`) while the market is `PumpMigrating` too; `sweep_trader` alone sets the
/// trader bit only (cashback outstanding); `[pump claim_cashback(user = trader), sweep_trader]` —
/// no signature but the fee payer's — credits the cashback to the wiped epoch's record and sets
/// both bits (no deadlock: the curve kinds' sweep sets the bit once nothing is claimable), and the
/// deposit lands. T-WIPE-GATE: since D1 (a) on a non-cashback curve, the claimable balance seeded
/// into the trader's pump UVA after the open ([`seed_uva`]) — every wipe-gate assertion kept.
#[test]
fn m6_sec_5_a_full_wipe_during_the_migration_pause_waits_for_the_curve_cashback() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let market_key = pm.market();
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let trader = pm.trader();
    assert!(
        seed_uva(&mut env, &pm) > 0,
        "a claimable balance in the trader's pump UVA"
    );
    withdraw_all_free(&mut env, &pm);
    // 30 min before the position's deadline an outside buy completes the curve and nobody
    // migrates it: the market pauses, and the expired position settles S3 after the pause (ARCH
    // §5.3; `a_stuck_migration_pauses_exits_and_settles_s3_after_the_pause`).
    warp_to_deadline(&mut env, &pm, &b.key(), nonce, -1_800);
    crank(&mut env, &pm);
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    let t0 = env.now();
    let kp = keeper(&mut env);
    let (ev, at) = settle_through_the_pause(&mut env, &pm, &kp, &b.key(), nonce, |_| {});
    assert_eq!(ev.condition, 3, "S3 during the migration pause");
    assert!(at >= t0 + 4_200, "after max_migration_pause_s + s3_wait_s");
    let m = market(&env, &pm);
    assert_eq!(venue(&env, &pm), VenueKind::PumpMigrating);
    assert_eq!(m.total_shares.get(), 0);
    assert!(m.has_flag(MARKET_FLAG_WIPE_LIVE), "a full wipe");
    let cashback = claimable_cashback(&env, &trader);
    assert!(cashback > 0, "the pre-wipe cashback is still in the UVA");

    // A new lender (tokens from a balance planted in its own account: the curve cannot sell).
    let amount = q;
    let lender2 = env.funded_keypair();
    let acc = fork::fund_token(
        &mut env,
        &lender2.pubkey(),
        &pm.keys.mint,
        pm.curve.token_program,
        amount,
    );
    let k = ix::LenderKeys {
        owner: lender2.pubkey(),
        mint: pm.keys.mint,
        token_program: pm.keys.token_program,
        owner_token_account: acc,
    };
    let deposit = |env: &Env| {
        let m = env.market(&market_key);
        ix::with_remaining(
            ix::deposit(&k, amount, 0),
            &ix::epoch_records(&market_key, 0, m.share_epoch),
        )
    };
    // The fix: the deposit waits for the venue-reward bit on `PumpMigrating` as well.
    let d0 = deposit(&env);
    assert_err(&env.send(&[d0], &[&lender2]), E::WipeSweepPending);

    // The sweep alone: the trader bit, not the venue-reward bit; the deposit still waits.
    let meta = ok(env.send(&[pump::sweep_trader_pump_ix(&env, &pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(ev.wipe_bits & WIPE_TRADER_SWEPT, WIPE_TRADER_SWEPT);
    assert_eq!(
        ev.wipe_bits & WIPE_REWARDS_CLAIMED,
        0,
        "cashback outstanding"
    );
    assert!(market(&env, &pm).has_flag(MARKET_FLAG_WIPE_TRADER_SWEPT));
    let d0 = deposit(&env);
    assert_err(&env.send(&[d0], &[&lender2]), E::WipeSweepPending);

    // The unlocking bundle, signed by the fee payer only.
    let claim = pump::claim_cashback_ix(&trader);
    assert!(claim.accounts.iter().all(|a| !a.is_signer));
    let lender_sol0 = env.market_vault(&market_key).lender_sol;
    let meta = ok(env.send(&[claim, pump::sweep_trader_pump_ix(&env, &pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(
        ev.lamports, cashback,
        "the whole cashback, through the trader"
    );
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    assert!(market(&env, &pm).has_flag(MARKET_FLAG_WIPE_REWARDS_CLAIMED));
    assert_eq!(claimable_cashback(&env, &trader), 0);
    assert_eq!(
        env.market_vault(&market_key).lender_sol - lender_sol0,
        cashback,
        "credited to the wiped epoch's record"
    );
    let d1 = deposit(&env);
    ok(env.send(&[d1], &[&lender2]));
    assert!(!market(&env, &pm).has_flag(MARKET_FLAG_WIPE_LIVE));
}

/// ★ AIR-05 (I-05). `engine::lender::rewards_required` read the market's current venue kind, so a
/// curve market that left pump — `→ None` once its curve graduated elsewhere, then onto a Raydium
/// pool — dropped the `wipe_rewards_claimed` requirement while the trader's curve-era cashback still
/// sat in its pump UVA (`Market.pump_uva` survives both moves; pump's `claim_cashback` is
/// permissionless), and `sweep_trader` stopped reading that UVA. A full wipe settled on `None` (S3)
/// was ended by anyone's `[sweep_trader, deposit]`, and the cashback claimed afterwards vested to the
/// new lenders instead of the wiped ones. Reproduced on the pre-fix build: the deposit below landed
/// right after the wipe, with the cashback unclaimed. Now every market with a pump UVA requires the
/// venue-reward sweep and `sweep_trader` reads the UVA there (it alone is the token leg off the pump
/// venues): the deposit waits on `None` and still after the Raydium adoption, until `[pump
/// claim_cashback(user = trader), sweep_trader]` credits the cashback to the wiped epoch's record.
/// T-WIPE-GATE: since D1 (a) on a non-cashback curve, the claimable balance seeded into the trader's
/// pump UVA after the open ([`seed_uva`]) — every wipe-gate assertion kept.
#[test]
fn air05_a_full_wipe_after_the_market_left_pump_waits_for_the_curve_cashback() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let market_key = pm.market();
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let trader = pm.trader();
    assert!(
        seed_uva(&mut env, &pm) > 0,
        "a claimable balance in the trader's pump UVA"
    );
    withdraw_all_free(&mut env, &pm);
    // The position expires; the curve completes and pump migrates it elsewhere: `→ None`.
    let kp = keeper(&mut env);
    warp_to_deadline(&mut env, &pm, &b.key(), nonce, 1);
    crank(&mut env, &pm);
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    let pool = plant_at_spot(&mut env, &pm, 1_200 * q);
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpMigrating,
        VenueKind::None,
    );
    // The S3 chain on `None` settles the last borrowed tokens: a full wipe.
    let none = AsNone(&pm);
    let ev = loop {
        env.warp(30);
        crank(&mut env, &none);
        let meta = ok(settle(&mut env, &none, &kp, &b.key(), nonce));
        if let Some(ev) = settled(&meta) {
            break ev;
        }
    };
    assert_eq!(ev.condition, 3);
    let m = market(&env, &pm);
    assert_eq!(m.venue_kind(), Some(VenueKind::None));
    assert!(m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
    assert_eq!(m.pump_uva, fork::pump_user_volume_accumulator(&trader));
    assert_eq!(m.total_shares.get(), 0);
    assert!(m.has_flag(MARKET_FLAG_WIPE_LIVE), "a full wipe");
    let cashback = claimable_cashback(&env, &trader);
    assert!(cashback > 0, "the curve-era cashback is still in the UVA");

    // A new lender, and the trader sweep anyone can send first.
    let amount = q;
    let lender2 = env.funded_keypair();
    let acc = fork::fund_token(
        &mut env,
        &lender2.pubkey(),
        &pm.keys.mint,
        pm.curve.token_program,
        amount,
    );
    let k = ix::LenderKeys {
        owner: lender2.pubkey(),
        mint: pm.keys.mint,
        token_program: pm.keys.token_program,
        owner_token_account: acc,
    };
    let deposit = |env: &Env| {
        let m = env.market(&market_key);
        ix::with_remaining(
            ix::deposit(&k, amount, 0),
            &ix::epoch_records(&market_key, 0, m.share_epoch),
        )
    };
    let sweep = |env: &Env| shortwin_litesvm_tests::engine::sweep_trader_ix_for(env, &market_key);
    let meta = ok(env.send(&[sweep(&env)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(ev.wipe_bits & WIPE_TRADER_SWEPT, WIPE_TRADER_SWEPT);
    assert_eq!(
        ev.wipe_bits & WIPE_REWARDS_CLAIMED,
        0,
        "cashback outstanding"
    );
    // The fix: the deposit waits for the venue-reward bit on `None` …
    let d = deposit(&env);
    assert_err(&env.send(&[d], &[&lender2]), E::WipeSweepPending);

    // … and still once the market has adopted a Raydium pool of its mint (list-only, no
    // position left: adopted at once).
    env.warp(1);
    ok(env.propose(None, &pool));
    assert_eq!(market(&env, &pm).venue_kind(), Some(VenueKind::RaydiumCpmm));
    let meta = ok(env.send(&[sweep(&env)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(ev.wipe_bits & WIPE_REWARDS_CLAIMED, 0);
    let d = deposit(&env);
    assert_err(&env.send(&[d], &[&lender2]), E::WipeSweepPending);

    // The unlocking bundle, signed by the fee payer only: the cashback reaches the wiped record.
    let lender_sol0 = env.market_vault(&market_key).lender_sol;
    let meta = ok(env.send(&[pump::claim_cashback_ix(&trader), sweep(&env)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(
        ev.lamports, cashback,
        "the whole cashback, through the trader"
    );
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    assert_eq!(claimable_cashback(&env, &trader), 0);
    assert_eq!(
        env.market_vault(&market_key).lender_sol - lender_sol0,
        cashback,
        "credited to the wiped epoch's record"
    );
    let d = deposit(&env);
    ok(env.send(&[d], &[&lender2]));
    assert!(!market(&env, &pm).has_flag(MARKET_FLAG_WIPE_LIVE));
}

/// M6-SEC-5, no deadlock: on a non-cashback curve the same S3 wipe during the migration pause is
/// unlocked by `sweep_trader` alone (the pump UVA holds nothing claimable — unfunded, unlike the
/// T-WIPE-GATE twins: both bits at once).
#[test]
fn m6_sec_5_without_cashback_the_sweep_alone_unlocks_a_migrating_market() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let market_key = pm.market();
    assert!(!pm.curve.state(&env.svm).is_cashback_coin);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    withdraw_all_free(&mut env, &pm);
    warp_to_deadline(&mut env, &pm, &b.key(), nonce, -1_800);
    crank(&mut env, &pm);
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    crank(&mut env, &pm);
    let kp = keeper(&mut env);
    let (ev, _) = settle_through_the_pause(&mut env, &pm, &kp, &b.key(), nonce, |_| {});
    assert_eq!(ev.condition, 3);
    assert_eq!(venue(&env, &pm), VenueKind::PumpMigrating);
    assert!(market(&env, &pm).has_flag(MARKET_FLAG_WIPE_LIVE));
    let meta = ok(env.send(&[pump::sweep_trader_pump_ix(&env, &pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    let amount = q;
    let lender2 = env.funded_keypair();
    let acc = fork::fund_token(
        &mut env,
        &lender2.pubkey(),
        &pm.keys.mint,
        pm.curve.token_program,
        amount,
    );
    let k = ix::LenderKeys {
        owner: lender2.pubkey(),
        mint: pm.keys.mint,
        token_program: pm.keys.token_program,
        owner_token_account: acc,
    };
    let m = env.market(&market_key);
    let d = ix::with_remaining(
        ix::deposit(&k, amount, 0),
        &ix::epoch_records(&market_key, 0, m.share_epoch),
    );
    ok(env.send(&[d], &[&lender2]));
}

/// M6-SEC-3 (owner decision of 2026-09-30; ARCH §3.4 step 3, §3.11, §5.5; SPEC §14): PumpSwap's
/// `disable_flags` bit 0 halts `create_pool` and with it pump's `migrate_v2` (T-DISABLE, R-VEN
/// Update H4), so a curve that completes under it cannot graduate. Curve opens read the flag from
/// the curve ReadSet's C6 (no extra account) and refuse `GraduationHalted` (6104) **exactly while
/// bit 0 is set**: every other bit leaves them alone (they halt PumpSwap's own instructions, not
/// the curve), clearing the bit re-enables them, and nothing of a refused open is kept. Positions
/// already open are unaffected: a close runs under the halt (exits never read the bit). As built
/// before the decision the open landed (q 1,444,060,820,237, S 0.5476 SOL) and then stalled until
/// S3 (below).
#[test]
fn m6_sec_3_curve_opens_are_refused_exactly_while_create_pool_is_halted() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    for flags in [0x01u8, 0x03, 0x09, 0x19, 0xff] {
        pamm::set_disable_flags(&mut env.svm, flags);
        let before = market(&env, &pm);
        let (res, _) = env.open(&pm, &mut b, q);
        assert_err(&res, E::GraduationHalted);
        let after = market(&env, &pm);
        assert_eq!(
            (after.open_positions, after.borrowed_q, after.tokens_free),
            (before.open_positions, before.borrowed_q, before.tokens_free),
            "flags {flags:#04x}: nothing of the refused open is kept"
        );
        env.warp(1);
    }
    // Every other bit: the curve open lands (and exits under bit 0 run).
    pamm::set_disable_flags(&mut env.svm, 0xfe);
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    pamm::set_disable_flags(&mut env.svm, 0x01);
    assert_err(&env.open(&pm, &mut b, q).0, E::GraduationHalted);
    close_ok(&mut env, &pm, &b, nonce);
    env.warp(1);
    // Cleared: opens resume.
    pamm::set_disable_flags(&mut env.svm, 0x00);
    let (open, _, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(open.venue, VenueKind::PumpCurve as u8);
    assert_eq!(code(E::GraduationHalted), 6104);
}

/// M6-SEC-3's remaining exposure (disclosed, ARCH §5.5): a position opened **before** PumpSwap's
/// `create_pool` halt keeps the stalled-graduation path. Once the curve completes under bit 0 the
/// market stays `PumpMigrating` — every `migrate_v2` fails 6017 `DisabledCreatePool`, exits refuse
/// `VenueMigrating`. S3 needs the position liquidatable or expired (ARCH §5.3): a healthy position
/// is frozen past the 70-min pause until its deadline, then settles S3 after `s3_wait_s`, paying the
/// lenders FV in SOL instead of the tokens.
#[test]
fn m6_sec_3_a_position_opened_before_the_create_pool_halt_stalls_until_s3() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    let (open, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    pamm::set_disable_flags(&mut env.svm, 0x01);
    assert_err(&env.open(&pm, &mut b, q).0, E::GraduationHalted);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    let t0 = env.now();
    let pos = env.position(&pm, &b.key(), nonce);
    let migrate = |env: &mut Env| {
        let migrator = env.funded_keypair();
        let ix = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
        let res = env.send(&[pump::cu_limit_ix(600_000), ix], &[&migrator]);
        assert_eq!(custom_code(&res), Some(6017), "DisabledCreatePool");
    };
    migrate(&mut env);
    env.warp(1);
    assert_err(&env.close(&pm, &b, nonce), E::VenueMigrating);
    // The keeper's chain through the pause and 20 min past it, `migrate_v2` retried every 30 s: the
    // healthy position is not settled.
    let kp = keeper(&mut env);
    let pause_end = t0 + i64::from(pos.max_migration_pause_s) + i64::from(pos.s3_wait_s);
    while env.now() < pause_end + 1_200 {
        env.warp(30);
        crank(&mut env, &pm);
        migrate(&mut env);
        let meta = ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
        assert!(settled(&meta).is_none(), "a healthy position stays frozen");
    }
    assert_err(&env.close(&pm, &b, nonce), E::VenueMigrating);
    // At its deadline the position is expired: S3 after `s3_wait_s` of the chain.
    warp_to_deadline(&mut env, &pm, &b.key(), nonce, 0);
    let (ev, at) = settle_through_the_pause(&mut env, &pm, &kp, &b.key(), nonce, migrate);
    assert_eq!(ev.condition, 3, "S3");
    assert!(
        at >= pos.deadline + i64::from(pos.s3_wait_s),
        "S3 after the deadline's chain"
    );
    assert!(at < pos.deadline + i64::from(pos.s3_wait_s) + 60);
    assert_eq!(venue(&env, &pm), VenueKind::PumpMigrating);
    eprintln!(
        "M6-SEC-3: open q {} (S {} lamports) before the create_pool halt; the curve completed at \
         t0; migrate_v2 6017 throughout; healthy and frozen through the 70-min pause; S3 at t0 + \
         {} s (deadline + s3_wait): FV {} lamports, lenders {} lamports in SOL, no tokens back",
        open.q,
        open.s,
        at - t0,
        ev.fv,
        ev.lender_amount
    );
}

/// A third party's real, valid PumpSwap pool of the coin at the curve's `→ PumpSwap` transition
/// (ARCH §2.5 "On → PumpSwap", `canonical_pool_valid`; M10 G-A2): after the completion, before any
/// `migrate_v2`, a stranger creates its own WSOL pool of the mint with real liquidity
/// (`create_pool(index = 1)` on the dumped pAMM ELF). It is not the canonical pool, so the cranks
/// keep the market `PumpMigrating` (no `VenueStateChanged`, the migration pause kept); its bytes
/// planted at the canonical address (index 1, a creator that is not pump's pool authority; a state
/// mainnet cannot reach) are not a valid canonical pool either: still `PumpMigrating`, never
/// `PumpSwap`, never "graduated elsewhere" (M5-SEC-5's `Unreadable`). The control: the address
/// emptied again, the real `migrate_v2` creates the canonical pool and the next crank moves the
/// market `PumpMigrating → PumpSwap`.
#[test]
fn a_real_non_canonical_pool_does_not_graduate_the_market() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    let seen = market(&env, &pm).migration_seen_at;
    let (np, _) = pamm::create_third_party_pool(&mut env, &pm.keys.mint, 1, q * 10, 20_000_000_000);
    assert_eq!(np.state(&env.svm).index, 1);
    let canonical = pda::canonical_pool(&pm.keys.mint);
    assert_ne!(np.pool, canonical);
    assert!(
        env.svm
            .get_account(&canonical)
            .is_none_or(|a| a.data.is_empty())
    );
    env.warp(1);
    no_venue_change(&crank(&mut env, &pm));
    assert_eq!(venue(&env, &pm), VenueKind::PumpMigrating);
    // Its bytes at the canonical address.
    let planted = env.svm.get_account(&np.pool).unwrap();
    env.svm.set_account(canonical, planted).unwrap();
    env.warp(1);
    no_venue_change(&crank(&mut env, &pm));
    let m = market(&env, &pm);
    assert_eq!(m.venue_kind(), Some(VenueKind::PumpMigrating));
    assert!(!m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
    assert_eq!(m.migration_seen_at, seen, "the pause is kept");
    // The control.
    env.svm
        .set_account(canonical, solana_account::Account::default())
        .unwrap();
    let migrator = env.funded_keypair();
    let ix = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
    ok(env.send(&[pump::cu_limit_ix(600_000), ix], &[&migrator]));
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpMigrating,
        VenueKind::PumpSwap,
    );
}

// ---- pump's synthetic migration on a PumpMigrating market (pump-final §5.4, §6.1.5) -------------

/// A third party's `buy_v3(rT + δ)`: the curve completes and `δ` comes from the would-be pool
/// (`pcB = δ`).
fn synthetic_completion(env: &mut Env, pm: &PumpMarket, delta: u64) {
    let u = CurveUser::new(env, &pm.curve);
    env.fund(&u.kp.pubkey(), 1_000_000_000_000);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    let ix = u.buy_v3_ix(&env.svm, &pm.curve, rt + delta, u64::MAX);
    ok(u.send(env, ix));
    let c = pm.curve.state(&env.svm);
    assert!(c.complete && c.post_complete_base_out == delta);
}

/// `migrate_v2` by anyone.
fn migrate(env: &mut Env, pm: &PumpMarket) -> TxResult {
    let migrator = env.funded_keypair();
    let ix = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
    env.send(&[pump::cu_limit_ix(600_000), ix], &[&migrator])
}

/// The pool `migrate_v2` will create from the complete curve as short.win prices it (§5.4):
/// `migrated_pool` on the live `curve_base_ata` and `pcQ` after a pool part, else on the standard
/// 206.9e12 (a donation ignored, ARCH §4.3).
fn priced_pool(env: &Env, pm: &PumpMarket) -> (u128, u128) {
    let c = pm.curve.state(&env.svm);
    let g = env
        .svm
        .get_account(&shortwin::constants::PUMP_GLOBAL)
        .unwrap();
    let fee = vpump::read_global(&g.owner.to_bytes(), &g.data)
        .unwrap()
        .pool_migration_fee();
    let pc = c.post_complete_base_out != 0 || c.post_complete_quote_in != 0;
    mq::migrated_pool(
        &pm.curve.curve_state(&env.svm),
        &MigrationParams {
            migration_fee: fee,
            pool_tokens: if pc {
                u128::from(tokens(env, &pm.curve.curve_base_ata))
            } else {
                shortwin::venues::STANDARD_POOL_TOKENS
            },
            post_complete_quote_in: c.post_complete_quote_in,
        },
    )
    .unwrap()
}

/// FV of `pos` at the EMA point now (ARCH §4.5, D3; R4A-3): `B̂_FV + ⌈p'·B̂_FV/10⁴⌉ + R_due`,
/// `p'` the expiry fee if expired and not CR-liquidatable there, else the penalty.
fn fv_of(pos: &ShortPosition, now: i64, r_due: u64, b_fv: u64) -> u64 {
    use shortwin_math::risk;
    let v_eff = risk::v_eff(pos.v, r_due);
    let below = risk::cr_below(v_eff, Some(b_fv), pos.cr_liq_bps).unwrap();
    let rate = risk::fv_rate_bps(
        !(now >= pos.deadline && !below),
        u32::from(pos.penalty_bps),
        u32::from(pos.expiry_fee_bps),
    );
    risk::fair_value(Some(b_fv), rate, r_due).unwrap()
}

/// A position on `near166` 30 min from its deadline, the keeper's crank, and the market's
/// keeper: `(env, pm, b, nonce, q, keeper)`.
fn near166_before_deadline() -> (Env, PumpMarket, Borrower, u64, u64, solana_keypair::Keypair) {
    let (mut env, pm, q) = setup(PumpCoin::Near166);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let kp = keeper(&mut env);
    warp_to_deadline(&mut env, &pm, &b.key(), nonce, -1_800);
    crank(&mut env, &pm);
    env.warp(1);
    (env, pm, b, nonce, q, kp)
}

/// S3 of the position after its pause, checked against [`migrating_fv_quote`] at `P_ema` (§5.4)
/// and [`fv_of`]; returns the event.
fn s3_at_the_priced_pool(
    env: &mut Env,
    pm: &PumpMarket,
    kp: &solana_keypair::Keypair,
    b: &Borrower,
    nonce: u64,
) -> shortwin::events::SolSettle {
    let t0 = market(env, pm).migration_seen_at;
    let pos = env.position(pm, &b.key(), nonce);
    let (ev, at) = settle_through_the_pause(env, pm, kp, &b.key(), nonce, |_| {});
    assert_eq!(ev.condition, 3, "S3");
    assert!(at >= t0 + i64::from(pos.max_migration_pause_s) + i64::from(pos.s3_wait_s));
    let m = market(env, pm);
    let b_fv = migrating_fv_quote(
        env,
        pm,
        pos.q,
        m.ema_price.get(),
        u64::from(pos.venue_fee_hard_cap_bps),
    );
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let fv = fv_of(&pos, at, r_due, b_fv);
    assert!(ev.fv > 0, "a defined FV");
    assert_eq!(
        ev.fv, fv,
        "FV at §5.4's price on the pool migrate_v2 creates"
    );
    assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
    ev
}

/// T-MIG-PRICE (pump-final §5.4): a third party's synthetic completion (`buy_v3(rT + 10e12)`), the
/// crank moves the market to `PumpMigrating`. On pump's ELF `rQ` excludes `pcQ` (`vQ − rQ =
/// ivqr`); the program prices the pause on `(X₁, Y₁)` = (the live `curve_base_ata`, `rQ − fee +
/// pcQ`) with its post-complete fields known, and `migrate_v2` creates exactly that pool. The EMA
/// leg's quote on the completed curve's ring state, `B̂_str` on `(X₁, Y₁)`, covers a PumpSwap `buy(q)`
/// on the new pool within a few lamports (it was off by ρ before pump-final).
#[test]
fn a_third_partys_synthetic_completion_is_priced_on_the_pool_migrate_v2_creates() {
    let (mut env, pm, q) = setup(PumpCoin::Near166);
    let mut b = env.borrower();
    open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    synthetic_completion(&mut env, &pm, 10_000_000_000_000);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    let c = pm.curve.state(&env.svm);
    assert_eq!(
        c.virtual_quote_reserves - c.real_quote_reserves,
        c.initial_virtual_quote_reserves,
        "rQ excludes the pool part"
    );
    let a = u128::from(tokens(&env, &pm.curve.curve_base_ata));
    let st = host_state_with(&env, &pm, Some(mint_supply(&env, &pm)));
    let shortwin::venues::Quoter::Curve(cq) = st.quoter().expect("the curve quoter") else {
        panic!("a curve quoter");
    };
    assert!(cq.migrating && cq.post_complete_known);
    assert_eq!(
        (
            cq.pricing.migration.pool_tokens,
            cq.pricing.migration.post_complete_quote_in
        ),
        (a, c.post_complete_quote_in)
    );
    assert!(cq.pricing.synthetic.is_none(), "no synthetic route: G-2");
    let (x1, y1) = priced_pool(&env, &pm);
    assert_eq!(
        mq::migrated_pool(&cq.live, &cq.pricing.migration),
        Some((x1, y1))
    );
    // The EMA leg's quote on a ring state `(vT, vQ)`, here the completed curve's own (a ring entry
    // recorded before the completion would price an older, cheaper pool).
    let b_ema = st
        .buy_at(
            q,
            u128::from(c.virtual_token_reserves),
            u128::from(c.virtual_quote_reserves),
        )
        .expect("the EMA leg's quote")
        .cost;
    ok(migrate(&mut env, &pm));
    let np = pump::NewPool::of(&pm.curve);
    let r = np.reserves(&env.svm).expect("the canonical pool");
    assert_eq!(
        (u128::from(r.base), r.effective_quote),
        (x1, y1),
        "migrate_v2 created (X₁, Y₁)"
    );
    let buyer = env.funded_keypair();
    let payer = env.payer.pubkey();
    let program = pm.curve.token_program.id();
    ok(env.send(
        &[fork::create_ata_idempotent_ix(
            &payer,
            &buyer.pubkey(),
            &pm.keys.mint,
            &program,
        )],
        &[],
    ));
    let buyer_base = pda::ata(&buyer.pubkey(), &pm.keys.mint, &program);
    let wsol = fork::fund_wsol(&mut env, &buyer.pubkey(), b_ema + 1_000_000_000);
    let wsol0 = tokens(&env, &wsol);
    let ixs = [
        pump::pamm_extend_account_ix(&buyer.pubkey(), &np.pool),
        pump::pamm_buy_ix(&env.svm, &np, &buyer.pubkey(), &buyer_base, &wsol, q, b_ema),
    ];
    ok(env.send(&ixs, &[&buyer]));
    let spent = wsol0 - tokens(&env, &wsol);
    assert_eq!(tokens(&env, &buyer_base), q);
    eprintln!("EMA leg B̂ {b_ema} vs the pool's buy(q) {spent}");
    assert!(spent <= b_ema && b_ema - spent <= 7);
}

/// T-MIG-S3 (pump-final §5.4): a third party's synthetic completion 30 min before the position's
/// deadline, PumpSwap's `create_pool` halted (bit 0): nobody can migrate. The keeper's chain
/// settles the expired position S3 after `max_migration_pause_s + s3_wait_s` at FV on the pool
/// `migrate_v2` would create, `P_ema` ρ-scaled — above the pre-v3 FV on the standard pool.
#[test]
fn s3_on_a_stuck_synthetic_migration_settles_at_the_post_buy_pool() {
    let (mut env, pm, b, nonce, _, kp) = near166_before_deadline();
    synthetic_completion(&mut env, &pm, 10_000_000_000_000);
    pamm::set_disable_flags(&mut env.svm, 0x01);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    let pos = env.position(&pm, &b.key(), nonce);
    let ev = s3_at_the_priced_pool(&mut env, &pm, &kp, &b, nonce);
    // Before pump-final the same FV priced the standard pool at P_ema (ρ = 1): lower by ≈ 1 − 1/ρ.
    let live = pm.curve.curve_state(&env.svm);
    let m = market(&env, &pm);
    let g = env
        .svm
        .get_account(&shortwin::constants::PUMP_GLOBAL)
        .unwrap();
    let fee = vpump::read_global(&g.owner.to_bytes(), &g.data)
        .unwrap()
        .pool_migration_fee();
    let (x0, y0) = mq::migrated_pool(
        &live,
        &MigrationParams {
            migration_fee: fee,
            pool_tokens: shortwin::venues::STANDARD_POOL_TOKENS,
            post_complete_quote_in: 0,
        },
    )
    .unwrap();
    let (x1, y1) = priced_pool(&env, &pm);
    let p = m.ema_price.get();
    let rho_p = mq::migrating_fv_price(p, x0, y0, x1, y1).unwrap();
    eprintln!(
        "T-MIG-S3: ρ = {:.4}, FV {} (lenders {}), take {}",
        rho_p as f64 / p as f64,
        ev.fv,
        ev.lender_amount,
        pos.v - k_s(&pos)
    );
    assert!(rho_p > p);
}

/// T-MIG-DONATION (pump-final §5.4, §6.8; R-2): a token donation and a synthetic completion, each
/// case followed by a `create_pool` halt, an S3 and — the bit cleared — `migrate_v2`. In every case
/// with a pool part `X₁` is the live `curve_base_ata` and the S3's FV is defined and equals §5.4's
/// on the live `(X₁, Y₁)`; the lenders get `min(FV, take)`; `migrate_v2` then creates exactly `(X₁,
/// Y₁)`:
///
/// 1. `D` = 1e12 donated before a synthetic completion of `δ` = 10e12;
/// 2. the review's edge: `D` = 206.9e12, then `δ` = 250e12 (`pcB ≥ 206.9e12`: revision 1's
///    deterministic `206.9e12 − pcB` had no value at all);
/// 3. `D` = 3e12 donated after the completion (`δ` = 10e12);
/// 4. `D` = 206.9e12 and `δ` = 206.9e12 − 1e9 (revision 1's rule gave ρ ≈ 2e5): the `B̂_FV` behind the
///    settled FV is within 0.1 % of the same capped-fee quote on the pool `migrate_v2` then creates;
/// 5. control: `D` = 2e12 and a completion of exactly `rT` (no pool part): the donation is ignored
///    (ARCH §4.3) — FV on the standard 206.9e12 — while `migrate_v2` seeds it.
#[test]
fn a_donation_and_a_synthetic_completion_price_the_pool_migrate_v2_creates() {
    const T: u64 = 206_900_000_000_000;
    for (case, before, delta, after) in [
        (1, 1_000_000_000_000, 10_000_000_000_000, 0),
        (2, T, 250_000_000_000_000, 0),
        (3, 0, 10_000_000_000_000, 3_000_000_000_000),
        (4, T, T - 1_000_000_000, 0),
        (5, 2_000_000_000_000, 0, 0),
    ] {
        let (mut env, pm, b, nonce, q, kp) = near166_before_deadline();
        if before > 0 {
            donate_tokens(&mut env, &pm, &pm.curve.curve_base_ata, before);
        }
        if delta > 0 {
            synthetic_completion(&mut env, &pm, delta);
        } else {
            buy_down_to(&mut env, &pm, 0);
        }
        if after > 0 {
            donate_tokens(&mut env, &pm, &pm.curve.curve_base_ata, after);
        }
        pamm::set_disable_flags(&mut env.svm, 0x01);
        env.warp(1);
        venue_changed(
            &crank(&mut env, &pm),
            VenueKind::PumpCurve,
            VenueKind::PumpMigrating,
        );
        let a = u128::from(tokens(&env, &pm.curve.curve_base_ata));
        let (x1, y1) = priced_pool(&env, &pm);
        if delta > 0 {
            assert_eq!(x1, a, "case {case}: X₁ = the live curve_base_ata");
        } else {
            assert_eq!(x1, u128::from(T), "case {case}: the donation ignored");
            assert_eq!(a, u128::from(T + before));
        }
        let pos = env.position(&pm, &b.key(), nonce);
        let ev = s3_at_the_priced_pool(&mut env, &pm, &kp, &b, nonce);
        if case == 4 {
            let m = market(&env, &pm);
            let cap = u64::from(pos.venue_fee_hard_cap_bps);
            let b_fv = migrating_fv_quote(&env, &pm, q, m.ema_price.get(), cap);
            let pfc = env
                .svm
                .get_account(&shortwin::constants::PUMP_AMM_FEE_CONFIG)
                .unwrap();
            let tiers: Vec<fees::FeeTier> =
                vpump::read_fee_config(&pfc.owner.to_bytes(), &pfc.data)
                    .unwrap()
                    .fee_tiers
                    .iter()
                    .map(|t| fees::FeeTier {
                        threshold: t.threshold,
                        fees: FeeBps::new(t.fees.lp, t.fees.protocol, t.fees.creator),
                    })
                    .collect();
            let gc = env
                .svm
                .get_account(&shortwin::constants::PUMP_AMM_GLOBAL_CONFIG)
                .unwrap();
            let gcfg = vamm::read_global_config(&gc.owner.to_bytes(), &gc.data).unwrap();
            let f = fees::pump_amm(
                y1,
                x1,
                &PammFeeSchedule {
                    tiers: &tiers,
                    base_supply: 1_000_000_000_000_000,
                    creator_fee_configurable: gcfg.creator_fee_configurable(),
                    pool_creator_fee_bps: 0,
                    coin_creator_is_default: pm.curve.state(&env.svm).creator_is_default(),
                    cap_bps: Some(cap),
                },
            )
            .unwrap();
            let on_pool = sol_leg::buy_exact_out(q, x1, y1, false, &f).unwrap().cost;
            let dev = u128::from(b_fv.abs_diff(on_pool)) * 10_000 / u128::from(on_pool);
            eprintln!("case 4: B̂_FV {b_fv} vs the created pool's quote {on_pool}");
            assert!(dev <= 10, "within 0.1 %");
        }
        eprintln!(
            "T-MIG-DONATION case {case}: X₁ {x1}, Y₁ {y1}, FV {}, lenders {}",
            ev.fv, ev.lender_amount
        );
        pamm::set_disable_flags(&mut env.svm, 0);
        ok(migrate(&mut env, &pm));
        let r = pump::NewPool::of(&pm.curve)
            .reserves(&env.svm)
            .expect("the canonical pool");
        if delta > 0 {
            assert_eq!(
                (u128::from(r.base), r.effective_quote),
                (x1, y1),
                "case {case}: migrate_v2 created (X₁, Y₁)"
            );
        } else {
            assert_eq!(u128::from(r.base), a, "the donation seeded");
            assert_eq!(r.effective_quote, y1);
        }
    }
}

/// T-MIG-GUARD (pump-final §6.1.5): a synthetic completion's post-complete fields patched so they
/// are not known — (i) `vQ − rQ ≠ ivqr` with both set, (ii) `pcQ` = 0 with `pcB` set, (iii) `pcB` =
/// 0 with `pcQ` set. The program keeps the curve quoter (never the cap-only view) and has no FV: the
/// S3 pays the lenders the whole take (`V − K_s`), the borrower nothing, `fv` 0. The control, the
/// fields as pump wrote them, settles at a defined FV below the take.
#[test]
fn a_migrating_curve_with_unknown_post_complete_fields_has_no_fv() {
    for case in 0..4 {
        let (mut env, pm, b, nonce, _, kp) = near166_before_deadline();
        synthetic_completion(&mut env, &pm, 10_000_000_000_000);
        let c = pm.curve.state(&env.svm);
        fork::patch(&mut env.svm, &pm.curve.curve, |d| {
            let put =
                |d: &mut Vec<u8>, o: usize, v: u64| d[o..o + 8].copy_from_slice(&v.to_le_bytes());
            match case {
                1 => put(
                    d,
                    vpump::OFF_INITIAL_VIRTUAL_QUOTE_RESERVES,
                    c.initial_virtual_quote_reserves + 1,
                ),
                2 => put(d, vpump::OFF_POST_COMPLETE_QUOTE_IN, 0),
                3 => put(d, vpump::OFF_POST_COMPLETE_BASE_OUT, 0),
                _ => {}
            }
        });
        pamm::set_disable_flags(&mut env.svm, 0x01);
        env.warp(1);
        venue_changed(
            &crank(&mut env, &pm),
            VenueKind::PumpCurve,
            VenueKind::PumpMigrating,
        );
        let pos = env.position(&pm, &b.key(), nonce);
        let st = host_state_with(&env, &pm, Some(mint_supply(&env, &pm)));
        let m = market(&env, &pm);
        let r = shortwin::venues::risk_quoter(&m, &st, &[], u64::from(pos.venue_fee_hard_cap_bps))
            .unwrap();
        assert!(
            matches!(r, shortwin::venues::Quoter::Curve(_)),
            "case {case}: the curve quoter, never cap-only"
        );
        let fv = r.fv_repriced(pos.q, m.ema_liq.get(), m.ema_price.get());
        assert_eq!(
            fv.is_some(),
            case == 0,
            "case {case}: FV defined only when known"
        );
        let (ev, _) = settle_through_the_pause(&mut env, &pm, &kp, &b.key(), nonce, |_| {});
        assert_eq!(ev.condition, 3);
        let take = pos.v - k_s(&pos);
        if case == 0 {
            assert!(ev.fv > 0 && ev.lender_amount < take, "the control");
        } else {
            assert_eq!(ev.fv, 0, "case {case}: undefined FV");
            assert_eq!(
                ev.lender_amount, take,
                "case {case}: the lenders take it all"
            );
            assert_eq!(ev.borrower_amount, 0);
        }
    }
}

/// T-LIVE-PC-GUARD (pump-final G-3, §6.1.5): post-complete fields on a **live** curve (planted @150 /
/// @158 — pump writes them only in the buy that completes a curve): the venue is not executable
/// (closes and opens `NoRoute`), not observed, and the S3 chain counts; the expired position
/// settles S3 at A63's cap-only view on the frozen `(P_ema, L_ema)`.
#[test]
fn a_live_curve_carrying_post_complete_fields_is_not_executable() {
    let (mut env, pm, q) = setup(PumpCoin::Near166);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    let kp = keeper(&mut env);
    warp_to_deadline(&mut env, &pm, &b.key(), nonce, 1);
    crank(&mut env, &pm);
    fork::patch(&mut env.svm, &pm.curve.curve, |d| {
        d[vpump::OFF_POST_COMPLETE_BASE_OUT..vpump::OFF_POST_COMPLETE_BASE_OUT + 8]
            .copy_from_slice(&1u64.to_le_bytes());
        d[vpump::OFF_POST_COMPLETE_QUOTE_IN..vpump::OFF_POST_COMPLETE_QUOTE_IN + 8]
            .copy_from_slice(&1u64.to_le_bytes());
    });
    let st = host_state_with(&env, &pm, Some(mint_supply(&env, &pm)));
    assert!(!st.executable && st.quoter().is_none(), "no pricing (G-3)");
    env.warp(1);
    no_venue_change(&crank(&mut env, &pm));
    let m0 = market(&env, &pm);
    assert!(m0.no_route_since > 0, "the S3 chain counts");
    assert_err(&env.close(&pm, &b, nonce), E::NoRoute);
    let mut b2 = env.borrower();
    assert_err(&env.open(&pm, &mut b2, q).0, E::NoRoute);
    let pos = env.position(&pm, &b.key(), nonce);
    let since = m0.no_route_since;
    let ev = loop {
        env.warp(30);
        crank(&mut env, &pm);
        let meta = ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
        if let Some(ev) = settled(&meta) {
            break ev;
        }
        assert!(env.now() < since + i64::from(pos.s3_wait_s) + 60);
    };
    assert_eq!(ev.condition, 3);
    let m = market(&env, &pm);
    let (p, l) = (m.ema_price.get(), m.ema_liq.get());
    assert_eq!((p, l), (m0.ema_price.get(), m0.ema_liq.get()), "frozen");
    let cap = shortwin_math::quote::Pricing::PumpSwap(FeeBps::new(
        0,
        u64::from(pos.venue_fee_hard_cap_bps),
        0,
    ));
    let bq = cap.buy_repriced(pos.q, l, p).unwrap().cost;
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    assert_eq!(
        ev.fv,
        fv_of(&pos, ev.ts, r_due, bq),
        "FV at the cap-only view"
    );
}

/// T-LISTING (pump-final §6.1.6, R-4): a listed market whose curve is patched to a cashback coin
/// while live (pump has no such instruction) is not executable (`NoRoute`) and records no
/// observation; once the curve completes (a third party's `buy_v2` of `rT`; v2 still trades a
/// cashback coin) the market is `PumpMigrating`, its observations are recorded again (`listable`
/// does not refuse cashback: C2), and the expired position settles S3 at a defined FV.
#[test]
fn a_complete_cashback_curve_is_observed_during_the_pause() {
    let (mut env, pm, b, nonce, _, kp) = near166_before_deadline();
    fork::patch(&mut env.svm, &pm.curve.curve, |d| {
        d[vpump::OFF_IS_CASHBACK_COIN] = 1
    });
    env.warp(1);
    no_venue_change(&crank(&mut env, &pm));
    let m0 = market(&env, &pm);
    assert!(
        m0.b_sec < env.now(),
        "a live cashback curve is not observed"
    );
    assert_err(&env.close(&pm, &b, nonce), E::NoRoute);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    env.warp(1);
    crank(&mut env, &pm);
    assert_eq!(
        market(&env, &pm).b_sec,
        env.now(),
        "observed during the pause"
    );
    let st = host_state_with(&env, &pm, Some(mint_supply(&env, &pm)));
    assert!(st.observed() && !st.executable);
    let ev = s3_at_the_priced_pool(&mut env, &pm, &kp, &b, nonce);
    assert!(ev.fv > 0);
}
