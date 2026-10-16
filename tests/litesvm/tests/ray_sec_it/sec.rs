//! The M3 security review's findings as regressions (see `main.rs`). Each test was first run as
//! the audit's reproducer against the unfixed program, then inverted to assert the fix.

use shortwin::events::{MarketCreated, SolSettle};
use shortwin::state::ShortPosition;
use shortwin_litesvm_tests::engine::{liquidate_ix, sol_settle_ix};
use shortwin_litesvm_tests::fork::Swap;
use shortwin_litesvm_tests::invariants;
use shortwin_math::fees::Direction;
use shortwin_math::quote::{Pricing, cpmm as qcpmm};
use shortwin_math::rent;
use solana_keypair::Keypair;

use crate::common::*;

/// `K_s` of a position (its locked settlement-keeper terms).
fn k_s(p: &ShortPosition) -> u64 {
    let pct = u128::from(p.v) * u128::from(p.settle_keeper_bps) / 10_000;
    (u128::from(p.settle_keeper_fixed) + pct)
        .min(u128::from(p.settle_keeper_cap))
        .min(u128::from(p.v)) as u64
}

/// `FV(P, L) = B̂(Q; reprice(L, P); pricing) + ⌈p'·B̂/10⁴⌉ + R_due` (ARCH §4.5).
fn fair_value(pricing: Pricing, q: u64, p: u128, l: u128, pprime_bps: u16, r_due: u64) -> u64 {
    let b = pricing.buy_repriced(q, l, p).expect("FV quote").cost;
    b + (u128::from(b) * u128::from(pprime_bps)).div_ceil(10_000) as u64 + r_due
}

/// One `sol_settle` by `keeper`: `Some(event)` if it settled.
fn settle(
    env: &mut Env,
    rm: &RayMarket,
    keeper: &Keypair,
    b: &Borrower,
    nonce: u64,
) -> Option<SolSettle> {
    let ix = sol_settle_ix(env, rm, &keeper.pubkey(), &b.key(), nonce);
    let meta = ok(env.send(&[ix], &[keeper]));
    events::of::<SolSettle>(&meta).into_iter().next()
}

/// Cranks every `step` seconds and calls `sol_settle` after each crank, `n` times: the first
/// settlement, if any.
fn run_settle(
    env: &mut Env,
    rm: &RayMarket,
    keeper: &Keypair,
    b: &Borrower,
    nonce: u64,
    step: i64,
    n: i64,
) -> Option<SolSettle> {
    for _ in 0..n {
        env.warp(step);
        env.crank(rm);
        if let Some(ev) = settle(env, rm, keeper, b, nonce) {
            return Some(ev);
        }
    }
    None
}

/// The pool admin's real `update_amm_config(param 0 = trade_fee_rate)` on the market's config.
fn set_trade_fee(env: &mut Env, rm: &RayMarket, ppm: u64) {
    let cfg = rm.cpmm().config_key(&env.svm);
    ok(fork::raydium_admin_send(
        env,
        fork::cpmm_update_amm_config_ix(&cfg, 0, ppm),
    ));
    assert_eq!(rm.cpmm().config(&env.svm).trade_fee_rate, ppm);
}

