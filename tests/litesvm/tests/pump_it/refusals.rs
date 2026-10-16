//! Every refusal of `open_short` on a curve market (ARCH §3.4 steps 1–9, §4.6; ECON §6.1; M5),
//! each on its own fork with the rest of the open valid, in the program's order:
//!
//! | Step | Refusal | How |
//! |---|---|---|
//! | 0 | `ZeroAmount` | `q = 0` |
//! | 1 | `OpensPaused`, `RouteBroken`, `VenueDisabled` | Emergency: a global pause, the market's route flag, the curve venue bit |
//! | 1 | `VenueMigrating` / `NoSupportedVenue` | a market already `PumpMigrating` / list-only |
//! | 1 | `InsufficientInventory` | nothing lent |
//! | 2 | `VenueMigrating` / `NoRoute` | the open's own venue sync: the curve completed (`vT > 0`) / completed and migrated elsewhere |
//! | 3 | `VenueUpgraded` | the latch (`exits::the_latch_blocks_curve_opens_only`) |
//! | 3 | `NoRoute` | the curve not executable (pump's `FeeConfig` no longer parses; the curve turned mayhem) |
//! | 3 | `VenueFeeTooHigh` | the curve's fee tier above `venue_fee_cap_bps` |
//! | 4 | `MarketWarmingUp` | an open inside `market_warmup_s` of the listing |
//! | 4 | `PriceDeviatesFromEma` | the spot pushed 10 %+ off the EMA by a buy just before |
//! | 6 | `CapPositionImpact`, `CapWallet` | `q` one above the Tier D position cap / at it (the wallet cap binds first below it); a second open past the wallet cap |
//! | 6 | `CapMarketOi` | wallets at their cap until `ΣQ + q` passes the per-market cap on `X₁` |
//! | 6 | `CapUtilization` | `q` above 90 % of the lent tokens |
//! | 6 | `CapCurveGamma` | `ΣQ + q > γ·(rT + q)` with the curve's `rT` lowered |
//! | 8 | pump 6003 | `min_proceeds` above the sale: pump's own `min_sol_output` binds first (short.win's net check is `rent::a_small_sale_on_a_short_curve_is_refused`) |
//! | 9 | `BelowMinNotional`, `MarginAboveMax`, `OpenFeeAboveMax` | a dust `q`; `max_margin`, `max_open_fee` 0 |
//!
//! The venue's key checks (`VenueAccountMismatch` / `BadRemainingAccounts`) are `structure`'s.
//! Every instruction goes through `Env::send`: the invariant hooks run after each one. The only
//! planted state is the venue's (pump's curve, `FeeConfig`).

use shortwin::PauseScope;
use shortwin::constants::VENUE_BIT_PUMP_CURVE;
use shortwin_litesvm_tests::ix;
use shortwin_math::caps;
use solana_keypair::Keypair;

use crate::common::*;

/// `open_short` of `q` by `b` with bounds (`min_proceeds`, `max_margin`, `max_open_fee`).
fn open_with(
    env: &mut Env,
    pm: &PumpMarket,
    b: &Borrower,
    q: u64,
    bounds: (u64, u64, u64),
) -> TxResult {
    let ix = open_short_ix(
        pm,
        &b.key(),
        b.next_nonce,
        q,
        bounds.0,
        bounds.1,
        bounds.2,
        ExtrasSel::NONE,
        &[],
    );
    let kp = b.kp.insecure_clone();
    env.send(
        &shortwin_litesvm_tests::engine::with_cu_limit(pm, ix),
        &[&kp],
    )
}

fn refused(env: &mut Env, pm: &PumpMarket, q: u64, e: E) {
    let mut b = env.borrower();
    let (res, _) = env.open(pm, &mut b, q);
    assert_err(&res, e);
}

fn emergency(env: &mut Env, ix: Instruction) {
    let ea: Keypair = env.pause_authority.insecure_clone();
    ok(env.send(&[ix], &[&ea]));
}

#[test]
fn zero_amount() {
    let (mut env, pm, _) = setup(PumpCoin::T22_115);
    refused(&mut env, &pm, 0, E::ZeroAmount);
}

#[test]
fn emergency_controls() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let ek = env.pause_authority.pubkey();
    emergency(&mut env, ix::set_pause(&ek, PauseScope::Global, true, None));
    refused(&mut env, &pm, q, E::OpensPaused);
    emergency(
        &mut env,
        ix::set_pause(&ek, PauseScope::Global, false, None),
    );
    let rb = shortwin_litesvm_tests::engine::set_route_broken_ix(
        &ek,
        PauseScope::Market,
        0,
        true,
        Some(pm.market()),
    );
    emergency(&mut env, rb);
    refused(&mut env, &pm, q, E::RouteBroken);
    let rb = shortwin_litesvm_tests::engine::set_route_broken_ix(
        &ek,
        PauseScope::Market,
        0,
        false,
        Some(pm.market()),
    );
    emergency(&mut env, rb);
    let bit = VENUE_BIT_PUMP_CURVE.trailing_zeros() as u8;
    emergency(&mut env, ix::disable_venue(&ek, bit));
    refused(&mut env, &pm, q, E::VenueDisabled);
}

