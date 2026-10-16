//! The market prologue (ARCH §3.0) on the mock: params and kink versions (forced EMA adoption),
//! accrual with the vest release, the observation roll and record (the one-second lag, record
//! only while executable), `CoverageGap`, the S3 and `pool_fail` chains (the route-broken flag
//! feeds only the S3 chain, T-S3-BROKEN's structural part), and `crank_price` never reverting
//! on a venue-state account that no longer parses (A63).

use shortwin::PauseScope;
use shortwin::events::{CoverageGap, PriceCranked, RouteChainChanged};
use shortwin::state::{ConfigChange, EmaParamsArgs};
use shortwin_litesvm_tests::engine::set_route_broken_ix;
use shortwin_litesvm_tests::ix;
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};
use shortwin_math::num::WAD;
use solana_signer::Signer;

use crate::common::*;

/// Proposes `change`, waits the timelock and applies it (both must succeed).
fn govern(env: &mut Env, change: ConfigChange) {
    let gov = env.governance.insecure_clone();
    ok(env.send(&[ix::propose_config(&gov.pubkey(), change)], &[&gov]));
    env.warp(i64::from(env.config().timelock_s));
    let apply = env.apply_config_ix();
    ok(env.send(&[apply], &[]));
}

/// ECON §5.2: an instruction never reads its own observation. A price push and a crank in the
/// same transaction put the pushed spot in the current bucket, not in `P_ema`; the next second's
/// roll integrates it.
#[test]
fn one_second_lag() {
    let (mut env, mm) = setup();
    env.crank(&mm);
    env.warp(1);
    let actor = env.actor(&mm, 0, 200_000_000_000);
    let saved = env.svm.clone();
    // Reference: the same crank without the push.
    env.crank(&mm);
    let honest = env.market(&mm.market());
    env.svm = saved;
    let spot = env.spot(&mm);
    let (_, _, push) = env.push_price_ix(&mm, &actor, spot * 105 / 100).unwrap();
    ok(env.send(&[push, crank_price_ix(&mm)], &[&actor.kp]));
    let m = env.market(&mm.market());
    assert_eq!(
        m.ema_price.get(),
        honest.ema_price.get(),
        "the push is not in P_ema"
    );
    assert_eq!(m.ema_count, honest.ema_count);
    assert!(
        m.b_hi.get() > honest.b_hi.get(),
        "the pushed spot is in the open bucket"
    );
    // Next second: the bucket (hi = the push) rolls in; P_ema moves toward it.
    env.warp(1);
    env.crank(&mm);
    assert!(env.market(&mm.market()).ema_price.get() > honest.ema_price.get());
}

/// A69: an integration that follows more than `COVERAGE_GAP_S` (40 s since OWNER-M7-12; was
/// 30) without one, on a market with open positions, emits the `CoverageGap` log event; without
/// positions it does not.
#[test]
fn coverage_gap_is_reported_with_open_positions_only() {
    let (mut env, mm) = setup();
    env.crank(&mm);
    // Gaps well over the threshold, without positions.
    env.warp(50);
    let meta = ok(env.send(&[crank_price_ix(&mm)], &[]));
    env.warp(50);
    let meta2 = ok(env.send(&[crank_price_ix(&mm)], &[]));
    assert!(events::logged::<CoverageGap>(&meta).is_empty());
    assert!(events::logged::<CoverageGap>(&meta2).is_empty());
    let mut b = env.borrower();
    open_ok(&mut env, &mm, &mut b, Q);
    let gap = shortwin_math::consts::COVERAGE_GAP_S + 1;
    env.warp(gap);
    let meta = ok(env.send(&[crank_price_ix(&mm)], &[]));
    let gaps: Vec<CoverageGap> = events::logged(&meta);
    assert_eq!(gaps.len(), 1);
    assert_eq!((gaps[0].gap_s, gaps[0].open_positions), (gap, 1));
    // Per-second cranks: no gap.
    env.warp(1);
    let meta = ok(env.send(&[crank_price_ix(&mm)], &[]));
    assert!(events::logged::<CoverageGap>(&meta).is_empty());
}