/// M3-SEC-1. The pool admin sets config 0's trade fee to 0 (`update_amm_config(0, 0)`, accepted:
/// `t + c < 10⁶`). USELESS has no creator fee and `creator_fee_on = 0`, so every swap of the
/// pool reverts in the venue (`split_creator_fee` divides by `t + c = 0`: `ZeroTradingTokens`).
///
/// Before the fix the reader called the pool executable and every quote on it `+∞`: a healthy
/// position turned liquidatable on both legs and a keeper S1-settled it within ≈ 40 s at an
/// unchanged price (lenders `V − K_s`, the borrower 0). Now the pool is **not executable** (ARCH
/// §5.3's CPMM predicate with M3-SEC-1's fee-split term): nothing is observed, the S3 and
/// `pool_fail` chains start, opens and closes fail with `NoRoute`, and the healthy position stays
/// open past `s3_wait_s + 120 s` (ARCH §5.3 "healthy positions stay frozen"); with the fee
/// restored it closes normally at the live fee.
#[test]
fn m3_sec_1_a_zero_cpmm_trade_fee_is_no_route_not_an_uncapped_s1() {
    let (mut env, rm, q) = setup(USELESS);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    env.tick(&rm, 30);
    let keeper = env.funded_keypair();
    assert!(
        settle(&mut env, &rm, &keeper, &b, nonce).is_none(),
        "healthy"
    );
    let pos = env.position(&rm, &b.key(), nonce);
    assert!(
        pos.v > 2 * pos.n_open,
        "a healthy position (V {} vs notional {})",
        pos.v,
        pos.n_open
    );
    let trade0 = rm.cpmm().config(&env.svm).trade_fee_rate;
    assert_eq!(trade0, 2_500);

    set_trade_fee(&mut env, &rm, 0);

    // The venue: every swap of the pool reverts, both directions, both modes.
    let (mint, program) = rm.pool.token();
    let t = fork::trader(&mut env, &mint, program, 10_000_000_000, q);
    for (dir, s) in [
        (Direction::Buy, Swap::ExactIn(1_000_000_000, 0)),
        (Direction::Buy, Swap::ExactOut(u64::MAX, q / 10)),
        (Direction::Sell, Swap::ExactIn(q / 10, 0)),
    ] {
        let res = fork::swap(&mut env, &rm.pool, &t, dir, s);
        assert_eq!(
            custom_code(&res),
            Some(fork::CPMM_ERR_ZERO_TRADING_TOKENS),
            "{dir:?} {s:?}"
        );
    }
    assert!(!rm.cpmm().venue_executable(&env.svm, env.now()));

    // short.win: no route. The chains start, the pool is not observed.
    env.warp(1);
    let m0 = env.market(&rm.market());
    env.crank(&rm);
    let m = env.market(&rm.market());
    assert_eq!(m.no_route_since, env.now(), "the S3 chain starts");
    assert_eq!(m.pool_fail_since, env.now(), "the pool_fail chain starts");
    assert_eq!(m.b_lo.get(), 0, "a pool that cannot swap is not observed");
    assert_eq!(
        m.ema_count,
        m0.ema_count + 1,
        "the bucket before the fee change integrates"
    );
    let (ema, since) = (m.ema_price.get(), m.no_route_since);
    assert_err(&env.close(&rm, &b, nonce), E::NoRoute);
    let mut b2 = env.borrower();
    assert_err(&env.open(&rm, &mut b2, q).0, E::NoRoute);
    let liq = liquidate_ix(&rm, &keeper.pubkey(), &b.key(), nonce);
    let res = env.send(&[liq], &[&keeper]);
    assert!(res.is_err(), "no buyback can execute");
    println!(
        "M3-SEC-1: liquidate on the zero-fee pool fails with {:?}",
        custom_code(&res)
    );

    // Past the whole S3 wait and more: no settlement of the healthy position, S1 or S3.
    let wait = i64::from(pos.s3_wait_s) + 120;
    assert!(
        run_settle(&mut env, &rm, &keeper, &b, nonce, 10, wait / 10).is_none(),
        "a healthy position stays frozen"
    );
    let m = env.market(&rm.market());
    assert_eq!(m.no_route_since, since, "one continuous chain");
    assert!(
        env.now() >= since + i64::from(pos.s3_wait_s) + 60,
        "the chain ran s3_wait_s"
    );
    assert_eq!(m.ema_price.get(), ema, "the EMA is frozen");
    assert!(env.now() < pos.deadline);

    // The fee returns: the route and the chains clear, the position closes at the live fee.
    set_trade_fee(&mut env, &rm, trade0);
    env.warp(1);
    env.crank(&rm);
    let m = env.market(&rm.market());
    assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0));
    env.warp(1);
    let b_exp = rm.quote_buy(&env.svm, q).unwrap().cost;
    let (close, _) = close_ok(&mut env, &rm, &b, nonce);
    assert_eq!(close.buyback_cost, b_exp, "the live fee again");
}

