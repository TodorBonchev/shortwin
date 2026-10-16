//! M4V regressions: the verification of the M4 security fixes (the skeptic's findings M4V-1 to
//! M4V-5) on the mainnet fork, each as the owner's delegated builder decided it (ARCH §2.6, §3.4,
//! §8.2 T-VENUE-SWITCH; owner item O18). Every test started as the skeptic's reproducer and now
//! asserts the decided behaviour; every instruction goes through `Env::send`, so the invariant
//! hooks (I15 with the window floor, the adoption anchor and the warm-up; I17; I18) run after
//! each one.
//!
//! | Test | Finding and what is asserted now |
//! |---|---|
//! | `m4v_1_*` | the listing floor admitted fully locked pools of 12.5–25 SOL, so the fast path moved an open position onto a 20- or 13-SOL pool with no manipulation. A window's candidate now needs its **locked SOL alone** at `P_ema` to reach `pool_min_real_sol` (the window floor): both are refused `PoolTooThin`, a 26-SOL one is adopted; a list-only market with no position still adopts the 20-SOL pool (the listing floor keeps its `/λ_min`) |
//! | `m4v_2_*` | a deposit around the recording proposal halved the recorded `λ` and doubled `L_ema` and the ring of a λ 99.78 % pool. Since M4R an adoption reads no sample: deposits around the proposal, the execution or both change nothing — `L_ema` is the anchor at the adoption and the pool's depth after the warm-up, the box narrow from the gate on; the underwater position is liquidated at adoption + 9 s with and without the pushes (`m4v_2b`); a deposit-wrapped crank every second (a capital freeze of `L_ema`) does not block the liquidation of the largest position the candidate takes (switch only if all fit, owner decision of 2026-09-29: 95 % of its per-position cap; as built the incumbent's size cap, ≈ 1.9 % of the candidate's `X`, was carried) — the reference guard keeps `B̂_ref` on the ring's lows (`m4v_2c`) |
//! | `m4v_3_*` | an honest fast path onto a candidate with `λ < λ_min` wrote `D/λ_min` (half its depth at λ 25 %) and left the box wide for about 20 s. Since M4R the warm-up seeds its depth: `L_ema = √(X·Y)` ±1 % at the gate and the box ≤ 1 % from then on, for λ 99.78 %, 50 % and 25 %; no exit lands during the warm-up (`EmaWarmingUp`, `sol_settle` a no-op); an attacker withdraw-wrapping cranks and liquidations every second on the λ 25 % pool liquidates nothing |
//! | `m4v_4_*` | a flag on the window candidate's program cancels the window instead of reverting; the chain stays armed, a proposal on the other program opens a new window at once and is adopted well before the S3 wait |
//! | `m4v_5_*` | a list-only market captured through `[buy, propose, sell]` starts its price state at the pushed spot, marked (M4R); the first open (plain or `[buy, open, sell]`) is refused `EmaWarmingUp` until the post-change warm-up is over (`ema_warm == 0`: 8 integrations after the adoption, which re-seed `P_ema` from the honest bucket lows), and then the pushed open is refused on `R_eff` / the spot-EMA check; the main pool then replaces the capture (no pin) |
//!
//! The keeper half of M4V-5 (observations of a zero-position Raydium market during and at the end
//! of its listing window) is `services/keeper-litesvm/tests/scenarios/pool_select.rs`
//! (`a_listing_window_is_observed_and_the_first_open_is_warm`).

#![allow(clippy::result_large_err)]

#[allow(unused_imports)]
#[path = "ray_it/common.rs"]
mod common;

#[path = "pool_select_it/support.rs"]
mod support;

/// The attacker's liquidity moves on a CPMM pool through the dumped program's own `deposit`
/// and `withdraw` (as `m4_sec_it.rs`).
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

    /// Tokens (SOL side, token side) of a provider's accounts.
    pub fn balances(env: &Env, p: &CpmmPool, who: &Provider) -> (u64, u64) {
        let (a0, a1) = (tokens(env, &who.t0), tokens(env, &who.t1));
        if p.sol_is_token0 { (a0, a1) } else { (a1, a0) }
    }

    /// The SOL and tokens a deposit of `lp` LP needs (rounded up, plus one).
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

    /// `(env, market, borrower, nonce, q)`: Fartcoin listed on its main pool (the harness warms
    /// its price state up at the end of the listing window), five seconds of cranks, one
    /// position of the default size, three more seconds (not halted).
    pub fn with_position() -> (Env, RayMarket, Borrower, u64, u64) {
        let (mut env, rm, q) = setup(FART);
        crank_every(&mut env, &rm, 1, 5);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        crank_every(&mut env, &rm, 1, 3);
        (env, rm, b, nonce, q)
    }

    /// Whether the settlement box of `market` is wider than `fv_dev` bps in `L` now.
    pub fn box_wide(env: &Env, market: &Pubkey, fv_dev: u128) -> bool {
        let o = env.market(market).observation();
        let bx = shortwin_math::settle::box_of(&o.ema, &o.ring, env.now(), true).unwrap();
        (bx.l_hi - bx.l_lo) * 10_000 > fv_dev * bx.l_lo
    }
}

/// Box width over the next `n` per-second cranks of `rm2`: the seconds whose settlement box is
/// wider than `fv_dev` bps in `L`.
fn wide_seconds(env: &mut common::Env, rm2: &common::RayMarket, fv_dev: u128, n: u32) -> u32 {
    use common::VenueMarket;
    let mut wide = 0;
    for _ in 0..n {
        env.warp(1);
        env.crank(rm2);
        if helpers::box_wide(env, &rm2.market(), fv_dev) {
            wide += 1;
        }
    }
    wide
}

