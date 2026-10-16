//! Every `open_short` refusal a Raydium market adds or reaches differently (ARCH §3.4 steps 1,
//! 3, 4, 6, 8; §2.6 rules 1 and 5; §3.1 the latch masks), each next to the passing case, on
//! the real programs. The generic refusals (pauses, coverage, warm-up, caps' arithmetic,
//! notional, margin, fee bounds) are the M2 engine's and are tested on the mock
//! (`tests/engine_it/refusals.rs`).

use shortwin::PauseScope;
use shortwin_litesvm_tests::engine::set_route_broken_ix;
use shortwin_litesvm_tests::ix;
use shortwin_litesvm_tests::raydium::{lp_counter, lp_mint, set_mint_supply, supply_for_lambda};
use shortwin_math::fees::Direction;

use crate::common::*;

fn try_open(env: &mut Env, rm: &RayMarket, q: u64) -> TxResult {
    let mut b = env.borrower();
    env.open(rm, &mut b, q).0
}

fn try_open_min(env: &mut Env, rm: &RayMarket, q: u64, min_proceeds: u64) -> TxResult {
    let b = env.borrower();
    let ix = open_short_ix(
        rm,
        &b.key(),
        0,
        q,
        min_proceeds,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let kp = b.kp.insecure_clone();
    env.send(&[ix], &[&kp])
}

/// ARCH §2.6 rule 1: opens wait `pool_list_window_s` (1 h) after the listing, to the second
/// (the price state warmed up by per-second observations before the window ends, M4V-5).
#[test]
fn listing_window() {
    for fixture in [FART, USELESS] {
        let mut env = Env::ray_fork();
        let pool = env.load_ray_pool(fixture);
        ok(env.list_ray(&pool, fixture.hint()));
        let q = q_of(&env, &pool);
        let rm = env.ray_market_listed(
            RaySpec {
                warmup: 60,
                ..RaySpec::new(fixture, q * 40)
            },
            pool,
        );
        let seen = env.market(&rm.market()).pool_seen_at;
        let window = i64::from(env.config().params.pool_list_window_s);
        assert_err(&try_open(&mut env, &rm, q), E::PoolListWindow);
        let slot = env.clock().slot;
        env.set_clock(
            seen + window - 1 - raydium::PRICE_WARMUP_CRANKS,
            slot + 10_000,
        );
        env.warm_up_price(&rm);
        env.crank(&rm);
        assert_eq!(env.now(), seen + window - 1);
        assert_err(&try_open(&mut env, &rm, q), E::PoolListWindow);
        let slot = env.clock().slot;
        env.set_clock(seen + window, slot + 3);
        ok(try_open(&mut env, &rm, q));
    }
}

/// ARCH §2.6 rule 5, `R_eff ≥ pool_min_real_sol`: a pool drained below 25 SOL of real reserve
/// is too thin; so is a deep pool whose locked depth `D` is small — opens are sized on locked
/// depth (`R_eff ≤ locked SOL / λ_min`), never on unlocked liquidity (V2-8).
#[test]
fn too_thin() {
    // Real SOL below the minimum (the WSOL vault drained, pool still executable).
    let (mut env, rm, q) = setup(FART);
    let min = env.config().params.pool_min_real_sol;
    let (_, sol) = rm.vaults();
    fork::set_token_amount(&mut env.svm, &sol, min - 1);
    env.warp(1);
    assert_err(&try_open(&mut env, &rm, q / 1_000), E::PoolTooThin);

    // Locked depth: λ of 1 bps on Fartcoin's 38,000 SOL leaves ≈ 7.6 SOL of R_eff.
    for fixture in [FART, USELESS] {
        let (mut env, rm, q) = setup(fixture);
        let supply = supply_for_lambda(lp_counter(&env.svm, &rm.pool), 1);
        set_mint_supply(&mut env.svm, &lp_mint(&rm.pool), supply);
        env.warp(1);
        assert_err(&try_open(&mut env, &rm, q), E::PoolTooThin);
        // λ below λ_min but D still ample: the open is allowed (λ_now is not an open gate).
        let supply = supply_for_lambda(lp_counter(&env.svm, &rm.pool), 4_000);
        set_mint_supply(&mut env.svm, &lp_mint(&rm.pool), supply);
        env.warp(1);
        ok(try_open(&mut env, &rm, q / 4));
    }
}

/// ARCH §4.6, V2-8: the conservative reserve `X₀` of the caps is bounded by `D` over `λ_min`,
/// so the fee-scaled size cap `3·φ·X₀` halves when only a quarter of the LP is locked. On
/// `FQd8z3…` (φ = 30 bps) `q = X/200` fits at the pool's own λ (99.8 %) and not at λ = 25 %.
#[test]
fn caps_are_sized_on_locked_depth() {
    let (mut env, rm, _) = setup(CREATOR_FEE);
    let (x, _) = rm.reserves(&env.svm).unwrap();
    let q = x / 200;
    let counter = lp_counter(&env.svm, &rm.pool);
    let natural = shortwin_litesvm_tests::raydium::mint_supply(&env.svm, &lp_mint(&rm.pool));
    set_mint_supply(
        &mut env.svm,
        &lp_mint(&rm.pool),
        supply_for_lambda(counter, 2_500),
    );
    env.warp(1);
    assert_err(&try_open(&mut env, &rm, q), E::CapPositionImpact);
    set_mint_supply(&mut env.svm, &lp_mint(&rm.pool), natural);
    env.warp(1);
    ok(try_open(&mut env, &rm, q));
}

/// The one-way fee against `venue_fee_cap_bps` (150) and, through `s3_no_route`, the global
/// `venue_fee_hard_cap_bps` (500), set by the real admin instructions: AMM v4 `SetParams` (per
/// pool), CPMM `update_amm_config` (the whole config).
#[test]
fn fee_caps() {
    // AMM v4.
    for (num, want) in [
        (150u64, None),
        (151, Some(E::VenueFeeTooHigh)),
        (600, Some(E::VenueFeeTooHigh)),
    ] {
        let (mut env, rm, q) = setup(FART);
        let mut fees = fork::V4Fees::of(&env.svm, rm.v4());
        fees.swap_fee = (num, 10_000);
        ok(fork::raydium_admin_send(
            &mut env,
            fork::v4_set_fees_ix(rm.v4(), &fees),
        ));
        env.warp(1);
        let res = try_open(&mut env, &rm, q / 4);
        match want {
            None => {
                ok(res);
            }
            Some(e) => assert_err(&res, e),
        }
    }
    // CPMM: the trade fee of config 0 (USELESS's) raised for every pool on it.
    for (rate, want) in [
        (15_000u64, None),
        (15_001, Some(E::VenueFeeTooHigh)),
        (60_000, Some(E::VenueFeeTooHigh)),
    ] {
        let (mut env, rm, q) = setup(USELESS);
        let cfg = rm.cpmm().config_key(&env.svm);
        ok(fork::raydium_admin_send(
            &mut env,
            fork::cpmm_update_amm_config_ix(&cfg, 0, rate),
        ));
        env.warp(1);
        let res = try_open(&mut env, &rm, q / 4);
        match want {
            None => {
                ok(res);
            }
            Some(e) => assert_err(&res, e),
        }
    }
    // CPMM: the creator fee counts when the pool enabled it (FQd8z3…, t = 0.25 %).
    let (mut env, rm, q) = setup(CREATOR_FEE);
    let cfg = rm.cpmm().config_key(&env.svm);
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_amm_config_ix(&cfg, 7, 12_501),
    ));
    env.warp(1);
    assert_err(&try_open(&mut env, &rm, q / 4), E::VenueFeeTooHigh);
}

