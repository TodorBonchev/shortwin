//! Fork E2E on Fartcoin's AMM v4 pool (M3 exit criterion; ARCH §8.2, §8.4, §9): the release
//! short.win against the **dumped** AMM v4 program and Fartcoin's main pool `Bzc9NZfM…` (38,000
//! SOL, WSOL as the coin, 25/10,000, λ 99.78 %), driven only by real instructions — short.win's,
//! the venue's own swaps and the pool admin's `SetParams` — with the invariant hooks after
//! every instruction and every short.win instruction checked against `shortwin-math` by the
//! differential oracle of `raydium_v4_support.rs` (its module docs list what each check
//! recomputes).
//!
//! | Test | Path |
//! |---|---|
//! | `lifecycle` | list (D1 detection on Fartcoin's real graduated curve) → deposit → opens refused in the listing window, to the second → opens (Tier C for 24 h despite a fresh oracle: O13, AMM v4 age from the listing) → real sells move the price, a checked crank every second → a profitable close → real buys pump ×2.5 → two liquidations by normal buyback (every refusal before them predicted) → an open at the new price held to its deadline with hourly cranks → `liquidate` refuses `UseExpire`, `expire` executes → seven days in, an open at Tier A and its close → the lender's full withdrawal (`pool::withdraw` + `pool::claim`: its tokens back, its rent, penalty and fee income) |
//! | `admin_disable_reaches_s3_after_the_timer` | a liquidatable position whose pool the Raydium admin halts (`SetParams` status 4) before the keeper acts: exits refuse `NoRoute`, the S3 chain runs with a crank every 30 s, `sol_settle` pays S3 once `s3_wait_s` has passed — FV at the frozen EMA point, at the halted pool's own fee, with the penalty — and re-enabling clears both chains |
//! | `lp_removal_cannot_reach_s2_at_default_params` | the "thin pool via LP removal" path as far as the fixture allows it at the defaults: the dumped pool's LP is 99.78 % burned, so its own circulating LP can remove 0.22 % of the depth; with the LP redistributed to exactly `λ_min` (50 %) before listing, positions at the SPEC size caps and every unlocked LP token withdrawn afterwards, the pool keeps half its depth, the buyback's σ after a pump into liquidation stays far below `σ_max` (228 of 2,000 bps) and the positions are liquidated by normal buyback (ARCH §2.6 rule 5: opens are sized on locked depth) |
//! | `thin_pool_by_lp_removal_settles_s2` | the same under the governance corner that makes S2 reachable ([`support::s2_corner`], every value inside its compiled bounds: `λ_min` 20 %, Tier C `σ_max` 5 %, `fee_size_cap_x100` 500) and a pool at λ = 20 % ([`support::thin_pool_market`]): the LP withdrawal takes 80 % of the pool and leaves σ above `σ_max`, a real pump makes the position liquidatable, `liquidate` refuses `UseSolSettle`, the clock arms, and after `settle_persist_n` EMA updates `sol_settle` pays S2 capped at FV (the excess to the borrower, the premium over `Q·P_ema + R_due` vesting) |
//! | `a_real_gap_settles_s1` | a gap beyond the S1 boundary in one second: the clock arms and S1 pays `V − K_s`, never capped |

#![allow(clippy::result_large_err)]

#[path = "raydium_v4_support.rs"]
mod support;

use shortwin::state::LenderPosition;
use shortwin_math::fees::RayV4Fee;
use shortwin_math::pool as mpool;
use shortwin_math::quote::Pricing;
use shortwin_venue_state::raydium_v4;
use support::*;

/// Lent tokens of the default market: 10⁶ Fartcoin (≈ 1,600 SOL of value).
const LEND: u64 = 1_000_000_000_000;

// ---- lifecycle ---------------------------------------------------------------------------------

