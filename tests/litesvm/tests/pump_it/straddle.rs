//! Above the curve's `rT` (pump-final §5.2–§5.4, §6.1.7; owner D3): since pump's 2026-10-08
//! redeploy a `buy_v3` past `rT` buys `rT` on the curve, which completes, and the rest from the
//! would-be PumpSwap pool by **synthetic migration**, in one instruction. short.win trades every
//! curve leg on v3, so a close of `q > rT` is an ordinary exit that completes the curve and moves
//! the market to `PumpMigrating` in the same instruction; its quote `B̂_v3` prices exactly that
//! path (both parts at the fee pump charges them, the tier of the curve's market cap on the mint's
//! live supply). `B̂_str` — all of `q` on the post-migration pool at its PumpSwap tier — now prices
//! only a `PumpMigrating` market's states: it still covers a third party's completion, `migrate_v2`
//! and a PumpSwap buy on the real ELFs. Checked against the program's own quotes (the host build
//! of `venues::read_state` on the same accounts).
//!
//! Every instruction goes through `Env::send`: the invariant hooks run after each one.

use shortwin::constants::{PUMP_FEE_CONFIG, PUMP_GLOBAL};
use shortwin_math::fees::{self, FeeBps, PammFeeSchedule};
use shortwin_math::quote::{self as mq, MigrationParams, sol_leg};
use shortwin_venue_state::{pump as vpump, pump_amm as vamm};

use crate::common::*;

/// `Global.pool_migration_fee` of the dump.
fn migration_fee(env: &Env) -> u64 {
    let g = env.svm.get_account(&PUMP_GLOBAL).unwrap();
    vpump::read_global(&g.owner.to_bytes(), &g.data)
        .unwrap()
        .pool_migration_fee()
}

/// `q > rT`: the close completes the curve by synthetic migration in one instruction — pump's
/// curve part (`rT`) and pool part (`q − rT`) in its events, `pcB = q − rT`, the buyback cost
/// their charge plus the curve's growth rent, within `B̂_v3` (the program's quote, recomputed
/// from `shortwin-math`) — and the venue sync moves the market to `PumpMigrating` (the
/// `VenueStateChanged` of the close itself).
#[test]
fn a_buyback_above_rt_completes_the_curve_by_synthetic_migration() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(2);
    buy_down_to(&mut env, &pm, q - 1);
    env.warp(1);
    let st = host_state_with(&env, &pm, Some(mint_supply(&env, &pm)));
    let live = pm.curve.curve_state(&env.svm);
    assert_eq!(live.rt, u128::from(q - 1), "a straddle: q = rT + 1");
    let quote = st.buy(q).expect("B̂_v3");
    let fs = pm.curve.synthetic_fees(&env.svm).unwrap();
    let base = u128::from(tokens(&env, &pm.curve.curve_base_ata));
    let m = MigrationParams {
        migration_fee: migration_fee(&env),
        pool_tokens: base - live.rt,
        post_complete_quote_in: 0,
    };
    let want = mq::curve_synthetic(q, &live, &pm.curve.fees(&env.svm).unwrap(), &fs, &m).unwrap();
    assert_eq!(quote, want, "the program's B̂_v3 = shortwin-math's");
    let rent = pm.curve.growth_top_up(&env.svm);
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let (close, meta) = close_ok(&mut env, &pm, &b, nonce);
    assert_eq!(close.b_live, quote.cost);
    let curve_part = fork::pump_trade_events(&meta);
    let pool_part = fork::pump_post_complete_buy_events(&meta);
    assert_eq!((curve_part.len(), pool_part.len()), (1, 1));
    assert_eq!(curve_part[0].token_amount, q - 1);
    assert_eq!(pool_part[0].base_out, 1);
    let charge = curve_part[0].sol_amount
        + curve_part[0].fee
        + curve_part[0].creator_fee
        + pool_part[0].quote_in
        + pool_part[0].fee
        + pool_part[0].creator_fee;
    assert_eq!(
        close.buyback_cost,
        charge + rent,
        "B = pump's charge + growth"
    );
    assert!(close.b_live >= charge && close.b_live - charge <= 7);
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, q);
    let c = pm.curve.state(&env.svm);
    assert!(c.complete && c.real_token_reserves == 0);
    assert_eq!(
        (c.post_complete_base_out, c.post_complete_quote_in),
        (1, pool_part[0].quote_in)
    );
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpMigrating);
    assert_eq!(market(&env, &pm).migration_seen_at, env.now());
    assert_trader_clean(&env, &pm);
}