/// M3-SEC-1 (the risk view). Where the pool is correctly not executable — the admin also halts
/// it (status bit 2) — the zero fee used to leave every risk quote undefined (`quote::cpmm`
/// fails the creator-fee split on `t + c = 0`): the EMA leg read `+∞` and FV was undefined, so
/// the S3 chain settled a healthy, unexpired position with lenders taking `V − K_s`. Now the risk
/// view prices a zero fee fee-free (`CpmmFees::capped`): with or without the zero fee a halt
/// freezes the healthy position past `s3_wait_s + 60 s`, and at the deadline S3 pays FV on the
/// frozen EMA point at that fee-free price, the excess to the borrower.
#[test]
fn m3_sec_1b_a_halted_zero_fee_pool_freezes_healthy_positions() {
    for zero_fee in [false, true] {
        let (mut env, rm, q) = setup(USELESS);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        env.tick(&rm, 30);
        let keeper = env.funded_keypair();
        let pos = env.position(&rm, &b.key(), nonce);
        if zero_fee {
            set_trade_fee(&mut env, &rm, 0);
        }
        ok(fork::raydium_admin_send(
            &mut env,
            fork::cpmm_update_pool_status_ix(rm.cpmm(), 4),
        ));
        let wait = i64::from(pos.s3_wait_s) + 60;
        assert!(
            run_settle(&mut env, &rm, &keeper, &b, nonce, 10, wait / 10).is_none(),
            "zero fee {zero_fee}: a halt freezes a healthy position"
        );
        assert!(env.now() < pos.deadline);
        // At the deadline: S3 at FV on the frozen EMA point, priced at the risk view.
        let m0 = env.market(&rm.market());
        let slot = env.clock().slot;
        env.set_clock(pos.deadline - 5, slot + 1_500_000);
        let ev = run_settle(&mut env, &rm, &keeper, &b, nonce, 30, 30).expect("S3 at the deadline");
        assert_eq!(ev.condition, 3, "zero fee {zero_fee}");
        let m = env.market(&rm.market());
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        assert_eq!((p, l), (m0.ema_price.get(), m0.ema_liq.get()), "frozen");
        assert_eq!(ev.pprime_lo, pos.expiry_fee_bps, "O15: an expired position");
        let cap = u64::from(pos.venue_fee_hard_cap_bps);
        let risk = |dir: Direction| rm.cpmm().fees(&env.svm, dir).unwrap().capped(cap);
        let pricing = Pricing::Cpmm {
            buy: risk(Direction::Buy),
            sell: risk(Direction::Sell),
        };
        if zero_fee {
            let Pricing::Cpmm { buy, .. } = pricing else {
                unreachable!()
            };
            assert_eq!(
                (buy.trade, buy.creator, buy.creator_on_input),
                (0, 0, false)
            );
        }
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let fv = fair_value(pricing, pos.q, p, l, ev.pprime_lo, r_due);
        assert_eq!(ev.fv, fv, "zero fee {zero_fee}: FV at the risk view");
        assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
        assert_eq!(ev.lender_amount + ev.k_s + ev.borrower_amount, pos.v);
        assert!(
            ev.borrower_amount > 0,
            "zero fee {zero_fee}: the excess returns"
        );
        println!(
            "M3-SEC-1b (zero fee {zero_fee}): S3 at the deadline; V {} FV {} lenders {} borrower {}",
            pos.v, ev.fv, ev.lender_amount, ev.borrower_amount
        );
    }
}