/// A17, A59, R3A-9: with a position open, new EMA parameters are adopted only at `force_at`
/// (the forced adoption), and the kink never with positions open — only once the market has
/// none again.
#[test]
fn forced_ema_adoption_and_the_kink_wait_for_zero_positions() {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    let m0 = env.market(&mm.market());
    govern(
        &mut env,
        ConfigChange::EmaParams {
            ema: EmaParamsArgs {
                ema_dtmax_s: 1,
                ema_clamp_bps: 5_000,
                liq_slow_tau_s: 7_200,
                liq_slow_dtmax_s: 30,
            },
        },
    );
    govern(
        &mut env,
        ConfigChange::Kink {
            util_kink_bps: 7_000,
        },
    );
    let config = env.config();
    env.crank(&mm);
    let m = env.market(&mm.market());
    assert_eq!(
        (m.ema_params_version, m.ema_dtmax_s),
        (m0.ema_params_version, m0.ema_dtmax_s),
        "before force_at"
    );
    assert_eq!(m.kink_version, m0.kink_version);
    // Past force_at: the EMA fields are forced, the kink is not.
    let wait = config.ema_params.force_at - env.now();
    env.warp(wait);
    env.crank(&mm);
    let m = env.market(&mm.market());
    assert_eq!(m.ema_params_version, config.ema_params.version);
    assert_eq!(
        (m.ema_dtmax_s, m.ema_clamp_bps, m.liq_slow_tau_s),
        (1, 5_000, 7_200)
    );
    assert_eq!(
        (m.kink_version, m.util_kink_bps),
        (m0.kink_version, m0.util_kink_bps)
    );
    // Drain to 0 positions: the next prologue adopts the kink.
    close_ok(&mut env, &mm, &b, nonce);
    env.warp(1);
    env.crank(&mm);
    let m = env.market(&mm.market());
    assert_eq!(
        (m.kink_version, m.util_kink_bps),
        (config.kink.version, 7_000)
    );
}

/// A64, V2-5, A63: a disabled venue or a changed pool layout is "not executable": the crank
/// succeeds, records nothing, and both chains start; a venue that trades again clears them.
#[test]
fn record_only_while_executable_and_the_chains() {
    let (mut env, mm) = setup();
    env.crank(&mm);
    env.warp(1);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.disabled = true);
    let t0 = env.now();
    let meta = ok(env.send(&[crank_price_ix(&mm)], &[]));
    let m = env.market(&mm.market());
    assert_eq!(m.b_lo.get(), 0, "nothing recorded");
    assert_eq!((m.no_route_since, m.last_no_route_obs), (t0, t0));
    assert_eq!((m.pool_fail_since, m.last_pool_fail_obs), (t0, t0));
    let changed: RouteChainChanged = events::one(&meta);
    assert_eq!(changed.no_route_since, t0);
    let cranked: PriceCranked = events::one(&meta);
    assert_eq!(cranked.spot_wad, 0);
    // A continuing chain within S3_OBS_GAP_S (60 s) keeps its start; no RouteChainChanged.
    env.warp(60);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| {
        k.disabled = false;
        k.layout_version = 1;
    });
    let meta = ok(env.send(&[crank_price_ix(&mm)], &[]));
    assert!(events::of::<RouteChainChanged>(&meta).is_empty());
    let m = env.market(&mm.market());
    assert_eq!((m.no_route_since, m.last_no_route_obs), (t0, t0 + 60));
    assert_eq!(m.b_lo.get(), 0);
    // Observations further apart than 60 s restart it (C0-16).
    env.warp(61);
    ok(env.send(&[crank_price_ix(&mm)], &[]));
    let m = env.market(&mm.market());
    assert_eq!(m.no_route_since, t0 + 121);
    // The venue trades again: the chains clear and the crank records.
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.layout_version = 0);
    env.warp(1);
    let meta = ok(env.send(&[crank_price_ix(&mm)], &[]));
    let m = env.market(&mm.market());
    assert_eq!((m.no_route_since, m.pool_fail_since), (0, 0));
    assert!(m.b_lo.get() > 0);
    let changed: RouteChainChanged = events::one(&meta);
    assert_eq!(changed.no_route_since, 0);
}