/// The PumpSwap `FeeConfig` tiers of the dump, as `shortwin-math` tiers.
fn pool_tiers(env: &Env) -> Vec<fees::FeeTier> {
    let pfc = env
        .svm
        .get_account(&shortwin::constants::PUMP_AMM_FEE_CONFIG)
        .unwrap();
    vpump::read_fee_config(&pfc.owner.to_bytes(), &pfc.data)
        .unwrap()
        .fee_tiers
        .iter()
        .map(|t| fees::FeeTier {
            threshold: t.threshold,
            fees: FeeBps::new(t.fees.lp, t.fees.protocol, t.fees.creator),
        })
        .collect()
}

/// With `rT = q/2` the program's live quote (`VenueState::buy`, the mint's supply passed as every
/// exit passes it) is `B̂_v3` — `shortwin-math`'s `curve_synthetic` on the live `curve_base_ata
/// − rT` — and lies below `B̂_str = B̂_pool(q; X_p, Y_p)` on the post-migration pool at the
/// PumpSwap tier of its market cap (owner D3: price what executes). `B̂_str` itself still prices
/// the path a third party opens: it completes the curve (`buy_v2(rT)`), anyone calls `migrate_v2`
/// (the pool holds exactly `X_p` and `Y_p − 1`: the quote's intended `+1`), `extend_account` takes
/// the new pool (287 bytes) to 301, and PumpSwap `buy(q, B̂_str)` delivers `q` for at most `B̂_str`,
/// within a few lamports.
#[test]
fn b_v3_prices_the_live_exit_and_b_str_the_post_migration_pool() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    open_ok(&mut env, &pm, &mut b, q);
    env.warp(2);
    buy_down_to(&mut env, &pm, q / 2);
    env.warp(1);
    crank(&mut env, &pm);

    // ---- the program's quote on the pre-completion state ----
    let st = host_state_with(&env, &pm, Some(mint_supply(&env, &pm)));
    assert!(st.executable, "the curve is live");
    let live = pm.curve.curve_state(&env.svm);
    assert!(live.rt < u128::from(q), "a straddle: q > rT");
    let quote = st.buy(q).expect("B̂_v3");
    let migration_fee = migration_fee(&env);
    let base_ata = u128::from(tokens(&env, &pm.curve.curve_base_ata));
    let m = MigrationParams {
        migration_fee,
        pool_tokens: base_ata - live.rt,
        post_complete_quote_in: 0,
    };
    let curve_fees = pm.curve.fees(&env.svm).unwrap();
    let fs = pm.curve.synthetic_fees(&env.svm).unwrap();
    let b_v3 = mq::curve_synthetic(q, &live, &curve_fees, &fs, &m).unwrap();
    assert_eq!(quote, b_v3, "B̂_live = B̂_v3");
    let cap = u64::from(env.config().params.venue_fee_hard_cap_bps);
    let risk = st.buy_risk(q, cap).expect("risk quote");
    assert!(
        risk.cost >= quote.cost,
        "the risk view never quotes below the live one"
    );
    let (xp, yp) = mq::migrated_pool(&live, &m).unwrap();
    let tiers = pool_tiers(&env);
    let gc = env
        .svm
        .get_account(&shortwin::constants::PUMP_AMM_GLOBAL_CONFIG)
        .unwrap();
    let gcfg = vamm::read_global_config(&gc.owner.to_bytes(), &gc.data).unwrap();
    let pool_sched = PammFeeSchedule {
        tiers: &tiers,
        base_supply: 1_000_000_000_000_000,
        creator_fee_configurable: gcfg.creator_fee_configurable(),
        pool_creator_fee_bps: 0,
        coin_creator_is_default: pm.curve.state(&env.svm).creator_is_default(),
        cap_bps: None,
    };
    let b_str = mq::curve_straddle(q, &live, &curve_fees, &m, &pool_sched).unwrap();
    let pool_fees = fees::pump_amm(yp, xp, &pool_sched).unwrap();
    let expect = sol_leg::buy_exact_out(q, xp, yp, false, &pool_fees).unwrap();
    assert_eq!(b_str.cost, expect.cost, "B̂_str = B̂_pool(q; X_p, Y_p)");
    eprintln!(
        "q {q}, rT {}: B̂_v3 {} vs B̂_str {}",
        live.rt, b_v3.cost, b_str.cost
    );
    assert!(
        b_v3.cost < b_str.cost,
        "the synthetic exit is the cheaper path"
    );

    // ---- a third party's completion, migrate_v2 and a PumpSwap buy on the real ELFs ----
    let keeper = buy_down_to(&mut env, &pm, 0);
    drop(keeper);
    let migrator = env.funded_keypair();
    let mig = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
    ok(env.send(&[pump::cu_limit_ix(600_000), mig], &[&migrator]));
    let np = pump::NewPool::of(&pm.curve);
    let r = np.reserves(&env.svm).expect("the canonical pool");
    assert_eq!(u128::from(r.base), xp, "X_p = the tokens migrate_v2 seeded");
    assert_eq!(
        r.effective_quote + 1,
        yp,
        "Y_p is one lamport above the pool"
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
    let wsol = fork::fund_wsol(&mut env, &buyer.pubkey(), b_str.cost + 1_000_000_000);
    let wsol0 = tokens(&env, &wsol);
    let ixs = [
        pump::pamm_extend_account_ix(&buyer.pubkey(), &np.pool),
        pump::pamm_buy_ix(
            &env.svm,
            &np,
            &buyer.pubkey(),
            &buyer_base,
            &wsol,
            q,
            b_str.cost,
        ),
    ];
    let meta = ok(env.send(&ixs, &[&buyer]));
    assert_eq!(
        env.svm.get_account(&np.pool).unwrap().data.len(),
        301,
        "extended (as every boosted mainnet pool, R-VEN Update 2026-09-29)"
    );
    let spent = wsol0 - tokens(&env, &wsol);
    assert_eq!(tokens(&env, &buyer_base), q, "the pool delivered q");
    eprintln!(
        "B̂_str {} vs executed {} ({} CU)",
        b_str.cost, spent, meta.compute_units_consumed
    );
    assert!(spent <= b_str.cost, "B̂_str covers the execution");
    assert!(b_str.cost - spent <= 4, "within a few lamports of it");
}

