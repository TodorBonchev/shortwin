//! T-RAY-SWITCH's attacks (ARCH §2.6 "Attack | Outcome" table, §8.2; R-RAY §6.3, §12.2; V2-7,
//! V2-8) on the real programs.

use shortwin_litesvm_tests::fork::Swap;
use shortwin_litesvm_tests::pool_select::propose_pool_ix;
use shortwin_math::fees::Direction;
use solana_keypair::Keypair;

use crate::common::*;
use crate::support::*;

/// A fake deep pool of self-supplied liquidity: ten times Fartcoin's reserves with 90 % of its
/// LP held by the attacker (λ = 10 %) passes the eligibility floor (its locked depth is what
/// counts, M4-SEC-1) but that locked depth is only Fartcoin's own — `0.1·√(100·X·Y)` — so it is
/// no dominant candidate (`PoolNotDominant`): the unlocked nine tenths buy nothing. With half
/// its LP burned (λ = 50 %, three times the reserves) it outranks the incumbent on locked depth,
/// but a market with an open position does not move (`PositionsOpen`). Adopted once the market
/// is empty, the attacker's withdrawal of every unlocked LP halves the pool's reserves and
/// leaves its locked depth — and so every open's sizing bound, `R_eff ≤ D·√P_ema/λ_min` —
/// unchanged.
#[test]
fn a_fake_deep_pool_and_an_lp_pull() {
    let (mut env, rm, q) = setup(FART);
    let fake = plant(&mut env, &rm.pool, Plant::Cpmm, true, (10, 1), 1_000);
    let d_inc = env.market(&rm.market()).pool_d.get();
    let d_fake = d_of(&env, &fake);
    assert!(d_fake * 10_000 < d_inc * 12_500 && d_fake * 10_000 > d_inc * 9_900);
    assert_err(&env.propose(Some(&rm.pool), &fake), E::PoolNotDominant);
    let burned = plant(&mut env, &rm.pool, Plant::Cpmm, true, (3, 1), 5_000);
    assert!(d_of(&env, &burned) * 10_000 >= env.market(&rm.market()).pool_d.get() * 12_500);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    env.warp(1);
    assert_err(&env.propose(Some(&rm.pool), &burned), E::PositionsOpen);
    close_ok(&mut env, &rm, &b, nonce);
    env.warp(1);
    adopt_ok(&mut env, Some(&rm.pool), &burned);
    let rm2 = rm.on_pool(&env.svm, burned);
    env.warp(1);
    env.crank(&rm2);
    let d_before = env.market(&rm.market()).pool_d.get();
    // The attacker withdraws all unlocked LP (half the counter): both vaults halve, the counter
    // halves and the circulating supply goes to 0 (λ = 100 %).
    let AnyPool::Cpmm(p) = &rm2.pool else {
        unreachable!()
    };
    for v in [p.token_0_vault, p.token_1_vault] {
        let a = fork::vault_amount(&env.svm, &v);
        fork::set_token_amount(&mut env.svm, &v, a / 2);
    }
    let counter = raydium::lp_counter(&env.svm, &rm2.pool);
    fork::patch(&mut env.svm, &p.pool, |d| {
        let off = shortwin_venue_state::raydium_cpmm::OFF_LP_SUPPLY;
        d[off..off + 8].copy_from_slice(&(counter / 2).to_le_bytes());
    });
    raydium::set_mint_supply(&mut env.svm, &p.lp_mint, 0);
    env.warp(1);
    env.crank(&rm2);
    let d_after = env.market(&rm.market()).pool_d.get();
    assert!(
        d_after.abs_diff(d_before) * 10_000 <= d_before,
        "D {d_before} -> {d_after} after the pull"
    );
}

/// Proposal spam against a running collection window: five weaker pools (each a real pool, 0.15
/// SOL on mainnet) cannot displace the recorded candidate; the window's `fp_*` never moves.
#[test]
fn proposal_spam_cannot_displace_a_candidate() {
    let (mut env, rm, q) = setup(FART);
    let mut b = env.borrower();
    open_ok(&mut env, &rm, &mut b, q);
    halt(&mut env, &rm.pool);
    let lam = lambda_of(&env, &rm.pool);
    let good = plant(&mut env, &rm.pool, Plant::V4, true, (60, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &good);
    let fp = |env: &Env| {
        let m = env.market(&rm.market());
        (m.fp_keys, m.fp_d.get(), m.fp_t0)
    };
    let before = fp(&env);
    for i in 0..5u64 {
        let spam = plant(
            &mut env,
            &rm.pool,
            Plant::Cpmm,
            i % 2 == 0,
            (30 + 5 * i, 100),
            lam,
        );
        env.warp(5);
        assert_err(&env.propose(Some(&rm.pool), &spam), E::PoolNotDominant);
        assert_eq!(fp(&env), before, "spam {i}");
    }
}

/// An atomic price push at the proposal (`[buy → propose → sell]` in one transaction, and the
/// same with a sell first): a swap leaves `√(X·Y)` unchanged but for the fee the pool keeps, so
/// a candidate just below the 25 % margin stays below it (`PoolNotDominant`) however far the
/// bundle pushes its price; its `D` moved by less than 1 % after the round trip.
#[test]
fn an_atomic_price_push_does_not_inflate_d() {
    let (mut env, rm, _) = setup(FART);
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (124, 100), lam);
    let (mint, program) = cand.token();
    let (x, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
    let t = fork::trader(&mut env, &mint, program, y, x);
    let owner: Keypair = t.owner.insecure_clone();
    let prefix = raydium::candidate_read_set(&env.svm, &rm.pool, false);
    let d0 = d_of(&env, &cand);
    for dir in [Direction::Buy, Direction::Sell] {
        let back = match dir {
            Direction::Buy => Direction::Sell,
            Direction::Sell => Direction::Buy,
        };
        // Push by a third of the input reserve, propose, unwind.
        let (xin, _) = cand.io_reserves(&env.svm, dir).unwrap();
        let push = cand.swap_ix(
            &env.svm,
            &owner.pubkey(),
            &t,
            dir,
            Swap::ExactIn(xin / 3, 0),
        );
        let propose = propose_pool_ix(&env.svm, &mint, &prefix, &cand);
        let (bin, _) = cand.io_reserves(&env.svm, back).unwrap();
        let unwind = cand.swap_ix(
            &env.svm,
            &owner.pubkey(),
            &t,
            back,
            Swap::ExactIn(bin / 10, 0),
        );
        env.warp(1);
        assert_err(
            &env.send(&[push, propose, unwind], &[&owner]),
            E::PoolNotDominant,
        );
        // The push alone, then the proposal alone: still below the margin.
        let push = cand.swap_ix(
            &env.svm,
            &owner.pubkey(),
            &t,
            dir,
            Swap::ExactIn(xin / 3, 0),
        );
        ok(env.send(&[push], &[&owner]));
        assert_err(&env.propose(Some(&rm.pool), &cand), E::PoolNotDominant);
    }
    let d1 = d_of(&env, &cand);
    assert!(
        d1 >= d0 && (d1 - d0) * 100 < d0,
        "D {d0} -> {d1}: only fee growth"
    );
}