/// V2-1, V1-7 (T-S3-BROKEN, structural part): the route-broken flag starts the S3 chain but not
/// `pool_fail`, observation continues (the venue executes), and a close under the flag equals
/// the close without it.
#[test]
fn route_broken_feeds_s3_only() {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (_o, nonce, _) = open_ok(&mut env, &mm, &mut b, Q);
    env.tick(&mm, 2);
    let saved = env.svm.clone();
    let (honest, _) = close_ok(&mut env, &mm, &b, nonce);
    env.svm = saved;
    let ea = env.pause_authority.insecure_clone();
    ok(env.send(
        &[set_route_broken_ix(
            &ea.pubkey(),
            PauseScope::Market,
            0,
            true,
            Some(mm.market()),
        )],
        &[&ea],
    ));
    // Same second as the saved state: the close sees the same prices.
    let (flagged, _) = close_ok(&mut env, &mm, &b, nonce);
    assert_eq!(flagged.buyback_cost, honest.buyback_cost);
    assert_eq!(flagged.rent, honest.rent);
    assert_eq!(flagged.borrower_payout, honest.borrower_payout);
    let m = env.market(&mm.market());
    assert_eq!(m.no_route_since, env.now(), "the flag is S3 evidence");
    assert_eq!(m.pool_fail_since, 0, "pool_fail never reads the flag");
    assert!(m.b_lo.get() > 0, "the venue is still observed");
}

/// A fee above the global hard cap: `s3_no_route` and `pool_fail` hold, but the pool still
/// trades, so it is still observed (ARCH §3.0 step 7).
#[test]
fn a_fee_above_the_hard_cap_is_still_observed() {
    let (mut env, mm) = setup();
    env.crank(&mm);
    env.warp(1);
    update_knobs(&mut env, &mm.pool, |k: &mut Knobs| k.fee_ppm = 50_001);
    env.crank(&mm);
    let m = env.market(&mm.market());
    assert_eq!(
        (m.no_route_since, m.pool_fail_since),
        (env.now(), env.now())
    );
    assert!(m.b_lo.get() > 0);
}

/// ARCH §3.0 step 4 through `crank_price`: rent basis indices advance at `P̃ = min(P_ema,
/// P_ACC_MAX)` before the roll, the lender credit reaches `receivable`, and a running vest is
/// released linearly.
#[test]
fn accrual_and_the_vest_release() {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    // A donation to the trader becomes a 6-hour vest at the open.
    let griefer = env.funded_keypair();
    let mut data = vec![2, 0, 0, 0];
    data.extend_from_slice(&21_600_000u64.to_le_bytes());
    let donate = anchor_lang::solana_program::instruction::Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![
            anchor_lang::solana_program::instruction::AccountMeta::new(griefer.pubkey(), true),
            anchor_lang::solana_program::instruction::AccountMeta::new(mm.keys.trader, false),
        ],
        data,
    };
    ok(env.send(&[donate], &[&griefer]));
    open_ok(&mut env, &mm, &mut b, Q);
    let m0 = env.market(&mm.market());
    assert_eq!(m0.vest_bal, 21_600_000);
    env.warp(3 * 3_600);
    env.crank(&mm);
    let m1 = env.market(&mm.market());
    assert_eq!(
        m1.vest_bal, 10_800_000,
        "half of the vest after half of lump_vest_s"
    );
    assert!(m1.acc_r.get() > m0.acc_r.get());
    let dt = u128::from((m1.last_accrual_ts - m0.last_accrual_ts) as u64);
    assert_eq!(
        m1.rent_basis[0].get() - m0.rent_basis[0].get(),
        m0.ema_price.get() * dt
    );
    assert!(m1.receivable > m0.receivable, "rent credited continuously");
    assert!(m0.ema_price.get() < WAD);
}

