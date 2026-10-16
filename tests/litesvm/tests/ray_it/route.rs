//! T-RAY-DISABLE and T-RAY-FEE (ARCH §5.3, §5.5, §4.3 "two fee views", §8.2; A43, A63, A64,
//! V2-1, V2-5, R3A-4) on the real programs, driven by the pool admin's **real** instructions
//! (`SetParams`, `update_pool_status`, `update_amm_config`, the admin's signature forged) and
//! by byte builders for what no admin instruction reaches (AMM v4 status 8, a CPMM open time).
//!
//! - **Disable**: the route predicate equals the venue's own (every AMM v4 status and open-time
//!   case, every CPMM status bit and open time): a halted pool is not executable — nothing is
//!   observed, the S3 chain and the `pool_fail` chain start, opens and closes fail with
//!   `NoRoute` — and an expired position on it settles S3 once the chain has run `s3_wait_s`,
//!   at FV on the frozen EMA point priced at the halted pool's own fee; re-enabling clears both
//!   chains and trading resumes.
//! - **Fee**: a hike above `venue_fee_cap_bps` refuses opens and is not S3 evidence; above the
//!   global `venue_fee_hard_cap_bps` it is (`s3_no_route`, `pool_fail`), closes still execute
//!   at the live fee, liquidations price risk at the position's locked cap (a 70 % fee cannot
//!   make a healthy position liquidatable), and an S3 pays FV at that cap, never at the live fee.

use shortwin::events::SolSettle;
use shortwin::state::ShortPosition;
use shortwin_litesvm_tests::engine::{liquidate_ix, sol_settle_ix};
use shortwin_math::fees::{CpmmFees, RayV4Fee};
use shortwin_math::quote::Pricing;
use shortwin_math::rent;
use shortwin_venue_state::raydium_v4;
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

/// Cranks every `step` seconds, calling `sol_settle` after each crank, for at most `n` steps:
/// the settlement event if one executed.
fn run_to_s3(
    env: &mut Env,
    rm: &RayMarket,
    keeper: &Keypair,
    b: &Borrower,
    nonce: u64,
    step: i64,
    n: usize,
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

fn warp_to(env: &mut Env, t: i64) {
    let now = env.now();
    assert!(t >= now);
    env.warp(t - now);
}

/// The AMM v4 route predicate is the venue's own (R-RAY §5.3), statuses set by `SetParams`
/// where the admin can (1–7) and by bytes where it cannot (0, 8): executable exactly for 1, 6,
/// and 7 from its open time.
#[test]
fn amm_v4_disable_matrix() {
    for status in 0u64..=8 {
        let (mut env, rm, _) = setup(FART);
        if (1..=7).contains(&status) {
            ok(fork::raydium_admin_send(
                &mut env,
                fork::v4_set_params_ix(rm.v4(), 0, status),
            ));
        } else {
            fork::set_v4_status(&mut env.svm, rm.v4(), status);
        }
        env.warp(1);
        let t0 = env.market(&rm.market());
        env.crank(&rm);
        let m = env.market(&rm.market());
        let executable = matches!(status, 1 | 6 | 7);
        assert_eq!(
            m.no_route_since == 0,
            executable,
            "status {status}: the S3 chain"
        );
        assert_eq!(
            m.pool_fail_since == 0,
            executable,
            "status {status}: pool_fail"
        );
        if !executable {
            assert_eq!(m.no_route_since, env.now());
            assert_eq!(m.b_lo.get(), 0, "status {status}: nothing observed");
            assert_eq!(
                m.ema_count,
                t0.ema_count + 1,
                "the bucket before the halt integrates"
            );
        } else {
            assert!(m.b_lo.get() > 0, "status {status}: observed");
        }
    }
    // WaitingTrade with a future open time: not executable until then (SetParams param 3).
    let (mut env, rm, q) = setup(FART);
    let open_at = env.now() + 120;
    ok(fork::raydium_admin_send(
        &mut env,
        fork::v4_set_params_ix(rm.v4(), 3, open_at as u64),
    ));
    ok(fork::raydium_admin_send(
        &mut env,
        fork::v4_set_params_ix(rm.v4(), 0, 7),
    ));
    env.warp(1);
    env.crank(&rm);
    assert!(env.market(&rm.market()).no_route_since > 0);
    let mut b = env.borrower();
    assert_err(&env.open(&rm, &mut b, q).0, E::NoRoute);
    warp_to(&mut env, open_at);
    env.crank(&rm);
    assert_eq!(
        env.market(&rm.market()).no_route_since,
        0,
        "open from pool_open_time"
    );
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    assert_eq!(
        rm.v4().info(&env.svm).status,
        raydium_v4::STATUS_SWAP_ONLY,
        "the first swap moves WaitingTrade to SwapOnly"
    );
    env.warp(1);
    close_ok(&mut env, &rm, &b, nonce);
}

/// CPMM: the swap bit (2) of `update_pool_status` halts swaps; the deposit and withdraw bits do
/// not; a future `open_time` (bytes: no instruction can change it) halts until then.
#[test]
fn cpmm_disable_matrix() {
    for status in 0u8..=7 {
        let (mut env, rm, _) = setup(USELESS);
        ok(fork::raydium_admin_send(
            &mut env,
            fork::cpmm_update_pool_status_ix(rm.cpmm(), status),
        ));
        env.warp(1);
        env.crank(&rm);
        let m = env.market(&rm.market());
        let executable = status & 4 == 0;
        assert_eq!(m.no_route_since == 0, executable, "status {status}");
        assert_eq!(m.pool_fail_since == 0, executable, "status {status}");
    }
    let (mut env, rm, q) = setup(USELESS);
    let open_at = env.now() + 90;
    fork::set_cpmm_open_time(&mut env.svm, rm.cpmm(), open_at as u64);
    env.warp(1);
    env.crank(&rm);
    assert!(env.market(&rm.market()).no_route_since > 0);
    warp_to(&mut env, open_at);
    env.crank(&rm);
    assert_eq!(env.market(&rm.market()).no_route_since, 0);
    let mut b = env.borrower();
    open_ok(&mut env, &rm, &mut b, q);
}

/// M3-SEC-1 (T-RAY-DISABLE with zero fees). `update_amm_config(0, 0)` zeroes config 0's trade
/// fee; USELESS (fee model 0, creator fee off) then cannot split a buy's fee and the venue
/// refuses every swap (`ZeroTradingTokens`): not executable — both chains start, nothing is
/// observed, opens and closes fail with `NoRoute`. A zero fee that only the sale cannot split
/// (the creator rate also 0 via `update_amm_config(7, 0)`, the pool's fee model byte-set to 2:
/// the creator fee on the token, the sale's input and the buy's output) keeps the route: the
/// chains clear and the position closes fee-free; opens are refused by the fee-scaled
/// per-position cap, which is `3·φ·X₀ = 0` at `φ = 0` (`CapPositionImpact`, ECON §6.1) before
/// the undefined sale is quoted. With the fees back, trading resumes.
#[test]
fn cpmm_zero_fee_disable_matrix() {
    let (mut env, rm, q) = setup(USELESS);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    let cfg = rm.cpmm().config_key(&env.svm);
    let c0 = rm.cpmm().config(&env.svm);
    set_fee(&mut env, &rm, 0);
    env.warp(1);
    env.crank(&rm);
    let m = env.market(&rm.market());
    assert_eq!(
        (m.no_route_since, m.pool_fail_since),
        (env.now(), env.now())
    );
    assert_eq!(m.b_lo.get(), 0, "nothing observed");
    assert_err(&env.close(&rm, &b, nonce), E::NoRoute);
    let mut b2 = env.borrower();
    assert_err(&env.open(&rm, &mut b2, q).0, E::NoRoute);

    // Sell-only: the creator fee on the token side at rate 0.
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_amm_config_ix(&cfg, 7, 0),
    ));
    fork::set_cpmm_creator_fee(&mut env.svm, rm.cpmm(), true, 2);
    assert!(rm.cpmm().venue_executable(&env.svm, env.now()));
    env.warp(1);
    env.crank(&rm);
    let m = env.market(&rm.market());
    assert_eq!(
        (m.no_route_since, m.pool_fail_since),
        (0, 0),
        "the buy route is back"
    );
    assert!(m.b_lo.get() > 0, "observed");
    assert_err(&env.open(&rm, &mut b2, q).0, E::CapPositionImpact);
    env.warp(1);
    let quote = rm.quote_buy(&env.svm, q).unwrap();
    assert_eq!(quote.cost, quote.net, "fee-free");
    let (close, _) = close_ok(&mut env, &rm, &b, nonce);
    assert_eq!(close.buyback_cost, quote.cost);

    // Both rates and the fee model back: opens resume.
    fork::set_cpmm_creator_fee(&mut env.svm, rm.cpmm(), false, 0);
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_amm_config_ix(&cfg, 7, c0.creator_fee_rate),
    ));
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_amm_config_ix(&cfg, 0, c0.trade_fee_rate),
    ));
    env.warp(1);
    env.crank(&rm);
    let (_, n2, _) = open_ok(&mut env, &rm, &mut b2, q);
    env.warp(1);
    close_ok(&mut env, &rm, &b2, n2);
}