#[test]
fn lifecycle() {
    let mut env = fork_env();
    let pool = env.load_ray_pool(FART);
    let created = list_checked(&mut env, &pool);
    let rm = env.ray_market_listed(
        RaySpec {
            fixture: FART,
            lend: 0,
            warmup: 0,
        },
        pool,
    );
    // Deposit: the lender's tokens, its shares.
    let lender = env.funded_keypair();
    let lender_acc = fork::fund_token(
        &mut env,
        &lender.pubkey(),
        &rm.keys.mint,
        TokenProgram::Spl,
        LEND,
    );
    let dep = deposit_checked(&mut env, &rm, &lender, &lender_acc, LEND);
    // Opens wait out the listing window (ARCH §2.6 rule 1), to the second.
    let window = i64::from(env.config().params.pool_list_window_s);
    let open_at = created.ts + window;
    // The keeper's observations at the end of the listing window (M4V-5's price warm-up).
    env.warp(open_at - 1 - raydium::PRICE_WARMUP_CRANKS - env.now());
    env.warm_up_price(&rm);
    crank_checked(&mut env, &rm);
    let q_a = q_for_sol(&env, &rm, 4 * SOL);
    let mut probe = env.borrower();
    assert_err(&env.open(&rm, &mut probe, q_a).0, E::PoolListWindow);
    env.warp(1);
    crank_checked(&mut env, &rm);
    assert_eq!(env.now(), open_at);

    let whale = actor(&mut env, &rm, 60_000 * SOL, 40_000_000_000_000);
    let keeper = env.funded_keypair();
    // A fresh SOL/USD price: the pool's Λ is far above θ_A, yet the market is Tier C for 24 h
    // (O13: an AMM v4 pool's age counts from the listing).
    env.fresh_oracle();

    // Two opens at the listing price.
    let mut a = env.borrower();
    let (open_a, nonce_a) = open_checked(&mut env, &rm, &mut a, q_a);
    let q_b = q_for_sol(&env, &rm, 5 * SOL);
    let mut b = env.borrower();
    let (_open_b, nonce_b) = open_checked(&mut env, &rm, &mut b, q_b);
    assert_eq!(open_a.tier, 2, "Tier C: listed less than 24 h ago (O13)");
    tick_checked(&mut env, &rm, 5);

    // Real sells take the price 12 % down over 3 s; the keeper cranks every second.
    let p0 = spot(&env, &rm);
    for k in 1..=3 {
        honest_second(
            &mut env,
            &rm,
            &whale,
            Some(scale(p0, 1.0 - 0.04 * f64::from(k))),
        );
    }
    tick_checked(&mut env, &rm, 3);
    // A closes in profit: its buyback costs less than its sale brought.
    let close_a = close_checked(&mut env, &rm, &a, nonce_a, u64::MAX).expect("A closes");
    assert!(
        close_a.buyback_cost < open_a.s,
        "a short gains when the price falls"
    );
    assert!(
        close_a.borrower_payout > open_a.m,
        "A takes back more than its margin"
    );

    // C opens at the lower price.
    let q_c = q_for_sol(&env, &rm, 4 * SOL);
    let mut c = env.borrower();
    let (_open_c, nonce_c) = open_checked(&mut env, &rm, &mut c, q_c);
    tick_checked(&mut env, &rm, 3);

    // Real buys pump the price ×2.5 over 5 s. B (opened at the listing price) and C both fall
    // below their liquidation CR; neither gaps into S1.
    let p1 = spot(&env, &rm);
    for k in 1..=5 {
        honest_second(
            &mut env,
            &rm,
            &whale,
            Some(scale(p1, 2.5f64.powf(f64::from(k) / 5.0))),
        );
    }
    for (bk, n) in [(&b, nonce_b), (&c, nonce_c)] {
        let p = env.position(&rm, &bk.key(), n);
        assert!(
            s1_boundary(&env, &rm, &p) > 1.05,
            "no gap: a normal buyback is possible"
        );
    }
    // The keeper tries every second; every refusal before the liquidation is the oracle's.
    let mut liquidated = Vec::new();
    for (bk, n) in [(&b, nonce_b), (&c, nonce_c)] {
        let (ex, refusals) = attempts(&mut env, &rm, Which::Liquidate, &keeper, &bk.key(), n, 40);
        let ex = ex.unwrap_or_else(|| panic!("never liquidated; refusals {refusals:?}"));
        assert!(
            refusals
                .iter()
                .all(|e| matches!(e, E::NotLiquidatable | E::ExitBoundNotMet)),
            "{refusals:?}"
        );
        let ev = clone_event(ex.liquidated());
        assert!(!ev.expired && ev.penalty > 0);
        assert!(
            u128::from(ev.buyback_cost) * 10_000
                <= u128::from(ev.b_ref)
                    * (10_000 + u128::from(ex.predicted().view.pos.delta_liq_bps)),
            "the fixed bound over the reference"
        );
        liquidated.push((ev, ex.predicted().k_s));
    }
    for (who, (l, _)) in ["B", "C"].iter().zip(&liquidated) {
        eprintln!(
            "liquidation of {who}: B {:.4} SOL (B̂_ref {:.4}, max_in {:.4}), rent {:.6}, penalty \
             {:.4} (lenders {:.4}, protocol {:.4}, keeper {:.4} + top-up {:.4}), borrower {:.4}",
            l.buyback_cost as f64 / 1e9,
            l.b_ref as f64 / 1e9,
            l.max_in as f64 / 1e9,
            l.rent as f64 / 1e9,
            l.penalty as f64 / 1e9,
            l.penalty_lender as f64 / 1e9,
            l.penalty_protocol as f64 / 1e9,
            l.penalty_keeper as f64 / 1e9,
            l.keeper_topup as f64 / 1e9,
            l.borrower_payout as f64 / 1e9
        );
    }
    // Both leave the borrower a remainder, so each liquidation paid its keeper at least the
    // position's K_s (the keeper floor, N22).
    for (l, k_s) in &liquidated {
        assert!(l.borrower_payout > 0 && l.penalty_keeper + l.keeper_topup >= *k_s);
    }

    // D opens at the new price once the EMA has caught up, then the price stays put until its
    // deadline (7 days, Tier C): an expiry, not a liquidation.
    tick_checked(&mut env, &rm, 30);
    let q_d = q_for_sol(&env, &rm, 4 * SOL);
    let mut d = env.borrower();
    let (_open_d, nonce_d) = open_checked(&mut env, &rm, &mut d, q_d);
    let pos_d = env.position(&rm, &d.key(), nonce_d);
    // Hourly cranks keep the rent accruing on a live EMA.
    while env.now() + 3_600 < pos_d.deadline - 10 {
        env.warp(3_600);
        crank_checked(&mut env, &rm);
    }
    env.warp(pos_d.deadline - 10 - env.now());
    tick_checked(&mut env, &rm, 10);
    assert_eq!(env.now(), pos_d.deadline);
    let pred = predict(&view(&mut env, &rm, &d.key(), nonce_d));
    assert!(pred.expired && !pred.cr_liquidatable());
    let ex = exit_alone(&mut env, &rm, Which::Liquidate, &keeper, &d.key(), nonce_d);
    assert_eq!(ex.refused(), E::UseExpire);
    let ex = exit_alone(&mut env, &rm, Which::Expire, &keeper, &d.key(), nonce_d);
    let e = clone_event(ex.expired());
    assert!(e.expiry_fee > 0 && e.keeper_reward > 0 && e.borrower_payout > 0);
    assert!(e.rent >= pos_d.r_min, "a week of rent");
    eprintln!(
        "expiry of D: B {:.4} SOL, rent {:.6}, expiry fee {:.6}, keeper {:.6}, borrower {:.4}",
        e.buyback_cost as f64 / 1e9,
        e.rent as f64 / 1e9,
        e.expiry_fee as f64 / 1e9,
        e.keeper_reward as f64 / 1e9,
        e.borrower_payout as f64 / 1e9
    );

    // A last round trip, seven days after the listing with a fresh oracle: Tier A now (O13).
    env.fresh_oracle();
    let q_e = q_for_sol(&env, &rm, 3 * SOL);
    let mut eb = env.borrower();
    let (open_e, nonce_e) = open_checked(&mut env, &rm, &mut eb, q_e);
    assert_eq!(
        open_e.tier, 0,
        "Tier A: Λ ≥ θ_A and 7 days since the listing"
    );
    tick_checked(&mut env, &rm, 30);
    close_checked(&mut env, &rm, &eb, nonce_e, u64::MAX).expect("E closes");

    // Every position is gone; every borrowed token came back.
    let m = env.market(&rm.market());
    assert_eq!(
        (m.open_positions, m.borrowed_q, m.total_notional_open),
        (0, 0, 0)
    );
    assert_eq!(m.tokens_free, LEND, "all lent tokens are back in the pool");

    // The lender withdraws every share once the lumps have vested: its tokens and its SOL
    // (`pool::withdraw`, then the SOL part as `pool::claim`), and a later claim pays 0.
    let lock_grace_s = env.config().params.lock_grace_s;
    let vest = i64::from(env.config().params.lump_vest_s);
    env.warp(i64::from(lock_grace_s).max(vest) + 86_400);
    crank_checked(&mut env, &rm);
    let market = rm.market();
    let keys = lender_keys(&rm, &lender.pubkey(), &lender_acc);
    let lp: LenderPosition =
        read::borsh(&env.svm, &pda::lender(&market, &lender.pubkey()).0).unwrap();
    assert_eq!(lp.shares, dep.shares);
    let now = env.now();
    let (mut p, mut v, mut l) = (
        env.market(&market).pool(),
        *env.market_vault(&market).vault(),
        lp.lender(),
    );
    let want_tokens = mpool::withdraw(
        &mut p,
        &mut v,
        &mut l,
        &mut [],
        now,
        lock_grace_s,
        lp.shares,
    )
    .unwrap();
    let want_sol = mpool::claim(&p, &mut v, &mut l, &mut [], now).unwrap();
    let lp_key = pda::lender(&market, &lender.pubkey()).0;
    let lp_rent = lamports(&env, &lp_key);
    let (acc0, l0) = (tokens(&env, &lender_acc), lamports(&env, &lender.pubkey()));
    let meta = ok(env.send(&[ix::withdraw(&keys, lp.shares, 0)], &[&lender]));
    let w: shortwin::events::Withdraw = events::one(&meta);
    assert_eq!(
        (w.amount, w.sol_paid),
        (want_tokens, want_sol),
        "pool::withdraw, pool::claim"
    );
    assert_eq!(tokens(&env, &lender_acc) - acc0, want_tokens);
    let closed = read::account(&env.svm, &lp_key).is_none();
    assert!(closed, "a lender with nothing left is closed to its owner");
    assert_eq!(lamports(&env, &lender.pubkey()) - l0, want_sol + lp_rent);
    assert_eq!(env.market(&market).pool(), p);
    assert_eq!(*env.market_vault(&market).vault(), v);
    assert!(
        w.amount + 1 >= LEND,
        "the lender gets its tokens back (dust aside)"
    );
    let income: u64 = liquidated
        .iter()
        .map(|(x, _)| x.penalty_lender)
        .sum::<u64>()
        + e.expiry_fee;
    assert!(
        want_sol > income,
        "rent, penalties and the expiry fee reach the lender"
    );
    eprintln!(
        "lender exit: {} tokens back of {LEND}, {:.6} SOL of income",
        w.amount,
        want_sol as f64 / 1e9
    );
}

