//! Every `propose_pool` / `execute_pool_switch` refusal (ARCH §2.6, §3.0, §3.9), each next to
//! the passing case where one exists: the accounts (every slot mandatory and key-checked), the
//! candidate kind, the market's venue, eligibility (the locked-depth floor — a locker-LP pool,
//! a thin pool —, the fee, a halted pool, another mint), the incumbent itself, the latch, the
//! margins and the windows.

use anchor_lang::error::ErrorCode as AnchorErrorCode;
use shortwin::constants::GUARD_RAYDIUM_V4;
use shortwin_litesvm_tests::errors::assert_anchor_err;
use shortwin_litesvm_tests::fork::{FIXTURE_GUARD_SLOTS, RaydiumProgram};
use shortwin_litesvm_tests::ix;
use shortwin_litesvm_tests::pool_select::{
    PlantSpec, detection_set, execute_pool_switch_raw, kind_of, propose_pool_ix, propose_pool_raw,
};
use shortwin_litesvm_tests::raydium::candidate_read_set;

use crate::common::*;
use crate::support::*;

fn ro(k: Pubkey) -> AccountMeta {
    AccountMeta::new_readonly(k, false)
}

/// Account checks on a list-only market (the detection set) and on a Raydium market (the
/// incumbent's ReadSet), and on the candidate's ReadSet: missing, extra, swapped or foreign
/// accounts; the named mint; the candidate kind.
#[test]
fn accounts_are_strict() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(FART);
    let (mint, _) = pool.token();
    list_auto(&mut env, &pool);
    let cand = candidate_read_set(&env.svm, &pool, false);
    let det = detection_set(&mint);
    let kind = kind_of(&pool);
    let send = |env: &mut Env, k: u8, rem: Vec<AccountMeta>| {
        env.send(&[propose_pool_raw(&mint, k, &rem)], &[])
    };
    // The detection set: missing, swapped, foreign.
    assert_err(&send(&mut env, kind, vec![]), E::BadRemainingAccounts);
    assert_err(
        &send(&mut env, kind, vec![det[0].clone()]),
        E::BadRemainingAccounts,
    );
    let swapped = [vec![det[1].clone(), det[0].clone()], cand.clone()].concat();
    assert_err(&send(&mut env, kind, swapped), E::VenueAccountMismatch);
    let foreign = [vec![det[0].clone(), ro(Pubkey::new_unique())], cand.clone()].concat();
    assert_err(&send(&mut env, kind, foreign), E::VenueAccountMismatch);
    // The candidate: no ReadSet, one slot short, one extra, a vault that is not the pool's,
    // the LP mint replaced.
    assert_err(&send(&mut env, kind, det.clone()), E::BadRemainingAccounts);
    let short = [det.clone(), cand[..3].to_vec()].concat();
    assert_err(&send(&mut env, kind, short), E::BadRemainingAccounts);
    let extra = [det.clone(), cand.clone(), vec![ro(Pubkey::new_unique())]].concat();
    assert_err(&send(&mut env, kind, extra), E::BadRemainingAccounts);
    let mut wrong_vault = cand.clone();
    wrong_vault.swap(1, 2);
    assert_err(
        &send(&mut env, kind, [det.clone(), wrong_vault].concat()),
        E::VenueAccountMismatch,
    );
    let mut wrong_lp = cand.clone();
    wrong_lp[3] = ro(Pubkey::new_unique());
    assert_err(
        &send(&mut env, kind, [det.clone(), wrong_lp].concat()),
        E::VenueAccountMismatch,
    );
    // The candidate kind: not a Raydium kind, or the other program's.
    for k in [0u8, 1, 2, 3, 6, 200] {
        let res = send(&mut env, k, [det.clone(), cand.clone()].concat());
        assert_err(&res, E::NoSupportedVenue);
    }
    let other = VenueKind::RaydiumCpmm as u8;
    assert_err(
        &send(&mut env, other, [det.clone(), cand.clone()].concat()),
        E::BadRemainingAccounts,
    );
    // The named mint: another market's PDA.
    let mut bad = propose_pool_ix(&env.svm, &mint, &det, &pool);
    bad.accounts[3] = ro(Pubkey::new_unique());
    assert_anchor_err(&env.send(&[bad], &[]), AnchorErrorCode::ConstraintSeeds);
    // The passing case.
    adopt_ok(&mut env, None, &pool);

    // A Raydium market: the incumbent's ReadSet is key-checked (a thin pool in its place).
    let lam = lambda_of(&env, &pool);
    let next = plant(&mut env, &pool, Plant::V4, true, (2, 1), lam);
    let thin = env.load_ray_pool(FART_REV);
    let wrong_inc = [
        candidate_read_set(&env.svm, &thin, false),
        candidate_read_set(&env.svm, &next, false),
    ]
    .concat();
    assert_err(&send(&mut env, kind, wrong_inc), E::VenueAccountMismatch);
    let inc = candidate_read_set(&env.svm, &pool, false);
    assert_err(
        &send(&mut env, kind, inc[..2].to_vec()),
        E::BadRemainingAccounts,
    );
    adopt_ok(&mut env, Some(&pool), &next);
}