/// AIR-03 through `crank_price`: a market cranked every second credits its lenders the rent of
/// a small position. As built each accrual floored its credit and kept no remainder, so a
/// position crediting a fraction of a lamport a second lost that fraction at every crank, and
/// the exit's true-up (`credited_for`, ceil) left the difference in `lender_sol` unclaimable.
/// With the remainder carried (`Market.lender_credit_rem`) ten minutes of per-second cranks
/// credit the position's exact share within one lamport, and its close still takes
/// `receivable` back to 0 (ECON I-P7).
#[test]
fn per_second_cranks_credit_a_small_positions_rent() {
    let (mut env, mm) = setup();
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &mm, &mut b, Q / 20);
    let m0 = env.market(&mm.market());
    for _ in 0..600 {
        env.warp(1);
        env.crank(&mm);
    }
    let m1 = env.market(&mm.market());
    let pos = env.position(&mm, &b.key(), nonce);
    let credited = shortwin_math::rent::credited_for(&pos.pos_rent(), &m1.basis()).unwrap();
    let credits = m1.receivable - m0.receivable;
    println!(
        "AIR-03: 600 per-second cranks credited {credits} of the position's {credited} lamports"
    );
    assert!(credited > 600, "a fraction of a lamport or more a second");
    assert!(
        credits <= credited && credited - credits <= 1,
        "per-second cranks credited {credits} of the position's {credited} lamports"
    );
    close_ok(&mut env, &mm, &b, nonce);
    assert_eq!(env.market(&mm.market()).receivable, 0, "ECON I-P7");
}

/// `crank_price` on a list-only market: the detection set only (key-checked), no record, the S3
/// chain runs (a `None` market has no route).
#[test]
fn crank_on_a_list_only_market() {
    let mut env = Env::test_venue();
    let (keys, _auth) = env.install_market(TokenProgram::Spl, 6);
    let base = shortwin_litesvm_tests::ix::with_remaining(
        anchor_lang::solana_program::instruction::Instruction {
            program_id: shortwin::ID,
            accounts: anchor_lang::ToAccountMetas::to_account_metas(
                &shortwin::accounts::CrankPrice {
                    config: pda::config(),
                    market: keys.market,
                    mint: keys.mint,
                    oracle_cache: pda::oracle_cache(),
                    event_authority: pda::event_authority(),
                    program: shortwin::ID,
                },
                None,
            ),
            data: anchor_lang::InstructionData::data(&shortwin::instruction::CrankPrice {}),
        },
        &[],
    );
    let detection = [
        anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
            pda::bonding_curve(&keys.mint),
            false,
        ),
        anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
            pda::canonical_pool(&keys.mint),
            false,
        ),
    ];
    let ok_ix = shortwin_litesvm_tests::ix::with_remaining(base.clone(), &detection);
    ok(env.send(&[ok_ix], &[]));
    let m = env.market(&keys.market);
    assert_eq!(m.no_route_since, env.now());
    assert_eq!(m.b_lo.get(), 0);
    let bad = shortwin_litesvm_tests::ix::with_remaining(
        base.clone(),
        &[detection[1].clone(), detection[0].clone()],
    );
    assert_err(
        &env.send(&[bad], &[]),
        shortwin::errors::ShortwinError::VenueAccountMismatch,
    );
    let short = shortwin_litesvm_tests::ix::with_remaining(base, &detection[..1]);
    assert_err(
        &env.send(&[short], &[]),
        shortwin::errors::ShortwinError::BadRemainingAccounts,
    );
}