// ---- S3: an admin halt ------------------------------------------------------------------------

/// SPEC §5.3 S3 on the real venue: a liquidatable position whose pool the Raydium admin halts
/// before any keeper liquidates it (module docs).
#[test]
fn admin_disable_reaches_s3_after_the_timer() {
    let mut f = Fart::new(fork_env(), LEND);
    let q = q_for_sol(&f.env, &f.rm, 5 * SOL);
    let (b, nonce, _) = f.open(q);
    f.tick(5);
    // A real pump (×2.3) makes the position liquidatable on both legs.
    f.pump_until_liquidatable(&b.key(), nonce, 2.3, 5, 60);
    let pos = f.pos(&b, nonce);
    // The admin halts the pool (SetParams status 4: LiquidityOnly) before the keeper acts.
    f.env.warp(1);
    set_status(&mut f.env, &f.rm, raydium_v4::STATUS_LIQUIDITY_ONLY);
    f.second();
    let m = f.market();
    let halted_at = f.env.now();
    assert_eq!(
        (m.no_route_since, m.pool_fail_since),
        (halted_at, halted_at)
    );
    assert_eq!(m.b_lo.get(), 0, "nothing observed on a halted pool");
    // Exits and closes refuse: no route.
    assert_eq!(
        f.exit(Which::Liquidate, &b.key(), nonce).refused(),
        E::NoRoute
    );
    assert_eq!(
        close_checked(&mut f.env, &f.rm, &b, nonce, u64::MAX).err(),
        Some(E::NoRoute)
    );
    // The keeper cranks every 30 s (the continuous chain, ECON §5.8 duty 5) and calls
    // sol_settle each time: no settlement until s3_wait_s has run.
    let mut settled = None;
    for _ in 0..40 {
        f.env.warp(30);
        crank_checked(&mut f.env, &f.rm);
        let s = f.settle(&b.key(), nonce);
        if let Some(ev) = s.ev {
            settled = Some((f.env.now(), ev, s.pred));
            break;
        }
    }
    let (at, ev, pred) = settled.expect("S3");
    assert_eq!(ev.condition, 3, "S3");
    assert!(at >= halted_at + i64::from(pos.s3_wait_s));
    assert!(
        at < halted_at + i64::from(pos.s3_wait_s) + 30,
        "the first call past the timer settles"
    );
    // The box is the frozen EMA point (a halted pool records nothing for RING_LEN seconds).
    let m = f.market();
    assert_eq!((ev.p_lo, ev.p_hi), (m.ema_price.get(), m.ema_price.get()));
    assert_eq!((ev.l_lo, ev.l_hi), (m.ema_liq.get(), m.ema_liq.get()));
    // FV at the halted pool's own fee (25/10,000), with the liquidation penalty (not expired).
    assert_eq!(
        (ev.pprime_lo, ev.pprime_hi),
        (pos.penalty_bps, pos.penalty_bps)
    );
    let r_due = pred.inp.r_due;
    let b_fv = Pricing::RayV4(RayV4Fee {
        num: 25,
        den: 10_000,
    })
    .buy_repriced(pos.q, m.ema_liq.get(), m.ema_price.get())
    .unwrap()
    .cost;
    let fv =
        b_fv + (u128::from(b_fv) * u128::from(pos.penalty_bps)).div_ceil(10_000) as u64 + r_due;
    assert_eq!(
        ev.fv, fv,
        "FV = B̂(Q; reprice(L_ema, P_ema)) + ⌈p·B̂⌉ + R_due"
    );
    assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
    assert!(
        ev.lender_amount < pos.v - k_s(&pos),
        "the FV cap binds: the borrower keeps the excess"
    );
    assert_eq!(ev.borrower_amount, pos.v - k_s(&pos) - ev.lender_amount);
    eprintln!(
        "S3 {} s after the halt: V {:.4} SOL, FV {:.4}, lenders {:.4} (lump {:.4}), keeper {:.4}, \
         borrower {:.4}",
        at - halted_at,
        pos.v as f64 / 1e9,
        ev.fv as f64 / 1e9,
        ev.lender_amount as f64 / 1e9,
        ev.lump as f64 / 1e9,
        ev.k_s as f64 / 1e9,
        ev.borrower_amount as f64 / 1e9
    );
    // Re-enabling the pool clears both chains; the market observes again.
    f.env.warp(1);
    set_status(&mut f.env, &f.rm, raydium_v4::STATUS_SWAP_ONLY);
    f.second();
    let m = f.market();
    assert_eq!(
        (m.no_route_since, m.pool_fail_since, m.open_positions),
        (0, 0, 0)
    );
    assert!(m.b_lo.get() > 0);
}