/// The **EMA leg** above `rT` (ARCH §4.3, §4.4; pump-final §5.2): a ring entry's cached
/// reprojection `(x, y)` is the curve at that price with the live offsets (`curve_at`), so its
/// `rT(x) = x − (vT − rT)`, and a `q` above it is priced `B̂_v3` on the **standard** would-be pool —
/// `X_s` = 206.9 M tokens (not the live `curve_base_ata − rT`; a live curve carries no pool part,
/// G-3), `Y_s` from that state — at the synthetic fee; below it on the curve at the live fees (a
/// donation to the curve's base ATA moves the live leg's `X_s` only). Checked on every entry of
/// the market's ring against `shortwin-math` (`curve_synthetic`), with the program's own quoter
/// (host build) on the live accounts and the mint's supply.
#[test]
fn the_ema_leg_prices_b_v3_on_the_standard_migration() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let mut b = env.borrower();
    open_ok(&mut env, &pm, &mut b, q);
    for _ in 0..8 {
        env.warp(1);
        crank(&mut env, &pm);
    }
    buy_down_to(&mut env, &pm, q / 2);
    // A donation to the curve's base ATA: pump prices a synthetic part on it, so the live leg's
    // `X_s` grows by it — the EMA leg keeps the standard 206.9 M.
    let donation = 1_000_000_000_000;
    donate_tokens(&mut env, &pm, &pm.curve.curve_base_ata, donation);
    // The keeper observes every second until the EMA (the ring's reprojections) has followed
    // the curve above `rT = q/2`: the newest entry's `rT(x)` falls below `q`.
    let offset = {
        let c = pm.curve.state(&env.svm);
        u128::from(c.virtual_token_reserves - c.real_token_reserves)
    };
    for _ in 0..1_800 {
        env.warp(1);
        crank(&mut env, &pm);
        let m = market(&env, &pm);
        let newest = m.ring[usize::from(m.ring_head)];
        if newest.x.get() < offset + u128::from(q) {
            break;
        }
    }
    let st = host_state_with(&env, &pm, Some(mint_supply(&env, &pm)));
    let quoter = st.quoter().expect("curve quoter");
    let shortwin::venues::Quoter::Curve(cq) = quoter else {
        panic!("a curve quoter");
    };
    assert_eq!(
        cq.live_pool_tokens,
        shortwin::venues::STANDARD_POOL_TOKENS + u128::from(donation),
        "the live leg's X_s = curve_base_ata − rT"
    );
    let fs = cq.pricing.synthetic.expect("the synthetic route");
    assert_eq!(Some(fs), pm.curve.synthetic_fees(&env.svm));
    assert_eq!(
        cq.pricing.migration.pool_tokens,
        shortwin::venues::STANDARD_POOL_TOKENS
    );
    let m = market(&env, &pm);
    let mut straddled = 0;
    for e in m.ring.iter().take(usize::from(m.ring_count)) {
        let (x, y) = (e.x.get(), e.y.get());
        let got = quoter.buy_xy(q, x, y).map(|bq| bq.cost);
        let state = shortwin::venues::curve_at(x, y, cq.pricing.offsets);
        let want = mq::curve_synthetic(
            q,
            &state,
            &cq.pricing.fees,
            &fs,
            &MigrationParams {
                migration_fee: cq.pricing.migration.migration_fee,
                pool_tokens: shortwin::venues::STANDARD_POOL_TOKENS,
                post_complete_quote_in: 0,
            },
        )
        .map(|bq| bq.cost);
        assert_eq!(got, want, "entry at {}", e.ts);
        if state.rt < u128::from(q) {
            straddled += 1;
        }
    }
    assert!(straddled > 0, "the newest entry is above rT");
}

