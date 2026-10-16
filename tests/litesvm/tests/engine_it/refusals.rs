//! Every `open_short` refusal reachable on the mock venue (ARCH §3.4 "Errors"), each in the
//! state that triggers it and nothing else.
//!
//! Not reachable here, by design of the mock or of the build: `VenueMigrating` from a live
//! venue (only a pump curve completes; the static guard is tested by writing the venue),
//! `BoostInProgress` (PumpSwap, M6), `PoolListWindow` / `PoolTooThin` (Raydium pool selection,
//! M3/M4), `CapCurveGamma` (Tier D curve, M5), `VenueRentOnSell` (pump venues), `VenueFlagged`
//! (no M2 path), `TraderStateInvalid` and `DelegateNotCleared` (need a trader with data or a
//! venue that leaves an allowance, neither of which a third party can produce),
//! `VenueResultMismatch` (a venue that delivers less than it quotes). `InsufficientInventory`
//! from `q > T` is implied by the utilisation cap (≤ 95 %) and reachable only as the
//! `total_shares == 0` refusal.

use anchor_lang::prelude::Pubkey;
use shortwin::PauseScope;
use shortwin::constants::GUARD_RAYDIUM_CPMM;
use shortwin::errors::ShortwinError as E;
use shortwin::state::{MARKET_FLAG_MINT_UNSAFE, MARKET_FLAG_PAUSED_COVERAGE, Market, VenueKind};
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::engine::set_route_broken_ix;
use shortwin_litesvm_tests::fixtures;
use shortwin_litesvm_tests::fork::FIXTURE_GUARD_SLOTS;
use shortwin_litesvm_tests::ix;
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};

use crate::common::*;

/// `open_short(q)` with the given bounds by a fresh borrower.
fn try_open(
    env: &mut Env,
    mm: &MockMarket,
    q: u64,
    min_proceeds: u64,
    max_margin: u64,
    max_fee: u64,
) -> TxResult {
    let b = env.borrower();
    let ix = open_short_ix(
        mm,
        &b.key(),
        0,
        q,
        min_proceeds,
        max_margin,
        max_fee,
        ExtrasSel::NONE,
        &[],
    );
    env.send(&[ix], &[&b.kp])
}

fn try_open_q(env: &mut Env, mm: &MockMarket, q: u64) -> TxResult {
    try_open(env, mm, q, 0, u64::MAX, u64::MAX)
}

/// Rewrites the stored `Market` with `f` (state the program itself writes elsewhere).
fn edit_market(env: &mut Env, market: &Pubkey, f: impl FnOnce(&mut Market)) {
    let mut m = env.market(market);
    f(&mut m);
    let extra = lamports(env, market) - env.svm.minimum_balance_for_rent_exemption(Market::SPACE);
    fixtures::set_zc(&mut env.svm, market, &m, extra);
}

#[test]
fn zero_q() {
    let (mut env, mm) = setup();
    assert_err(&try_open_q(&mut env, &mm, 0), E::ZeroAmount);
}

#[test]
fn opens_paused_globally_and_per_market() {
    let (mut env, mm) = setup();
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[ix::set_pause(&ea.pubkey_(), PauseScope::Global, true, None)],
        &[&ea],
    ));
    assert_err(&try_open_q(&mut env, &mm, Q), E::OpensPaused);
    ok(env.send(
        &[ix::set_pause(
            &ea.pubkey_(),
            PauseScope::Global,
            false,
            None,
        )],
        &[&ea],
    ));
    ok(env.send(
        &[ix::set_pause(
            &ea.pubkey_(),
            PauseScope::Market,
            true,
            Some(mm.market()),
        )],
        &[&ea],
    ));
    assert_err(&try_open_q(&mut env, &mm, Q), E::OpensPaused);
    ok(env.send(
        &[ix::set_pause(
            &ea.pubkey_(),
            PauseScope::Market,
            false,
            Some(mm.market()),
        )],
        &[&ea],
    ));
    ok(try_open_q(&mut env, &mm, Q));
}

#[test]
fn coverage_paused() {
    let (mut env, mm) = setup();
    edit_market(&mut env, &mm.market(), |m| {
        m.set_flag(MARKET_FLAG_PAUSED_COVERAGE, true)
    });
    assert_err(&try_open_q(&mut env, &mm, Q), E::CoveragePaused);
}

#[test]
fn route_broken_market_and_venue() {
    let (mut env, mm) = setup();
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey_(),
            PauseScope::Market,
            0,
            true,
            Some(mm.market()),
        )],
        &[&ea],
    ));
    assert_err(&try_open_q(&mut env, &mm, Q), E::RouteBroken);
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey_(),
            PauseScope::Market,
            0,
            false,
            Some(mm.market()),
        )],
        &[&ea],
    ));
    // The venue bit of the mock (5).
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey_(),
            PauseScope::Global,
            5,
            true,
            None,
        )],
        &[&ea],
    ));
    assert_err(&try_open_q(&mut env, &mm, Q), E::RouteBroken);
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey_(),
            PauseScope::Global,
            5,
            false,
            None,
        )],
        &[&ea],
    ));
    ok(try_open_q(&mut env, &mm, Q));
}

