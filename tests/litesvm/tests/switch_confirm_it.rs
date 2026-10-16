//! M4R's gate and guard (post-change confirmation, owner item O18, **PROPOSED**; ARCH §3.6
//! precondition 1b, §3.7, §3.8 step 3, §4.4; ECON §5.2, §5.5, §8.2): the warm-up after a
//! venue change gates the keeper exits and `sol_settle` on an executable venue — and only there
//! — and the reference guard clears at the gate on an honest pool; a market without a restart is
//! never marked and evolves exactly as the pre-M4R build (the lockstep differential against that
//! build runs when `SHORTWIN_PRE_M4R_SO` / `SHORTWIN_PRE_M4R_TEST_VENUE_SO` name it).
//!
//! | Test | What is asserted |
//! |---|---|
//! | `the_warm_up_gates_keeper_exits_and_settlements` | an underwater position after a fast path onto a pool where the token rose 2.5× during the halt: in each of the 8 warm-up seconds `liquidate` and `expire` fail `EmaWarmingUp` and `sol_settle` returns `Ok` without arming or settling; at +9 s (the gate) the liquidation lands |
//! | `a_new_pool_that_halts_in_the_warm_up_is_not_gated` | the adopted pool halts at +6 s: `liquidate` then fails `NoRoute` (not the gate), `sol_settle` evaluates, and S3 settles once the new no-route chain has run `s3_wait_s`, the warm-up still unfinished (6 of 8 integrations; the step-bounded seed has reached the pool's 2.5× by then — M4X-1: a halt in the first seconds leaves it below, and S3 then waits for the deadline, as M4 as built's carried `P_ema` did; O18 residual 8) |
//! | `the_guard_clears_at_the_gate_on_an_honest_pool` | `ema_guard` is set by the adoption, stays set through the warm-up and clears at the gate; `L_ema` is then the pool's depth, `P_ema` its spot |
//! | `a_market_without_a_switch_is_never_marked` | a mock market traded, cranked, settled and closed for 90 s: `ema_warm = ema_guard = 0` throughout |
//! | `lockstep_*` (ignored unless the pre-M4R builds are named) | on a mock market and on a Raydium market after its warm-up (`ema_warm = ema_guard = 0`), every short.win instruction of a 90-second scenario, sent from the same state to the pre-M4R build, leaves byte-identical `Market`, `MarketVault` and position accounts |

#![allow(clippy::result_large_err)]

#[allow(unused_imports)]
#[path = "ray_it/common.rs"]
mod common;

#[path = "pool_select_it/support.rs"]
mod support;

#[allow(unused_imports)]
#[path = "settle_support.rs"]
mod settle_support;

/// A Fartcoin market with one default-size position, halted, and a 60 % CPMM candidate on which
/// the token moved to `mult_bps` of its pre-halt spot; the window opened and ran. Returns the
/// environment right before `execute_pool_switch`.
fn switch_ready(
    mult_bps: u128,
) -> (
    common::Env,
    common::RayMarket,
    common::Borrower,
    u64,
    common::AnyPool,
) {
    use common::*;
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_math::fees::Direction;
    use support::*;

    let (mut env, rm, q) = setup(FART);
    crank_every(&mut env, &rm, 1, 5);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (60, 100), lam);
    if mult_bps != 10_000 {
        let (mint, program) = cand.token();
        let (_, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
        let root = ((mult_bps as f64) / 10_000.0).sqrt();
        let a = (y as f64 * (root - 1.0) / 0.99) as u64;
        let whale = fork::trader(&mut env, &mint, program, a + 1, 0);
        ok(fork::swap(
            &mut env,
            &cand,
            &whale,
            Direction::Buy,
            Swap::ExactIn(a, 0),
        ));
    }
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    (env, rm, b, nonce, cand)
}

