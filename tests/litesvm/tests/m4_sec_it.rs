//! M4 security review regressions (M4-SEC-n): Raydium pool selection on the mainnet fork
//! (release short.win against the dumped AMM v4 / CPMM ELFs, dumped and planted pools). Each
//! test started as the review's reproducer of the finding and now asserts the fixed behaviour;
//! every instruction goes through `Env::send`, so the invariant hooks (I15, I17 included) run
//! after each one.
//!
//! | Test | Finding and what is asserted now |
//! |---|---|
//! | `m4_sec_1_*` | a persistent unlocked deposit (λ of the only fast-path candidate under `λ_min`, `D` unchanged) no longer keeps the fast path from opening a window: eligibility tests the locked depth. With and without the deposit, the halted Fartcoin market adopts the candidate at 120 s + 60 s and the underwater position is liquidated on it |
//! | `m4_sec_2_*` | `execute_pool_switch` adopts only while the `pool_fail` chain that armed the window holds: a recovery and a new failure (flapping) or a one-second failure after an hour (stale window) cancel; improvements on a broken chain are refused (`PoolWindowClosed`); the fast path re-arms after 120 s of the new failure and adopts |
//! | `m4_sec_3_*` | eligibility cannot be moved inside a transaction on a market with a price: `[buy, propose, sell]` and `[sell, execute, buy]` change nothing (the floor is on `D` at `P_ema`; a 20-SOL candidate is under the window floor of M4V-1 plainly and inside the round trip alike), `[withdraw LP, propose, deposit]` neither (`λ` is no clause, `D` does not move); a list-only market (no price) can still be pushed over the floor at its spot — the disclosed, bounded residual: no position, no open (`EmaWarmingUp`, then `R_eff` under the floor), replaced by the zero-position rule |
//! | `m4_sec_4_*` | a list-only market with open positions (M5's curve that graduated elsewhere, simulated) collects candidates in a window and adopts the deepest, not the first proposal; a pump venue appearing meanwhile cancels the window (D1) |
//! | `m4_sec_5_*` | M4R: an adoption writes the anchor `⌊D·10⁴/λ_min⌋` into `L_ema` and the ring — no sample — and `L_slow` at most the locked depth: a deposit or an unlocked LP's withdrawal around the execution (or around the proposal and the execution) changes nothing; after the warm-up `L_ema` is the pool's depth in every variant and the box is narrow; the position is neither liquidated nor settled |
//! | `m4_sec_6_*` | a fast path onto a pool deeper or thinner than `L_ema`: after the warm-up `L_ema` is that pool's depth and the box is never > 1 % wide in `L` from the gate on (T-VENUE-SWITCH, M4R) |
//! | `m4_sec_8_*` | a flagged upgrade of the incumbent's Raydium program does not hold up the fast path onto the other program's pool; a candidate on the flagged program waits |
//!
//! M4-SEC-7 (keeper liveness) is `services/keeper-litesvm/tests/scenarios/pool_select.rs`.

#![allow(clippy::result_large_err)]

#[allow(unused_imports)]
#[path = "ray_it/common.rs"]
mod common;

#[path = "pool_select_it/support.rs"]
mod support;

/// The attacker's liquidity moves on a CPMM pool through the dumped program's own `deposit`
/// and `withdraw` (copied from `pool_attacks_it.rs`).
#[allow(dead_code)]
mod lp {
    use anchor_lang::prelude::pubkey;
    use shortwin::constants::{RAYDIUM_CPMM_AUTHORITY, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID};
    use shortwin_litesvm_tests::fork::CpmmPool;
    use shortwin_litesvm_tests::tokens::TokenProgram;
    use solana_keypair::Keypair;

    use crate::common::*;

    pub const MEMO: Pubkey = pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

    fn disc(name: &str) -> [u8; 8] {
        let h = fork::sha256(format!("global:{name}").as_bytes());
        let mut d = [0u8; 8];
        d.copy_from_slice(&h[..8]);
        d
    }

    pub struct Provider {
        pub owner: Keypair,
        pub t0: Pubkey,
        pub t1: Pubkey,
        pub lp: Pubkey,
    }

    impl Provider {
        pub fn key(&self) -> Pubkey {
            self.owner.pubkey()
        }
    }

    pub fn provider(env: &mut Env, p: &CpmmPool, sol: u64, tokens: u64, lp: u64) -> Provider {
        let owner = env.funded_keypair();
        let w = fork::fund_wsol(env, &owner.pubkey(), sol);
        let t = fork::fund_token(
            env,
            &owner.pubkey(),
            &p.token_mint(),
            p.token_program(),
            tokens,
        );
        let l = fork::fund_token(env, &owner.pubkey(), &p.lp_mint, TokenProgram::Spl, lp);
        let (t0, t1) = if p.sol_is_token0 { (w, t) } else { (t, w) };
        Provider {
            owner,
            t0,
            t1,
            lp: l,
        }
    }