#[test]
fn venue_kinds_without_opens() {
    let (mut env, mm) = setup();
    edit_market(&mut env, &mm.market(), |m| m.venue = VenueKind::None as u8);
    assert_err(&try_open_q(&mut env, &mm, Q), E::NoSupportedVenue);
    edit_market(&mut env, &mm.market(), |m| {
        m.venue = VenueKind::PumpMigrating as u8
    });
    assert_err(&try_open_q(&mut env, &mm, Q), E::VenueMigrating);
    // PumpSwap has an adapter since M6: its bit is enabled, and its strict parser refuses the
    // mock's accounts (the ReadSet is keyed by the market's canonical pool and PumpSwap cache).
    edit_market(&mut env, &mm.market(), |m| {
        m.venue = VenueKind::PumpSwap as u8
    });
    assert_err(&try_open_q(&mut env, &mm, Q), E::VenueAccountMismatch);
}

#[test]
fn venue_disabled() {
    let (mut env, mm) = setup();
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(&[ix::disable_venue(&ea.pubkey_(), 5)], &[&ea]));
    assert_err(&try_open_q(&mut env, &mm, Q), E::VenueDisabled);
}

#[test]
fn mint_flagged_unsafe() {
    let (mut env, mm) = setup();
    edit_market(&mut env, &mm.market(), |m| {
        m.set_flag(MARKET_FLAG_MINT_UNSAFE, true)
    });
    assert_err(&try_open_q(&mut env, &mm, Q), E::MintUnsafeFlagged);
}

#[test]
fn no_shares_no_borrow() {
    // ARCH §9 M0b note 4 / M1-SEC-3: tokens without shares are not lendable.
    let (mut env, mm) = setup_with(MarketSpec {
        lend: 0,
        ..MarketSpec::default()
    });
    assert_err(&try_open_q(&mut env, &mm, Q), E::InsufficientInventory);
}

#[test]
fn venue_upgraded_latch() {
    let (mut env, mm) = setup();
    env.set_all_venue_program_data();
    let slot = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_CPMM)] + 1;
    env.set_venue_program_data(GUARD_RAYDIUM_CPMM, slot);
    ok(env.send(&[ix::flag_venue_upgraded(GUARD_RAYDIUM_CPMM)], &[]));
    assert_err(&try_open_q(&mut env, &mm, Q), E::VenueUpgraded);
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[ix::revalidate_venue(
            &ea.pubkey_(),
            GUARD_RAYDIUM_CPMM,
            slot,
        )],
        &[&ea],
    ));
    ok(try_open_q(&mut env, &mm, Q));
}

#[test]
fn no_route_when_the_venue_cannot_execute() {
    let (mut env, mm) = setup();
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = true);
    assert_err(&try_open_q(&mut env, &mm, Q), E::NoRoute);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.disabled = false;
        k.layout_version = 1;
    });
    assert_err(&try_open_q(&mut env, &mm, Q), E::NoRoute);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.layout_version = 0);
    ok(try_open_q(&mut env, &mm, Q));
}

#[test]
fn venue_fee_above_the_caps() {
    let (mut env, mm) = setup();
    // Above venue_fee_cap_bps (150 bps = 15,000 ppm), below the hard cap (500 bps).
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.fee_ppm = 15_001);
    assert_err(&try_open_q(&mut env, &mm, Q), E::VenueFeeTooHigh);
    // Above the hard cap: s3_no_route.
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.fee_ppm = 50_001);
    assert_err(&try_open_q(&mut env, &mm, Q), E::VenueFeeTooHigh);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.fee_ppm = 15_000);
    ok(try_open_q(&mut env, &mm, Q));
}

#[test]
fn warming_up() {
    let (mut env, mm) = setup_with(MarketSpec {
        warmup: 49,
        ..MarketSpec::default()
    });
    assert_err(&try_open_q(&mut env, &mm, Q), E::MarketWarmingUp);
    env.warp(1);
    ok(try_open_q(&mut env, &mm, Q));
}

#[test]
fn spot_far_from_the_ema() {
    let (mut env, mm) = setup();
    env.crank(&mm);
    env.warp(1);
    let actor = env.actor(&mm, 0, 500_000_000_000);
    let spot = env.spot(&mm);
    // +10.5 %: outside `open_ema_dev_bps` (10 %); the EMA has not seen it (one-second lag).
    env.push_price(&mm, &actor, spot * 1_105 / 1_000);
    assert_err(&try_open_q(&mut env, &mm, Q), E::PriceDeviatesFromEma);
    // Back inside the band.
    let actor2 = env.actor(&mm, 100_000_000_000_000, 0);
    env.push_price(&mm, &actor2, spot * 1_095 / 1_000);
    ok(try_open_q(&mut env, &mm, Q));
}