/// The gate (ARCH §3.6 precondition 1b, §3.7, §3.8 step 3): 8 warm-up seconds of refused exits
/// and no-op settlements, then the liquidation at the gate.
#[test]
fn the_warm_up_gates_keeper_exits_and_settlements() {
    use common::*;
    use shortwin::events::{SettleArmed, SettleDisarmed, SolSettle};
    use shortwin_litesvm_tests::engine::{expire_ix, liquidate_ix, sol_settle_ix};
    use support::*;

    let (mut env, rm, b, nonce, cand) = switch_ready(25_000);
    execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    let rm2 = rm.on_pool(&env.svm, cand);
    let k = env.funded_keypair();
    for s in 1..=8i64 {
        env.warp(1);
        env.crank(&rm2);
        assert!(env.market(&rm.market()).ema_warm > 0, "second {s}");
        let liq = env.send(&[liquidate_ix(&rm2, &k.pubkey(), &b.key(), nonce)], &[&k]);
        assert_err(&liq, E::EmaWarmingUp);
        let exp = env.send(&[expire_ix(&rm2, &k.pubkey(), &b.key(), nonce)], &[&k]);
        assert_err(&exp, E::EmaWarmingUp);
        let meta = ok(env.send(
            &[sol_settle_ix(&env, &rm2, &k.pubkey(), &b.key(), nonce)],
            &[&k],
        ));
        assert!(
            events::of::<SolSettle>(&meta).is_empty()
                && events::of::<SettleArmed>(&meta).is_empty()
                && events::of::<SettleDisarmed>(&meta).is_empty(),
            "second {s}: sol_settle decided something while warming"
        );
        assert_eq!(env.position(&rm, &b.key(), nonce).s_armed, 0);
    }
    env.warp(1);
    env.crank(&rm2);
    assert_eq!(env.market(&rm.market()).ema_warm, 0, "the gate at +9 s");
    let exp = env.send(&[expire_ix(&rm2, &k.pubkey(), &b.key(), nonce)], &[&k]);
    assert_err(&exp, E::NotExpired);
    ok(env.send(&[liquidate_ix(&rm2, &k.pubkey(), &b.key(), nonce)], &[&k]));
    println!("gate: 8 seconds of EmaWarmingUp / no-op settlements, liquidated at +9 s");
}

/// A venue that cannot execute is never gated: the adopted pool halts in the warm-up, its
/// positions follow the S3 path on the new no-route chain (O18 residual 8). The halt comes at
/// +6 s: five warm-up integrations before it and one after (the bucket of +6 s, `dt = 2`) take
/// the step-bounded price seed (M4X-1: ×9/7, then ×7/6, then ×9/7) to the pool's 2.5×, so the
/// position is liquidatable on it; a halt at +3 s would leave the seed at ≈ 1.9× and S3 would
/// wait for the deadline — as M4 as built's carried `P_ema` did (ECON §12.12 (8) (c)).
#[test]
fn a_new_pool_that_halts_in_the_warm_up_is_not_gated() {
    use common::*;
    use shortwin::events::SolSettle;
    use shortwin_litesvm_tests::engine::{liquidate_ix, sol_settle_ix};
    use support::*;

    let (mut env, rm, b, nonce, cand) = switch_ready(25_000);
    execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    let rm2 = rm.on_pool(&env.svm, cand);
    let k = env.funded_keypair();
    crank_every(&mut env, &rm2, 1, 6);
    halt(&mut env, &cand);
    let s3_wait = i64::from(env.config().params.s3_wait_s);
    let t_halt = env.now();
    let mut settled = None;
    for _ in 0..(s3_wait / 30 + 5) {
        env.warp(30);
        env.crank(&rm2);
        let m = env.market(&rm.market());
        assert!(m.ema_warm > 0, "the warm-up waits for an executable pool");
        let liq = env.send(&[liquidate_ix(&rm2, &k.pubkey(), &b.key(), nonce)], &[&k]);
        assert_err(&liq, E::NoRoute);
        let meta = ok(env.send(
            &[sol_settle_ix(&env, &rm2, &k.pubkey(), &b.key(), nonce)],
            &[&k],
        ));
        if let Some(e) = events::of::<SolSettle>(&meta).into_iter().next() {
            settled = Some((env.now() - t_halt, e.condition, m.ema_warm));
            break;
        }
    }
    println!("halted in the warm-up: settled (s after the halt, condition, ema_warm) {settled:?}");
    let (at, cond, warm) = settled.expect("S3 on the halted new pool");
    assert_eq!(cond, 3);
    assert!(at >= s3_wait && warm > 0);
}