    fn metas(p: &CpmmPool, who: &Provider) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(who.key(), true),
            AccountMeta::new_readonly(RAYDIUM_CPMM_AUTHORITY, false),
            AccountMeta::new(p.pool, false),
            AccountMeta::new(who.lp, false),
            AccountMeta::new(who.t0, false),
            AccountMeta::new(who.t1, false),
            AccountMeta::new(p.token_0_vault, false),
            AccountMeta::new(p.token_1_vault, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new_readonly(p.token_0_mint, false),
            AccountMeta::new_readonly(p.token_1_mint, false),
            AccountMeta::new(p.lp_mint, false),
        ]
    }

    fn data(name: &str, a: u64, b: u64, c: u64) -> Vec<u8> {
        let mut d = disc(name).to_vec();
        for v in [a, b, c] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d
    }

    pub fn deposit_ix(p: &CpmmPool, who: &Provider, lp: u64) -> Instruction {
        Instruction {
            program_id: fork::RaydiumProgram::Cpmm.id(),
            accounts: metas(p, who),
            data: data("deposit", lp, u64::MAX, u64::MAX),
        }
    }

    pub fn withdraw_ix(p: &CpmmPool, who: &Provider, lp: u64) -> Instruction {
        let mut accounts = metas(p, who);
        accounts.push(AccountMeta::new_readonly(MEMO, false));
        Instruction {
            program_id: fork::RaydiumProgram::Cpmm.id(),
            accounts,
            data: data("withdraw", lp, 0, 0),
        }
    }

    pub fn balances(env: &Env, p: &CpmmPool, who: &Provider) -> (u64, u64) {
        let (a0, a1) = (tokens(env, &who.t0), tokens(env, &who.t1));
        if p.sol_is_token0 { (a0, a1) } else { (a1, a0) }
    }

    pub fn needed(env: &Env, p: &CpmmPool, lp: u64) -> (u64, u64) {
        let counter = p.state(&env.svm).lp_supply;
        let (x, y) = AnyPool::Cpmm(*p)
            .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
            .expect("reserves");
        let part =
            |r: u64| (u128::from(r) * u128::from(lp)).div_ceil(u128::from(counter)) as u64 + 1;
        (part(y), part(x))
    }
}

mod helpers {
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_math::fees::Direction;
    use shortwin_math::num::WAD;

    use crate::common::*;
    use crate::support::*;

    /// Spot price (WAD) of a pool.
    pub fn spot(env: &Env, pool: &AnyPool) -> u128 {
        let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        u128::from(y) * WAD / u128::from(x)
    }