/// ARCH §5.3 "Liveness without a route" on both programs: an admin halt freezes the position
/// (no close, no S3 while healthy and not expired); at its deadline the continuous chain lets
/// `sol_settle` pay S3 at FV on the frozen EMA point, priced at the halted pool's own fee, with
/// the expiry fee (O15). Re-enabling the pool clears both chains.
#[test]
fn a_halted_pool_reaches_s3_at_the_deadline() {
    for fixture in [FART, USELESS] {
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        let pos = env.position(&rm, &b.key(), nonce);
        env.tick(&rm, 3);
        let live_fee = match &rm.pool {
            AnyPool::V4(p) => {
                let i = p.info(&env.svm);
                Pricing::RayV4(RayV4Fee {
                    num: i.swap_fee_numerator,
                    den: i.swap_fee_denominator,
                })
            }
            AnyPool::Cpmm(p) => Pricing::Cpmm {
                buy: p
                    .fees(&env.svm, shortwin_math::fees::Direction::Buy)
                    .unwrap(),
                sell: p
                    .fees(&env.svm, shortwin_math::fees::Direction::Sell)
                    .unwrap(),
            },
        };
        match &rm.pool {
            AnyPool::V4(p) => {
                ok(fork::raydium_admin_send(
                    &mut env,
                    fork::v4_set_params_ix(p, 0, 2),
                ));
            }
            AnyPool::Cpmm(p) => {
                ok(fork::raydium_admin_send(
                    &mut env,
                    fork::cpmm_update_pool_status_ix(p, 4),
                ));
            }
        }
        env.warp(1);
        let keeper = env.funded_keypair();
        assert!(
            settle(&mut env, &rm, &keeper, &b, nonce).is_none(),
            "healthy, not expired"
        );
        let m0 = env.market(&rm.market());
        assert_eq!(m0.no_route_since, env.now());
        assert_eq!(m0.pool_fail_since, env.now());
        assert_err(&env.close(&rm, &b, nonce), E::NoRoute);
        // The chain stays continuous only with observations ≤ 60 s apart: a jump to the
        // deadline restarts it, and S3 waits s3_wait_s from there.
        let slot = env.clock().slot;
        env.set_clock(pos.deadline - 5, slot + 1_500_000);
        env.crank(&rm);
        let since = env.market(&rm.market()).no_route_since;
        assert_eq!(since, env.now());
        let ev = run_to_s3(&mut env, &rm, &keeper, &b, nonce, 30, 30).expect("S3");
        assert_eq!(ev.condition, 3);
        assert!(env.now() >= since + i64::from(pos.s3_wait_s));
        let m = env.market(&rm.market());
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        assert_eq!(
            (p, l, m.ema_count),
            (m0.ema_price.get(), m0.ema_liq.get(), m0.ema_count),
            "the EMA is frozen while the pool is halted"
        );
        assert_eq!(
            (ev.p_lo, ev.p_hi, ev.l_lo, ev.l_hi),
            (p, p, l, l),
            "the EMA point"
        );
        assert_eq!(ev.pprime_lo, pos.expiry_fee_bps, "O15: an expired position");
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let cap = u64::from(pos.venue_fee_hard_cap_bps);
        let fv = fair_value(live_fee.capped(cap), pos.q, p, l, ev.pprime_lo, r_due);
        assert_eq!(ev.fv, fv, "{fixture:?}: FV at the halted pool's own fee");
        assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
        // Re-enabling clears both chains.
        match &rm.pool {
            AnyPool::V4(p) => {
                ok(fork::raydium_admin_send(
                    &mut env,
                    fork::v4_set_params_ix(p, 0, 6),
                ));
            }
            AnyPool::Cpmm(p) => {
                ok(fork::raydium_admin_send(
                    &mut env,
                    fork::cpmm_update_pool_status_ix(p, 0),
                ));
            }
        }
        env.warp(1);
        env.crank(&rm);
        let m = env.market(&rm.market());
        assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0));
        assert!(m.b_lo.get() > 0, "observed again");
    }
}