/// A halted pool is not executable: `NoRoute` (the real admin instructions).
#[test]
fn halted_pool() {
    let (mut env, rm, q) = setup(FART);
    ok(fork::raydium_admin_send(
        &mut env,
        fork::v4_set_params_ix(rm.v4(), 0, 2),
    ));
    env.warp(1);
    assert_err(&try_open(&mut env, &rm, q), E::NoRoute);
    let (mut env, rm, q) = setup(USELESS);
    ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_pool_status_ix(rm.cpmm(), 4),
    ));
    env.warp(1);
    assert_err(&try_open(&mut env, &rm, q), E::NoRoute);
}

/// The route-broken flag (market scope and each Raydium venue bit) and `disable_venue` refuse
/// opens on their own venue only.
#[test]
fn flags_and_disabled_venues() {
    let mut env = Env::ray_fork();
    let (fart, qf) = add_market(&mut env, FART);
    let (useless, qu) = add_market(&mut env, USELESS);
    let ea = env.pause_authority.insecure_clone();
    let flag = |env: &mut Env, bit: u8, on: bool| {
        ok(env.send(
            &[set_route_broken_ix(
                &ea.pubkey(),
                PauseScope::Global,
                bit,
                on,
                None,
            )],
            &[&ea],
        ));
    };
    // AMM v4 is venue bit 2, CPMM bit 3.
    flag(&mut env, 2, true);
    assert_err(&try_open(&mut env, &fart, qf), E::RouteBroken);
    ok(try_open(&mut env, &useless, qu));
    flag(&mut env, 2, false);
    flag(&mut env, 3, true);
    assert_err(&try_open(&mut env, &useless, qu), E::RouteBroken);
    ok(try_open(&mut env, &fart, qf));
    flag(&mut env, 3, false);
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Market,
            0,
            true,
            Some(fart.market()),
        )],
        &[&ea],
    ));
    assert_err(&try_open(&mut env, &fart, qf), E::RouteBroken);
    ok(try_open(&mut env, &useless, qu));
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Market,
            0,
            false,
            Some(fart.market()),
        )],
        &[&ea],
    ));
    ok(env.send(&[ix::disable_venue(&ea.pubkey(), 3)], &[&ea]));
    assert_err(&try_open(&mut env, &useless, qu), E::VenueDisabled);
    ok(try_open(&mut env, &fart, qf));
    ok(env.send(&[ix::disable_venue(&ea.pubkey(), 2)], &[&ea]));
    assert_err(&try_open(&mut env, &fart, qf), E::VenueDisabled);
}