/// The reference guard (ARCH §4.4; ECON §8.2): set by the adoption, set through the warm-up,
/// clear at the gate on an honest pool.
#[test]
fn the_guard_clears_at_the_gate_on_an_honest_pool() {
    use common::*;
    use shortwin_math::fees::Direction;
    use support::*;

    let (mut env, rm, b, nonce, cand) = switch_ready(10_000);
    execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    let rm2 = rm.on_pool(&env.svm, cand);
    let m = env.market(&rm.market());
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1));
    for s in 1..=9u8 {
        env.warp(1);
        env.crank(&rm2);
        let m = env.market(&rm.market());
        let want = if s < 9 { (9 - s, 1) } else { (0, 0) };
        assert_eq!((m.ema_warm, m.ema_guard), want, "second {s}");
    }
    let m = env.market(&rm.market());
    let (x, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
    let depth = (u128::from(x) * u128::from(y)).isqrt();
    let spot = u128::from(y) * shortwin_math::num::WAD / u128::from(x);
    assert!(m.ema_liq.get().abs_diff(depth) * 1_000 <= depth);
    assert_eq!(m.ema_price.get(), spot);
    close_ok(&mut env, &rm2, &b, nonce);
}

mod lockstep {
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::solana_program::instruction::Instruction;
    use litesvm::LiteSVM;
    use shortwin_litesvm_tests::Env;
    use solana_keypair::Keypair;

    /// The pre-M4R build named by `var`, if any.
    pub fn old_build(var: &str) -> Option<Vec<u8>> {
        let path = std::env::var_os(var)?;
        Some(std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.to_string_lossy())))
    }

    /// Sends `ixs` through the harness (the new build) and, from the same state, through `old`
    /// on a clone of the VM; the two must agree on success and leave `accounts` byte-identical.
    /// Returns the harness's result.
    pub fn send(
        env: &mut Env,
        old: Option<&[u8]>,
        accounts: &[Pubkey],
        ixs: &[Instruction],
        signers: &[&Keypair],
    ) -> shortwin_litesvm_tests::TxResult {
        let Some(old) = old else {
            return env.send(ixs, signers);
        };
        let mut shadow: LiteSVM = env.svm.clone();
        shadow
            .add_program(shortwin::ID, old)
            .unwrap_or_else(|e| panic!("add_program: {e:?}"));
        let tx = env.transaction(ixs, signers);
        let r_old = shadow.send_transaction(tx);
        let r_new = env.send(ixs, signers);
        assert_eq!(
            r_old.is_ok(),
            r_new.is_ok(),
            "the pre-M4R build {} where the new one {}",
            if r_old.is_ok() { "succeeded" } else { "failed" },
            if r_new.is_ok() { "succeeded" } else { "failed" }
        );
        for key in accounts {
            let a = shadow.get_account(key).map(|a| a.data);
            let b = env.svm.get_account(key).map(|a| a.data);
            assert!(a == b, "account {key} differs from the pre-M4R build's");
        }
        r_new
    }
}

/// A 90-second mock-market scenario (a position, whale moves every few seconds, a keeper
/// cranking and settling every second or two, a liquidation attempt, the close), through
/// [`lockstep::send`]; asserts `ema_warm = ema_guard = 0` after every instruction.
fn mock_scenario(old: Option<&[u8]>) {
    use shortwin::venues::ExtrasSel;
    use shortwin_litesvm_tests::engine::{
        close_short_ix, crank_price_ix, liquidate_ix, sol_settle_ix,
    };
    use shortwin_litesvm_tests::errors::ok;

    let mut sc = settle_support::Sc::default_market();
    let market = sc.mm.market();
    let accounts = [market, sc.mm.keys.market_vault, sc.position_key()];
    let k = sc.keeper.insecure_clone();
    let mut seed = 0x5eed_u64;
    let mut rnd = |n: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n
    };
    let check = |sc: &settle_support::Sc| {
        let m = sc.market();
        assert_eq!((m.ema_warm, m.ema_guard), (0, 0), "never marked");
    };
    for _ in 0..90 {
        sc.env.warp(1 + i64::try_from(rnd(2)).unwrap());
        if rnd(3) == 0 {
            let mult = 0.97 + 0.06 * (rnd(1_000) as f64 / 1_000.0);
            sc.move_by(mult);
        }
        ok(lockstep::send(
            &mut sc.env,
            old,
            &accounts,
            &[crank_price_ix(&sc.mm)],
            &[],
        ));
        check(&sc);
        if rnd(2) == 0 {
            let ix = sol_settle_ix(&sc.env, &sc.mm, &k.pubkey(), &sc.b.key(), sc.nonce);
            ok(lockstep::send(&mut sc.env, old, &accounts, &[ix], &[&k]));
            check(&sc);
        }
        if rnd(10) == 0 {
            let ix = liquidate_ix(&sc.mm, &k.pubkey(), &sc.b.key(), sc.nonce);
            let _ = lockstep::send(&mut sc.env, old, &accounts, &[ix], &[&k]);
            check(&sc);
        }
    }
    let bk = sc.b.kp.insecure_clone();
    let ix = close_short_ix(
        &sc.mm,
        &sc.b.key(),
        sc.nonce,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    ok(lockstep::send(&mut sc.env, old, &accounts, &[ix], &[&bk]));
    check(&sc);
}