/// Sets the pool's live fee by the admin's own instruction: AMM v4 swap fee `bps/10⁴` of this
/// pool, CPMM trade fee `bps·100` ppm of its whole config.
fn set_fee(env: &mut Env, rm: &RayMarket, bps: u64) {
    match &rm.pool {
        AnyPool::V4(p) => {
            let mut fees = fork::V4Fees::of(&env.svm, p);
            fees.swap_fee = (bps, 10_000);
            ok(fork::raydium_admin_send(
                env,
                fork::v4_set_fees_ix(p, &fees),
            ));
        }
        AnyPool::Cpmm(p) => {
            let cfg = p.config_key(&env.svm);
            ok(fork::raydium_admin_send(
                env,
                fork::cpmm_update_amm_config_ix(&cfg, 0, bps * 100),
            ));
        }
    }
}

/// A fee between `venue_fee_cap_bps` and the hard cap: opens refused, no S3 or `pool_fail`
/// evidence, closes at the live fee.
#[test]
fn fee_between_the_caps() {
    for fixture in [FART, USELESS] {
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        set_fee(&mut env, &rm, 300);
        env.warp(1);
        env.crank(&rm);
        let m = env.market(&rm.market());
        assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0), "{fixture:?}");
        let mut b2 = env.borrower();
        assert_err(&env.open(&rm, &mut b2, q).0, E::VenueFeeTooHigh);
        env.warp(1);
        let b_exp = rm.quote_buy(&env.svm, q).unwrap().cost;
        let (close, _) = close_ok(&mut env, &rm, &b, nonce);
        assert_eq!(close.buyback_cost, b_exp, "the live 3 % fee");
    }
}

/// A fee far above the hard cap (70 %): opens refused; the S3 and `pool_fail` chains run while
/// the pool is still observed; risk is priced at the position's locked cap (500 bps), so the
/// healthy position is not liquidatable — at the live 70 % it would be; the borrower can still
/// close at the live fee; at the deadline S3 pays FV at the cap. On CPMM also (M10 WP-8a's CPMM
/// part) on the Token-2022 pool and on the creator-fee pool, whose creator fee the cap leaves no
/// room for (`CpmmFees::capped`: FV at a 500 bps trade fee, no creator fee).
#[test]
fn fee_above_the_hard_cap() {
    for fixture in [FART, USELESS, T22, CREATOR_FEE] {
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let (open, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        let pos = env.position(&rm, &b.key(), nonce);
        env.tick(&rm, 3);
        set_fee(&mut env, &rm, 7_000);
        env.warp(1);
        env.crank(&rm);
        let m = env.market(&rm.market());
        assert!(m.no_route_since > 0 && m.pool_fail_since > 0, "{fixture:?}");
        assert!(
            m.b_lo.get() > 0,
            "a fee above the cap does not stop observation"
        );
        let mut b2 = env.borrower();
        assert_err(&env.open(&rm, &mut b2, q).0, E::VenueFeeTooHigh);
        // At the live fee the vault would not cover 1.5 × the buyback; at the cap it does.
        let live = rm.quote_buy(&env.svm, q).unwrap().cost;
        assert!(u128::from(open.v) * 10_000 < u128::from(live) * u128::from(pos.cr_liq_bps));
        env.tick(&rm, 4);
        let keeper = env.funded_keypair();
        let ix = liquidate_ix(&rm, &keeper.pubkey(), &b.key(), nonce);
        assert_err(&env.send(&[ix], &[&keeper]), E::NotLiquidatable);
        // S3 at the deadline, FV at the locked cap.
        let slot = env.clock().slot;
        env.set_clock(pos.deadline - 5, slot + 1_500_000);
        let ev = run_to_s3(&mut env, &rm, &keeper, &b, nonce, 30, 30).expect("S3");
        assert_eq!(ev.condition, 3);
        let m = env.market(&rm.market());
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let cap = u64::from(pos.venue_fee_hard_cap_bps);
        let live = live_pricing(&env, &rm);
        let at_cap = pricing_at_cap(live, cap);
        if let Pricing::Cpmm { buy, .. } = at_cap {
            assert_eq!((buy.trade, buy.creator), (cap * 100, 0), "{fixture:?}");
        }
        assert_eq!(
            ev.fv,
            fair_value(at_cap, pos.q, p, l, ev.pprime_lo, r_due),
            "{fixture:?}: FV at the locked cap"
        );
        assert_ne!(
            ev.fv,
            fair_value(live, pos.q, p, l, ev.pprime_lo, r_due),
            "{fixture:?}: never at the live fee"
        );
    }
}

/// M3-SEC-1 (T-RAY-FEE with a zero fee): a fee of 0 is below every cap. AMM v4 (`SetParams` fees
/// 0/10,000) executes at it: no chain, the pool is observed, closes are fee-free and the risk
/// view equals the execution quote; new opens are refused by the fee-scaled per-position cap
/// (`3·φ·X₀ = 0` at `φ = 0`: `CapPositionImpact`, ECON §6.1), never by a fee-evidence path.
/// CPMM at `t + c_eff = 0` with the fee on the buy's input is no route
/// (`cpmm_zero_fee_disable_matrix`), never fee evidence (`VenueFeeTooHigh`), and its risk view
/// is fee-free, not `+∞`: a healthy position is neither liquidated nor settled, and at the
/// deadline S3 pays FV at that fee-free price (`ray_sec_it` `m3_sec_1*`).
#[test]
fn a_zero_fee_is_below_every_cap() {
    let (mut env, rm, q) = setup(FART);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    set_fee(&mut env, &rm, 0);
    env.warp(1);
    env.crank(&rm);
    let m = env.market(&rm.market());
    assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0));
    assert!(m.b_lo.get() > 0, "observed");
    let mut b2 = env.borrower();
    assert_err(&env.open(&rm, &mut b2, q).0, E::CapPositionImpact);
    env.warp(1);
    let quote = rm.quote_buy(&env.svm, q).unwrap();
    assert_eq!(quote.cost, quote.net, "fee-free");
    let pos = env.position(&rm, &b.key(), nonce);
    let risk = Pricing::RayV4(RayV4Fee {
        num: 0,
        den: 10_000,
    })
    .capped(u64::from(pos.venue_fee_hard_cap_bps));
    let (x, y) = rm.reserves(&env.svm).unwrap();
    assert_eq!(
        risk.buy(q, u128::from(x), u128::from(y)).unwrap().cost,
        quote.cost,
        "risk = execution"
    );
    let (close, _) = close_ok(&mut env, &rm, &b, nonce);
    assert_eq!(close.buyback_cost, quote.cost);
}