    /// Real buys on `pool` until its spot reaches `mult_bps/10⁴` of `p0` (one swap).
    pub fn pump_to(env: &mut Env, pool: &AnyPool, p0: u128, mult_bps: u128) {
        let (mint, program) = pool.token();
        let (_, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        let now_p = spot(env, pool) as f64;
        let target = p0 as f64 * mult_bps as f64 / 10_000.0;
        let a = (y as f64 * ((target / now_p).sqrt() - 1.0) / 0.99) as u64;
        let whale = fork::trader(env, &mint, program, a + 1, 0);
        ok(fork::swap(
            env,
            pool,
            &whale,
            Direction::Buy,
            Swap::ExactIn(a, 0),
        ));
    }

    /// `(env, market, borrower, nonce, q)`: Fartcoin listed on its main pool, five seconds of
    /// cranks, one position of the default size, three more seconds (not halted).
    pub fn with_position() -> (Env, RayMarket, Borrower, u64, u64) {
        let (mut env, rm, q) = setup(FART);
        crank_every(&mut env, &rm, 1, 5);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        crank_every(&mut env, &rm, 1, 3);
        (env, rm, b, nonce, q)
    }
}

/// Box width over the next `n` per-second cranks of `rm2`: the seconds whose settlement box is
/// wider than `fv_dev` bps in `L`.
fn wide_seconds(env: &mut common::Env, rm2: &common::RayMarket, fv_dev: u128, n: u32) -> u32 {
    use common::VenueMarket;
    use shortwin_math::settle;
    let mut wide = 0;
    for _ in 0..n {
        env.warp(1);
        env.crank(rm2);
        let o = env.market(&rm2.market()).observation();
        let bx = settle::box_of(&o.ema, &o.ring, env.now(), true).unwrap();
        if (bx.l_hi - bx.l_lo) * 10_000 > fv_dev * bx.l_lo {
            wide += 1;
        }
    }
    wide
}

/// **M4-SEC-1** (MEDIUM; FIXED). v0.4.1's eligibility clause `λ_now ≥ λ_min` could be failed by
/// anyone who *added* unlocked liquidity to a candidate, and a deposit is an ordinary LP position
/// that can simply stay: the review's run refused 780 proposals over 900 s and left an
/// underwater position frozen at the pre-halt `P_ema` until S3 at its deadline paid lenders 41 %
/// of the tokens' value. Eligibility now tests the locked depth `D` (the floor
/// `⌊⌊D·isqrt(P_ema)/10⁹⌋·10⁴/λ_min⌋ ≥ pool_min_real_sol`), which the deposit does not move. The
/// same scenario with and without the attacker's deposit (as large as the candidate's LP
/// counter, λ 99.78 % → 49.9 %, left in place): the halted incumbent's fast path opens its window
/// at 120 s, adopts at 180 s and the position is liquidated on the candidate — with the deposit
/// too. The adoption credits at most `D/λ_min` of the diluted pool (its record is capped there).
#[test]
fn m4_sec_1_persistent_lp_dilution_no_longer_blocks_the_fast_path() {
    use shortwin_litesvm_tests::engine::liquidate_ix;

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    let mut outcome = Vec::new();
    for attack in [false, true] {
        let (mut env, rm, b, nonce, _q) = with_position();
        let lam = lambda_of(&env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (50, 100), lam);
        let AnyPool::Cpmm(cp) = cand else {
            unreachable!()
        };
        let d0 = d_of(&env, &cand);
        if attack {
            let add = cp.state(&env.svm).lp_supply;
            let (sol, tok) = lp::needed(&env, &cp, add);
            let a = lp::provider(&mut env, &cp, sol + 1_000, tok + 1_000, 0);
            let signer = a.owner.insecure_clone();
            ok(env.send(&[lp::deposit_ix(&cp, &a, add)], &[&signer]));
            assert!(lambda_of(&env, &cand) < 5_000, "diluted under λ_min");
            let d1 = d_of(&env, &cand);
            assert!(
                d1.abs_diff(d0) * 10_000 <= d0 * 3,
                "D unchanged: {d0} -> {d1}"
            );
        }
        let p_pre = helpers::spot(&env, &rm.pool);
        halt(&mut env, &rm.pool);
        // The real market moves on the candidate during the halt: 2.5×.
        let c0 = helpers::spot(&env, &cand);
        pump_to(&mut env, &cand, c0, 25_000);
        let t_halt = env.now();
        let keeper = env.funded_keypair();
        let s3_wait = i64::from(env.config().params.s3_wait_s);
        let route_s = i64::from(env.config().params.pool_fast_route_s);
        let window_s = i64::from(env.config().params.pool_fast_window_s);
        let mut adopted_at = None;
        let mut liquidated_at = None;
        let mut l_ema_after = 0;
        let mut cur = rm.on_pool(&env.svm, rm.pool);
        for s in 1..=(s3_wait + 300) {
            env.warp(1);
            env.crank(&cur);
            if adopted_at.is_none() {
                let m = env.market(&rm.market());
                let armed = m.pool_fail_since > 0 && env.now() - m.pool_fail_since >= route_s;
                if m.fp_t0 == 0 && armed {
                    // Never refused: the diluted candidate is as eligible as the honest one.
                    window_ok(&mut env, &rm.pool, &cand);
                } else if m.fp_t0 != 0 && env.now() >= m.fp_t0 + window_s {
                    let ev = execute_ok(&mut env, &rm.pool, &cand)
                        .unwrap_or_else(|_| panic!("cancelled"));
                    assert_eq!(ev.candidate, cp.pool);
                    adopted_at = Some(s);
                    l_ema_after = env.market(&rm.market()).ema_liq.get();
                    cur = rm.on_pool(&env.svm, cand);
                }
            }
            if liquidated_at.is_none() && adopted_at.is_some() {
                let res = env.send(
                    &[liquidate_ix(&cur, &keeper.pubkey(), &b.key(), nonce)],
                    &[&keeper],
                );
                match custom_code(&res) {
                    None => {
                        ok(res);
                        liquidated_at = Some(s);
                        break;
                    }
                    Some(c) => assert!(
                        c == code(E::NotLiquidatable)
                            || c == code(E::ExitBoundNotMet)
                            || c == code(E::ReferenceStale)
                            || c == code(E::EmaWarmingUp),
                        "second {s}: liquidate failed with {c}"
                    ),
                }
            }
        }
        assert!(
            adopted_at.is_some_and(|s| s <= 190),
            "{attack}: adopted at {adopted_at:?}"
        );
        assert!(
            liquidated_at.is_some(),
            "{attack}: liquidated on the candidate"
        );
        let d = d_of(&env, &cand);
        let lam_min = u64::from(env.config().params.pool_lock_min_bps);
        let cap = shortwin_math::pool_select::depth_at_lambda(d, lam_min);
        assert!(l_ema_after <= cap + cap / 1_000, "the anchor");
        println!(
            "M4-SEC-1 {}: adopted at {adopted_at:?} s, liquidated at {liquidated_at:?} s after the halt; L_ema after {l_ema_after} (D {d}); P_ema/pre-halt spot {:.3}; candidate spot/pre-halt spot {:.3}",
            if attack { "attack " } else { "control" },
            env.market(&rm.market()).ema_price.get() as f64 / p_pre as f64,
            helpers::spot(&env, &cand) as f64 / p_pre as f64,
        );
        let _ = t_halt;
        outcome.push((adopted_at, liquidated_at));
    }
    assert_eq!(outcome[0].0, outcome[1].0, "the deposit changed nothing");
}

/// **M4-SEC-2** (LOW; FIXED). `execute_pool_switch` adopted whenever its own observation of the
/// incumbent failed `pool_fail`, not only while the chain that armed the window held: a flapping
/// incumbent was switched away after 40 s of its new failure, a window left unresolved for an
/// hour after 0 s. Now the execution adopts only if `pool_fail_since + pool_fast_route_s ≤ fp_t0`
/// on this instruction's chain (the chain that armed the window is unbroken): both variants
/// **cancel**, improvements on the broken chain are refused (`PoolWindowClosed`), and once the new
/// failure has itself lasted `pool_fast_route_s` the fast path re-arms and adopts.
#[test]
fn m4_sec_2_a_restarted_chain_cancels_the_window() {
    use shortwin::events::POOL_KIND_FAST;
    use shortwin_litesvm_tests::pool_select::pool_key;

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    for stale in [false, true] {
        let (mut env, rm, b, nonce, _q) = with_position();
        halt(&mut env, &rm.pool);
        let lam = lambda_of(&env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
        let deeper = plant(&mut env, &rm.pool, Plant::V4, true, (60, 100), lam);
        crank_every(&mut env, &rm, 30, 5);
        let e = window_ok(&mut env, &rm.pool, &cand);
        // The incumbent recovers; a crank clears the pool_fail chain.
        env.warp(10);
        resume(&mut env, &rm.pool);
        env.crank(&rm);
        assert_eq!(env.market(&rm.market()).pool_fail_since, 0, "chain cleared");
        if stale {
            // Nobody resolves the window for an hour (no keeper); the pool is healthy.
            crank_every(&mut env, &rm, 30, 120);
            halt(&mut env, &rm.pool);
            env.warp(1);
        } else {
            env.warp(10);
            halt(&mut env, &rm.pool);
            env.crank(&rm);
            // The window still runs, but its chain broke: no improvement.
            assert_err(&env.propose(Some(&rm.pool), &deeper), E::PoolWindowClosed);
            let now = env.now();
            env.warp(e.window_ends - now);
        }
        let c = execute_ok(&mut env, &rm.pool, &cand)
            .map(|_| ())
            .expect_err("a restarted chain must cancel");
        assert_eq!((c.kind, c.candidate), (POOL_KIND_FAST, pool_key(&cand)));
        let m = env.market(&rm.market());
        assert_eq!(
            (m.fp_t0, m.pool_cache[0]),
            (0, pool_key(&rm.pool)),
            "nothing adopted"
        );
        let since = m.pool_fail_since;
        assert!(since > 0, "the new chain runs");
        println!(
            "M4-SEC-2 {}: cancelled; the new chain started {} s before the execution",
            if stale {
                "stale window, 1 s failure"
            } else {
                "flapping incumbent"
            },
            env.now() - since
        );
        // The new failure arms the fast path in its own right after pool_fast_route_s.
        let route = i64::from(env.config().params.pool_fast_route_s);
        while env.now() - since < route {
            env.warp(30.min(route - (env.now() - since)).max(1));
            env.crank(&rm);
        }
        window_ok(&mut env, &rm.pool, &deeper);
        crank_every(&mut env, &rm, 30, 2);
        let ev = execute_ok(&mut env, &rm.pool, &deeper).unwrap_or_else(|_| panic!("cancelled"));
        assert_eq!((ev.kind, ev.candidate), (POOL_KIND_FAST, pool_key(&deeper)));
        let rm2 = rm.on_pool(&env.svm, deeper);
        env.warp(1);
        env.crank(&rm2);
        close_ok(&mut env, &rm2, &b, nonce);
    }
}

/// **M4-SEC-3** (LOW; FIXED for markets with a price, bounded residual without one).
/// Eligibility read the live SOL reserve (a buy raises it for one instruction) and the live `λ`
/// (the pool's own LP holder raises it by withdrawing). It now tests the locked depth at the
/// market's `P_ema`, which neither a swap nor an LP move changes:
///
/// (b) a fast path with an open position (incumbent: a planted AMM v4 pool of 40 SOL) — the
///     review's reproducer moved the position onto a 20-SOL candidate through `[buy, propose,
///     sell]` and `[buy, execute, sell]`: a fully locked 20-SOL candidate (50 % of `pool_d`)
///     passes the listing floor (locked SOL over λ_min ≈ 40 SOL) but not the **window floor** of
///     an adoption carrying positions (its locked SOL alone, 20 < 25 SOL; M4V-1), and is refused
///     `PoolTooThin` plainly and inside `[buy 5.1 SOL, propose_pool, sell]` alike; a 26-SOL one
///     opens the window plainly, and `[sell to 20 SOL, execute, buy back]` adopts it — the round
///     trip changes the verdict in neither direction;
/// (c) a list-only market: a 30-SOL CPMM pool at λ = 30 % (≈ 18 SOL) is refused plainly and
///     inside `[withdraw 60 % of the LP (λ → 52 %), propose_pool, deposit back]`; a pool of a
///     tenth of Fartcoin's reserves at λ = 30 % is adopted plainly (λ is no clause);
/// (a) the residual: a list-only market has no price, so the floor is judged at the candidate's
///     spot, which `[buy, propose_pool, sell]` still moves: a 10-SOL pool (≈ 20 SOL) is adopted
///     that way. It is bounded: the market has no position, no open passes `R_eff ≥
///     pool_min_real_sol` once honest observations warmed the price state up (`PoolTooThin`;
///     before them `EmaWarmingUp`, M4V-5 — `m4v_it::m4v_5_*`), and a deeper pool replaces it by
///     the zero-position rule.
#[test]
fn m4_sec_3_eligibility_is_not_movable_inside_a_transaction() {
    use shortwin::events::{
        POOL_KIND_FAST, POOL_KIND_LISTING, POOL_KIND_ZERO_POSITION, PoolSwitched,
    };
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_litesvm_tests::pool_select::{
        PlantSpec, detection_set, execute_pool_switch_ix, passes_floor, passes_window_floor,
        pool_key, propose_pool_ix, spec_like,
    };
    use shortwin_litesvm_tests::raydium::candidate_read_set;
    use shortwin_math::fees::Direction;

    use crate::common::*;
    use crate::support::*;

    let floor = |env: &Env| env.config().params.pool_min_real_sol;
    // A pool of `base`'s price with `sol` of real SOL and locked share `lam`.
    fn plant_spec_sized(env: &mut Env, kind: Plant, base: &AnyPool, lam: u64, sol: u64) -> AnyPool {
        let s = spec_like(&env.svm, base, true, lam);
        let spec = PlantSpec {
            sol,
            tokens: (u128::from(s.tokens) * u128::from(sol) / u128::from(s.sol)) as u64,
            ..s
        };
        plant_spec(env, kind, spec)
    }

    // (b) The fast path with an open position.
    {
        let mut env = Env::ray_fork();
        let base = env.load_ray_pool(FART);
        let lam = lambda_of(&env, &base);
        let inc = plant_spec_sized(&mut env, Plant::V4, &base, lam, 40_000_000_000);
        ok(env.list_ray(&inc, VENUE_HINT_V4));
        let q = q_of(&env, &inc);
        let rm = env.ray_market_listed(RaySpec::new(FART, q.saturating_mul(40)), inc);
        env.crank(&rm);
        crank_every(&mut env, &rm, 1, 5);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        crank_every(&mut env, &rm, 1, 3);
        halt(&mut env, &rm.pool);
        let thin = plant_spec_sized(&mut env, Plant::Cpmm, &base, 10_000, 20_000_000_000);
        let ok_pool = plant_spec_sized(&mut env, Plant::Cpmm, &base, 10_000, 26_000_000_000);
        crank_every(&mut env, &rm, 30, 5);
        let p_ema = env.market(&rm.market()).ema_price.get();
        assert!(
            passes_floor(&env, &thin, Some(p_ema))
                && !passes_window_floor(&env, &thin, Some(p_ema))
                && passes_window_floor(&env, &ok_pool, Some(p_ema)),
            "the 20-SOL pool is listable but under the window floor; the 26-SOL one passes both"
        );
        assert!(d_of(&env, &thin) * 10_000 >= env.market(&rm.market()).pool_d.get() * 2_500);
        assert_err(&env.propose(Some(&rm.pool), &thin), E::PoolTooThin);
        let (mint, program) = thin.token();
        let push = 5_100_000_000u64;
        let t = fork::trader(&mut env, &mint, program, 2 * push, 0);
        let owner = t.owner.insecure_clone();
        let inc_read = candidate_read_set(&env.svm, &rm.pool, false);
        let buy = thin.swap_ix(
            &env.svm,
            &owner.pubkey(),
            &t,
            Direction::Buy,
            Swap::ExactIn(push, 0),
        );
        let bought = thin
            .quote_exact_in(&env.svm, Direction::Buy, push)
            .unwrap()
            .proceeds;
        let sell = thin.swap_ix(
            &env.svm,
            &owner.pubkey(),
            &t,
            Direction::Sell,
            Swap::ExactIn(bought, 0),
        );
        let prop = propose_pool_ix(&env.svm, &mint, &inc_read, &thin);
        assert_err(&env.send(&[buy, prop, sell], &[&owner]), E::PoolTooThin);
        // The 26-SOL pool opens the window plainly; a sell round trip does not cancel it.
        window_ok(&mut env, &rm.pool, &ok_pool);
        crank_every(&mut env, &rm, 30, 2);
        let (_, y) = ok_pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        let out = y - 20_000_000_000;
        let tokens_in = ok_pool
            .quote_exact_out(&env.svm, Direction::Sell, out)
            .unwrap()
            .cost;
        let seller = fork::trader(&mut env, &mint, program, 0, tokens_in);
        let so = seller.owner.insecure_clone();
        let sell = ok_pool.swap_ix(
            &env.svm,
            &so.pubkey(),
            &seller,
            Direction::Sell,
            Swap::ExactOut(tokens_in, out),
        );
        let exec = execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &ok_pool);
        let back = ok_pool.swap_ix(
            &env.svm,
            &so.pubkey(),
            &seller,
            Direction::Buy,
            Swap::ExactIn(out, 0),
        );
        let meta = ok(env.send(&[sell, exec, back], &[&so]));
        let e: PoolSwitched = events::one(&meta);
        assert_eq!((e.kind, e.candidate), (POOL_KIND_FAST, pool_key(&ok_pool)));
        println!(
            "M4-SEC-3 (b): a 20-SOL candidate (listable, under the window floor) refused with and without [buy, propose, sell]; a 26-SOL one adopted through [sell to 20 SOL, execute, buy] ({:.1} % of pool_d)",
            e.d_cand as f64 * 100.0 / e.d_inc as f64
        );
        let rm2 = rm.on_pool(&env.svm, ok_pool);
        env.warp(1);
        env.crank(&rm2);
        close_ok(&mut env, &rm2, &b, nonce);
    }

    // (c) λ raised for one instruction by the pool's own LP holder: no effect.
    {
        let mut env = Env::ray_fork();
        let base = env.load_ray_pool(FART);
        list_auto(&mut env, &base);
        let pool = plant_spec_sized(&mut env, Plant::Cpmm, &base, 3_000, 30_000_000_000);
        let AnyPool::Cpmm(p) = pool else {
            unreachable!()
        };
        assert!(!passes_floor(&env, &pool, None));
        assert_err(&env.propose(None, &pool), E::PoolTooThin);
        let counter = p.state(&env.svm).lp_supply;
        let supply = raydium::mint_supply(&env.svm, &p.lp_mint);
        let who = lp::provider(&mut env, &p, 0, 0, supply);
        let signer = who.owner.insecure_clone();
        // Withdraw w: λ' = (c − s)/(c − w) ≥ 50 % ⇔ w ≥ c − 2(c − s).
        let w = counter - 2 * (counter - supply) + 1;
        let (mint, _) = pool.token();
        let prop = propose_pool_ix(&env.svm, &mint, &detection_set(&mint), &pool);
        let ixs = [
            lp::withdraw_ix(&p, &who, w),
            prop,
            lp::deposit_ix(&p, &who, w - 2),
        ];
        assert_err(&env.send(&ixs, &[&signer]), E::PoolTooThin);
        // λ = 30 % on a deep pool: adopted plainly.
        let deep = plant(&mut env, &base, Plant::Cpmm, true, (1, 10), 3_000);
        let e = adopt_ok(&mut env, None, &deep);
        assert_eq!(e.kind, POOL_KIND_LISTING);
        println!(
            "M4-SEC-3 (c): [withdraw, propose, deposit] refused at λ 30 % under the floor; a deep λ 30 % pool adopted plainly"
        );
    }

    // (a) The residual on a market without a price.
    {
        let mut env = Env::ray_fork();
        let base = env.load_ray_pool(FART);
        list_auto(&mut env, &base);
        let lam = lambda_of(&env, &base);
        let sol = 10_000_000_000u64;
        let thin = plant_spec_sized(&mut env, Plant::Cpmm, &base, lam, sol);
        assert_err(&env.propose(None, &thin), E::PoolTooThin);
        let (mint, program) = thin.token();
        let push = 2_700_000_000u64;
        let t = fork::trader(&mut env, &mint, program, push + 1_000_000, 0);
        let owner = t.owner.insecure_clone();
        let buy = thin.swap_ix(
            &env.svm,
            &owner.pubkey(),
            &t,
            Direction::Buy,
            Swap::ExactIn(push, 0),
        );
        let prop = propose_pool_ix(&env.svm, &mint, &detection_set(&mint), &thin);
        let bought = thin
            .quote_exact_in(&env.svm, Direction::Buy, push)
            .unwrap()
            .proceeds;
        let sell = thin.swap_ix(
            &env.svm,
            &owner.pubkey(),
            &t,
            Direction::Sell,
            Swap::ExactIn(bought, 0),
        );
        let meta = ok(env.send(&[buy, prop, sell], &[&owner]));
        let e: PoolSwitched = events::one(&meta);
        assert_eq!((e.kind, e.candidate), (POOL_KIND_LISTING, pool_key(&thin)));
        let (_, y) = thin.io_reserves(&env.svm, Direction::Sell).unwrap();
        assert!(y < floor(&env));
        // Bounded: no open after the listing window, and the zero-position rule replaces it.
        let (rm, _) = adopted_market(&mut env, thin);
        let mut b = env.borrower();
        assert_err(&env.open(&rm, &mut b, 1_000_000).0, E::PoolTooThin);
        env.warp(1);
        let e = adopt_ok(&mut env, Some(&thin), &base);
        assert_eq!(e.kind, POOL_KIND_ZERO_POSITION);
        println!(
            "M4-SEC-3 (a): residual — a list-only market adopted a {:.2}-SOL pool through [buy, propose, sell]; no open (PoolTooThin), replaced by the zero-position rule",
            y as f64 / 1e9
        );
    }
}

/// **M4-SEC-4** (LOW, latent until M5; FIXED). ARCH §2.5 lets a live curve market become `None`
/// with its positions open (the curve completes with zeroed reserves and no canonical pool); a
/// Raydium pool is then adopted through `propose_pool`. v0.4.1's `decide` adopted the **first**
/// eligible proposal there, after which decision 7 refused every later pool while positions
/// were open. Now a list-only market with open positions collects candidates in a window of
/// `pool_fast_window_s` and `execute_pool_switch` adopts the deepest (the same rule as the fast
/// path; `I15`: a non-fast adoption only at zero positions). Host part: `decide` itself. Fork
/// part (the M5 transition simulated by rewriting the venue byte of a market with an open
/// position to `None`): an attacker's 26-SOL pool is refused outright (switch only if all fit,
/// owner decision of 2026-09-29: the position, 2 SOL of notional, is above its per-position cap
/// — `CarriedPositionsTooLarge`, no window); a 400-SOL pool the position fits opens the window,
/// Fartcoin's main pool (≈ 95 times deeper) replaces it, the execution adopts the main pool and
/// the position closes on it; with a live pump curve planted during the window, the execution
/// cancels (D1).
#[test]
fn m4_sec_4_a_list_only_market_with_positions_collects_the_deepest_pool() {
    use shortwin::engine::pool_select::{Proposal, SelectParams, Situation, decide};
    use shortwin::events::{POOL_KIND_FAST, PoolProposed, PoolSwitched};
    use shortwin_litesvm_tests::fixtures::set_zc;
    use shortwin_litesvm_tests::pool_select::{PlantSpec, pool_key, spec_like};
    use shortwin_litesvm_tests::raydium::{CurveSpec, curve_bytes, plant_curve};

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    let p = SelectParams {
        list_window_s: 3_600,
        list_margin_bps: 500,
        switch_margin_bps: 2_500,
        fast_route_s: 120,
        fast_min_bps: 2_500,
        fast_window_s: 60,
    };
    let none = Situation {
        venue: VenueKind::None,
        now: 10_000,
        open_positions: 3,
        pool_seen_at: 0,
        pool_d: 0,
        pool_fail: false,
        pool_fail_since: 0,
        fp_t0: 0,
        fp_d: 0,
    };
    assert_eq!(decide(&none, 1, &p).unwrap(), Proposal::OpenWindow);
    let open = Situation {
        fp_t0: 10_000,
        fp_d: 1,
        now: 10_030,
        ..none
    };
    assert_eq!(
        decide(&open, 1_000_000, &p).unwrap(),
        Proposal::ImproveWindow
    );

    for pump_appears in [false, true] {
        let (mut env, rm, b, nonce, _q) = with_position();
        let lam = lambda_of(&env, &rm.pool);
        let s = spec_like(&env.svm, &rm.pool, true, lam);
        let sized = |env: &mut Env, sol: u64| {
            plant_spec(
                env,
                Plant::Cpmm,
                PlantSpec {
                    sol,
                    tokens: (u128::from(s.tokens) * u128::from(sol) / u128::from(s.sol)) as u64,
                    ..s
                },
            )
        };
        let tiny = sized(&mut env, 26_000_000_000);
        let sol = 400_000_000_000u64;
        let thin = sized(&mut env, sol);
        let mut m = env.market(&rm.market());
        m.venue = VenueKind::None as u8;
        // As a curve market that graduated elsewhere would be: no Raydium pool, no locked depth.
        m.pool_cache = [Pubkey::default(); 10];
        m.pool_d.set(0);
        set_zc(&mut env.svm, &rm.market(), &m, 0);
        // Switch only if all fit: the 26-SOL pool cannot even open the window.
        assert_err(&env.propose(None, &tiny), E::CarriedPositionsTooLarge);
        assert_eq!(env.market(&rm.market()).fp_t0, 0, "no window");
        let meta = ok(env.propose(None, &thin));
        assert!(
            events::of::<PoolSwitched>(&meta).is_empty(),
            "no first-come adoption"
        );
        let w: PoolProposed = events::one(&meta);
        assert_eq!((w.kind, w.candidate), (POOL_KIND_FAST, pool_key(&thin)));
        env.warp(30);
        let meta = ok(env.propose(None, &rm.pool));
        let w2: PoolProposed = events::one(&meta);
        assert_eq!(w2.candidate, pool_key(&rm.pool));
        assert_err(&env.propose(None, &thin), E::PoolNotDominant);
        assert_err(&env.execute_switch_list_only(&rm.pool), E::PoolSwitchNotDue);
        let ratio = d_of(&env, &rm.pool) / d_of(&env, &thin);
        if pump_appears {
            let (mint, _) = rm.pool.token();
            let live = CurveSpec {
                virtual_token_reserves: 1_000_000_000_000,
                virtual_quote_reserves: 30_000_000_000,
                real_token_reserves: 500_000_000_000,
                real_quote_reserves: 1,
                complete: false,
                len: 151,
                ..CurveSpec::default()
            };
            plant_curve(&mut env.svm, &mint, curve_bytes(&live));
            assert_err(&env.propose(None, &thin), E::PumpVenuePrecedence);
        }
        let now = env.now();
        env.warp(w.window_ends - now);
        let meta = ok(env.execute_switch_list_only(&rm.pool));
        if pump_appears {
            assert!(
                events::of::<PoolSwitched>(&meta).is_empty(),
                "D1: cancelled"
            );
            let m = env.market(&rm.market());
            assert_eq!((m.venue_kind(), m.fp_t0), (Some(VenueKind::None), 0));
            println!("M4-SEC-4: a pump venue appeared during the window: cancelled (D1)");
            continue;
        }
        let e: PoolSwitched = events::one(&meta);
        assert_eq!((e.kind, e.candidate), (POOL_KIND_FAST, pool_key(&rm.pool)));
        assert_eq!(env.market(&rm.market()).open_positions, 1);
        println!(
            "M4-SEC-4: a None market with an open position collected a {:.0}-SOL pool and the one {ratio}× deeper, and adopted the deeper",
            sol as f64 / 1e9
        );
        env.warp(1);
        env.crank(&rm);
        close_ok(&mut env, &rm, &b, nonce);
    }
}

/// **M4-SEC-5** (LOW; FIXED by M4R). The adoption's ring rewrite took the candidate's `√(X·Y)` in
/// the adopting instruction: an unlocked LP of the winning candidate that wrapped the execution in
/// `[withdraw its LP, execute_pool_switch, deposit it back]` wrote the locked-only depth into
/// `L_ema`, `L_slow` and every ring entry (M4V-2's two-sample minimum still took it). An adoption
/// now reads no sample: it writes the anchor `⌊D·10⁴/λ_min⌋` into `L_ema` and the ring and
/// `L_slow = min(L_slow, D)`, and the warm-up seeds the pool's depth from its buckets' highs. A
/// withdrawal or a deposit around the execution, or a withdrawal around both the proposal and the
/// execution (M4V-2's residual), changes nothing: `L_ema` at the adoption, `L_slow` and, after the
/// warm-up, `L_ema` (the pool's depth) and the box (narrow) equal the honest run's; the position
/// is neither liquidated nor settled and closes normally.
#[test]
fn m4_sec_5_the_ring_rewrite_bounds_the_executions_sample() {
    use shortwin_litesvm_tests::pool_select::{execute_pool_switch_ix, propose_pool_ix};
    use shortwin_litesvm_tests::raydium::candidate_read_set;

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    let mut rows = Vec::new();
    for variant in ["honest", "withdraw", "deposit", "residual"] {
        let (mut env, rm, b, nonce, _q) = with_position();
        halt(&mut env, &rm.pool);
        // The attacker's pool: 60 % of Fartcoin's reserves, λ 50 %, the attacker holding every
        // circulating LP token (the unlocked half).
        let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (60, 100), 5_000);
        let AnyPool::Cpmm(cp) = cand else {
            unreachable!()
        };
        let supply = raydium::mint_supply(&env.svm, &cp.lp_mint);
        let add = 4 * cp.state(&env.svm).lp_supply;
        let (sol, tok) = lp::needed(&env, &cp, add);
        let who = lp::provider(&mut env, &cp, sol + 1_000, tok + 1_000, supply);
        let signer = who.owner.insecure_clone();
        crank_every(&mut env, &rm, 30, 5);
        let (mint, _) = rm.pool.token();
        if variant == "residual" {
            let inc = candidate_read_set(&env.svm, &rm.pool, false);
            let ixs = [
                lp::withdraw_ix(&cp, &who, supply),
                propose_pool_ix(&env.svm, &mint, &inc, &cand),
                lp::deposit_ix(&cp, &who, supply - 2),
            ];
            ok(env.send(&ixs, &[&signer]));
        } else {
            window_ok(&mut env, &rm.pool, &cand);
        }
        crank_every(&mut env, &rm, 30, 2);
        let d = d_of(&env, &cand);
        let exec = execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &cand);
        match variant {
            "withdraw" | "residual" => {
                let held = tokens(&env, &who.lp);
                let ixs = [
                    lp::withdraw_ix(&cp, &who, held),
                    exec,
                    lp::deposit_ix(&cp, &who, held - 4),
                ];
                ok(env.send(&ixs, &[&signer]));
            }
            "deposit" => {
                let ixs = [
                    lp::deposit_ix(&cp, &who, add),
                    exec,
                    lp::withdraw_ix(&cp, &who, add),
                ];
                ok(env.send(&ixs, &[&signer]));
            }
            _ => {
                ok(env.send(&[exec], &[]));
            }
        }
        let m = env.market(&rm.market());
        assert_eq!(m.pool_cache[0], cp.pool, "adopted");
        let (l_ema, l_slow) = (m.ema_liq.get(), m.liq_slow.get());
        let ring_l = m.ring_math().iter().map(|e| e.l).max().unwrap();
        let rm2 = rm.on_pool(&env.svm, cand);
        let fv_dev = u128::from(env.position(&rm, &b.key(), nonce).fv_dev_bps);
        crank_every(&mut env, &rm2, 1, 9);
        let l_warm = env.market(&rm.market()).ema_liq.get();
        let wide = wide_seconds(&mut env, &rm2, fv_dev, 30);
        let l_slow_30 = env.market(&rm.market()).liq_slow.get();
        println!(
            "M4-SEC-5 {variant:8}: L_ema {l_ema}, L_slow {l_slow}, ring l {ring_l} (D {d}); L_ema after the warm-up {l_warm}; box > {fv_dev} bps wide in L over the next 30 s: {wide}; L_slow after 30 s {l_slow_30}"
        );
        rows.push((variant, l_ema, l_slow, ring_l, wide, d, l_slow_30, l_warm));
        // Delay only: the position was neither liquidated nor settled meanwhile.
        close_ok(&mut env, &rm2, &b, nonce);
    }
    let honest = rows[0];
    let cap = shortwin_math::pool_select::depth_at_lambda(honest.5, 5_000);
    assert_eq!((honest.1, honest.3), (cap, cap), "honest: the anchor");
    assert_eq!(
        honest.2, honest.5,
        "L_slow the locked depth, not the sample"
    );
    // Every variant equals the honest run (to `λ`'s floor to whole bps inside a deposit-wrapped
    // instruction, which moves `D` by ≤ 1/10⁴).
    let near = |a: u128, b: u128| a.abs_diff(b) * 10_000 <= b;
    for r in &rows {
        assert!(
            near(r.1, honest.1) && near(r.3, honest.3) && near(r.2, honest.2),
            "{}: L_ema {} / ring {} / L_slow {} vs honest {} / {} / {}",
            r.0,
            r.1,
            r.3,
            r.2,
            honest.1,
            honest.3,
            honest.2
        );
        assert!(near(r.7, honest.7), "{}: the warm-up's seed", r.0);
        assert!(near(r.6, honest.6), "{}: L_slow after 30 s", r.0);
        assert_eq!(r.4, 0, "{}: the box is narrow from the gate on", r.0);
    }
}