/// Eligibility (ARCH §2.6 as amended by M4-SEC-1 / M4-SEC-3): the floor on locked depth,
/// `⌊⌊D·isqrt(P_ref)/10⁹⌋·10⁴/λ_min⌋ ≥ pool_min_real_sol` (`PoolTooThin`), at the pool's own spot
/// on a list-only market and at the market's `P_ema` once it has one; `λ` below
/// `pool_lock_min_bps` is no refusal by itself (a pool of a tenth of Fartcoin's reserves at
/// λ = 10 % passes, a pool whose LP is all burned fails with 12.4 SOL and passes with 12.6 SOL:
/// `2·λ·R` against 25 SOL); a launchpad pool whose LP sits in a locker (every LP circulating,
/// λ = 0 so `D = 0`: the dumped `3Cv9uPpa…`, list-only, V2-11) never; a fee above
/// `venue_fee_cap_bps` or a halted pool (`PoolIneligible`); a pool of another mint
/// (`PoolIneligible`); the incumbent itself (`PoolNotDominant`). The harness's own floor
/// (`pool_select::passes_floor`) agrees with every verdict.
#[test]
fn eligibility() {
    use shortwin_litesvm_tests::pool_select::passes_floor;
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(FART);
    list_auto(&mut env, &pool);
    let lam_min = u64::from(env.config().params.pool_lock_min_bps);
    let low = plant(&mut env, &pool, Plant::Cpmm, true, (1, 10), 1_000);
    assert!(lambda_of(&env, &low) < lam_min);
    let burned = shortwin_litesvm_tests::pool_select::spec_like(&env.svm, &pool, true, 10_000);
    let sized = |sol: u64| PlantSpec {
        sol,
        tokens: (u128::from(burned.tokens) * u128::from(sol) / u128::from(burned.sol)) as u64,
        ..burned
    };
    let below = plant_spec(&mut env, Plant::V4, sized(12_400_000_000));
    let above = plant_spec(&mut env, Plant::V4, sized(12_600_000_000));
    assert!(!passes_floor(&env, &below, None) && passes_floor(&env, &above, None));
    assert!(passes_floor(&env, &low, None));
    assert_err(&env.propose(None, &below), E::PoolTooThin);
    let fee = plant(&mut env, &pool, Plant::V4, true, (1, 10), lam_min);
    let AnyPool::V4(fp) = &fee else {
        unreachable!()
    };
    fork::set_v4_swap_fee(&mut env.svm, fp, 151, 10_000);
    assert_err(&env.propose(None, &fee), E::PoolIneligible);
    fork::set_v4_swap_fee(&mut env.svm, fp, 150, 10_000);
    let halted = plant(&mut env, &pool, Plant::Cpmm, false, (1, 10), lam_min);
    halt(&mut env, &halted);
    assert_err(&env.propose(None, &halted), E::PoolIneligible);
    // Another mint's pool, proposed for Fartcoin's market.
    let useless = env.load_ray_pool(USELESS);
    let res = env.send(
        &[propose_pool_ix(
            &env.svm,
            &pool.token().0,
            &detection_set(&pool.token().0),
            &useless,
        )],
        &[],
    );
    assert_err(&res, E::PoolIneligible);
    // λ at the minimum and the fee at the cap pass.
    adopt_ok(&mut env, None, &fee);
    env.warp(1);
    let inc = fee;
    assert_err(&env.propose(Some(&inc), &inc), E::PoolNotDominant);
    // On the Raydium market (a price now: P_ema): the floor still decides first — the pools
    // that pass it reach the margins (`PoolNotDominant`), the one under it does not.
    let p_ema = market_of(&env, &inc).ema_price.get();
    for (cand, passes) in [(&low, true), (&above, true), (&below, false)] {
        assert_eq!(passes_floor(&env, cand, Some(p_ema)), passes);
        let want = if passes {
            E::PoolNotDominant
        } else {
            E::PoolTooThin
        };
        assert_err(&env.propose(Some(&inc), cand), want);
    }

    // The locker-LP pool: list-only for good (λ = 0 as dumped, so D = 0).
    let mut env = Env::ray_fork();
    let locker = env.load_ray_pool(WSOL_T1);
    list_auto(&mut env, &locker);
    assert_eq!((lambda_of(&env, &locker), d_of(&env, &locker)), (0, 0));
    assert_err(&env.propose(None, &locker), E::PoolTooThin);
    assert_eq!(market_of(&env, &locker).venue_kind(), Some(VenueKind::None));
}