/// Plants a pool of `base`'s price with `sol` of real SOL and locked share `lam`.
fn sized(
    env: &mut common::Env,
    kind: support::Plant,
    base: &common::AnyPool,
    lam: u64,
    sol: u64,
) -> common::AnyPool {
    use shortwin_litesvm_tests::pool_select::{PlantSpec, spec_like};
    let s = spec_like(&env.svm, base, true, lam);
    let spec = PlantSpec {
        sol,
        tokens: (u128::from(s.tokens) * u128::from(sol) / u128::from(s.sol)) as u64,
        ..s
    };
    support::plant_spec(env, kind, spec)
}

/// `√(X·Y)` of a pool's pricing reserves now.
fn root_xy(env: &common::Env, pool: &common::AnyPool) -> u128 {
    let (x, y) = pool
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .unwrap();
    (u128::from(x) * u128::from(y)).isqrt()
}

/// **M4V-1** (decided: option 1). The M4-SEC-1/-3 floor `⌊⌊D·isqrt(P_ema)/10⁹⌋·10⁴/λ_min⌋ ≥
/// pool_min_real_sol` admits a fully locked pool of only `λ_min·pool_min_real_sol` (12.5 SOL) of
/// real SOL, so the fast path moved an open position onto a 20-SOL or 13-SOL pool with no
/// manipulation at all — the harm M4-SEC-3 (b) named. A window's candidate now needs the **window
/// floor**, its locked SOL at `P_ema` alone `≥ pool_min_real_sol`: both are refused
/// `PoolTooThin` (while passing the listing floor), a fully locked 26-SOL pool opens the window
/// and is adopted, and the position closes on it. The listing floor keeps its `/λ_min` where no
/// position is carried: a list-only market with no position adopts the 20-SOL pool plainly (its
/// opens stay gated by the price warm-up and `R_eff`).
#[test]
fn m4v_1_the_fast_path_needs_the_sol_floor_in_locked_sol() {
    use shortwin::events::{POOL_KIND_FAST, POOL_KIND_LISTING};
    use shortwin_litesvm_tests::pool_select::{passes_floor, passes_window_floor, pool_key};
    use shortwin_math::fees::Direction;

    use crate::common::*;
    use crate::support::*;

    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let lam = lambda_of(&env, &base);
    let inc = sized(&mut env, Plant::V4, &base, lam, 40_000_000_000);
    ok(env.list_ray(&inc, VENUE_HINT_V4));
    let q = q_of(&env, &inc);
    let rm = env.ray_market_listed(RaySpec::new(FART, q.saturating_mul(40)), inc);
    env.crank(&rm);
    crank_every(&mut env, &rm, 1, 5);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    // Honest, fully locked pools (no trick, no LP game) under the SOL floor, and one above it.
    let c20 = sized(&mut env, Plant::Cpmm, &base, 10_000, 20_000_000_000);
    let c13 = sized(&mut env, Plant::Cpmm, &base, 10_000, 13_000_000_000);
    let c26 = sized(&mut env, Plant::Cpmm, &base, 10_000, 26_000_000_000);
    crank_every(&mut env, &rm, 30, 5);
    let floor = env.config().params.pool_min_real_sol;
    let p_ema = env.market(&rm.market()).ema_price.get();
    for (cand, sol) in [(&c20, 20), (&c13, 13)] {
        let (_, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
        assert!(y < floor, "candidate {y} under the floor {floor}");
        assert!(
            passes_floor(&env, cand, Some(p_ema)) && !passes_window_floor(&env, cand, Some(p_ema)),
            "{sol} SOL: listable, under the window floor"
        );
        assert_err(&env.propose(Some(&rm.pool), cand), E::PoolTooThin);
        println!("M4V-1: a fully locked {sol}-SOL fast-path candidate is refused PoolTooThin");
    }
    assert!(passes_window_floor(&env, &c26, Some(p_ema)));
    window_ok(&mut env, &rm.pool, &c26);
    crank_every(&mut env, &rm, 30, 2);
    let ev = execute_ok(&mut env, &rm.pool, &c26).unwrap_or_else(|_| panic!("cancelled"));
    assert_eq!((ev.kind, ev.candidate), (POOL_KIND_FAST, pool_key(&c26)));
    let rm2 = rm.on_pool(&env.svm, c26);
    env.warp(1);
    env.crank(&rm2);
    close_ok(&mut env, &rm2, &b, nonce);
    println!(
        "M4V-1: a fully locked 26-SOL candidate ({:.1} % of pool_d) is adopted and the position closes on it",
        ev.d_cand as f64 * 100.0 / ev.d_inc as f64
    );

    // Where no position is carried the listing floor keeps its `/λ_min`.
    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    list_auto(&mut env, &base);
    let c20 = sized(&mut env, Plant::Cpmm, &base, 10_000, 20_000_000_000);
    assert!(passes_floor(&env, &c20, None) && !passes_window_floor(&env, &c20, None));
    let e = adopt_ok(&mut env, None, &c20);
    assert_eq!((e.kind, e.candidate), (POOL_KIND_LISTING, pool_key(&c20)));
}

/// **M4V-2** (superseded by M4R: an adoption reads no sample of the pool). The M4-SEC-5 fix took
/// the adoption liquidity from the recording proposal alone, so a stranger holding no LP wrapped
/// that proposal in `[deposit ≈ the LP counter, propose_pool, withdraw]` and doubled `L_ema` and
/// the ring of a λ 99.78 % pool; M4V-2's `min(L_rec, L_live, D/λ_min)` still let both samples be
/// wrapped. Now every adoption writes the anchor `⌊D·10⁴/λ_min⌋` and the warm-up seeds the pool's
/// depth from its buckets' highs: a deposit around the proposal, the execution or both changes
/// nothing — `L_ema = cap` at the adoption in every variant, the pool's own depth once the warm-up
/// is over (±0.1 %), and the settlement box is narrow from the gate on; `L_slow ≤ D`.
#[test]
fn m4v_2_a_deposit_around_the_recording_proposal_alone_changes_nothing() {
    use shortwin_litesvm_tests::pool_select::{execute_pool_switch_ix, propose_pool_ix};
    use shortwin_litesvm_tests::raydium::candidate_read_set;

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    let mut rows = Vec::new();
    for variant in ["honest", "proposal", "execution", "both"] {
        let (mut env, rm, b, nonce, _q) = with_position();
        halt(&mut env, &rm.pool);
        let lam = lambda_of(&env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (60, 100), lam);
        let AnyPool::Cpmm(cp) = cand else {
            unreachable!()
        };
        crank_every(&mut env, &rm, 30, 5);
        let (mint, _) = rm.pool.token();
        // A fresh provider with the capital for one deposit of the whole LP counter.
        let add = cp.state(&env.svm).lp_supply;
        let (sol, tok) = lp::needed(&env, &cp, add);
        let who = lp::provider(&mut env, &cp, 2 * sol + 2_000, 2 * tok + 2_000, 0);
        let signer = who.owner.insecure_clone();
        if matches!(variant, "honest" | "execution") {
            window_ok(&mut env, &rm.pool, &cand);
        } else {
            let inc = candidate_read_set(&env.svm, &rm.pool, false);
            let ixs = [
                lp::deposit_ix(&cp, &who, add),
                propose_pool_ix(&env.svm, &mint, &inc, &cand),
                lp::withdraw_ix(&cp, &who, add),
            ];
            ok(env.send(&ixs, &[&signer]));
        }
        crank_every(&mut env, &rm, 30, 2);
        let l_real = root_xy(&env, &cand);
        let d = d_of(&env, &cand);
        let exec = execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &cand);
        if matches!(variant, "execution" | "both") {
            let add = cp.state(&env.svm).lp_supply;
            let ixs = [
                lp::deposit_ix(&cp, &who, add),
                exec,
                lp::withdraw_ix(&cp, &who, add),
            ];
            ok(env.send(&ixs, &[&signer]));
        } else {
            ok(env.send(&[exec], &[]));
        }
        let m = env.market(&rm.market());
        assert_eq!(m.pool_cache[0], cp.pool, "adopted");
        let (l_adopt, l_slow) = (m.ema_liq.get(), m.liq_slow.get());
        assert_eq!((m.ema_warm, m.ema_guard), (8, 1), "{variant}: marked");
        let rm2 = rm.on_pool(&env.svm, cand);
        let fv_dev = u128::from(env.position(&rm, &b.key(), nonce).fv_dev_bps);
        // The warm-up (8 integrations: 9 per-second cranks), then 40 s of the new pool.
        crank_every(&mut env, &rm2, 1, 9);
        let m = env.market(&rm.market());
        assert_eq!((m.ema_warm, m.ema_guard), (0, 0), "{variant}: confirmed");
        let l_ema = m.ema_liq.get();
        let wide = wide_seconds(&mut env, &rm2, fv_dev, 40);
        println!(
            "M4V-2 {variant:9}: √(X·Y) {l_real}; L_ema {l_adopt} at the adoption ({:.3}×), {l_ema} after the warm-up ({:.4}×), L_slow {l_slow} (D {d}); box > {fv_dev} bps wide in L for {wide} of the next 40 s",
            l_adopt as f64 / l_real as f64,
            l_ema as f64 / l_real as f64
        );
        rows.push((l_adopt, l_slow, wide, d, l_real, l_ema));
        close_ok(&mut env, &rm2, &b, nonce);
    }
    let h = rows[0];
    let cap = shortwin_math::pool_select::depth_at_lambda(h.3, 5_000);
    for (variant, r) in ["honest", "proposal", "execution", "both"]
        .iter()
        .zip(&rows)
    {
        // `D` read inside a deposit-wrapped instruction differs from the honest one only by
        // `λ`'s floor to whole bps; the anchor is read from the honest `D` at the adoption.
        assert!(
            r.0.abs_diff(cap) * 1_000 <= cap,
            "{variant}: the anchor at the adoption ({} vs {cap})",
            r.0
        );
        assert!(
            r.5.abs_diff(r.4) * 1_000 <= r.4,
            "{variant}: the pool's depth after the warm-up ({} vs {})",
            r.5,
            r.4
        );
        assert_eq!(r.2, 0, "{variant}: narrow box after the gate");
        assert!(
            r.1 <= r.3 + r.3 / 1_000 && r.1.abs_diff(h.1) * 1_000 <= h.1,
            "{variant}: L_slow ≤ D, never a sample"
        );
    }
}