/// P1 correction (a), the program's wiring (pump-final §5.2): the curve reader resolves the
/// synthetic legs' fee (`CurvePricingInputs::synthetic_fees`) at the curve's market cap on the
/// supply its caller passes — the named mint's, burned to 999,818,465,159,445 on `near166` —
/// while every other curve fee keeps the fixed 10¹⁵. Under three tiers that split the two market
/// caps (G0's tier tripwire: 290,200,481,257 on the live supply, 290,253,172,320 on 10¹⁵) the
/// curve path's `φ` is 70/22 and `φ_s` 80/25, the tier pump charges a buy past `rT` at; the
/// supply passed is the one used; without a supply (`venues::read_state`), on a complete curve or
/// on a cashback curve (v3 refuses it: no synthetic route, and not executable) `φ_s` is
/// unresolved.
#[test]
fn the_reader_resolves_the_synthetic_fee_on_the_live_supply() {
    let (mut env, pm, _) = setup(PumpCoin::Near166);
    let supply = mint_supply(&env, &pm);
    assert_eq!(supply, 999_818_465_159_445, "near166's burned supply");
    let c = pm.curve.state(&env.svm);
    let (vt, vq) = (
        u128::from(c.virtual_token_reserves),
        u128::from(c.virtual_quote_reserves),
    );
    assert_eq!(
        (
            vq * u128::from(supply) / vt,
            vq * fees::PUMP_CURVE_MCAP_SUPPLY / vt
        ),
        (290_200_481_257, 290_253_172_320),
        "the market caps on the live supply and on 10¹⁵"
    );
    let tiers = [
        (0, [0, 90, 28]),
        (290_174_135_726, [0, 80, 25]),
        (290_226_826_789, [0, 70, 22]),
    ];
    fork::set_fee_config(
        &mut env.svm,
        &PUMP_FEE_CONFIG,
        [0, 95, 30],
        &tiers,
        &tiers,
        [0, 95, 30],
    );
    let pricing = |st: &shortwin::venues::VenueState| {
        st.curve
            .as_ref()
            .and_then(|cv| cv.pricing.clone())
            .expect("the curve's pricing inputs")
    };
    let st = host_state_with(&env, &pm, Some(supply));
    assert!(st.executable, "a live curve");
    let p = pricing(&st);
    assert_eq!(p.fees, FeeBps::new(0, 70, 22), "the curve path: 10¹⁵");
    assert_eq!(
        p.synthetic_fees(),
        Some(FeeBps::new(0, 80, 25)),
        "φ_s: the live supply's tier"
    );
    assert_eq!(
        p.synthetic_fees(),
        pm.curve.synthetic_fees(&env.svm),
        "the harness's φ_s (pump_math_it's tripwire)"
    );
    assert_eq!(p.curve_pricing().synthetic, p.synthetic_fees());
    // The supply passed is the one used: at 10¹⁵ φ_s is the curve path's tier.
    let at_1e15 = pricing(&host_state_with(&env, &pm, Some(1_000_000_000_000_000)));
    assert_eq!(at_1e15.synthetic_fees(), Some(p.fees));
    // Without a supply: unresolved, the rest of the state unchanged.
    let without = host_state(&env, &pm);
    let pw = pricing(&without);
    assert_eq!(pw.synthetic_fees(), None, "no supply, no φ_s");
    assert_eq!(
        (
            without.executable,
            without.x,
            without.y,
            without.r,
            without.fee_ppm,
            pw.fees,
            pw.live_pool_tokens,
            pw.fee_ppm_at_ema
        ),
        (
            st.executable,
            st.x,
            st.y,
            st.r,
            st.fee_ppm,
            p.fees,
            p.live_pool_tokens,
            p.fee_ppm_at_ema
        )
    );
    // A cashback coin: no synthetic route, not executable (pump's v3 refuses it, D1 (a)).
    let saved = env.svm.get_account(&pm.curve.curve).unwrap();
    fork::patch(&mut env.svm, &pm.curve.curve, |d| {
        d[vpump::OFF_IS_CASHBACK_COIN] = 1
    });
    let cb = host_state_with(&env, &pm, Some(supply));
    assert!(!cb.executable, "a live cashback curve never executes");
    assert_eq!(pricing(&cb).synthetic_fees(), None, "cashback: no φ_s");
    env.svm.set_account(pm.curve.curve, saved).unwrap();
    // A complete curve (an outside buy of all of rT: migrating) has no synthetic route.
    buy_down_to(&mut env, &pm, 0);
    let done = host_state_with(&env, &pm, Some(supply));
    assert!(
        done.curve.as_ref().is_some_and(|cv| cv.migrating()),
        "migrating"
    );
    assert_eq!(pricing(&done).synthetic_fees(), None, "complete: no φ_s");
}