/// M3-SEC-1 (root cause, differential against the ELF), T-RAY-DISABLE / T-RAY-MATH with zero-fee
/// configs: on both WSOL sides, every creator-fee setting, `(t, c) ∈ {(0, 0), (0, 500), (1, 0)}`,
/// both directions and modes:
///
/// - the execution quote is defined exactly when the dumped CPMM ELF swaps, and a swap fails
///   exactly when `t + c_eff = 0` with the creator fee on its input (`ZeroTradingTokens`);
/// - `venue_executable` is exactly "the **buy** can swap" (a sell-only failure keeps the route);
/// - the risk view (`CpmmFees::capped`) quotes every case, fee-free where the fee is zero.
#[test]
fn m3_sec_1c_cpmm_zero_fee_predicate_vs_elf() {
    use shortwin_litesvm_tests::fork::Trader;
    let (mut broken_swaps, mut not_executable) = (0, 0);
    for which in [CpmmFixture::Useless, CpmmFixture::WsolToken1] {
        let mut env = Env::initialized();
        env.load_raydium_programs();
        let p = env.load_cpmm(which);
        env.set_fork_clock();
        let pool = AnyPool::Cpmm(p);
        let (mint, program) = pool.token();
        let t: Trader = fork::trader(&mut env, &mint, program, 0, 0);
        let cfg = p.config_key(&env.svm);
        let base = env.svm.clone();
        for (trade, creator) in [(0u64, 0u64), (0, 500), (1, 0)] {
            for (enable, on) in [(false, 0u8), (true, 0), (true, 1), (true, 2)] {
                let c_eff = if enable { creator } else { 0 };
                let on_input = |dir: Direction| {
                    on == 0 || (on == 1) == ((dir == Direction::Buy) == p.sol_is_token0)
                };
                let broken = |dir: Direction| trade + c_eff == 0 && on_input(dir);
                let case = format!("{which:?} t={trade} c={creator} enable={enable} on={on}");
                env.svm = base.clone();
                fork::set_cpmm_config_rates(&mut env.svm, &cfg, trade, 120_000, 40_000, creator);
                fork::set_cpmm_creator_fee(&mut env.svm, &p, enable, on);
                let exec = p.venue_executable(&env.svm, env.now());
                assert_eq!(
                    exec,
                    !broken(Direction::Buy),
                    "{case}: executable = the buy swaps"
                );
                if !exec {
                    not_executable += 1;
                }
                for dir in [Direction::Buy, Direction::Sell] {
                    for exact_in in [true, false] {
                        env.svm = base.clone();
                        fork::set_cpmm_config_rates(
                            &mut env.svm,
                            &cfg,
                            trade,
                            120_000,
                            40_000,
                            creator,
                        );
                        fork::set_cpmm_creator_fee(&mut env.svm, &p, enable, on);
                        let case = format!("{case} {dir:?} exact_in={exact_in}");
                        let (x_in, x_out) = pool.io_reserves(&env.svm, dir).expect("reserves");
                        let amount = if exact_in {
                            x_in / 10_000
                        } else {
                            x_out / 10_000
                        };
                        let fees = p.fees(&env.svm, dir).expect("fees resolve");
                        let risk = fees.capped(500);
                        let (quoted, risk_quoted) = if exact_in {
                            (
                                pool.quote_exact_in(&env.svm, dir, amount).is_some(),
                                qcpmm::sell_exact_in(amount, x_in.into(), x_out.into(), &risk)
                                    .is_some(),
                            )
                        } else {
                            (
                                pool.quote_exact_out(&env.svm, dir, amount).is_some(),
                                qcpmm::buy_exact_out(amount, x_in.into(), x_out.into(), &risk)
                                    .is_some(),
                            )
                        };
                        let (src, _) = t.accounts(dir);
                        let need = if exact_in { amount } else { x_in / 100 };
                        match dir {
                            Direction::Buy => fork::wrap(&mut env, &src, need),
                            Direction::Sell => fork::set_token_amount(&mut env.svm, &src, need),
                        }
                        let s = if exact_in {
                            Swap::ExactIn(amount, 0)
                        } else {
                            Swap::ExactOut(u64::MAX, amount)
                        };
                        let res = fork::swap(&mut env, &pool, &t, dir, s);
                        assert_eq!(
                            quoted,
                            res.is_ok(),
                            "{case}: the execution quote is faithful"
                        );
                        assert_eq!(res.is_ok(), !broken(dir), "{case}");
                        if broken(dir) {
                            broken_swaps += 1;
                            assert_eq!(
                                custom_code(&res),
                                Some(fork::CPMM_ERR_ZERO_TRADING_TOKENS),
                                "{case}"
                            );
                            assert_eq!(
                                (risk.trade, risk.creator, risk.creator_on_input),
                                (0, 0, false),
                                "{case}: fee-free risk view"
                            );
                        }
                        assert!(risk_quoted, "{case}: the risk view always quotes");
                    }
                }
            }
        }
    }
    println!(
        "M3-SEC-1c: {broken_swaps} ZeroTradingTokens swaps, {not_executable} not-executable configs"
    );
    // The audit's 32 executable-but-reverting swaps: 8 configs (of 24) whose buy cannot split
    // its fee are now not executable; the other breaks are sell-only and keep the route.
    assert_eq!(broken_swaps, 32);
    assert_eq!(not_executable, 8);
}