/// **M4V-2 (b)**, since M4R: the same two-sided push (a deposit around the recording proposal
/// and the execution) in the M4-SEC-1 control scenario — the token 2.5× on the candidate during
/// the halt, the position underwater — changes nothing: the position is liquidated at adoption +
/// 9 s in both runs (the keeper cranking every second), after 8 warm-up integrations refused
/// `EmaWarmingUp`.
#[test]
fn m4v_2b_the_wrapped_record_only_delays_the_liquidation() {
    use shortwin_litesvm_tests::engine::liquidate_ix;
    use shortwin_litesvm_tests::pool_select::{execute_pool_switch_ix, propose_pool_ix};
    use shortwin_litesvm_tests::raydium::candidate_read_set;

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    let mut out = Vec::new();
    for attack in [false, true] {
        let (mut env, rm, b, nonce, _q) = with_position();
        let lam = lambda_of(&env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (50, 100), lam);
        let AnyPool::Cpmm(cp) = cand else {
            unreachable!()
        };
        halt(&mut env, &rm.pool);
        let c0 = helpers::spot(&env, &cand);
        pump_to(&mut env, &cand, c0, 25_000);
        let keeper = env.funded_keypair();
        let route_s = i64::from(env.config().params.pool_fast_route_s);
        let win_s = i64::from(env.config().params.pool_fast_window_s);
        let (mint, _) = rm.pool.token();
        let add = cp.state(&env.svm).lp_supply;
        let (sol, tok) = lp::needed(&env, &cp, add);
        let who = lp::provider(&mut env, &cp, 2 * sol + 2_000, 2 * tok + 2_000, 0);
        let signer = who.owner.insecure_clone();
        let mut cur = rm.on_pool(&env.svm, rm.pool);
        let (mut adopted_at, mut liquidated_at) = (None, None);
        let mut codes = std::collections::BTreeSet::new();
        for s in 1..=400i64 {
            env.warp(1);
            env.crank(&cur);
            if adopted_at.is_none() {
                let m = env.market(&rm.market());
                let armed = m.pool_fail_since > 0 && env.now() - m.pool_fail_since >= route_s;
                if m.fp_t0 == 0 && armed {
                    if attack {
                        let add = cp.state(&env.svm).lp_supply;
                        let inc = candidate_read_set(&env.svm, &rm.pool, false);
                        let ixs = [
                            lp::deposit_ix(&cp, &who, add),
                            propose_pool_ix(&env.svm, &mint, &inc, &cand),
                            lp::withdraw_ix(&cp, &who, add),
                        ];
                        ok(env.send(&ixs, &[&signer]));
                    } else {
                        window_ok(&mut env, &rm.pool, &cand);
                    }
                } else if m.fp_t0 != 0 && env.now() >= m.fp_t0 + win_s {
                    let exec = execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &cand);
                    if attack {
                        let add = cp.state(&env.svm).lp_supply;
                        let ixs = [
                            lp::deposit_ix(&cp, &who, add),
                            exec,
                            lp::withdraw_ix(&cp, &who, add),
                        ];
                        ok(env.send(&ixs, &[&signer]));
                    } else {
                        ok(env.send(&[exec], &[]));
                    }
                    assert_eq!(env.market(&rm.market()).pool_cache[0], cp.pool, "adopted");
                    adopted_at = Some(s);
                    cur = rm.on_pool(&env.svm, cand);
                }
            }
            if liquidated_at.is_none() && adopted_at.is_some() {
                let warm = env.market(&rm.market()).ema_warm;
                let res = env.send(
                    &[liquidate_ix(&cur, &keeper.pubkey(), &b.key(), nonce)],
                    &[&keeper],
                );
                if custom_code(&res).is_none() {
                    ok(res);
                    liquidated_at = Some(s);
                    break;
                }
                if warm > 0 {
                    codes.insert(custom_code(&res));
                }
            }
        }
        println!(
            "M4V-2b {}: adopted at {adopted_at:?} s, liquidated at {liquidated_at:?} s; refusals during the warm-up {codes:?}",
            if attack { "both wrapped" } else { "honest" }
        );
        assert_eq!(codes, [Some(code(E::EmaWarmingUp))].into());
        out.push((
            adopted_at.expect("adopted"),
            liquidated_at.expect("liquidated"),
        ));
    }
    for (a, l) in out {
        assert_eq!(l - a, 9, "liquidation delay after the adoption");
    }
}