/// **M4-SEC-6** (LOW; FIXED, then M4R). With `L_ema = min(L_new, current)` a fast path onto a pool
/// **deeper** than the market's `L_ema` kept the old liquidity, and the first honest observations
/// left the settlement box > 1 % wide in `L` for 18 of 30 s. Since M4R the adoption writes the
/// anchor and the warm-up seeds the new pool's depth: after the warm-up `L_ema` is the candidate's
/// depth (±0.1 %), deeper or thinner, the box is never wide over the 30 s from the gate on for a
/// candidate of 0.5× or 1.5× the incumbent's reserves (T-VENUE-SWITCH), and `L_slow` is never
/// raised.
#[test]
fn m4_sec_6_a_deeper_fast_path_candidate_keeps_the_box_narrow() {
    use crate::common::*;
    use crate::support::*;

    for (num, den) in [(50u64, 100u64), (150, 100)] {
        let (mut env, rm, b, nonce, _q) = helpers::with_position();
        halt(&mut env, &rm.pool);
        let lam = lambda_of(&env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (num, den), lam);
        crank_every(&mut env, &rm, 30, 5);
        window_ok(&mut env, &rm.pool, &cand);
        crank_every(&mut env, &rm, 30, 2);
        let before = env.market(&rm.market());
        execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
        let rm2 = rm.on_pool(&env.svm, cand);
        crank_every(&mut env, &rm2, 1, 9);
        let after = env.market(&rm.market());
        let depth = {
            let (x, y) = cand
                .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
                .unwrap();
            (u128::from(x) * u128::from(y)).isqrt()
        };
        let fv_dev = u128::from(env.position(&rm, &b.key(), nonce).fv_dev_bps);
        let wide = wide_seconds(&mut env, &rm2, fv_dev, 30);
        println!(
            "M4-SEC-6 candidate {num}/{den} of the incumbent: L_ema {} -> {} after the warm-up (√(X·Y) {depth}); L_slow {} -> {}; seconds with the box > {fv_dev} bps wide in L over the 30 s from the gate on: {wide}",
            before.ema_liq.get(),
            after.ema_liq.get(),
            before.liq_slow.get(),
            after.liq_slow.get()
        );
        assert_eq!(wide, 0, "{num}/{den}");
        assert!(
            after.ema_liq.get().abs_diff(depth) * 1_000 <= depth,
            "{num}/{den}: the candidate's depth"
        );
        assert!(
            after.liq_slow.get() <= before.liq_slow.get(),
            "L_slow never raised"
        );
        if num > den {
            assert!(
                after.ema_liq.get() > before.ema_liq.get(),
                "the deeper pool's depth"
            );
        }
        close_ok(&mut env, &rm2, &b, nonce);
    }
}