/// The borrower's own close under a fee above the hard cap executes at the live fee (closes use
/// the live fee and `venue_executable`, never the cap or the flag; V2-1).
#[test]
fn close_under_a_fee_above_the_hard_cap() {
    for fixture in [FART, USELESS, T22] {
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        set_fee(&mut env, &rm, 800);
        env.warp(1);
        let b_exp = rm.quote_buy(&env.svm, q).unwrap().cost;
        let (close, _) = close_ok(&mut env, &rm, &b, nonce);
        assert_eq!(close.buyback_cost, b_exp, "{fixture:?}: the live 8 % fee");
    }
}

// ---- M10 WP-8a: venue admin with positions open (G-D6, G-G5, G-H3) ----------------------------
//
// Each case is one body run on AMM v4 (Fartcoin, `amm_v4_*`) and on CPMM (USELESS, `cpmm_*`; the
// 2026-10 redeploy), driven by the pool admin's real instructions where they exist.

/// The Emergency multisig sets or clears the route-broken flag of `rm`'s market
/// (`venue_scope = false`) or the market's venue bit of `venue_route_broken` (O6, A40).
fn set_flag(env: &mut Env, rm: &RayMarket, venue_scope: bool, broken: bool) {
    let ea = env.pause_authority.insecure_clone();
    let ix = if venue_scope {
        let bit = match rm.kind() {
            VenueKind::RaydiumV4 => shortwin::constants::VENUE_BIT_RAYDIUM_V4,
            VenueKind::RaydiumCpmm => shortwin::constants::VENUE_BIT_RAYDIUM_CPMM,
            k => panic!("not a Raydium venue: {k:?}"),
        }
        .trailing_zeros() as u8;
        shortwin_litesvm_tests::engine::set_route_broken_ix(
            &ea.pubkey(),
            shortwin::PauseScope::Global,
            bit,
            broken,
            None,
        )
    } else {
        shortwin_litesvm_tests::engine::set_route_broken_ix(
            &ea.pubkey(),
            shortwin::PauseScope::Market,
            0,
            broken,
            Some(rm.market()),
        )
    };
    ok(env.send(&[ix], &[&ea]));
}

/// The pool's live fee as a pricing: AMM v4's swap fee, CPMM's resolved fees of both
/// directions (trade and creator fee of the config and the pool's fee model).
fn live_pricing(env: &Env, rm: &RayMarket) -> Pricing {
    match &rm.pool {
        AnyPool::V4(p) => {
            let i = p.info(&env.svm);
            Pricing::RayV4(RayV4Fee {
                num: i.swap_fee_numerator,
                den: i.swap_fee_denominator,
            })
        }
        AnyPool::Cpmm(p) => Pricing::Cpmm {
            buy: p
                .fees(&env.svm, shortwin_math::fees::Direction::Buy)
                .unwrap(),
            sell: p
                .fees(&env.svm, shortwin_math::fees::Direction::Sell)
                .unwrap(),
        },
    }
}

/// `own` with its trade fee at `cap` bps — the pricing of a fee hiked to exactly the hard cap:
/// AMM v4 `cap/10⁴`; CPMM a trade fee of `cap·100` ppm with the creator fee trimmed to what the
/// cap leaves (`CpmmFees::capped`; 0 on every fixture used here).
fn pricing_at_cap(own: Pricing, cap: u64) -> Pricing {
    match own {
        Pricing::RayV4(_) => Pricing::RayV4(RayV4Fee {
            num: cap,
            den: 10_000,
        }),
        Pricing::Cpmm { buy, sell } => Pricing::Cpmm {
            buy: CpmmFees {
                trade: cap * 100,
                ..buy
            },
            sell: CpmmFees {
                trade: cap * 100,
                ..sell
            },
        }
        .capped(cap),
        _ => unreachable!("not a Raydium pricing"),
    }
}