use solana_signer::Signer as _;

/// A market that never had a restart is never marked (the mock: no Raydium listing, no
/// adoption).
#[test]
fn a_market_without_a_switch_is_never_marked() {
    mock_scenario(None);
}

/// The lockstep differential on the mock (module docs): needs `SHORTWIN_PRE_M4R_TEST_VENUE_SO`.
#[test]
#[ignore = "needs SHORTWIN_PRE_M4R_TEST_VENUE_SO (the pre-M4R test-venue build)"]
fn lockstep_a_mock_market_matches_the_pre_m4r_build() {
    let old = lockstep::old_build("SHORTWIN_PRE_M4R_TEST_VENUE_SO")
        .expect("SHORTWIN_PRE_M4R_TEST_VENUE_SO");
    mock_scenario(Some(&old));
    println!("lockstep (mock): byte-identical to the pre-M4R build");
}

/// The lockstep differential on a Raydium market once its listing's warm-up is over (the state
/// every market created before M4R has: `ema_warm = ema_guard = 0`): opens, cranks, real trades,
/// liquidation attempts and closes, byte-identical to the pre-M4R release build. Needs
/// `SHORTWIN_PRE_M4R_SO`.
#[test]
#[ignore = "needs SHORTWIN_PRE_M4R_SO (the pre-M4R release build)"]
fn lockstep_a_raydium_market_matches_the_pre_m4r_build() {
    use common::*;
    use shortwin_litesvm_tests::engine::liquidate_ix;
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_math::fees::Direction;

    let old = lockstep::old_build("SHORTWIN_PRE_M4R_SO").expect("SHORTWIN_PRE_M4R_SO");
    let (mut env, rm, q) = setup(FART);
    let m = env.market(&rm.market());
    assert_eq!(
        (m.ema_warm, m.ema_guard),
        (0, 0),
        "after the listing's warm-up"
    );
    let market = rm.market();
    let vault = m.market_vault;
    let mut b = env.borrower();
    let bk = b.kp.insecure_clone();
    let nonce = b.next_nonce;
    let pos = pda::position(&market, &b.key(), nonce).0;
    let accounts = [market, vault, pos];
    let open = open_short_ix(
        &rm,
        &b.key(),
        nonce,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    ok(lockstep::send(
        &mut env,
        Some(&old),
        &accounts,
        &[open],
        &[&bk],
    ));
    b.next_nonce += 1;
    let (mint, program) = rm.pool.token();
    let k = env.funded_keypair();
    let mut seed = 0xfa57_u64;
    let mut rnd = |n: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n
    };
    for _ in 0..60 {
        env.warp(1 + i64::try_from(rnd(2)).unwrap());
        if rnd(3) == 0 {
            let (x, y) = rm.pool.io_reserves(&env.svm, Direction::Sell).unwrap();
            let buy = rnd(2) == 0;
            let t = if buy {
                fork::trader(&mut env, &mint, program, y / 500 + 1, 0)
            } else {
                fork::trader(&mut env, &mint, program, 0, x / 500 + 1)
            };
            let (dir, amount) = if buy {
                (Direction::Buy, y / 500)
            } else {
                (Direction::Sell, x / 500)
            };
            ok(fork::swap(
                &mut env,
                &rm.pool,
                &t,
                dir,
                Swap::ExactIn(amount, 0),
            ));
        }
        ok(lockstep::send(
            &mut env,
            Some(&old),
            &accounts,
            &[crank_price_ix(&rm)],
            &[],
        ));
        let m = env.market(&market);
        assert_eq!((m.ema_warm, m.ema_guard), (0, 0));
        if rnd(8) == 0 {
            let ix = liquidate_ix(&rm, &k.pubkey(), &b.key(), nonce);
            let _ = lockstep::send(&mut env, Some(&old), &accounts, &[ix], &[&k]);
        }
    }
    let close = close_short_ix(&rm, &b.key(), nonce, u64::MAX, ExtrasSel::NONE, &[]);
    ok(lockstep::send(
        &mut env,
        Some(&old),
        &accounts,
        &[close],
        &[&bk],
    ));
    println!("lockstep (Raydium): byte-identical to the pre-M4R build");
}