/// **M4-SEC-8** (LOW; FIXED). `propose_pool` / `execute_pool_switch` latched on the whole
/// Raydium mask, so a flagged upgrade of either program — the likely cause of the incumbent's
/// `pool_fail` — disabled the fast path in the incident it exists for. They now latch on the
/// candidate's own program only: with the AMM v4 program (Fartcoin's incumbent) flagged, an AMM
/// v4 candidate is refused (`VenueUpgraded`) while a CPMM candidate's window opens and is adopted,
/// moving the position off the halted pool; opens keep the whole mask (R4B-7).
#[test]
fn m4_sec_8_the_latch_holds_only_the_candidates_program() {
    use shortwin::constants::{GUARD_MASK_RAYDIUM, GUARD_RAYDIUM_V4};
    use shortwin::events::POOL_KIND_FAST;
    use shortwin_litesvm_tests::fork::{FIXTURE_GUARD_SLOTS, RaydiumProgram};
    use shortwin_litesvm_tests::ix;
    use shortwin_litesvm_tests::pool_select::pool_key;

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    let (mut env, rm, b, nonce, _q) = with_position();
    let lam = lambda_of(&env, &rm.pool);
    let v4 = plant(&mut env, &rm.pool, Plant::V4, true, (60, 100), lam);
    let cpmm = plant(&mut env, &rm.pool, Plant::Cpmm, true, (50, 100), lam);
    // An AMM v4 upgrade: flagged, and the incumbent's pools halt.
    let dumped = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_V4)];
    fork::set_deploy_slot(&mut env.svm, RaydiumProgram::AmmV4, dumped + 1);
    ok(env.send(&[ix::flag_venue_upgraded(GUARD_RAYDIUM_V4)], &[]));
    assert!(
        env.venue_guard().blocks(GUARD_MASK_RAYDIUM),
        "opens latched"
    );
    halt(&mut env, &rm.pool);
    crank_every(&mut env, &rm, 30, 5);
    assert_err(&env.propose(Some(&rm.pool), &v4), E::VenueUpgraded);
    window_ok(&mut env, &rm.pool, &cpmm);
    crank_every(&mut env, &rm, 30, 2);
    let ev = execute_ok(&mut env, &rm.pool, &cpmm).unwrap_or_else(|_| panic!("cancelled"));
    assert_eq!((ev.kind, ev.candidate), (POOL_KIND_FAST, pool_key(&cpmm)));
    println!("M4-SEC-8: adopted a CPMM pool with the AMM v4 upgrade still under review");
    let rm2 = rm.on_pool(&env.svm, cpmm);
    env.warp(1);
    env.crank(&rm2);
    close_ok(&mut env, &rm2, &b, nonce);
}