// ---- S2: a thin pool by LP removal -----------------------------------------------------------

/// Opens positions at the SPEC size caps (the per-position cap `q_max`, until the market cap
/// binds), one per new borrower. Returns them.
fn open_to_the_caps(f: &mut Fart, max: usize) -> Vec<(Borrower, u64)> {
    let mut out = Vec::new();
    for _ in 0..max {
        let q = max_open_q(f);
        if q == 0 {
            break;
        }
        let mut b = f.env.borrower();
        let saved = f.env.svm.clone();
        let (res, _) = f.env.open(&f.rm, &mut b, q);
        f.env.svm = saved;
        if res.is_err() {
            break;
        }
        let (_, nonce) = open_checked(&mut f.env, &f.rm, &mut b, q);
        out.push((b, nonce));
        f.second();
    }
    out
}

/// ARCH §2.6 rule 5 on the real pool: a pool at exactly `λ_min` (50 %), positions at the size
/// caps, every unlocked LP token withdrawn — the pool keeps half its depth, the buyback's σ stays
/// far below `σ_max` (20 %, Tier C) even after the pump that makes the positions liquidatable,
/// and they are liquidated by a normal buyback.
#[test]
fn lp_removal_cannot_reach_s2_at_default_params() {
    let lp_holder = Keypair::new();
    let holder_key = lp_holder.pubkey();
    let mut lp_acc = Pubkey::default();
    let mut f = Fart::with_pool(fork_env_with(lift_notional), 2_000_000_000_000, |env, p| {
        lp_acc = hold_unlocked_lp(env, p, &holder_key, 5_000).0;
    });
    assert_eq!(lambda_bps(&f.env, &f.rm), 5_000);
    let d0 = locked_depth(&f.env, &f.rm);
    let poss = open_to_the_caps(&mut f, 3);
    assert!(!poss.is_empty());
    // The holder withdraws every unlocked LP token.
    let holder = Trader {
        owner: lp_holder.insecure_clone(),
        wsol: fork::fund_wsol(&mut f.env, &holder_key, 0),
        token: fork::fund_token(
            &mut f.env,
            &holder_key,
            &f.rm.keys.mint,
            TokenProgram::Spl,
            0,
        ),
    };
    let (x0, _) = reserves(&f.env, &f.rm).unwrap();
    let held = tokens(&f.env, &lp_acc);
    remove_liquidity(&mut f.env, &f.rm, &holder, &lp_acc, held);
    let (x1, _) = reserves(&f.env, &f.rm).unwrap();
    assert!(
        x1 * 2 >= x0 - 2 && x1 * 2 <= x0 + 2,
        "half the depth left: {x0} → {x1}"
    );
    assert_eq!(lambda_bps(&f.env, &f.rm), 10_000, "only locked LP remains");
    let d1 = locked_depth(&f.env, &f.rm);
    assert!(
        d1 * 1_000 >= d0 * 999,
        "the locked depth does not move: {d0} → {d1}"
    );
    f.tick(90);
    // A pump until the first position is liquidatable; σ stays below σ_max throughout.
    let (b0, n0) = (poss[0].0.key(), poss[0].1);
    f.pump_until_liquidatable(&b0, n0, 2.3, 5, 60);
    let pred = f.predict(&b0, n0);
    let sigma_max = u64::from(pred.view.pos.max_liq_slip_bps);
    eprintln!(
        "λ_min pool, {} positions at the caps, every unlocked LP withdrawn ({x0} → {x1} tokens), \
         pumped into liquidation: σ_live {} bps against σ_max {sigma_max} bps",
        poss.len(),
        pred.sigma_live
    );
    assert!(
        pred.sigma_live * 4 < sigma_max,
        "σ_live {} bps ≪ σ_max {sigma_max} bps: no S2 by LP removal",
        pred.sigma_live
    );
    // Every position exits by a normal buyback (the oracle checks every one).
    let keeper = f.keeper.insecure_clone();
    for (b, n) in &poss {
        let (ex, refusals) = attempts(
            &mut f.env,
            &f.rm,
            Which::Liquidate,
            &keeper,
            &b.key(),
            *n,
            60,
        );
        let ex = ex.unwrap_or_else(|| panic!("never liquidated: {refusals:?}"));
        assert!(!refusals.contains(&E::UseSolSettle));
        assert!(ex.predicted().sigma_live < sigma_max);
    }
}