#[test]
fn price_above_the_open_maximum() {
    // P = 10¹⁰ lamports per raw unit (10²⁸ WAD) > P_OPEN_MAX = 10²⁷.
    let (mut env, mm) = setup_with(MarketSpec {
        base: 1_000_000,
        quote: 10_000_000_000_000_000,
        lend: 100_000,
        ..MarketSpec::default()
    });
    assert_err(&try_open_q(&mut env, &mm, 10), E::PriceAboveOpenMax);
}

#[test]
fn position_impact_caps() {
    let (mut env, mm) = setup();
    // The fee-scaled per-position cap: 3.0·φ·X₀ = 0.75 % of X₀ at 25 bps.
    assert_err(
        &try_open_q(&mut env, &mm, 7_600_000_000_000),
        E::CapPositionImpact,
    );
    // The SPEC impact cap (2.47 % of X₀) binds at a 1 % fee (fee cap 3 %).
    let (mut env, mm) = setup_with(MarketSpec {
        fee_ppm: 10_000,
        ..MarketSpec::default()
    });
    assert_err(
        &try_open_q(&mut env, &mm, 25_000_000_000_000),
        E::CapPositionImpact,
    );
}

#[test]
fn market_oi_cap() {
    // OI cap at 25 bps: 7.5·φ·X₁ ≈ 1.875 % of X₁; wallets of 0.3 % each fill it.
    let (mut env, mm) = setup();
    let mut last = None;
    for _ in 0..8 {
        let res = try_open_q(&mut env, &mm, Q);
        if res.is_err() {
            last = Some(res);
            break;
        }
        env.tick(&mm, 1);
    }
    assert_err(&last.expect("the OI cap binds"), E::CapMarketOi);
}

#[test]
fn utilisation_cap() {
    let (mut env, mm) = setup_with(MarketSpec {
        lend: 3_000_000_000_000,
        ..MarketSpec::default()
    });
    // 2.8·10¹² of 3·10¹² is above 90 %.
    assert_err(
        &try_open_q(&mut env, &mm, 2_800_000_000_000),
        E::CapUtilization,
    );
    ok(try_open_q(&mut env, &mm, 2_600_000_000_000));
}

#[test]
fn wallet_cap() {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (res, _) = env.open(&mm, &mut b, Q);
    ok(res);
    env.tick(&mm, 1);
    // 25 % of the OI cap ≈ 0.47 % of X₁: a second 0.3 % from the same wallet exceeds it.
    let (res, _) = env.open(&mm, &mut b, Q);
    assert_err(&res, E::CapWallet);
}

#[test]
fn notional_bounds() {
    let (mut env, mm) = setup();
    // 10¹⁰ raw ≈ 0.01 SOL < min_notional (0.1 SOL).
    assert_err(
        &try_open_q(&mut env, &mm, 10_000_000_000),
        E::BelowMinNotional,
    );
    // market_max_notional (25 SOL): 18 SOL + 10 SOL at a 1 % fee (larger caps).
    let (mut env, mm) = setup_with(MarketSpec {
        fee_ppm: 10_000,
        ..MarketSpec::default()
    });
    ok(try_open_q(&mut env, &mm, 18_000_000_000_000));
    env.tick(&mm, 1);
    assert_err(
        &try_open_q(&mut env, &mm, 10_000_000_000_000),
        E::MarketNotionalCap,
    );
}

#[test]
fn borrower_bounds() {
    let (mut env, mm) = setup();
    let s_exp = quote_sell(&env, &mm, Q);
    // `min_proceeds` above the quote: the venue's own bound refuses the sale (the mock's
    // `SlippageMinOut`, 6018; ARCH §2.4 rule 3 passes `min_out` to the venue).
    assert_eq!(
        custom_code(&try_open(&mut env, &mm, Q, s_exp + 1, u64::MAX, u64::MAX)),
        Some(6_018)
    );
    // A sale that would pay nothing (q = 1: the fee takes the whole input) has no quote.
    assert_err(
        &try_open(&mut env, &mm, 1, 0, u64::MAX, u64::MAX),
        E::ProceedsBelowMin,
    );
    assert_err(
        &try_open(&mut env, &mm, Q, 0, 1, u64::MAX),
        E::MarginAboveMax,
    );
    assert_err(
        &try_open(&mut env, &mm, Q, 0, u64::MAX, 0),
        E::OpenFeeAboveMax,
    );
    // Exactly the quote passes (ε only absorbs drift).
    ok(try_open(&mut env, &mm, Q, s_exp, u64::MAX, u64::MAX));
}

/// `Keypair::pubkey` without importing the trait everywhere.
trait Key {
    fn pubkey_(&self) -> Pubkey;
}

impl Key for solana_keypair::Keypair {
    fn pubkey_(&self) -> Pubkey {
        solana_signer::Signer::pubkey(self)
    }
}