/// The market's venue: a pump venue refuses every candidate (`PumpVenuePrecedence`, D1; a
/// planted state — no pump venue exists before M5); `execute_pool_switch` has nothing to
/// execute on a list-only market (`NoPendingSwitch`).
#[test]
fn venues_without_pool_selection() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(FART);
    let (mint, _) = pool.token();
    list_auto(&mut env, &pool);
    let market = pda::market(&mint).0;
    let any = candidate_read_set(&env.svm, &pool, false);
    assert_err(
        &env.send(&[execute_pool_switch_raw(&mint, &any)], &[]),
        E::NoPendingSwitch,
    );
    env.check_invariants = false;
    for venue in [
        VenueKind::PumpCurve,
        VenueKind::PumpMigrating,
        VenueKind::PumpSwap,
    ] {
        fork::patch(&mut env.svm, &market, |d| d[17] = venue as u8);
        assert_eq!(env.market(&market).venue_kind(), Some(venue));
        assert_err(&env.propose(None, &pool), E::PumpVenuePrecedence);
    }
}

/// The latch on a list-only market's adoption (M3-SEC-2's rule, M4-SEC-8's mask): a CPMM
/// candidate is refused while a CPMM upgrade awaits review and adopted after
/// `revalidate_venue`; an AMM v4 upgrade under review does not hold it up (its compiled reader
/// is not the one at risk).
#[test]
fn a_list_only_adoption_waits_for_the_venue_review() {
    use shortwin::constants::GUARD_RAYDIUM_CPMM;
    for (guard, program, held) in [
        (GUARD_RAYDIUM_CPMM, RaydiumProgram::Cpmm, true),
        (GUARD_RAYDIUM_V4, RaydiumProgram::AmmV4, false),
    ] {
        let mut env = Env::ray_fork();
        let pool = env.load_ray_pool(USELESS);
        list_auto(&mut env, &pool);
        let dumped = FIXTURE_GUARD_SLOTS[usize::from(guard)];
        fork::set_deploy_slot(&mut env.svm, program, dumped + 1);
        ok(env.send(&[ix::flag_venue_upgraded(guard)], &[]));
        if held {
            assert_err(&env.propose(None, &pool), E::VenueUpgraded);
            let ea = env.pause_authority.insecure_clone();
            ok(env.send(
                &[ix::revalidate_venue(&ea.pubkey(), guard, dumped + 1)],
                &[&ea],
            ));
        }
        adopt_ok(&mut env, None, &pool);
    }
}

/// `execute_pool_switch`'s accounts: the incumbent's ReadSet and exactly the window
/// candidate's ReadSet (`fp_keys`), else `BadRemainingAccounts` / `VenueAccountMismatch`; a
/// window that has not run is `PoolSwitchNotDue`.
#[test]
fn execute_accounts_are_strict() {
    let (mut env, rm, q) = setup(FART);
    let mut b = env.borrower();
    open_ok(&mut env, &rm, &mut b, q);
    halt(&mut env, &rm.pool);
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    let other = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    let (mint, _) = rm.pool.token();
    let inc = candidate_read_set(&env.svm, &rm.pool, false);
    let c = candidate_read_set(&env.svm, &cand, false);
    let o = candidate_read_set(&env.svm, &other, false);
    let send = |env: &mut Env, rem: Vec<AccountMeta>| {
        env.send(&[execute_pool_switch_raw(&mint, &rem)], &[])
    };
    assert_err(
        &send(&mut env, [inc.clone(), c.clone()].concat()),
        E::PoolSwitchNotDue,
    );
    crank_every(&mut env, &rm, 30, 2);
    assert_err(&send(&mut env, inc.clone()), E::BadRemainingAccounts);
    assert_err(
        &send(&mut env, [inc.clone(), c[..3].to_vec()].concat()),
        E::BadRemainingAccounts,
    );
    let extra = [inc.clone(), c.clone(), vec![ro(Pubkey::new_unique())]].concat();
    assert_err(&send(&mut env, extra), E::BadRemainingAccounts);
    assert_err(
        &send(&mut env, [inc.clone(), o].concat()),
        E::VenueAccountMismatch,
    );
    let wrong_inc = [candidate_read_set(&env.svm, &other, false), c.clone()].concat();
    assert_err(&send(&mut env, wrong_inc), E::VenueAccountMismatch);
    let ev = execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    assert_eq!(
        ev.candidate,
        shortwin_litesvm_tests::pool_select::pool_key(&cand)
    );
}

/// The named mint must be owned by the market's token program (`AccountMismatch`; a planted
/// owner change — no real mint changes owner).
#[test]
fn the_mint_owner_is_checked() {
    let mut env = Env::ray_fork();
    let pool = env.load_ray_pool(FART);
    let (mint, _) = pool.token();
    list_auto(&mut env, &pool);
    env.check_invariants = false;
    let good = env.svm.get_account(&mint).unwrap();
    let mut acc = good.clone();
    acc.owner = shortwin::constants::TOKEN_2022_PROGRAM_ID;
    env.svm.set_account(mint, acc).unwrap();
    assert_err(&env.propose(None, &pool), E::AccountMismatch);
    env.svm.set_account(mint, good).unwrap();
    env.check_invariants = true;
    adopt_ok(&mut env, None, &pool);
}