/// A planted, fully locked AMM v4 incumbent of 100 SOL at Fartcoin's price with one position of
/// the largest size that opens (up to 0.74 % of its reserve) **and fits the candidate** `cand`
/// (switch only if all fit: at most 95 % of its per-position cap; `cand` is planted and moved by
/// the caller's `plant` before the open), then halted.
fn small_market_with_big_position(
    plant_cand: impl FnOnce(&mut common::Env, &common::AnyPool) -> common::AnyPool,
) -> (
    common::Env,
    common::RayMarket,
    common::Borrower,
    u64,
    common::AnyPool,
) {
    use crate::common::*;
    use crate::support::*;

    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let lam = lambda_of(&env, &base);
    let inc = sized(&mut env, Plant::V4, &base, lam, 100_000_000_000);
    ok(env.list_ray(&inc, VENUE_HINT_V4));
    let q0 = q_of(&env, &inc);
    let rm = env.ray_market_listed(RaySpec::new(FART, q0.saturating_mul(60)), inc);
    env.crank(&rm);
    crank_every(&mut env, &rm, 1, 5);
    let (x_inc, _) = inc
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .unwrap();
    let cand = plant_cand(&mut env, &base);
    let cap = shortwin_litesvm_tests::pool_select::carried_caps(&env, &cand).0 / 100 * 95;
    let mut b = env.borrower();
    let mut opened = None;
    for bps in [74u64, 72, 70, 65, 60, 50, 40, 25] {
        let q = ((u128::from(x_inc) * u128::from(bps) / 10_000) as u64).min(cap);
        let (res, nonce) = env.open(&rm, &mut b, q);
        if res.is_ok() {
            opened = Some(nonce);
            break;
        }
    }
    let nonce = opened.expect("an open");
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    (env, rm, b, nonce, cand)
}