/// M3-SEC-2. `create_market(RaydiumV4 | RaydiumCpmm)` reads the `VenueGuard` latch: while an
/// unreviewed upgrade of **either** Raydium program is flagged, a Raydium listing is refused
/// (`VenueUpgraded`), as opens are — a listing verifies its candidate with the compiled readers
/// and fixes the pool, the SOL side and `graduated_at` for good. A list-only listing fixes
/// nothing venue-side and is not latched. After `revalidate_venue` the listing goes through.
/// Before the fix the CPMM listing succeeded on the flagged program (59,924 CU).
#[test]
fn m3_sec_2_a_raydium_listing_waits_for_the_venue_review() {
    use shortwin::constants::{GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4};
    use shortwin_litesvm_tests::fork::{FIXTURE_GUARD_SLOTS, RaydiumProgram};
    use shortwin_litesvm_tests::{ix, tokens};
    for (program, venue) in [
        (RaydiumProgram::AmmV4, GUARD_RAYDIUM_V4),
        (RaydiumProgram::Cpmm, GUARD_RAYDIUM_CPMM),
    ] {
        let mut env = Env::ray_fork();
        let dumped = FIXTURE_GUARD_SLOTS[usize::from(venue)];
        fork::set_deploy_slot(&mut env.svm, program, dumped + 1);
        ok(env.send(&[ix::flag_venue_upgraded(venue)], &[]));
        assert_ne!(env.venue_guard().upgraded, 0, "the latch is set");
        // Both Raydium listings are refused, whichever program was upgraded.
        let fart = env.load_ray_pool(FART);
        let useless = env.load_ray_pool(USELESS);
        assert_err(&env.list_ray(&fart, VENUE_HINT_V4), E::VenueUpgraded);
        assert_err(&env.list_ray(&useless, VENUE_HINT_CPMM), E::VenueUpgraded);
        for pool in [&fart, &useless] {
            let (mint, _) = pool.token();
            assert!(env.svm.get_account(&pda::market(&mint).0).is_none());
        }
        // A list-only listing is not latched.
        let payer = env.payer.insecure_clone();
        let authority = Keypair::new();
        let other = tokens::create_mint(
            &mut env.svm,
            &payer,
            &authority.pubkey(),
            6,
            TokenProgram::Spl,
        );
        let creator = env.funded_keypair();
        let auto = ix::create_market(
            &creator.pubkey(),
            &other,
            &TokenProgram::Spl.id(),
            0,
            &pda::canonical_pool(&other),
        );
        ok(env.send(&[auto], &[&creator]));
        assert_eq!(
            env.market(&pda::market(&other).0).venue,
            VenueKind::None as u8
        );
        // The Emergency review revalidates the reviewed slot: the listings go through.
        let ea = env.pause_authority.insecure_clone();
        ok(env.send(
            &[ix::revalidate_venue(&ea.pubkey(), venue, dumped + 1)],
            &[&ea],
        ));
        assert_eq!(env.venue_guard().upgraded, 0);
        let meta = ok(env.list_ray(&useless, VENUE_HINT_CPMM));
        ok(env.list_ray(&fart, VENUE_HINT_V4));
        let (mint, _) = useless.token();
        let m = env.market(&pda::market(&mint).0);
        assert_eq!(m.venue, VenueKind::RaydiumCpmm as u8);
        assert_eq!(m.graduated_at, 1_746_886_997, "O13: the pool's open_time");
        println!(
            "M3-SEC-2 ({program:?} flagged): CPMM listing after revalidation {} CU",
            meta.compute_units_consumed
        );
    }
}