/// A stored `PumpMigrating` market refuses at the static guard; so does a list-only one.
#[test]
fn a_migrating_or_list_only_market() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    buy_down_to(&mut env, &pm, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
    );
    env.warp(1);
    refused(&mut env, &pm, q, E::VenueMigrating);
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    env.warp(1);
    venue_changed(
        &crank(&mut env, &pm),
        VenueKind::PumpMigrating,
        VenueKind::None,
    );
    env.warp(1);
    let mut b = env.borrower();
    let ix = open_short_ix(
        &AsNone(&pm),
        &b.key(),
        0,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let kp = b.kp.insecure_clone();
    assert_err(&env.send(&[ix], &[&kp]), E::NoSupportedVenue);
    b.next_nonce = 0;
}

#[test]
fn nothing_lent() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::T22_115, 0), c);
    refused(&mut env, &pm, q, E::InsufficientInventory);
}

/// The open's own venue sync (ARCH §2.5, step 2): a curve completed since the last read moves
/// the market off the curve inside the open, which is then refused — `VenueMigrating` while
/// `vT > 0`, `NoRoute` once it migrated elsewhere. The refusal reverts the transition too; the
/// next crank makes it.
#[test]
fn a_curve_completed_since_the_last_read() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    fork::set_curve_complete(&mut env.svm, &pm.curve.curve, true);
    refused(&mut env, &pm, q, E::VenueMigrating);
    assert_eq!(market(&env, &pm).venue_kind(), Some(VenueKind::PumpCurve));
    fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
    refused(&mut env, &pm, q, E::NoRoute);
}

/// A curve that is not executable (A63): pump's `FeeConfig` no longer parses, or the curve
/// turned mayhem (Q6). The crank still succeeds and records nothing.
#[test]
fn a_curve_that_does_not_execute() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let saved = env
        .svm
        .get_account(&shortwin::constants::PUMP_FEE_CONFIG)
        .unwrap();
    fork::patch(&mut env.svm, &shortwin::constants::PUMP_FEE_CONFIG, |d| {
        d[0] ^= 0xff;
    });
    refused(&mut env, &pm, q, E::NoRoute);
    env.warp(1);
    crank(&mut env, &pm);
    env.svm
        .set_account(shortwin::constants::PUMP_FEE_CONFIG, saved)
        .unwrap();
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let off = shortwin_venue_state::pump::OFF_IS_MAYHEM_MODE;
    fork::patch(&mut env.svm, &pm.curve.curve, |d| d[off] = 1);
    refused(&mut env, &pm, q, E::NoRoute);
}

/// The curve's fee above `venue_fee_cap_bps` (150): every tier's protocol fee raised to 200 bps.
#[test]
fn a_fee_above_the_soft_cap() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let key = shortwin::constants::PUMP_FEE_CONFIG;
    let (flat, mut tiers, stable, exotic) = fork::fee_config_specs(&env.svm, &key);
    for t in &mut tiers {
        t.1[1] = 200;
    }
    fork::set_fee_config(&mut env.svm, &key, flat, &tiers, &stable, exotic);
    let fee = pm.curve.fees(&env.svm).unwrap();
    assert!(fee.oneway_bps() > 150);
    refused(&mut env, &pm, q, E::VenueFeeTooHigh);
}

#[test]
fn inside_the_listing_warm_up() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
    ok(env.list_pump(&c));
    let vt = c.state(&env.svm).virtual_token_reserves;
    let spec = PumpSpec {
        warmup: 10,
        ..PumpSpec::new(PumpCoin::T22_115, vt / 5)
    };
    let pm = env.pump_market_listed(spec, c);
    refused(&mut env, &pm, vt / 200, E::MarketWarmingUp);
}

/// A buy of 5 % of `vT` just before the open lifts the spot ≈ 10.5 % above the EMA.
#[test]
fn a_spot_pushed_off_the_ema() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let u = CurveUser::new(&mut env, &pm.curve);
    let vt = pm.curve.state(&env.svm).virtual_token_reserves;
    ok(u.buy(&mut env, &pm.curve, vt / 20, u64::MAX));
    refused(&mut env, &pm, q, E::PriceDeviatesFromEma);
}