/// **M4V-2 (c)** (M4R: the capital freeze and the reference guard, O18 residual 2). The largest
/// position a fully locked 30-SOL candidate on which the token doubled during the halt takes
/// (switch only if all fit: 95 % of its per-position cap, ≈ 0.5 % of its token reserve after the
/// pump; as built the largest the 100-SOL incumbent's caps allowed, ≈ 1.9 %, was carried) moves
/// onto it; from the adoption on, an attacker with capital wraps a crank in
/// `[deposit as much LP as the pool holds, crank, withdraw]` every second (the keeper cranks every
/// second too). The warm-up seeds `L_ema` from the pushed highs (≈ 2× the pool's depth) and the
/// guard stays set (the lows are honest); the position is liquidated nevertheless — no S1 — and
/// its `Liquidate.b_ref` is the risk quote at `L_b = max(L_live, min(L_ema, L_mlo), L_slow)`, not at
/// the pushed `L_ema` (which, without the guard, would have made the reference cheaper).
/// The candidate's price during the halt, bps of the pre-halt spot (M4V-2c).
const PUMP_BPS: u128 = 20_000;

#[test]
fn m4v_2c_a_deposit_freeze_does_not_block_the_liquidation() {
    use shortwin::events::{Liquidate, SolSettle};
    use shortwin_litesvm_tests::engine::{liquidate_ix, sol_settle_ix};
    use shortwin_math::observe::ring_medians;
    use shortwin_math::quote::liquidity;

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    let (mut env, rm, b, nonce, cand) = small_market_with_big_position(|env, base| {
        let cand = sized(env, Plant::Cpmm, base, 10_000, 30_000_000_000);
        let c0 = helpers::spot(env, &cand);
        pump_to(env, &cand, c0, PUMP_BPS);
        cand
    });
    let AnyPool::Cpmm(cp) = cand else {
        unreachable!()
    };
    let (x_c, _) = cand
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .unwrap();
    let pos0 = env.position(&rm, &b.key(), nonce);
    let q = pos0.q;
    let pricing = shortwin_litesvm_tests::invariants::venue_view::view(
        &env.svm,
        &shortwin_litesvm_tests::invariants::pool_select::market_on(
            &env.svm,
            &env.market(&rm.market()),
            VenueKind::RaydiumCpmm,
            &cp.pool,
        )
        .expect("the candidate as the market's pool"),
    )
    .and_then(|v| v.pricing)
    .expect("the candidate's fees");
    println!(
        "M4V-2c: q = {:.2} % of the candidate's token reserve (after the pump)",
        q as f64 * 100.0 / x_c as f64
    );
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    let rm2 = rm.on_pool(&env.svm, cand);
    let keeper = env.funded_keypair();
    let add = 4 * cp.state(&env.svm).lp_supply;
    let (sol, tok) = lp::needed(&env, &cp, add);
    let who = lp::provider(&mut env, &cp, 3 * sol, 3 * tok, 0);
    let signer = who.owner.insecure_clone();
    let mut liquidated = None;
    for s in 1..=120i64 {
        env.warp(1);
        env.crank(&rm2);
        let add = cp.state(&env.svm).lp_supply;
        ok(env.send(
            &[
                lp::deposit_ix(&cp, &who, add),
                crank_price_ix(&rm2),
                lp::withdraw_ix(&cp, &who, add),
            ],
            &[&signer],
        ));
        let m = env.market(&rm.market());
        if s >= 9 {
            assert_eq!(m.ema_guard, 1, "second {s}: the freeze keeps the guard set");
        }
        // The pre-trade reserves the liquidation's reference reads.
        let (x, y) = cand
            .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
            .unwrap();
        let res = env.send(
            &[liquidate_ix(&rm2, &keeper.pubkey(), &b.key(), nonce)],
            &[&keeper],
        );
        if let Ok(meta) = res {
            let e: Liquidate = events::one(&meta);
            // The liquidation's own roll moved nothing (same second as the cranks).
            let obs = env.market(&rm.market()).observation();
            let l_live = liquidity(u128::from(x), u128::from(y)).unwrap();
            let l_mlo = ring_medians(&obs.ring).unwrap().2;
            println!(
                "M4V-2c: liquidated at +{s} s: L_ema {} ({:.3}× L_mlo {l_mlo}), b_ref {}, p_ref {}",
                obs.ema.liq,
                obs.ema.liq as f64 / l_mlo as f64,
                e.b_ref,
                e.p_ref
            );
            assert_eq!(obs.ema.guard, 1, "the guard is set at the liquidation");
            assert!(
                obs.ema.liq > l_mlo,
                "the freeze pushed L_ema above the lows"
            );
            // `B̂_ref` at the guarded `L_b` (the lows), above the one at the pushed `L_ema`.
            let pos = &pos0;
            let quote_at = |l_b: u128| {
                pricing
                    .capped(u64::from(pos.venue_fee_hard_cap_bps))
                    .buy_repriced(pos.q, l_b, e.p_ref)
                    .map(|b| b.cost)
            };
            // M4F-1: the EMA terms capped at the pool's locked depth D on Raydium (AIR-14;
            // ⌊D·10⁴/λ_min⌋ before).
            let m_now = env.market(&rm.market());
            let cap = shortwin_math::bounds::ref_depth_cap(m_now.pool_d.get(), 10_000);
            let guarded = l_live.max(obs.ema.liq.min(l_mlo).max(obs.ema.liq_slow).min(cap));
            let pushed = l_live.max(obs.ema.liq).max(obs.ema.liq_slow);
            assert_eq!(
                quote_at(guarded),
                Some(e.b_ref),
                "b_ref at min(L_ema, L_mlo)"
            );
            assert!(
                quote_at(pushed).is_some_and(|b| b < e.b_ref),
                "the pushed L_ema would have made the reference cheaper"
            );
            liquidated = Some(s);
            break;
        }
        let settle = sol_settle_ix(&env, &rm2, &keeper.pubkey(), &b.key(), nonce);
        if let Ok(meta) = env.send(&[settle], &[&keeper]) {
            assert!(events::of::<SolSettle>(&meta).is_empty(), "no settlement");
        }
    }
    // The hooks (I18 d) re-derived `b_ref` at the guarded `L_b` for the landing liquidation.
    assert!(
        liquidated.is_some(),
        "the freeze did not block the liquidation"
    );
}