/// M3-SEC-2: the I17 hook's listing half fires on a Raydium (or mock) listing against a flagged
/// latch just before the instruction, not on a list-only listing, not on a clear latch.
#[test]
fn m3_sec_2_the_i17_hook_refuses_a_listing_on_a_flagged_venue() {
    use shortwin::constants::GUARD_RAYDIUM_CPMM;
    use shortwin_litesvm_tests::fork::{FIXTURE_GUARD_SLOTS, RaydiumProgram};
    use shortwin_litesvm_tests::ix;
    let mut env = Env::ray_fork();
    let clear = invariants::snapshot(&env.svm, &[]);
    let dumped = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_CPMM)];
    fork::set_deploy_slot(&mut env.svm, RaydiumProgram::Cpmm, dumped + 1);
    ok(env.send(&[ix::flag_venue_upgraded(GUARD_RAYDIUM_CPMM)], &[]));
    let flagged = invariants::snapshot(&env.svm, &[]);
    let created = |venue: VenueKind| invariants::IxEvents {
        created: vec![MarketCreated {
            market: Pubkey::new_unique(),
            slot: 0,
            ts: 0,
            mint: Pubkey::new_unique(),
            token_program: Pubkey::new_unique(),
            venue: venue as u8,
            creator: Pubkey::new_unique(),
            bonding_curve: Pubkey::new_unique(),
            canonical_pool: Pubkey::new_unique(),
            ray_pool: Pubkey::new_unique(),
            tier: 2,
            creation_fee: 0,
        }],
        ..Default::default()
    };
    for venue in [VenueKind::RaydiumV4, VenueKind::RaydiumCpmm] {
        let v = invariants::check_ix(&env.svm, &flagged, false, &created(venue)).unwrap_err();
        assert_eq!(v.invariant, "I17");
        assert!(invariants::check_ix(&env.svm, &clear, false, &created(venue)).is_ok());
    }
    assert!(invariants::check_ix(&env.svm, &flagged, false, &created(VenueKind::None)).is_ok());
}