/// ARCH §3.4 step 4: the pre-trade spot must sit within `open_ema_dev_bps` (10 %) of the
/// post-roll EMA; a whale buy that lifts the pool's price 15 % in the same second is refused.
#[test]
fn spot_far_from_the_ema() {
    for fixture in [FART, USELESS] {
        let (mut env, rm, q) = setup(fixture);
        let (tok, program) = rm.pool.token();
        let whale = fork::trader(&mut env, &tok, program, 10_000_000_000_000, 0);
        let (_, y) = rm.reserves(&env.svm).unwrap();
        // Buying 7 % of the SOL reserve's worth lifts the price ≈ 14.5 %.
        let amount = y / 100 * 7;
        ok(fork::swap(
            &mut env,
            &rm.pool,
            &whale,
            Direction::Buy,
            fork::Swap::ExactIn(amount, 0),
        ));
        assert_err(&try_open(&mut env, &rm, q), E::PriceDeviatesFromEma);
    }
}

/// The venue's own slippage argument is `min_out = max(min_proceeds, ⌊S_exp·(1 − ε)⌋)`: the
/// exact quote passes; one lamport more is the venue's own `ExceededSlippage` (AMM v4 0x1e,
/// CPMM 6005).
#[test]
fn min_proceeds_is_the_venues_bound() {
    for (fixture, venue_code) in [
        (FART, fork::V4_ERR_EXCEEDED_SLIPPAGE),
        (USELESS, fork::CPMM_ERR_EXCEEDED_SLIPPAGE),
    ] {
        let (mut env, rm, q) = setup(fixture);
        let s_exp = rm.quote_sell(&env.svm, q).unwrap().proceeds;
        let res = try_open_min(&mut env, &rm, q, s_exp + 1);
        assert_eq!(custom_code(&res), Some(venue_code), "{fixture:?}");
        ok(try_open_min(&mut env, &rm, q, s_exp));
    }
}

/// The notional bounds price on the venue (ARCH §3.4 step 9: `N_open = B̂` on the post-open
/// reserves): below `min_notional` (0.1 SOL) and above `market_max_notional` (25 SOL, beta).
#[test]
fn notional_bounds() {
    for fixture in [FART, USELESS] {
        let (mut env, rm, _) = setup(fixture);
        let (x, y) = rm.reserves(&env.svm).unwrap();
        let q_for = |sol: u64| (u128::from(x) * u128::from(sol) / u128::from(y)) as u64;
        assert_err(
            &try_open(&mut env, &rm, q_for(50_000_000)),
            E::BelowMinNotional,
        );
        assert_err(
            &try_open(&mut env, &rm, q_for(30_000_000_000)),
            E::MarketNotionalCap,
        );
        ok(try_open(&mut env, &rm, q_for(20_000_000_000)));
    }
}