/// **M4V-3** (resolved by M4R). Since M4-SEC-1 a candidate with `λ < λ_min` is eligible, and the
/// adoption liquidity was capped at `D/λ_min < √(X·Y) = D/λ`, leaving the box wide for about 20 s
/// after an honest fast path onto such a pool. Now the warm-up seeds the pool's own depth from its
/// buckets' highs: for λ 99.78 %, 50 % and 25 % (the deepest, 240 % of Fartcoin's reserves) `L_ema`
/// is `√(X·Y)` ±1 % at the gate and the box is ≤ 1 % wide from then on; during the warm-up the
/// keeper's liquidations are refused `EmaWarmingUp` and `sol_settle` settles nothing. With the
/// M4R-1 attacker on the λ 25 % pool (every circulating LP token: withdraw-wrapped cranks and
/// liquidations every second) nothing is liquidated either; the position closes normally.
#[test]
fn m4v_3_a_low_lambda_candidate_is_a_bounded_delay() {
    use shortwin::events::SolSettle;
    use shortwin_litesvm_tests::engine::{liquidate_ix, sol_settle_ix};

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    for (lam_c, num, attacker) in [
        (9_978u64, 60u64, false),
        (5_000, 120, false),
        (2_500, 240, false),
        (2_500, 240, true),
    ] {
        let (mut env, rm, b, nonce, _q) = with_position();
        halt(&mut env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (num, 100), lam_c);
        let AnyPool::Cpmm(cp) = cand else {
            unreachable!()
        };
        let who = attacker.then(|| {
            let supply = raydium::mint_supply(&env.svm, &cp.lp_mint);
            lp::provider(&mut env, &cp, 1_000, 1_000, supply)
        });
        crank_every(&mut env, &rm, 30, 5);
        window_ok(&mut env, &rm.pool, &cand);
        crank_every(&mut env, &rm, 30, 2);
        execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
        let rm2 = rm.on_pool(&env.svm, cand);
        let fv_dev = u128::from(env.position(&rm, &b.key(), nonce).fv_dev_bps);
        let keeper = env.funded_keypair();
        let (mut wide_after, mut l_gate) = (0u32, None);
        let mut codes = std::collections::BTreeSet::new();
        for s in 1..=30i64 {
            env.warp(1);
            env.crank(&rm2);
            if let Some(w) = &who {
                let signer = w.owner.insecure_clone();
                let held = tokens(&env, &w.lp);
                ok(env.send(
                    &[
                        lp::withdraw_ix(&cp, w, held),
                        crank_price_ix(&rm2),
                        lp::deposit_ix(&cp, w, held - 4),
                    ],
                    &[&signer],
                ));
                let held = tokens(&env, &w.lp);
                let res = env.send(
                    &[
                        lp::withdraw_ix(&cp, w, held),
                        liquidate_ix(&rm2, &w.key(), &b.key(), nonce),
                        lp::deposit_ix(&cp, w, held - 4),
                    ],
                    &[&signer],
                );
                assert!(res.is_err(), "λ {lam_c}: the attacker liquidated at +{s} s");
            }
            let m = env.market(&rm.market());
            if m.ema_warm == 0 {
                l_gate.get_or_insert(m.ema_liq.get());
                wide_after += u32::from(box_wide(&env, &rm2.market(), fv_dev));
            }
            // The keeper's exits: none lands (refused during the warm-up, not liquidatable after).
            let warm = m.ema_warm;
            let liq = env.send(
                &[liquidate_ix(&rm2, &keeper.pubkey(), &b.key(), nonce)],
                &[&keeper],
            );
            let liq_code = custom_code(&liq);
            assert!(
                liq_code.is_some(),
                "λ {lam_c}: a liquidation landed at +{s} s"
            );
            if warm > 0 {
                codes.insert(liq_code);
            }
            let settle = sol_settle_ix(&env, &rm2, &keeper.pubkey(), &b.key(), nonce);
            let res = env.send(&[settle], &[&keeper]);
            if let Ok(meta) = &res {
                assert!(
                    events::of::<SolSettle>(meta).is_empty(),
                    "λ {lam_c}: a settlement landed at +{s} s"
                );
            }
        }
        let l_real = root_xy(&env, &cand);
        let l_gate = l_gate.expect("the gate opened");
        println!(
            "M4V-3 λ {lam_c}{}: √(X·Y) {l_real}, L_ema at the gate {l_gate} ({:.4}×); box > {fv_dev} bps wide in L for {wide_after} s after the gate; warm-up refusals {codes:?}",
            if attacker { " (attacker)" } else { "" },
            l_gate as f64 / l_real as f64
        );
        assert_eq!(codes, [Some(code(E::EmaWarmingUp))].into());
        if !attacker {
            assert!(
                l_real.abs_diff(l_gate) * 100 <= l_real,
                "λ {lam_c}: its depth at the gate"
            );
            assert_eq!(wide_after, 0, "λ {lam_c}: narrow box from the gate on");
        }
        close_ok(&mut env, &rm2, &b, nonce);
    }
}