/// A whale that buys the token on `rm`'s pool with its WSOL.
fn whale(env: &mut Env, rm: &RayMarket) -> fork::Trader {
    let (tok, program) = rm.pool.token();
    let (_, y) = rm.reserves(&env.svm).unwrap();
    fork::trader(env, &tok, program, y.saturating_mul(4), 0)
}

/// The whale's exact-in buy of `sol` lamports (a real swap).
fn whale_buys(env: &mut Env, rm: &RayMarket, w: &fork::Trader, sol: u64) {
    ok(fork::swap(
        env,
        &rm.pool,
        w,
        shortwin_math::fees::Direction::Buy,
        fork::Swap::ExactIn(sol, 0),
    ));
}

/// Jumps the clock to `deadline + after` of `pos` (a new slot range) and cranks 3 s there.
fn jump_to_deadline(env: &mut Env, rm: &RayMarket, pos: &ShortPosition, after: i64) {
    let slot = env.clock().slot;
    env.set_clock(pos.deadline + after, slot + 1_500_000);
    env.tick(rm, 3);
}

/// One keeper second: a crank and a `liquidate` attempt: `Ok(event fields)` or the refusal
/// code.
type LiqAttempt = Result<[u64; 10], Option<u32>>;

fn liquidate_attempt(
    env: &mut Env,
    rm: &RayMarket,
    keeper: &Keypair,
    b: &Borrower,
    nonce: u64,
) -> LiqAttempt {
    env.warp(1);
    env.crank(rm);
    let ix = liquidate_ix(rm, &keeper.pubkey(), &b.key(), nonce);
    let res = env.send(&[ix], &[keeper]);
    match &res {
        Ok(meta) => {
            let e: shortwin::events::Liquidate = events::one(meta);
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
    }
}

/// G-D6 / T-S3-BROKEN (ARCH §5.3, §8.2; V2-1, O15): the Emergency flag — the market's or the
/// market's venue bit — on an executable pool starts the S3 chain at the next observation
/// and nothing else (`pool_fail` stays clear; the pool is still observed). A healthy position
/// never settles before its deadline; flagged 300 s before it, the expired position waits for
/// the chain's `s3_wait_s` and settles S3 at the first keeper call after `since + s3_wait_s`,
/// with the expiry fee at both corners and FV at the EMA point priced at the live fee (the
/// venue executes: `min(φ_live, cap)` = `φ_live`). Clearing the flag clears the chain.
fn an_emergency_flag_settles_expired_positions_after_the_timer(fixture: RayFixture) {
    for venue_scope in [false, true] {
        let (mut env, rm, q) = setup(fixture);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        let pos = env.position(&rm, &b.key(), nonce);
        jump_to_deadline(&mut env, &rm, &pos, -300);
        set_flag(&mut env, &rm, venue_scope, true);
        let mut b2 = env.borrower();
        assert_err(&env.open(&rm, &mut b2, q).0, E::RouteBroken);
        env.warp(1);
        env.crank(&rm);
        let m = env.market(&rm.market());
        let since = m.no_route_since;
        assert_eq!(
            since,
            env.now(),
            "venue scope {venue_scope}: the chain starts"
        );
        assert_eq!(m.pool_fail_since, 0, "pool_fail never reads the flag");
        assert!(m.b_lo.get() > 0, "the pool is still observed");
        let keeper = env.funded_keypair();
        let due = (since + i64::from(pos.s3_wait_s)).max(pos.deadline);
        let ev = loop {
            env.warp(30);
            env.crank(&rm);
            if let Some(ev) = settle(&mut env, &rm, &keeper, &b, nonce) {
                break ev;
            }
            assert!(env.now() < due, "S3 is due at {due}");
        };
        assert!(
            env.now() >= due && env.now() < due + 30,
            "S3 at the first call after the timer"
        );
        assert_eq!(ev.condition, 3);
        assert_eq!(
            (ev.pprime_lo, ev.pprime_hi),
            (pos.expiry_fee_bps, pos.expiry_fee_bps),
            "O15: an expired healthy position pays the expiry fee"
        );
        let m = env.market(&rm.market());
        let (p, l) = (m.ema_price.get(), m.ema_liq.get());
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let live = live_pricing(&env, &rm).capped(u64::from(pos.venue_fee_hard_cap_bps));
        let fv = fair_value(live, pos.q, p, l, ev.pprime_lo, r_due);
        assert_eq!(ev.fv, fv, "{fixture:?}: FV at the EMA point, the live fee");
        let take = pos.v - k_s(&pos);
        assert_eq!(ev.lender_amount, fv.min(take));
        assert_eq!(ev.borrower_amount, take - ev.lender_amount);
        // Cleared: the next observation clears the chain.
        set_flag(&mut env, &rm, venue_scope, false);
        env.warp(1);
        env.crank(&rm);
        assert_eq!(env.market(&rm.market()).no_route_since, 0);
    }
}

#[test]
fn amm_v4_an_emergency_flag_settles_expired_positions_after_the_timer() {
    an_emergency_flag_settles_expired_positions_after_the_timer(FART);
}

/// G-D6 on CPMM: the flag (the market's, then CPMM's venue bit) on USELESS.
#[test]
fn cpmm_an_emergency_flag_settles_expired_positions_after_the_timer() {
    an_emergency_flag_settles_expired_positions_after_the_timer(USELESS);
}

/// G-D6 / T-S3-BROKEN, the liquidation side (V2-1, I10): from the same state, along
/// the same path of real whale buys (1 % of the SOL reserve a second), with and without the
/// Emergency flag the keeper's `liquidate` refuses with the same codes in the same seconds and
/// executes in the same second with identical amounts; `pool_fail` stays clear in both runs and
/// only the flagged run has an S3 chain.
fn liquidations_are_identical_with_and_without_the_flag(fixture: RayFixture) {
    let (mut env, rm, q) = setup(fixture);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    env.tick(&rm, 3);
    let keeper = env.funded_keypair();
    let w = whale(&mut env, &rm);
    let (_, y) = rm.reserves(&env.svm).unwrap();
    let saved = env.svm.clone();
    let mut runs = Vec::new();
    for flag in [false, true] {
        env.svm = saved.clone();
        if flag {
            set_flag(&mut env, &rm, false, true);
        }
        let mut trace = Vec::new();
        for _ in 0..120 {
            whale_buys(&mut env, &rm, &w, y / 100);
            let a = liquidate_attempt(&mut env, &rm, &keeper, &b, nonce);
            let done = a.is_ok();
            trace.push(a);
            if done {
                break;
            }
        }
        let m = env.market(&rm.market());
        runs.push((trace, m.pool_fail_since, m.no_route_since));
    }
    let (plain, flagged) = (&runs[0], &runs[1]);
    assert!(
        plain.0.last().is_some_and(Result::is_ok),
        "{fixture:?}: the path liquidates"
    );
    assert!(plain.0.len() > 1, "healthy first");
    assert_eq!(plain.0, flagged.0, "the same attempts, second by second");
    assert_eq!(
        (plain.1, flagged.1),
        (0, 0),
        "pool_fail never reads the flag"
    );
    assert_eq!(plain.2, 0);
    assert!(flagged.2 > 0, "only the flagged run has an S3 chain");
}

#[test]
fn amm_v4_liquidations_are_identical_with_and_without_the_flag() {
    liquidations_are_identical_with_and_without_the_flag(FART);
}

/// G-D6's liquidation differential on CPMM (USELESS): the same path of real whale buys on the
/// 2026-10 ELF.
#[test]
fn cpmm_liquidations_are_identical_with_and_without_the_flag() {
    liquidations_are_identical_with_and_without_the_flag(USELESS);
}

/// ECON §8.6's race under the Emergency flag (O15; ARCH §8.2 round 3): at the
/// deadline of a flagged, expired, healthy position whose S3 chain has run `s3_wait_s`, the
/// honest keeper's `expire()` and a lender-keeper's `sol_settle(S3)` are both valid; whichever
/// lands first closes the position and the other finds none (Anchor's
/// `AccountNotInitialized`). The S3 take equals the expiry's buyback, fee and rent within
/// 0.1 %, and the borrower's difference is `K_s − K_e` within the same margin.
fn an_emergency_flag_s3_races_an_honest_expiry(fixture: RayFixture) {
    let (mut env, rm, q) = setup(fixture);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    let pos = env.position(&rm, &b.key(), nonce);
    jump_to_deadline(&mut env, &rm, &pos, -700);
    set_flag(&mut env, &rm, false, true);
    let keeper = env.funded_keypair();
    for _ in 0..23 {
        env.warp(30);
        env.crank(&rm);
        assert!(settle(&mut env, &rm, &keeper, &b, nonce).is_none());
    }
    let now = env.now();
    env.warp(pos.deadline - 5 - now);
    env.tick(&rm, 5);
    assert_eq!(env.now(), pos.deadline);
    let honest = env.funded_keypair();
    let saved = env.svm.clone();

    let ix = shortwin_litesvm_tests::engine::expire_ix(&rm, &honest.pubkey(), &b.key(), nonce);
    let exp: shortwin::events::Expire = events::one(&ok(env.send(&[ix], &[&honest])));
    let ix = sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce);
    assert_eq!(
        custom_code(&env.send(&[ix], &[&keeper])),
        Some(ANCHOR_ACCOUNT_NOT_INITIALIZED),
        "the lender-keeper's S3 finds no position"
    );

    env.svm = saved;
    let ev = settle(&mut env, &rm, &keeper, &b, nonce).expect("S3 at the deadline");
    assert_eq!(ev.condition, 3);
    assert_eq!(
        (ev.pprime_lo, ev.pprime_hi),
        (pos.expiry_fee_bps, pos.expiry_fee_bps)
    );
    let ix = shortwin_litesvm_tests::engine::expire_ix(&rm, &honest.pubkey(), &b.key(), nonce);
    assert_eq!(
        custom_code(&env.send(&[ix], &[&honest])),
        Some(ANCHOR_ACCOUNT_NOT_INITIALIZED),
        "the honest expiry finds no position"
    );
    let expiry_value = exp.buyback_cost + exp.expiry_fee + exp.rent;
    let diff = ev.lender_amount.abs_diff(expiry_value);
    assert!(
        diff * 1_000 <= expiry_value,
        "{fixture:?}: S3 take {} vs the expiry's B + fee + rent {expiry_value}",
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

#[test]
fn amm_v4_an_emergency_flag_s3_races_an_honest_expiry() {
    an_emergency_flag_s3_races_an_honest_expiry(FART);
}

/// O15's race under the flag on CPMM (USELESS).
#[test]
fn cpmm_an_emergency_flag_s3_races_an_honest_expiry() {
    an_emergency_flag_s3_races_an_honest_expiry(USELESS);
}

/// Anchor's `AccountNotInitialized`: a closed position account.
const ANCHOR_ACCOUNT_NOT_INITIALIZED: u32 = 3_012;

/// G-G5 (V2-1, I16 risk-quote clause): **the liquidatable set is unchanged above the hard cap**.
/// A real whale buy (sized so that the position is CR-liquidatable at the position's 500 bps cap
/// but not at the pool's own 25 bps), then the keeper every second. With
/// the pool's fee hiked to exactly the cap (500 bps: between the caps, no route evidence) and to
/// 70 % (far above it: both chains run), the risk view is the same `min(φ_live, cap)` = cap:
/// `liquidate` refuses `NotLiquidatable` in exactly the same seconds and stops doing so in the
/// same second. At the cap the liquidation executes; at 70 % the buyback at the live fee no
/// longer fits `V − K_s` (`UseSolSettle`): the keeper's per-second `sol_settle` settles it S3
/// once the hike's chain has run `s3_wait_s`, with the penalty and FV at the position's cap.
/// Control: without a hike (risk at 25 bps) the position is never liquidatable on this path.
fn the_liquidatable_set_is_unchanged_above_the_hard_cap(fixture: RayFixture) {
    let (mut env, rm, q) = setup(fixture);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    let pos = env.position(&rm, &b.key(), nonce);
    env.tick(&rm, 3);
    let keeper = env.funded_keypair();
    let w = whale(&mut env, &rm);
    let cap = u64::from(pos.venue_fee_hard_cap_bps);
    let own = live_pricing(&env, &rm);
    let at_cap = pricing_at_cap(own, cap);
    // The buy that puts B̂ at the CR_liq boundary between the two fee views: B̂_own·L <
    // V_eff·10⁴ < B̂_cap·L, by bisection on clones of the VM.
    let m = env.market(&rm.market());
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let target = u128::from(pos.v - r_due) * 10_000 / u128::from(pos.cr_liq_bps);
    let cost_after = |env: &mut Env, sol: u64, pr: Pricing| -> u128 {
        let saved = env.svm.clone();
        whale_buys(env, &rm, &w, sol);
        let (x, y) = rm.reserves(&env.svm).unwrap();
        env.svm = saved;
        u128::from(pr.buy(q, u128::from(x), u128::from(y)).unwrap().cost)
    };
    let (_, y) = rm.reserves(&env.svm).unwrap();
    let (mut lo, mut hi) = (0u64, y);
    for _ in 0..40 {
        let mid = lo + (hi - lo) / 2;
        // Aim B̂_own at 2.4 % under the boundary (B̂_cap is ≈ 4.8 % above B̂_own).
        if cost_after(&mut env, mid, own) * 1_024 < target * 1_000 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    whale_buys(&mut env, &rm, &w, lo);
    let (x1, y1) = rm.reserves(&env.svm).unwrap();
    let cost = |pr: Pricing| u128::from(pr.buy(q, u128::from(x1), u128::from(y1)).unwrap().cost);
    assert!(
        cost(own) < target && cost(at_cap) > target,
        "the boundary sits between"
    );
    let saved = env.svm.clone();
    let mut runs = Vec::new();
    let mut above_state = None;
    for hike in [Some(cap), Some(7_000), None] {
        env.svm = saved.clone();
        if let Some(bps) = hike {
            set_fee(&mut env, &rm, bps);
        }
        let mut trace = Vec::new();
        for _ in 0..60 {
            let a = liquidate_attempt(&mut env, &rm, &keeper, &b, nonce);
            let stop = a != Err(Some(code(E::NotLiquidatable)));
            trace.push(a);
            if stop {
                break;
            }
        }
        if hike == Some(7_000) {
            above_state = Some(env.svm.clone());
        }
        let m = env.market(&rm.market());
        runs.push((trace, m.no_route_since, m.pool_fail_since));
    }
    let (at, above, none) = (&runs[0], &runs[1], &runs[2]);
    assert!(
        at.0.last().unwrap().is_ok(),
        "liquidated with the fee at the cap"
    );
    assert!(at.0.len() > 1, "the EMA leg follows first");
    assert_eq!(at.0.len(), above.0.len(), "liquidatable in the same second");
    assert_eq!(
        at.0[..at.0.len() - 1],
        above.0[..above.0.len() - 1],
        "the same refusals before it"
    );
    assert_eq!(
        *above.0.last().unwrap(),
        Err(Some(code(E::UseSolSettle))),
        "70 %: the normal buyback is not allowed at the live fee"
    );
    assert_eq!(
        (at.1, at.2),
        (0, 0),
        "a fee at the cap is no route evidence"
    );
    assert!(above.1 > 0 && above.2 > 0, "70 %: both chains run");
    assert_eq!(none.0.len(), 60, "never liquidatable at 25 bps");
    assert!(
        none.0
            .iter()
            .all(|a| *a == Err(Some(code(E::NotLiquidatable))))
    );
    // At 70 % the CR-liquidatable position leaves through `sol_settle`: the keeper's call every
    // second settles it S3 once the chain (started by the hike) has run `s3_wait_s`, with the
    // penalty at both corners and FV at the EMA point priced at the position's cap.
    env.svm = above_state.expect("the 70 % run");
    let mut exit = None;
    for s in 1..=900u32 {
        env.warp(1);
        env.crank(&rm);
        if let Some(ev) = settle(&mut env, &rm, &keeper, &b, nonce) {
            exit = Some((s, ev));
            break;
        }
    }
    let (s, ev) = exit.expect("settled through sol_settle");
    eprintln!(
        "G-G5 at 70 % on {fixture:?}: sol_settle S{} after {s} s, lenders {} of V − K_s {}",
        ev.condition,
        ev.lender_amount,
        pos.v - k_s(&pos)
    );
    assert_eq!(ev.condition, 3);
    let m = env.market(&rm.market());
    assert!(env.now() >= m.no_route_since + i64::from(pos.s3_wait_s));
    assert!(env.now() < m.no_route_since + i64::from(pos.s3_wait_s) + 2);
    assert_eq!(
        (ev.pprime_lo, ev.pprime_hi),
        (pos.penalty_bps, pos.penalty_bps)
    );
    let (p, l) = (m.ema_price.get(), m.ema_liq.get());
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let fv = fair_value(at_cap, pos.q, p, l, ev.pprime_lo, r_due);
    assert_eq!(ev.fv, fv, "{fixture:?}: FV at the locked cap");
    assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
}

#[test]
fn amm_v4_the_liquidatable_set_is_unchanged_above_the_hard_cap() {
    the_liquidatable_set_is_unchanged_above_the_hard_cap(FART);
}

/// G-G5 on CPMM (USELESS, config 0's 25 bps): the hikes by the admin's real `update_amm_config`
/// (the config's trade fee to 500 bps, then to 70 %); FV at the cap is a 500 bps trade fee.
#[test]
fn cpmm_the_liquidatable_set_is_unchanged_above_the_hard_cap() {
    the_liquidatable_set_is_unchanged_above_the_hard_cap(USELESS);
}

/// G-H3 (A63, V1-8; I16 "no observation while not executable", I12 S3 rules): a venue-state
/// layout change with a position open, which the reader refuses (`WrongTag`), made by `change`
/// on the pool account. The crank and `sol_settle` succeed and record nothing; both
/// chains start; opens and closes fail with `NoRoute`. The expired position settles S3 once the
/// chain has run `s3_wait_s`, on the frozen EMA point with FV priced at the position's hard cap
/// — the pool's fee is unreadable (`min(φ_live, cap)` with `φ_live` unknown) — not at the
/// pool's last known 25 bps (that FV differs). The old layout back, both chains clear.
fn a_layout_change_with_a_position_open(fixture: RayFixture, change: impl FnOnce(&mut Vec<u8>)) {
    let (mut env, rm, q) = setup(fixture);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    let pos = env.position(&rm, &b.key(), nonce);
    env.tick(&rm, 3);
    let own = live_pricing(&env, &rm);
    let pool = shortwin_litesvm_tests::pool_select::pool_key(&rm.pool);
    let original = env.svm.get_account(&pool).unwrap();
    fork::patch(&mut env.svm, &pool, change);
    env.warp(1);
    let m0 = env.market(&rm.market());
    env.crank(&rm);
    let m = env.market(&rm.market());
    assert_eq!(
        (m.no_route_since, m.pool_fail_since),
        (env.now(), env.now())
    );
    assert_eq!(m.b_lo.get(), 0, "nothing recorded");
    // The hooks' own view (I13 / I16) sees the venue as not executable too, so a recorded
    // observation or a missing chain here would fail every instruction's hooks.
    let view = shortwin_litesvm_tests::invariants::venue_view::view(&env.svm, &m);
    assert!(!view.expect("a venue view").executable);
    assert_eq!(
        m.ema_count,
        m0.ema_count + 1,
        "the bucket before integrates"
    );
    let keeper = env.funded_keypair();
    assert!(settle(&mut env, &rm, &keeper, &b, nonce).is_none());
    assert_err(&env.close(&rm, &b, nonce), E::NoRoute);
    let mut b2 = env.borrower();
    assert_err(&env.open(&rm, &mut b2, q).0, E::NoRoute);
    let frozen = env.market(&rm.market());
    jump_to_deadline(&mut env, &rm, &pos, -5);
    let since = env.market(&rm.market()).no_route_since;
    let ev = run_to_s3(&mut env, &rm, &keeper, &b, nonce, 30, 30).expect("S3");
    assert_eq!(ev.condition, 3);
    assert!(env.now() >= since + i64::from(pos.s3_wait_s) && env.now() < since + 630);
    let m = env.market(&rm.market());
    let (p, l) = (m.ema_price.get(), m.ema_liq.get());
    assert_eq!(
        (p, l),
        (frozen.ema_price.get(), frozen.ema_liq.get()),
        "the EMA is frozen"
    );
    assert_eq!((ev.p_lo, ev.p_hi, ev.l_lo, ev.l_hi), (p, p, l, l));
    assert_eq!(ev.pprime_lo, pos.expiry_fee_bps);
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let cap = u64::from(pos.venue_fee_hard_cap_bps);
    // The program's fee view with the fee unreadable: the cap itself as the fee (AMM v4
    // `cap/10⁴`; CPMM a trade fee of `cap·100` ppm, no creator, protocol or fund share).
    let at_cap = match &rm.pool {
        AnyPool::V4(_) => Pricing::RayV4(RayV4Fee {
            num: cap,
            den: 10_000,
        }),
        AnyPool::Cpmm(_) => {
            let f = CpmmFees {
                trade: cap * 100,
                creator: 0,
                creator_on_input: true,
                protocol: 0,
                fund: 0,
            };
            Pricing::Cpmm { buy: f, sell: f }
        }
    };
    let fv = fair_value(at_cap, pos.q, p, l, ev.pprime_lo, r_due);
    assert_eq!(ev.fv, fv, "{fixture:?}: FV at the position's hard cap");
    assert_ne!(
        ev.fv,
        fair_value(own.capped(cap), pos.q, p, l, ev.pprime_lo, r_due),
        "not at the pool's last fee"
    );
    assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
    // The old layout back.
    env.svm.set_account(pool, original).unwrap();
    env.warp(1);
    env.crank(&rm);
    let m = env.market(&rm.market());
    assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0));
    assert!(m.b_lo.get() > 0);
}

/// G-H3 on AMM v4: the pool account grows by one byte.
#[test]
fn amm_v4_a_layout_change_with_a_position_open() {
    a_layout_change_with_a_position_open(FART, |d| d.push(0));
}

/// G-H3 on CPMM (USELESS): the `PoolState` discriminator changes (its first byte flipped) — the
/// pool no longer parses for short.win nor for the venue, and its fee is unreadable.
#[test]
fn cpmm_a_layout_change_with_a_position_open() {
    a_layout_change_with_a_position_open(USELESS, |d| d[0] ^= 0xff);
}