/// The Tier D per-position cap on `X₀ = vT` (the curve's liquidity cannot be withdrawn, ARCH
/// §4.6): `cap + 1` is `CapPositionImpact`; at the cap the wallet cap (25 % of the per-market
/// cap, 1.16 %) binds first (`CapWallet`); the largest `q` within the wallet cap opens, and one
/// more token for the same wallet is `CapWallet`.
#[test]
fn the_position_and_wallet_caps() {
    let (mut env, pm, _) = setup(PumpCoin::T22_115);
    let st = host_state(&env, &pm);
    let p = env.config().params;
    let tier = env.config().tiers[shortwin_math::tier::Tier::D.index()];
    let x0 = st.x;
    let pos_cap =
        caps::position_cap(x0, tier.impact_pos_bps, st.fee_ppm, p.fee_size_cap_x100).unwrap();
    refused(&mut env, &pm, pos_cap + 1, E::CapPositionImpact);
    refused(&mut env, &pm, pos_cap, E::CapWallet);
    let wallet_ok = |q: u64| {
        let oi = caps::oi_cap(x0 + q, tier.impact_mkt_bps, st.fee_ppm, p.fee_oi_cap_x100).unwrap();
        caps::wallet_ok(0, q, oi, p.wallet_cap_bps).unwrap()
    };
    let (mut lo, mut hi) = (1u64, pos_cap);
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if wallet_ok(mid) { lo = mid } else { hi = mid }
    }
    let q_w = lo;
    let mut b = env.borrower();
    let (res, _) = env.open(&pm, &mut b, q_w);
    ok(res);
    env.warp(1);
    let (res, _) = env.open(&pm, &mut b, 1_000_000);
    assert_err(&res, E::CapWallet);
}

/// Wallets at their cap until the per-market cap on `X₁` (Tier D: 10 % impact) is passed: the
/// open that would take `ΣQ + q` above it is `CapMarketOi`, as predicted on the pre-state. The
/// keeper cranks for a minute between opens so the EMA follows the sales.
#[test]
fn the_market_oi_cap() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
    ok(env.list_pump(&c));
    let vt = c.state(&env.svm).virtual_token_reserves;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::T22_115, vt / 4), c);
    let p = env.config().params;
    let tier = env.config().tiers[shortwin_math::tier::Tier::D.index()];
    let mut sum = 0u64;
    for i in 0..12 {
        let st = host_state(&env, &pm);
        let x0 = st.x;
        let oi = |q: u64| {
            caps::oi_cap(x0 + q, tier.impact_mkt_bps, st.fee_ppm, p.fee_oi_cap_x100).unwrap()
        };
        // 1 % of X₀: inside the position and wallet caps.
        let q = x0 / 100;
        let mut b = env.borrower();
        let (res, _) = env.open(&pm, &mut b, q);
        if sum + q > oi(q) {
            assert_err(&res, E::CapMarketOi);
            assert!(i >= 3, "the cap needs several positions");
            return;
        }
        ok(res);
        sum += q;
        for _ in 0..60 {
            env.warp(1);
            crank(&mut env, &pm);
        }
    }
    panic!("the per-market cap never bound");
}

#[test]
fn utilisation_above_the_cap() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    // (ΣB_Q + q)·10⁴ ≤ 9,000·T fails for T just below q/0.9.
    let lend = q * 10 / 9 - 1;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::T22_115, lend), c);
    refused(&mut env, &pm, q, E::CapUtilization);
}

/// Tier D's graduation guard `ΣQ + q ≤ γ·(rT + q)` (γ = 50 %): with the curve's `rT` lowered to
/// `q − 1` (planted on pump's account), `q` is refused and `q − 1` opens.
#[test]
fn the_curve_gamma_cap() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let s = pm.curve.state(&env.svm);
    fork::set_curve_reserves(
        &mut env.svm,
        &pm.curve.curve,
        s.virtual_token_reserves,
        s.virtual_quote_reserves,
        q - 1,
        s.real_quote_reserves,
    );
    refused(&mut env, &pm, q, E::CapCurveGamma);
    let mut b = env.borrower();
    let (res, _) = env.open(&pm, &mut b, q - 1);
    ok(res);
}

/// The borrower's bounds: pump's own `min_sol_output` (= `min_out ≥ min_proceeds`) refuses a
/// sale below `min_proceeds` first (6003 `TooLittleSolReceived`); `max_margin` and
/// `max_open_fee` below the open's are refused after the leg; a dust `q` is below
/// `min_notional`.
#[test]
fn the_borrowers_bounds_and_the_notional_floor() {
    let (mut env, pm, q) = setup(PumpCoin::T22_115);
    let s = pm.curve.quote_sell(&env.svm, q).unwrap().proceeds;
    let b = env.borrower();
    let res = open_with(&mut env, &pm, &b, q, (s + 1, u64::MAX, u64::MAX));
    assert_eq!(
        custom_code(&res),
        Some(fork::PUMP_ERR_TOO_LITTLE_SOL_RECEIVED),
        "pump's slippage check"
    );
    assert_err(
        &open_with(&mut env, &pm, &b, q, (0, 0, u64::MAX)),
        E::MarginAboveMax,
    );
    assert_err(
        &open_with(&mut env, &pm, &b, q, (0, u64::MAX, 0)),
        E::OpenFeeAboveMax,
    );
    assert_err(
        &open_with(&mut env, &pm, &b, q / 50, (0, u64::MAX, u64::MAX)),
        E::BelowMinNotional,
    );
    // Within its bounds the same open goes through.
    ok(open_with(&mut env, &pm, &b, q, (s, u64::MAX, u64::MAX)));
}