/// **M4V-4** (decided: a latched candidate program is "not adoptable now"). After M4-SEC-8 an
/// execution latched on the window candidate's program and **reverted**, so a CPMM upgrade
/// flagged during a window whose candidate was a CPMM pool froze the fast path — every other
/// proposal, on the unflagged AMM v4 program too, got `PoolWindowClosed` — until the Emergency
/// revalidated, while the halted incumbent's position ran towards S3. The execution now cancels
/// (`PoolSwitchCancelled`), the `pool_fail` chain stays armed, the AMM v4 candidate opens a new
/// window at once and is adopted, well before the S3 wait; the position closes on it.
#[test]
fn m4v_4_a_latched_window_candidate_cancels_and_the_fast_path_goes_on() {
    use shortwin::constants::GUARD_RAYDIUM_CPMM;
    use shortwin::events::POOL_KIND_FAST;
    use shortwin_litesvm_tests::fork::{FIXTURE_GUARD_SLOTS, RaydiumProgram};
    use shortwin_litesvm_tests::ix;
    use shortwin_litesvm_tests::pool_select::pool_key;

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    let (mut env, rm, b, nonce, _q) = with_position();
    let lam = lambda_of(&env, &rm.pool);
    let v4 = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
    let cpmm = plant(&mut env, &rm.pool, Plant::Cpmm, true, (60, 100), lam);
    halt(&mut env, &rm.pool);
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cpmm);
    // A CPMM program upgrade lands during the window and is flagged (permissionless).
    let dumped = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_CPMM)];
    fork::set_deploy_slot(&mut env.svm, RaydiumProgram::Cpmm, dumped + 1);
    ok(env.send(&[ix::flag_venue_upgraded(GUARD_RAYDIUM_CPMM)], &[]));
    let s3_wait = i64::from(env.config().params.s3_wait_s);
    let t_fail = env.market(&rm.market()).pool_fail_since;
    crank_every(&mut env, &rm, 30, 2);
    let c = execute_ok(&mut env, &rm.pool, &cpmm)
        .map(|_| ())
        .expect_err("a latched window candidate cancels");
    assert_eq!((c.kind, c.candidate), (POOL_KIND_FAST, pool_key(&cpmm)));
    let m = env.market(&rm.market());
    assert_eq!((m.fp_t0, m.fp_d.get()), (0, 0), "the window is cleared");
    assert_eq!(m.pool_fail_since, t_fail, "the chain stays armed");
    assert_err(&env.propose(Some(&rm.pool), &cpmm), E::VenueUpgraded);
    window_ok(&mut env, &rm.pool, &v4);
    crank_every(&mut env, &rm, 30, 2);
    let ev = execute_ok(&mut env, &rm.pool, &v4).unwrap_or_else(|_| panic!("cancelled"));
    assert_eq!((ev.kind, ev.candidate), (POOL_KIND_FAST, pool_key(&v4)));
    let at = env.now() - t_fail;
    assert!(
        at < s3_wait,
        "adopted after {at} s of pool_fail (S3 wait {s3_wait} s)"
    );
    println!(
        "M4V-4: the latched CPMM window cancelled; the AMM v4 candidate adopted after {at} s of pool_fail (S3 wait {s3_wait} s)"
    );
    let rm2 = rm.on_pool(&env.svm, v4);
    env.warp(1);
    env.crank(&rm2);
    close_ok(&mut env, &rm2, &b, nonce);
}