/// S2 on the real venue (module docs): the governance corner inside the compiled bounds that
/// makes a pool thin enough for S2 reachable by LP removal.
#[test]
fn thin_pool_by_lp_removal_settles_s2() {
    let ThinPool {
        mut f,
        b,
        nonce,
        open,
        pos,
        ..
    } = thin_pool_market();
    // The keeper: a liquidation is refused (σ above σ_max), sol_settle arms and settles S2.
    let mut armed_at = None;
    let mut done = None;
    for s in 0..120u32 {
        if s > 0 {
            f.second();
        }
        if s % 10 == 0 {
            assert_eq!(
                f.exit(Which::Liquidate, &b.key(), nonce).refused(),
                E::UseSolSettle
            );
        }
        let out = f.settle(&b.key(), nonce);
        if out.armed.is_some() {
            armed_at = Some(f.market().ema_count);
        }
        if let Some(ev) = out.ev {
            done = Some((ev, out.pred));
            break;
        }
    }
    let (ev, pred) = done.expect("S2");
    assert_eq!(ev.condition, 2, "S2");
    let armed_count = armed_at.expect("armed");
    assert!(pred.inp.ema_count >= armed_count + u64::from(pos.settle_persist_n));
    let take = pos.v - k_s(&pos);
    assert!(ev.fv > 0 && ev.lender_amount == ev.fv.min(take));
    assert!(ev.lender_amount < take, "the FV cap binds");
    assert_eq!(ev.borrower_amount, take - ev.lender_amount);
    assert_eq!(
        (ev.pprime_lo, ev.pprime_hi),
        (pos.penalty_bps, pos.penalty_bps)
    );
    assert!(ev.lump > 0, "the premium over Q·P_ema + R_due vests");
    eprintln!(
        "S2 on the thin pool (σ_live {} bps > σ_max {} bps): V {:.4} SOL, FV {:.4}, lenders {:.4} \
         (lump {:.4}), keeper {:.4}, borrower {:.4}",
        pred.inp.sigma_live,
        pos.max_liq_slip_bps,
        pos.v as f64 / 1e9,
        ev.fv as f64 / 1e9,
        ev.lender_amount as f64 / 1e9,
        ev.lump as f64 / 1e9,
        ev.k_s as f64 / 1e9,
        ev.borrower_amount as f64 / 1e9
    );
    assert!(open.v > ev.lender_amount);
}

// ---- S1: a real gap --------------------------------------------------------------------------

/// A genuine gap past the S1 boundary in one second (a whale's single buy): the clock arms on
/// the S1 legs and S1 pays `V − K_s` after `settle_persist_n` updates — never capped.
#[test]
fn a_real_gap_settles_s1() {
    let mut f = Fart::new(fork_env(), LEND);
    let q = q_for_sol(&f.env, &f.rm, 5 * SOL);
    let (b, nonce, _) = f.open(q);
    f.tick(5);
    let pos = f.pos(&b, nonce);
    let kb = s1_boundary(&f.env, &f.rm, &pos);
    f.env.warp(1);
    f.move_by(kb * 1.10);
    crank_checked(&mut f.env, &f.rm);
    let mut done = None;
    for _ in 0..90 {
        f.second();
        let out = f.settle(&b.key(), nonce);
        if let Some(ev) = out.ev {
            done = Some(ev);
            break;
        }
    }
    let ev = done.expect("S1");
    assert_eq!(ev.condition, 1, "S1");
    assert_eq!(ev.lender_amount, pos.v - k_s(&pos), "S1 is never capped");
    assert_eq!((ev.borrower_amount, ev.lump, ev.fv), (0, 0, 0));
}