/// M3-SEC-3, inverted by M4 (the tripwire of the M3 review; ARCH §9 M4, §2.5 `None`, §2.6 rules
/// 1 and 4). In M3 the first listing fixed a mint's venue. Now: (a) an `Auto` (list-only
/// `None`) listing of a Raydium token — a front-run of the real listing, which then fails because
/// the market exists — is moved onto the token's main pool by the permissionless
/// `propose_pool`; (b) a thin but eligible pool listed first (Fartcoin's reversed pool, its LP
/// burned to λ = 50 %, topped up to just over the locked-depth floor, i.e. `pool_min_real_sol`
/// of real SOL and the floor's rounding: M4-SEC-1 / -3) is replaced within
/// `pool_list_window_s` by the 38,000-SOL main pool, and the market's opens wait out the window
/// that the thin listing started (a replacement inside it does not extend it). This is the
/// program side of the M3 review's condition on the Raydium venue bits (ARCH §9 M4); enabling
/// them on a deployment stays the owner's call once M4's keeper duties are in.
#[test]
fn m3_sec_3_propose_pool_unpins_the_first_listing() {
    use shortwin::events::{POOL_KIND_LISTING, POOL_KIND_LISTING_WINDOW, PoolSwitched};
    use shortwin_litesvm_tests::raydium::{
        fartcoin_curve_bytes, lp_counter, lp_mint, plant_curve, set_mint_supply, supply_for_lambda,
    };
    // (a) list-only front-run.
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(FART);
    let (mint, program) = pool.token();
    plant_curve(&mut env.svm, &mint, fartcoin_curve_bytes());
    let griefer = env.funded_keypair();
    let auto = shortwin_litesvm_tests::ix::create_market(
        &griefer.pubkey(),
        &mint,
        &program.id(),
        0,
        &pda::canonical_pool(&mint),
    );
    ok(env.send(&[auto], &[&griefer]));
    assert!(
        env.list_ray(&pool, VENUE_HINT_V4).is_err(),
        "the market exists: no second listing"
    );
    let meta = ok(env.propose(None, &pool));
    let ev: PoolSwitched = events::one(&meta);
    let AnyPool::V4(p) = &pool else {
        unreachable!()
    };
    assert_eq!((ev.kind, ev.candidate), (POOL_KIND_LISTING, p.pool));
    let m = env.market(&pda::market(&mint).0);
    assert_eq!(m.venue, VenueKind::RaydiumV4 as u8);
    assert_eq!(m.pool_cache[0], p.pool);

    // (b) a thin eligible pool front-runs the deep one, and is replaced inside the window.
    let mut env = Env::ray_fork();
    let deep = env.load_ray_pool(FART);
    let thin = env.load_ray_pool(FART_REV);
    let min = env.config().params.pool_min_real_sol;
    let AnyPool::V4(tp) = &thin else {
        unreachable!()
    };
    let AnyPool::V4(dp) = &deep else {
        unreachable!()
    };
    let supply = supply_for_lambda(lp_counter(&env.svm, &thin), 5_000);
    set_mint_supply(&mut env.svm, &lp_mint(&thin), supply);
    let real = shortwin_litesvm_tests::pool_select::top_up_pc_to_floor(&mut env, &thin, min);
    assert!(real - min < 1_000_000, "{real} lamports of real SOL");
    ok(env.list_ray(&thin, VENUE_HINT_V4));
    let market = pda::market(&mint).0;
    let listed = env.market(&market);
    assert_eq!(listed.pool_cache[0], tp.pool, "the thin pool listed first");
    env.warp(i64::from(env.config().params.pool_list_window_s) - 1);
    let meta = ok(env.propose(Some(&thin), &deep));
    let ev: PoolSwitched = events::one(&meta);
    assert_eq!((ev.kind, ev.candidate), (POOL_KIND_LISTING_WINDOW, dp.pool));
    let m = env.market(&market);
    assert_eq!(m.pool_cache[0], dp.pool, "replaced by the deep pool");
    assert_eq!(
        m.pool_seen_at, listed.pool_seen_at,
        "the window is not extended"
    );
    let (_, y_thin) = thin.io_reserves(&env.svm, Direction::Sell).unwrap();
    let (_, y_deep) = deep.io_reserves(&env.svm, Direction::Sell).unwrap();
    assert!(y_deep > 1_000 * y_thin);
    println!(
        "M3-SEC-3: market moved from {} (SOL reserve {y_thin}) to {} ({y_deep})",
        tp.pool, dp.pool
    );
}