/// **M4V-5** (decided: the price warm-up, program half). The fix bounded the list-only residual
/// by "the first open is refused PoolTooThin", which needs an honest observation after the
/// capture, and the reference keeper observed only markets with open positions: `[buy 15.3 SOL,
/// propose_pool, sell]` adopted a 10-SOL pool and started `P_ema` at the pushed spot (6.4×), an
/// hour later a `[buy, open_short, sell]` passed `R_eff` and the spot/EMA check, and the open
/// position pinned the market (a pool 3,816× deeper refused `PositionsOpen`). Now:
///
/// - with no observation after the capture, a plain open and every `[buy, open, sell]` size are
///   refused `EmaWarmingUp` (the marked price state has integrated nothing since its start);
/// - once honest observations exist (the keeper's listing-window cranks: the post-change warm-up
///   of `EMA_WARMUP_BUCKETS` integrations re-seeds `P_ema` from the honest bucket lows, M4R — since
///   M6-SEC-1 (the owner's decision of 2026-09-30) falling at most one v0.4.1 step per
///   integration, so at the gate the pushed start still lies ≈ 2.7× above the pool, the safe side,
///   and the steady EMA meets the pool within a minute), the pushed open is refused on `R_eff` or
///   the spot/EMA check, and a plain open `PoolTooThin`;
/// - nothing was opened, so Fartcoin's main pool replaces the capture (zero-position rule).
#[test]
fn m4v_5_a_captured_list_only_market_opens_nothing_on_its_pushed_price() {
    use shortwin::events::POOL_KIND_ZERO_POSITION;
    use shortwin::venues::ExtrasSel;
    use shortwin_litesvm_tests::engine::open_short_ix;
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_litesvm_tests::pool_select::{detection_set, fixture_of, propose_pool_ix};
    use shortwin_math::fees::Direction;

    use crate::common::*;
    use crate::helpers::*;
    use crate::support::*;

    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    list_auto(&mut env, &base);
    let lam = lambda_of(&env, &base);
    let thin = sized(&mut env, Plant::Cpmm, &base, lam, 10_000_000_000);
    let p_true = spot(&env, &thin);
    assert_err(&env.propose(None, &thin), E::PoolTooThin);
    let (mint, program) = thin.token();
    let push = 15_300_000_000u64;
    let t = fork::trader(&mut env, &mint, program, 3 * push, 0);
    let owner = t.owner.insecure_clone();
    let wrap = |env: &Env, mid: Instruction| -> Vec<Instruction> {
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
        vec![buy, mid, sell]
    };
    let prop = propose_pool_ix(&env.svm, &mint, &detection_set(&mint), &thin);
    let ixs = wrap(&env, prop);
    ok(env.send(&ixs, &[&owner]));
    let q = q_of(&env, &thin);
    // Lend, then an hour with no crank at all (the listing window runs out unobserved).
    let spec = RaySpec {
        fixture: fixture_of(&thin),
        lend: q.saturating_mul(40),
        warmup: 0,
    };
    let rm = env.ray_market_listed(spec, thin);
    env.warp(i64::from(env.config().params.pool_list_window_s) + 1);
    let m = env.market(&rm.market());
    println!(
        "M4V-5: captured; P_ema/true spot {:.2}; {} s after the adoption, ema_warm {}",
        m.ema_price.get() as f64 / p_true as f64,
        env.now() - m.pool_seen_at,
        m.ema_warm
    );
    assert_eq!(
        m.ema_warm,
        shortwin_math::consts::EMA_WARMUP_BUCKETS,
        "marked"
    );
    let sizes = [15u64, 13, 11, 9, 7, 5];
    let mut b = env.borrower();
    let bk = b.kp.insecure_clone();
    let (plain, _) = env.open(&rm, &mut b, q);
    assert_err(&plain, E::EmaWarmingUp);
    for div in sizes {
        let open = open_short_ix(
            &rm,
            &b.key(),
            0,
            q * div / 16,
            0,
            u64::MAX,
            u64::MAX,
            ExtrasSel::NONE,
            &[],
        );
        let ixs = wrap(&env, open);
        assert_err(&env.send(&ixs, &[&owner, &bk]), E::EmaWarmingUp);
    }
    println!("M4V-5: before any observation every open is refused EmaWarmingUp");
    // Honest observations (what the keeper sends at the end of a listing window).
    env.warm_up_price(&rm);
    let m = env.market(&rm.market());
    println!(
        "M4V-5: after {} honest cranks P_ema/true spot {:.2} (ema_warm {})",
        raydium::PRICE_WARMUP_CRANKS,
        m.ema_price.get() as f64 / p_true as f64,
        m.ema_warm
    );
    assert_eq!(m.ema_warm, 0, "confirmed");
    let p_now = spot(&env, &thin);
    // M6-SEC-1: the seed fell by at most one v0.4.1 step per integration (×(6/7)·(11/12)⁷ from the
    // pushed start over the warm-up), so it still lies above the honest pool at the gate.
    assert!(
        m.ema_price.get() > p_now,
        "P_ema above the pool at the gate ({} vs spot {p_now})",
        m.ema_price.get()
    );
    for pass in 0..2 {
        if pass == 1 {
            // A minute of honest steady integrations: P_ema meets the pool.
            for _ in 0..60 {
                env.warp(1);
                env.crank(&rm);
            }
            let m = env.market(&rm.market());
            let p_now = spot(&env, &thin);
            println!(
                "M4V-5: a minute later P_ema/true spot {:.4}",
                m.ema_price.get() as f64 / p_now as f64
            );
            assert!(
                m.ema_price.get().abs_diff(p_now) * 1_000 <= p_now,
                "P_ema at the honest pool ({} vs spot {p_now})",
                m.ema_price.get()
            );
        }
        pushed_opens_refused(&mut env, &rm, &b, &bk, &owner, q, &sizes, &wrap);
    }
    let (plain, _) = env.open(&rm, &mut b, q);
    assert_err(&plain, E::PoolTooThin);
    env.warp(1);
    let e = adopt_ok(&mut env, Some(&thin), &base);
    assert_eq!(e.kind, POOL_KIND_ZERO_POSITION);
    println!(
        "M4V-5: after honest observations the pushed open is refused on R_eff / the spot-EMA check; the main pool replaces the capture"
    );
}

/// M4V-5's pushed opens `[buy, open q·div/16, sell]`, each refused `PoolTooThin` or
/// `PriceDeviatesFromEma`.
#[allow(clippy::too_many_arguments)]
fn pushed_opens_refused(
    env: &mut common::Env,
    rm: &common::RayMarket,
    b: &common::Borrower,
    bk: &solana_keypair::Keypair,
    owner: &solana_keypair::Keypair,
    q: u64,
    sizes: &[u64],
    wrap: &dyn Fn(&common::Env, common::Instruction) -> Vec<common::Instruction>,
) {
    use shortwin::venues::ExtrasSel;
    use shortwin_litesvm_tests::engine::open_short_ix;

    use crate::common::*;
    for &div in sizes {
        let open = open_short_ix(
            &rm,
            &b.key(),
            0,
            q * div / 16,
            0,
            u64::MAX,
            u64::MAX,
            ExtrasSel::NONE,
            &[],
        );
        let ixs = wrap(env, open);
        let r = env.send(&ixs, &[owner, bk]);
        let c = custom_code(&r);
        assert!(
            c == Some(code(E::PoolTooThin)) || c == Some(code(E::PriceDeviatesFromEma)),
            "[buy, open {div}/16 q, sell] after honest observations: {c:?}"
        );
    }
}
