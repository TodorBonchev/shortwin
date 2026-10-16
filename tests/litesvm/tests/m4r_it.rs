//! M4R regressions: the post-change confirmation of the observation state (owner item O18,
//! **PROPOSED**; ARCH §2.6 rule 4 as amended, §3.6 precondition 1b, §4.4; ECON §5.2, §12.12) on
//! the mainnet fork. Every instruction goes through `Env::send`, so the invariant hooks (I15 with
//! the anchor, the proposal window floor and "switch only if all fit", I18 with the countdown,
//! the seed, the gate and the guard) run after each one.
//!
//! **Switch only if all fit** (owner decision of 2026-09-29): a collection window adopts only a
//! candidate whose opening caps every carried position fits, so every scenario that moves a
//! position through a window sizes it to fit the planned candidate (planted before the open; at
//! most 90–95 % of its per-position cap, `pool_select::carried_caps`). As built at M4R they
//! carried the largest position the incumbent's caps allowed (1.3–6 % of the candidate's `X`);
//! the M4R-1 thresholds (healthy at 1.97× / 1.975×, liquidated at 2.05×) and the M4X-1 ones hold
//! at the fitting size too.
//!
//! | Test | Finding and what is asserted now |
//! |---|---|
//! | `m4r1_a_withdraw_wrapped_switch_forces_no_liquidation` | M4R-1: an unlocked LP of a λ 50 % candidate wrapped `execute_pool_switch` in `[withdraw all, execute, deposit]`, the adoption sampled `L = D`, and a healthy position (candidate at 1.97× / 1.975× of the pre-halt spot) was liquidated 24–26 s later; unwrapped, never in 90 s. Now, with the attacker's withdraw-wrapped cranks and liquidations every second and a keeper cranking every second: no liquidation in 90 s in any run; every attempt before the gate fails `EmaWarmingUp`; at +9 s `L_ema` is the pool's `√(X·Y)` ±1 % |
//! | `m4r1_a_liquidatable_position_is_liquidated_after_the_warm_up` | the same position at 2.05×: liquidated at +9 or +10 s, wrapped or not |
//! | `m4r1_withdraw_wraps_during_the_warm_up_are_never_read` | an attacker withdraw-wrapped crank in every warm-up second, keeper present: the seed is the honest `√(X·Y)` ±1 %, `P_ema` the pool's spot; the guard stays set while the lows are pushed |
//! | `m4r1_an_attacker_only_warm_up_is_the_disclosed_degraded_mode` | no keeper for the 8 warm-up seconds: the attacker's withdraw-wrapped cranks seed `L = D` (disclosed, O18 residual 1); nothing lands before the gate; one keeper second among the eight (7 attacker-only + 1) gives the honest seed ±1 % |
//! | `m4r2_a_dump_during_the_halt_keeps_a_thin_pool_out` | M4R-2: after a 4× dump on the candidate during the halt, its locked SOL is ≈ 30 at the stale `P_ema` and ≈ 15 live; the keeper's proposal is refused `PoolTooThin` (window floor at `min(P_ema, P_cand)`), no window opens; the disclosed residual: a pump-wrapped opening proposal opens one and the execution adopts at `P_ema` (never looser than as built) |
//! | `t_switch_confirm_*` (★T-SWITCH-CONFIRM) | a genuine 2.2× pump during the halt: liquidated at adoption + 9 s; no observer for 600 s after the adoption, then the keeper: liquidated 8–9 s after it returns |
//! | `m4r_explore_liquidation_grid` (ignored) | the M4R-1 exploration grid with its knobs `M4R_GRID`, `M4R_SECS`, `M4R_SOL` |
//! | `m4x1_attacker_only_price_bundles_reach_no_further_than_steady_state` | M4X-1: the warm-up price seed read the lowest post-change bucket low with no step bound, so 8 attacker-only `[buy, crank_price, sell]` bundles seeded `P_ema` at the pump and a `[buy, liquidate, sell]` at the gate liquidated a position at any threshold (margin +50 %: threshold ≈ 3×) while the pool traded flat at 1.00× (+9 s, CPMM and AMM v4 candidates). Now every warm-up step rises by at most the largest v0.4.1 step (`observe::price_up_bound`, checked here step by step: ≈ 2.78× at the 6th integration, 3.24× at the 7th) and the same run stays open for 20 s; the near-threshold run (2.5× bundles, threshold ≈ 2.05×) is steady state's degraded-mode class (O18 residual 1; M4 as built reached it at +7 s): it is still liquidated at the gate, the seed within the bound |
//! | `m4x2_a_held_deposit_freeze_no_longer_lifts_l_slow` | M4X-2: under the capital deposit freeze (`[deposit 7·LP, crank_price, withdraw]` every second from the adoption) `L_slow` integrated the frozen `L_ema` while the guard was set, reached 2.62× the pool's depth after 900 s, and the unguarded `L_slow` term of `L_b` turned a genuine 4× pump into S1 (lenders −10.056 SOL, borrower 0) instead of a liquidation. Now `L_slow` integrates `min(lobs, L_mlo)` while the guard is set: ≤ 1.0× at the ramp, and the position — the largest the candidate takes (switch only if all fit: q/x ≈ 0.71 %, the size cap's 3.1 % no longer carried) — liquidated with the lenders' `q` bought back; an `L_slow` lifted above the lows is an I18 violation |
//! | `m4x3_a_dump_inside_the_window_is_adopted_at_p_ema_then_re_seeded` | M4X-3 (a documentation finding): a genuine 4× dump 5 s into the collection window, after the proposal that opened it, is judged only by the execution at `P_ema` and adopted (the docs had claimed any honest dump is refused; disclosed with O18 residual 4); the warm-up then lowers `P_ema` towards the live price — since M6-SEC-1 by at most one v0.4.1 step per integration, ≈ 1.7× the pool at the gate, on it a minute later —, an open meanwhile waits out the adopted pool's listing window (`PoolListWindow`), and after it a new open is refused `PoolTooThin` |
//! | `m4f1_a_deposit_freeze_on_a_fitting_carried_position_only_delays_the_liquidation` | M4F-1 (keeper), restated by "switch only if all fit" (owner decision of 2026-09-29): as found, attacker-only `[deposit 7·LP, crank_price, withdraw]` seconds in the keeper's idle cadence cleared the guard, froze `L_ema` at 8× the depth and turned the genuine 4× pump into S1 (lenders −10.056 SOL) on a position of ≈ 3.1 % of the new pool's `X`, which is no longer carried. A position that fits the 320-SOL candidate (95 % of its per-position cap, q/x ≈ 0.71 %) is liquidated in every run — the freeze from the adoption with the shipped keeper (`KeeperModel`: per second while the guard is set; per-slot bursts on prices only, the M4F-1 liquidity bursts reverted; per-second coverage for 10 s after a foreign liquidity on a market with positions, FITS-2 (b)), from +40 s (no burst; the foreign liquidity keeps the keeper per second from the first wrap, so only that one second is the attacker's; `L_b` within the depth cap), and under the finding's cadence — at the control's second (+0 s: the pushes only delay, and here not measurably) |
//! | `m4f1_the_depth_cap_bounds_the_frozen_reference_at_the_live_pool` | M4F-1 (on chain, defense in depth, kept): under the finding's keeper cadence (`L_ema` = `L_mlo` = 8× the depth, the guard cleared) the reference liquidity is capped at the pool's locked depth `D` (AIR-14; `⌊D·10⁴/λ_min⌋` as built, ≈ 2.0× on the λ 99.78 % candidate): `L_b = L_live` on the λ 99.78 % and the λ 50 % candidates; with the carried position within the candidate's caps both pumps are liquidated |
//! | `air14_a_deposit_pulled_at_the_deadline_does_not_hold_a_raydium_expiry` | AIR-14 (the program's `engine::bounds` on a synthetic state): a 1 % CPMM pool wholly locked, a position at 95 % of the 2.47 % impact cap, a deposit of 2× the pool held 62 min into the deadline and pulled there; the cap at `⌊D·10⁴/λ_min⌋` = 2D read `L_b/L_live` 2.000 and refused the expiry (`B̂_live` 2,430,862,647 > `max_in` 2,425,980,014); at `D` it executes |
//! | `air23_a_governed_pool_lock_min_bps_moves_no_open_positions_exit_bound` | AIR-23 (the same synthetic state): the exit cap read `pool_lock_min_bps` at every exit, so a Governance change moved the exit bound of positions already open (2D → 5D at 2,000) against A26; since AIR-14 it reads no governed parameter, and `(L_b, B̂_ref, max_in, B̂_live)` is the same at `pool_lock_min_bps` 2,000, 3,500, 7,500 and 10,000 |
//! | `m4f1_an_oversized_carried_position_is_not_adopted_and_settles_in_s3` | M4F-1 / M4G-3, restated by "switch only if all fit": the finding's market (a position at the 2,500-SOL incumbent's size cap, 3.12 % of the 320-SOL candidate's `X`, its cap 0.75 %) halts 900 s before the deadline; every proposal of the CPMM candidate and of a 400-SOL AMM v4 sibling is refused `CarriedPositionsTooLarge`, also after an LP's real 7× deposit held in the candidate (`D` does not move); no window opens; at the deadline `sol_settle` settles S3 at fair value on the failed pool: the lenders get `FV(P_ema, L_ema)` in full (10.327 SOL for `q` worth 9.937 at `P_ema`), the borrower the rest; the mark resets at 0 |

#![allow(clippy::result_large_err)]

#[allow(unused_imports)]
#[path = "ray_it/common.rs"]
mod common;

#[path = "pool_select_it/support.rs"]
mod support;

/// The attacker's liquidity moves on a CPMM pool through the dumped program's own `deposit`
/// and `withdraw` (as `m4v_it.rs`).
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
}

mod helpers {
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_math::fees::Direction;
    use shortwin_math::num::WAD;

    use crate::common::*;

    pub fn spot(env: &Env, pool: &AnyPool) -> u128 {
        let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        u128::from(y) * WAD / u128::from(x)
    }

    /// `√(X·Y)` of a pool's pricing reserves now.
    pub fn root_xy(env: &Env, pool: &AnyPool) -> u128 {
        let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        (u128::from(x) * u128::from(y)).isqrt()
    }

    /// Real buys (`Buy`) or sells (`Sell`) on `pool` until its spot is `mult_bps/10⁴` of `p0`.
    pub fn move_to(env: &mut Env, pool: &AnyPool, p0: u128, mult_bps: u128) {
        let (mint, program) = pool.token();
        let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        let now_p = spot(env, pool) as f64;
        let target = p0 as f64 * mult_bps as f64 / 10_000.0;
        if target > now_p {
            let a = (y as f64 * ((target / now_p).sqrt() - 1.0) / 0.99) as u64;
            let whale = fork::trader(env, &mint, program, a + 1, 0);
            ok(fork::swap(
                env,
                pool,
                &whale,
                Direction::Buy,
                Swap::ExactIn(a, 0),
            ));
        } else if target < now_p {
            let a = (x as f64 * ((now_p / target).sqrt() - 1.0) / 0.99) as u64;
            let whale = fork::trader(env, &mint, program, 0, a + 1);
            ok(fork::swap(
                env,
                pool,
                &whale,
                Direction::Sell,
                Swap::ExactIn(a, 0),
            ));
        }
    }
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

/// Who acts in a second of an M4R-1 run.
#[derive(Clone, Copy, Debug)]
struct Run {
    /// The candidate's price at the execution, bps of the pre-halt spot.
    mult_bps: u128,
    /// The execution wrapped in the attacker's `[withdraw all LP, execute, deposit]`.
    wrap_exec: bool,
    /// Seconds observed after the adoption.
    secs: i64,
    /// The candidate's real SOL.
    cand_sol: u64,
    /// Seconds (after the adoption) in which the keeper does nothing.
    keeper_absent: (i64, i64),
    /// The attacker's withdraw-wrapped crank and liquidation every second.
    attacker: bool,
}

impl Run {
    fn new(mult_bps: u128, wrap_exec: bool, secs: i64) -> Self {
        Self {
            mult_bps,
            wrap_exec,
            secs,
            cand_sol: 52_000_000_000,
            keeper_absent: (0, -1),
            attacker: true,
        }
    }

    fn keeper_at(&self, s: i64) -> bool {
        !(self.keeper_absent.0..=self.keeper_absent.1).contains(&s)
    }
}

/// What an M4R-1 run saw.
#[derive(Debug, Clone, Default)]
struct LiqRun {
    q_bps_of_x: u64,
    /// Seconds after the adoption at which the attacker's `[withdraw, liquidate, deposit]` landed.
    attacker_liq: Option<i64>,
    /// Seconds after the adoption at which a plain keeper liquidation landed.
    keeper_liq: Option<i64>,
    l_ema_after: u128,
    l_true: u128,
    d: u128,
    cr_liq: u64,
    /// Per second after the adoption, after every action of that second: `(L_ema/√(X·Y),
    /// ema_warm, ema_guard, P_ema / spot)`.
    trace: Vec<(f64, u8, u8, f64)>,
    /// `L_ema` and `P_ema` when `ema_warm` first reached 0.
    seed: Option<(u128, u128)>,
    /// The candidate's spot at the execution.
    spot: u128,
    /// The custom codes of the liquidation attempts before the gate (`ema_warm > 0`).
    early_codes: std::collections::BTreeSet<Option<u32>>,
    att_codes: std::collections::BTreeSet<Option<u32>>,
}

/// One run: an incumbent AMM v4 pool (fully locked, 100 SOL) with one position of 95 % of the
/// largest size the candidate takes (switch only if all fit: a CPMM candidate, λ 50 %, `cand_sol`
/// SOL, the same price, whose unlocked half the attacker holds, planted and moved to `mult_bps`
/// of the pre-halt spot before the open); the incumbent halts; the window runs and is executed
/// plainly or inside the attacker's
/// `[withdraw all LP, execute_pool_switch, deposit]`. Then, for `secs` seconds, every second: the
/// keeper's honest crank and plain liquidation attempt (unless absent), and the attacker's
/// `[withdraw, crank, deposit]` and `[withdraw, liquidate, deposit]`.
fn liq_run(r: Run) -> LiqRun {
    use common::*;
    use shortwin::events::Liquidate as LiqEv;
    use shortwin_litesvm_tests::engine::liquidate_ix;
    use shortwin_litesvm_tests::pool_select::execute_pool_switch_ix;
    use support::*;

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
    let cand = sized(&mut env, Plant::Cpmm, &base, 5_000, r.cand_sol);
    let AnyPool::Cpmm(cp) = cand else {
        unreachable!()
    };
    let supply = raydium::mint_supply(&env.svm, &cp.lp_mint);
    let (x_c, _) = cand
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .unwrap();
    let who = lp::provider(&mut env, &cp, 50_000_000_000, x_c / 2, supply);
    let signer = who.owner.insecure_clone();
    let p0 = helpers::spot(&env, &cand);
    helpers::move_to(&mut env, &cand, p0, r.mult_bps);
    let (cap, _) = shortwin_litesvm_tests::pool_select::carried_caps(&env, &cand);
    let q = cap / 100 * 95;
    let q_bps = (u128::from(q) * 10_000 / u128::from(x_inc)) as u64;
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    let (mint, _) = rm.pool.token();
    let exec = execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &cand);
    if r.wrap_exec {
        let held = tokens(&env, &who.lp);
        ok(env.send(
            &[
                lp::withdraw_ix(&cp, &who, held),
                exec,
                lp::deposit_ix(&cp, &who, held - 4),
            ],
            &[&signer],
        ));
    } else {
        ok(env.send(&[exec], &[]));
    }
    let m = env.market(&rm.market());
    assert_eq!(m.pool_cache[0], cp.pool, "adopted");
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1), "marked at the adoption");
    let d = support::d_of(&env, &cand);
    let rm2 = rm.on_pool(&env.svm, cand);
    let keeper = env.funded_keypair();
    let mut out = LiqRun {
        q_bps_of_x: q_bps,
        l_ema_after: m.ema_liq.get(),
        l_true: helpers::root_xy(&env, &cand),
        d,
        cr_liq: u64::from(env.position(&rm, &b.key(), nonce).cr_liq_bps),
        spot: helpers::spot(&env, &cand),
        ..LiqRun::default()
    };
    let landed = |meta: &TransactionMetadata| -> LiqEv { events::one(meta) };
    for s in 1..=r.secs {
        env.warp(1);
        let warm_before = |env: &Env| env.market(&rm.market()).ema_warm;
        if r.keeper_at(s) {
            env.crank(&rm2);
            let warm = warm_before(&env);
            let res = env.send(
                &[liquidate_ix(&rm2, &keeper.pubkey(), &b.key(), nonce)],
                &[&keeper],
            );
            if warm > 0 {
                out.early_codes.insert(custom_code(&res));
            }
            if let Ok(meta) = res {
                landed(&meta);
                out.keeper_liq = Some(s);
                break;
            }
        }
        if r.attacker {
            let held = tokens(&env, &who.lp);
            ok(env.send(
                &[
                    lp::withdraw_ix(&cp, &who, held),
                    crank_price_ix(&rm2),
                    lp::deposit_ix(&cp, &who, held - 4),
                ],
                &[&signer],
            ));
            let warm = warm_before(&env);
            let held = tokens(&env, &who.lp);
            let res = env.send(
                &[
                    lp::withdraw_ix(&cp, &who, held),
                    liquidate_ix(&rm2, &who.key(), &b.key(), nonce),
                    lp::deposit_ix(&cp, &who, held - 4),
                ],
                &[&signer],
            );
            if warm > 0 {
                out.early_codes.insert(custom_code(&res));
            }
            if res.is_err() {
                out.att_codes.insert(custom_code(&res));
            }
            if let Ok(meta) = res {
                landed(&meta);
                out.attacker_liq = Some(s);
                break;
            }
        }
        let m = env.market(&rm.market());
        let l_true = helpers::root_xy(&env, &cand);
        let p = helpers::spot(&env, &cand);
        out.trace.push((
            m.ema_liq.get() as f64 / l_true as f64,
            m.ema_warm,
            m.ema_guard,
            m.ema_price.get() as f64 / p as f64,
        ));
        if m.ema_warm == 0 && out.seed.is_none() {
            out.seed = Some((m.ema_liq.get(), m.ema_price.get()));
        }
    }
    out
}

fn show(tag: &str, r: &LiqRun) {
    println!(
        "{tag}: q {} bps of x (cr_liq {}); attacker_liq {:?} keeper_liq {:?}; L_ema at adoption / √(X·Y) {:.3} (D/L {:.3}); seed {:?}; codes before the gate {:?}; attacker codes {:?}",
        r.q_bps_of_x,
        r.cr_liq,
        r.attacker_liq,
        r.keeper_liq,
        r.l_ema_after as f64 / r.l_true as f64,
        r.d as f64 / r.l_true as f64,
        r.seed
            .map(|(l, p)| (l as f64 / r.l_true as f64, p as f64 / r.spot as f64)),
        r.early_codes,
        r.att_codes
    );
}

fn warming() -> Option<u32> {
    Some(common::code(common::E::EmaWarmingUp))
}

/// **M4R-1** (converted from `m4r_explore_liquidation_grid`; as built the wrapped runs were
/// liquidated 24–26 s after the adoption, the unwrapped never in 90 s). An unlocked LP of the λ
/// 50 % candidate wraps the execution in `[withdraw all, execute, deposit]` (or not), then
/// withdraw-wraps a crank and a liquidation every second; the keeper cranks and tries to
/// liquidate every second. The position (healthy at the candidate's 1.97× / 1.975×) is never
/// liquidated in 90 s, every liquidation before the gate is refused `EmaWarmingUp`, and at the
/// gate (+9 s) `L_ema` is the pool's own `√(X·Y)` ±1 %.
#[test]
fn m4r1_a_withdraw_wrapped_switch_forces_no_liquidation() {
    for m in [19_700u128, 19_750] {
        for wrap in [false, true] {
            let r = liq_run(Run::new(m, wrap, 90));
            show(&format!("M4R-1 m {m} wrap {wrap}"), &r);
            assert_eq!(
                (r.attacker_liq, r.keeper_liq),
                (None, None),
                "m {m} wrap {wrap}: no forced liquidation"
            );
            assert_eq!(r.early_codes, [warming()].into(), "m {m} wrap {wrap}");
            let (l9, warm9, _, _) = r.trace[8];
            assert_eq!(warm9, 0, "m {m} wrap {wrap}: the gate opens at +9 s");
            assert!(
                (0.99..=1.01).contains(&l9),
                "m {m} wrap {wrap}: L_ema/√(X·Y) {l9} at +9 s"
            );
            assert!(r.trace[..8].iter().all(|t| t.1 > 0));
        }
    }
}

/// **M4R-1**, the control: the same run with the candidate at 2.05× (genuinely liquidatable) is
/// liquidated right after the gate, at +9 or +10 s, wrapped or not.
#[test]
fn m4r1_a_liquidatable_position_is_liquidated_after_the_warm_up() {
    for wrap in [false, true] {
        let r = liq_run(Run::new(20_500, wrap, 30));
        show(&format!("M4R-1 control wrap {wrap}"), &r);
        let at = r.keeper_liq.or(r.attacker_liq).expect("liquidated");
        assert!((9..=10).contains(&at), "wrap {wrap}: liquidated at +{at} s");
        assert_eq!(r.early_codes, [warming()].into());
    }
}

/// **M4R-1**: withdraw-wrapped cranks in every warm-up second (keeper present) are never read by
/// the seed: at the gate `L_ema` is the honest `√(X·Y)` ±1 % and `P_ema` the pool's spot; the
/// pushed lows keep the reference guard set (the exit reference reads the lower median of the
/// lows, O18 residual 3) — nothing is forced.
#[test]
fn m4r1_withdraw_wraps_during_the_warm_up_are_never_read() {
    let r = liq_run(Run::new(10_000, false, 12));
    show("M4R-1 warm-up wraps", &r);
    let (l, p) = r.seed.expect("the gate opened");
    let lf = l as f64 / r.l_true as f64;
    assert!((0.99..=1.01).contains(&lf), "seed L/√(X·Y) {lf}");
    assert!(
        p <= r.spot && p * 1_000 >= r.spot * 999,
        "seed P {p} vs spot {}",
        r.spot
    );
    let (_, warm9, guard9, _) = r.trace[8];
    assert_eq!((warm9, guard9), (0, 1), "the pushed lows keep the guard");
    assert_eq!((r.attacker_liq, r.keeper_liq), (None, None));
}

/// **O18 residual 1 (degraded mode)**: with no keeper during the 8 warm-up seconds the attacker's
/// withdraw-wrapped cranks are the only observations and seed `L = D` — the disclosed residual;
/// nothing lands before the gate (+9 s: the keeper's return). One keeper second among the eight
/// (7 attacker-only + 1) is enough for the honest seed on this λ 50 % candidate (`(1 + C)·D =
/// √(X·Y)`).
#[test]
fn m4r1_an_attacker_only_warm_up_is_the_disclosed_degraded_mode() {
    let mut all = Run::new(10_000, false, 10);
    all.keeper_absent = (1, 8);
    let r = liq_run(all);
    show("M4R-1 degraded (8 attacker-only seconds)", &r);
    let (l, _) = r.seed.expect("the gate opened");
    assert!(
        l >= r.d && l <= r.d + r.d / 1_000,
        "seed {l} vs D {} (degraded: the locked depth)",
        r.d
    );
    assert!(r.attacker_liq.is_none_or(|s| s >= 9) && r.keeper_liq.is_none_or(|s| s >= 9));
    assert!(r.early_codes.iter().all(|c| *c == warming()));

    let mut one = Run::new(10_000, false, 10);
    one.keeper_absent = (2, 8);
    let r = liq_run(one);
    show("M4R-1 degraded (7 attacker-only + 1 keeper second)", &r);
    let (l, _) = r.seed.expect("the gate opened");
    let lf = l as f64 / r.l_true as f64;
    assert!((0.99..=1.01).contains(&lf), "seed L/√(X·Y) {lf}");
}

/// Exploration (kept from the M4R-1 reproducer): the liquidation boundary in the candidate's
/// price with and without the withdraw-wrapped execution. Knobs: `M4R_GRID` (bps, comma
/// separated), `M4R_SECS`, `M4R_SOL`.
#[test]
#[ignore]
fn m4r_explore_liquidation_grid() {
    let grid: Vec<u128> = std::env::var("M4R_GRID")
        .ok()
        .map(|s| s.split(',').map(|v| v.trim().parse().unwrap()).collect())
        .unwrap_or_else(|| vec![10_000, 11_000, 12_000, 13_000, 14_000, 15_000]);
    let secs: i64 = std::env::var("M4R_SECS")
        .ok()
        .map(|s| s.parse().unwrap())
        .unwrap_or(60);
    let sol: u64 = std::env::var("M4R_SOL")
        .ok()
        .map(|s| s.parse().unwrap())
        .unwrap_or(52_000_000_000);
    for m in grid {
        for wrap in [false, true] {
            let mut run = Run::new(m, wrap, secs);
            run.cand_sol = sol;
            let r = liq_run(run);
            show(&format!("grid m {m} wrap {wrap}"), &r);
        }
    }
}

/// A market on a planted, fully locked AMM v4 incumbent of 100 SOL at Fartcoin's price with
/// one open position, halted, and a fully locked CPMM candidate holding `sol` of real SOL at the
/// same price (30 % of the incumbent's depth for 30 SOL: it qualifies for the fast path). The
/// position is the default size, at most 90 % of the candidate's per-position cap (switch only if
/// all fit: the candidate is planted first and the position sized to be carried onto it).
fn halted_with_candidate(
    sol: u64,
) -> (
    common::Env,
    common::RayMarket,
    common::Borrower,
    u64,
    common::AnyPool,
) {
    use common::*;
    use support::*;

    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let lam = lambda_of(&env, &base);
    let inc = sized(&mut env, Plant::V4, &base, lam, 100_000_000_000);
    ok(env.list_ray(&inc, VENUE_HINT_V4));
    let q = q_of(&env, &inc);
    let rm = env.ray_market_listed(RaySpec::new(FART, q.saturating_mul(40)), inc);
    env.crank(&rm);
    crank_every(&mut env, &rm, 1, 5);
    let cand = sized(&mut env, Plant::Cpmm, &base, 10_000, sol);
    let (cap, _) = shortwin_litesvm_tests::pool_select::carried_caps(&env, &cand);
    let q = q.min(cap / 10 * 9);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    (env, rm, b, nonce, cand)
}

/// **M4R-2**: a window proposal judges the window floor at `min(P_ema, P_cand)`. The token dumps
/// 4× on a fully locked candidate during the halt: its locked SOL is ≈ 30 at the stale `P_ema`
/// and ≈ 15 at its own price (floor 25). The keeper's proposal is refused `PoolTooThin` and no
/// window opens. The disclosed residual (O18 residual 4): a proposal wrapped in `[buy, propose,
/// sell]` opens the window (`P_cand` pushed to `P_ema`: never looser than the floor at `P_ema`),
/// and the execution — which judges the floor at `P_ema` — adopts.
#[test]
fn m4r2_a_dump_during_the_halt_keeps_a_thin_pool_out() {
    use common::*;
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_litesvm_tests::pool_select::{passes_window_floor, propose_pool_ix};
    use shortwin_litesvm_tests::raydium::candidate_read_set;
    use shortwin_math::fees::Direction;
    use support::*;

    let (mut env, rm, _b, _nonce, cand) = halted_with_candidate(30_000_000_000);
    let p_ema = env.market(&rm.market()).ema_price.get();
    let p0 = helpers::spot(&env, &cand);
    helpers::move_to(&mut env, &cand, p0, 2_500);
    let p_cand = helpers::spot(&env, &cand);
    assert!(
        p_cand * 4 <= p_ema * 101 / 100,
        "a 4× dump: {p_cand} vs {p_ema}"
    );
    crank_every(&mut env, &rm, 30, 5);
    let floor = env.config().params.pool_min_real_sol;
    let d = d_of(&env, &cand);
    let locked_ema = shortwin_math::tier::locked_sol(d, p_ema);
    let locked_live = shortwin_math::tier::locked_sol(d, p_cand);
    println!(
        "M4R-2: locked SOL {:.2} at P_ema, {:.2} live (floor {:.0})",
        locked_ema as f64 / 1e9,
        locked_live as f64 / 1e9,
        floor as f64 / 1e9
    );
    assert!(locked_ema >= u128::from(floor) && locked_live < u128::from(floor));
    assert!(passes_window_floor(&env, &cand, Some(p_ema)));
    assert!(!passes_window_floor(&env, &cand, Some(p_cand)));
    assert_err(&env.propose(Some(&rm.pool), &cand), E::PoolTooThin);
    assert_eq!(env.market(&rm.market()).fp_t0, 0, "no window");

    // The residual: the thin pool's backer pumps its spot back for the proposal.
    let (mint, program) = cand.token();
    let (_, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
    let push = y + y / 20;
    let t = fork::trader(&mut env, &mint, program, 3 * push, 0);
    let owner = t.owner.insecure_clone();
    let buy = cand.swap_ix(
        &env.svm,
        &owner.pubkey(),
        &t,
        Direction::Buy,
        Swap::ExactIn(push, 0),
    );
    let bought = cand
        .quote_exact_in(&env.svm, Direction::Buy, push)
        .unwrap()
        .proceeds;
    let sell = cand.swap_ix(
        &env.svm,
        &owner.pubkey(),
        &t,
        Direction::Sell,
        Swap::ExactIn(bought, 0),
    );
    let inc = candidate_read_set(&env.svm, &rm.pool, false);
    let prop = propose_pool_ix(&env.svm, &mint, &inc, &cand);
    ok(env.send(&[buy, prop, sell], &[&owner]));
    assert!(
        env.market(&rm.market()).fp_t0 > 0,
        "the pump-wrapped proposal opened a window"
    );
    crank_every(&mut env, &rm, 30, 2);
    let ev = execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    assert_eq!(
        ev.candidate,
        shortwin_litesvm_tests::pool_select::pool_key(&cand)
    );
    println!("M4R-2: the pump-wrapped proposal opened a window; the execution adopted at P_ema");
}

/// One fast path onto a fully locked 60 %-deep CPMM candidate whose price moved to `mult_bps`
/// of the pre-halt spot during the halt; after the adoption nobody observes for `absent` seconds,
/// then the keeper cranks and tries to liquidate every second. Returns the seconds after the
/// keeper's return at which the liquidation landed.
fn genuine_move(mult_bps: u128, absent: i64) -> Option<i64> {
    use common::*;
    use shortwin_litesvm_tests::engine::liquidate_ix;
    use support::*;

    let (mut env, rm, q) = setup(FART);
    crank_every(&mut env, &rm, 1, 5);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    let lam = lambda_of(&env, &rm.pool);
    let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (60, 100), lam);
    let p0 = helpers::spot(&env, &cand);
    helpers::move_to(&mut env, &cand, p0, mult_bps);
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    let rm2 = rm.on_pool(&env.svm, cand);
    let keeper = env.funded_keypair();
    env.warp(absent);
    for s in 1..=60i64 {
        env.warp(1);
        env.crank(&rm2);
        let res = env.send(
            &[liquidate_ix(&rm2, &keeper.pubkey(), &b.key(), nonce)],
            &[&keeper],
        );
        match custom_code(&res) {
            None => {
                ok(res);
                return Some(s);
            }
            Some(c) if env.market(&rm.market()).ema_warm > 0 => {
                assert_eq!(c, code(E::EmaWarmingUp), "second {s}");
            }
            Some(_) => {}
        }
    }
    None
}

/// **★T-SWITCH-CONFIRM** (liveness): a genuine 2.2× pump during the halt — the position is
/// underwater on the new pool — is liquidated at adoption + 9 s with a keeper cranking every
/// second from adoption + 1 (the fresh warm-up price, not the stale `P_ema`, decides).
#[test]
fn t_switch_confirm_a_genuine_pump_is_liquidated_at_nine_seconds() {
    let at = genuine_move(22_000, 0);
    println!("T-SWITCH-CONFIRM: 2.2× pump during the halt: liquidated at +{at:?} s");
    assert_eq!(at, Some(9));
}

/// **★T-SWITCH-CONFIRM** (no deadlock without a keeper): nobody observes for 600 s after the
/// adoption; once the keeper returns the warm-up runs and the position is liquidated 8–9 s later.
#[test]
fn t_switch_confirm_a_keeper_returning_after_ten_minutes_liquidates_in_nine_seconds() {
    let at = genuine_move(22_000, 600);
    println!("T-SWITCH-CONFIRM: keeper back after 600 s: liquidated at +{at:?} s");
    assert!(matches!(at, Some(8..=9)), "{at:?}");
}

// ================================================================================================
// M4X (the adversarial verification of M4R): regressions.
// ================================================================================================

/// `[buy until the spot is k×, ix, sell what was bought]` on `pool` by trader `t` (one atomic
/// transaction; a failing `ix` reverts the round trip too).
fn pump_wrapped(
    env: &mut common::Env,
    pool: &common::AnyPool,
    t: &shortwin_litesvm_tests::fork::Trader,
    k: f64,
    ix: common::Instruction,
) -> common::TxResult {
    use common::*;
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_math::fees::Direction;
    let (_, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
    let a = (y as f64 * (k.sqrt() - 1.0) / 0.99) as u64;
    let bought = pool
        .quote_exact_in(&env.svm, Direction::Buy, a)
        .unwrap()
        .proceeds;
    let buy = pool.swap_ix(
        &env.svm,
        &t.owner.pubkey(),
        t,
        Direction::Buy,
        Swap::ExactIn(a, 0),
    );
    let sell = pool.swap_ix(
        &env.svm,
        &t.owner.pubkey(),
        t,
        Direction::Sell,
        Swap::ExactIn(bought, 0),
    );
    let owner = t.owner.insecure_clone();
    env.send(&[buy, ix, sell], &[&owner])
}

/// The largest v0.4.1 price step (ECON §5.2, τ = 5, the observation at `+C`), recomputed.
fn up_bound(p: u128, dt: i64, c_bps: u32) -> u128 {
    let dt = u128::try_from(dt).unwrap();
    (p * 5 + p * (10_000 + u128::from(c_bps)) / 10_000 * dt) / (5 + dt)
}

/// What an attacker-only price-bundle run saw.
struct Bundles {
    /// The second after the adoption at which the attacker's liquidation landed.
    liquidated: Option<i64>,
    /// `P_ema` / the pool's true spot at the gate.
    p_gate: f64,
    /// A warm-up integration whose seed rose above the largest v0.4.1 step: (second, seed, bound).
    over_bound: Option<(i64, u128, u128)>,
}

/// A 100-SOL fully locked AMM v4 incumbent with the largest position the candidate takes (switch
/// only if all fit: 95 % of its per-position cap; its vault then raised by `margin_bps` of `V`),
/// halted; an honest window and execution adopt a 52-SOL λ 99.78 % candidate that trades FLAT at
/// the pre-halt price. The keeper is absent for `s ≤ absent`, then
/// cranks and tries to liquidate every second; every second the attacker sends `[buy kl×,
/// liquidate, sell]` and `[buy kc×, crank_price, sell]`. Every warm-up integration is checked
/// against the largest v0.4.1 step from the previous `P_ema` (M4X-1).
fn price_bundles(
    kind: support::Plant,
    margin_bps: u64,
    kc: f64,
    kl: f64,
    absent: i64,
    secs: i64,
) -> Bundles {
    use common::*;
    use shortwin_litesvm_tests::engine::{add_margin_ix, crank_price_ix, liquidate_ix};
    use support::*;

    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let lam = lambda_of(&env, &base);
    let inc = sized(&mut env, Plant::V4, &base, lam, 100_000_000_000);
    ok(env.list_ray(&inc, VENUE_HINT_V4));
    let q0 = q_of(&env, &inc);
    let rm = env.ray_market_listed(RaySpec::new(FART, q0.saturating_mul(60)), inc);
    env.crank(&rm);
    crank_every(&mut env, &rm, 1, 5);
    let cand = sized(&mut env, kind, &base, 9_978, 52_000_000_000);
    let (cap, _) = shortwin_litesvm_tests::pool_select::carried_caps(&env, &cand);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, cap / 100 * 95);
    if margin_bps > 0 {
        let v = env.position(&rm, &b.key(), nonce).v;
        let kp = b.kp.insecure_clone();
        ok(env.send(
            &[add_margin_ix(&rm, &b.key(), nonce, v * margin_bps / 10_000)],
            &[&kp],
        ));
    }
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    let market = rm.market();
    assert_eq!(
        (env.market(&market).ema_warm, env.market(&market).ema_guard),
        (8, 1)
    );
    let rm2 = rm.on_pool(&env.svm, cand);
    let keeper = env.funded_keypair();
    let (mint, program) = cand.token();
    let (_, y) = cand
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .unwrap();
    let att = fork::trader(&mut env, &mint, program, 4 * y, 0);
    let spot0 = helpers::spot(&env, &cand);
    let mut out = Bundles {
        liquidated: None,
        p_gate: 0.0,
        over_bound: None,
    };
    for s in 1..=secs {
        env.warp(1);
        // The state before this second's integration (at most one per second, whoever rolls):
        // a warm-up integration is checked against the largest v0.4.1 step from it.
        let pre = env.market(&market);
        let mut seen = false;
        let mut check = |env: &Env, out: &mut Bundles| {
            let m = env.market(&market);
            if seen || m.ema_count == pre.ema_count || pre.ema_warm == 0 {
                return;
            }
            seen = true;
            let dt = (m.ema_ts - pre.ema_ts).min(i64::from(m.ema_dtmax_s));
            let bound = up_bound(pre.ema_price.get(), dt, m.ema_clamp_bps);
            if m.ema_price.get() > bound && out.over_bound.is_none() {
                out.over_bound = Some((s, m.ema_price.get(), bound));
            }
            if m.ema_warm == 0 {
                out.p_gate = m.ema_price.get() as f64 / spot0 as f64;
            }
        };
        if s > absent {
            env.crank(&rm2);
            check(&env, &mut out);
            let res = env.send(
                &[liquidate_ix(&rm2, &keeper.pubkey(), &b.key(), nonce)],
                &[&keeper],
            );
            assert!(
                res.is_err(),
                "the keeper liquidated at +{s} at a flat price"
            );
        }
        let warming = env.market(&market).ema_warm > 0;
        let liq = liquidate_ix(&rm2, &att.owner.pubkey(), &b.key(), nonce);
        let res = pump_wrapped(&mut env, &cand, &att, kl, liq);
        if res.is_ok() {
            out.liquidated = Some(s);
            return out;
        }
        if warming {
            assert_eq!(custom_code(&res), Some(code(E::EmaWarmingUp)), "+{s}");
        }
        let _ = pump_wrapped(&mut env, &cand, &att, kc, crank_price_ix(&rm2));
        check(&env, &mut out);
    }
    out
}

/// **M4X-1**: attacker-only price bundles through the whole warm-up (the keeper absent for s ≤ 8)
/// on a position whose honest threshold is ≈ 3× (`add_margin` +50 % of `V`), the pool flat at
/// 1.00×. Before M4X-1 the seed read the pumped low at once and the attacker's `[buy 3.3×,
/// liquidate, sell]` landed at the gate (+9 s) on both programs; now the seed climbs by at most
/// the largest v0.4.1 step per integration (×9/7 then ×7/6: ≈ 2.78× at the 6th, 3.24× at the
/// 7th, the pump at the 8th), so the EMA leg's three newest entries are not all past the
/// threshold at the gate, the returning keeper's honest bucket fails the pumped spot leg after
/// it, and nothing lands in 20 s — what 8 attacker-only seconds give in steady state. The
/// near-threshold position (≈ 2.05×, 2.5× bundles) stays the disclosed degraded-mode class
/// (O18 residual 1; steady state and M4 as built reach it too, as built already at +7 s): the
/// seed still never rises faster than the bound.
#[test]
fn m4x1_attacker_only_price_bundles_reach_no_further_than_steady_state() {
    for kind in [support::Plant::Cpmm, support::Plant::V4] {
        let r = price_bundles(kind, 5_000, 3.3, 3.3, 8, 20);
        println!(
            "M4X-1 {kind:?}: threshold ≈ 3×, bundles 3.3×, keeper absent s ≤ 8: attacker liquidation {:?}, P_ema/spot at the gate {:.3}, over the bound {:?}",
            r.liquidated, r.p_gate, r.over_bound
        );
        assert_eq!(
            r.liquidated, None,
            "{kind:?}: a flat 1.00× pool, a ≈ 3× threshold"
        );
        assert_eq!(r.over_bound, None, "{kind:?}");
        assert!(r.p_gate > 3.0 && r.p_gate < 3.35, "{kind:?}: {}", r.p_gate);
    }
    let r = price_bundles(support::Plant::Cpmm, 0, 2.5, 2.5, 8, 20);
    println!(
        "M4X-1 (disclosed, steady-state class): threshold ≈ 2.05×, bundles 2.5×, keeper absent s ≤ 8: attacker liquidation {:?}, P_ema/spot at the gate {:.3}",
        r.liquidated, r.p_gate
    );
    assert_eq!(r.over_bound, None);
}

/// The sizes tried for a position on the 2,500-SOL incumbent, bps of its token reserve (the first
/// that opens; capped by the planned candidate's per-position cap).
const OPEN_BPS: [u64; 11] = [100, 90, 80, 74, 72, 70, 65, 60, 50, 40, 25];

/// The SOL and tokens a CPMM deposit of `lp` LP needs (rounded up, plus one).
fn lp_needed(env: &common::Env, p: &shortwin_litesvm_tests::fork::CpmmPool, lp: u64) -> (u64, u64) {
    use common::*;
    let counter = p.state(&env.svm).lp_supply;
    let (x, y) = AnyPool::Cpmm(*p)
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .expect("reserves");
    let part = |r: u64| (u128::from(r) * u128::from(lp)).div_ceil(u128::from(counter)) as u64 + 1;
    (part(y), part(x))
}

/// **M4X-2**: the capital deposit freeze held for 900 s. A 2,500-SOL λ 50 % AMM v4 incumbent
/// (Tier A) with a position, halted; an honest window adopts a 320-SOL λ 99.78 % CPMM candidate.
/// From the adoption the attacker sends `[deposit 7·LP counter, crank_price, withdraw]` every
/// second and the keeper cranks every second; the price is flat for 900 s, then genuinely ramps
/// 4×/120 s while the keeper tries `liquidate` and `sol_settle` every second. Before M4X-2, on the
/// incumbent's size cap (q ≈ 3.1 % of the candidate's `X`): `L_slow`/√(X·Y) 2.621 at the ramp, S1
/// at +1,025 s (lenders 29.946 SOL for `q` worth 40.002, the borrower 0). Now `L_slow` stays at
/// the honest lows (the guard still set) and I18 flags an `L_slow` lifted above them; the position
/// is the largest the candidate takes (switch only if all fit: 95 % of its per-position cap,
/// q ≈ 0.71 %; the size cap is no longer carried, `m4f1_an_oversized_…`), and its liquidation
/// lands in the ramp with the lenders' `q` back.
#[test]
fn m4x2_a_held_deposit_freeze_no_longer_lifts_l_slow() {
    use common::*;
    use shortwin::events::{Liquidate, SolSettle};
    use shortwin_litesvm_tests::engine::{crank_price_ix, liquidate_ix, sol_settle_ix};
    use shortwin_litesvm_tests::invariants::{self, IxEvents};
    use support::*;

    const W: i64 = 900;
    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let inc = sized(&mut env, Plant::V4, &base, 5_000, 2_500_000_000_000);
    ok(env.list_ray(&inc, VENUE_HINT_V4));
    let q0 = q_of(&env, &inc);
    let rm = env.ray_market_listed(RaySpec::new(FART, q0.saturating_mul(60)), inc);
    env.crank(&rm);
    crank_every(&mut env, &rm, 1, 5);
    let cand = sized(&mut env, Plant::Cpmm, &base, 9_978, 320_000_000_000);
    let AnyPool::Cpmm(cp) = cand else {
        unreachable!()
    };
    // Switch only if all fit: the position is 95 % of the candidate's per-position cap (the size
    // cap of the incumbent, ≈ 3.1 % of the candidate's X, is refused `CarriedPositionsTooLarge`:
    // `m4f1_an_oversized_carried_position_is_not_adopted_and_settles_in_s3`).
    let fit_q = shortwin_litesvm_tests::pool_select::carried_caps(&env, &cand).0 / 100 * 95;
    let (x_inc, _) = inc
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .unwrap();
    let mut b = env.borrower();
    let nonce = OPEN_BPS
        .into_iter()
        .find_map(|bps| {
            let q = (u128::from(x_inc) * u128::from(bps) / 10_000) as u64;
            let (res, nonce) = env.open(&rm, &mut b, q.min(fit_q));
            res.is_ok().then_some(nonce)
        })
        .expect("an open");
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    let q = env.position(&rm, &b.key(), nonce).q;
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    let market = rm.market();
    let rm2 = rm.on_pool(&env.svm, cand);
    let keeper = env.funded_keypair();
    let (sol, tok) = lp_needed(&env, &cp, 7 * cp.state(&env.svm).lp_supply);
    let who = lp::provider(&mut env, &cp, 6 * sol, 6 * tok, 0);
    let signer = who.owner.insecure_clone();
    let freeze = |env: &mut Env| {
        let add = 7 * cp.state(&env.svm).lp_supply;
        env.send(
            &[
                lp::deposit_ix(&cp, &who, add),
                crank_price_ix(&rm2),
                lp::withdraw_ix(&cp, &who, add),
            ],
            &[&signer],
        )
    };
    let p0 = helpers::spot(&env, &cand);
    for s in 1..=(W + 400) {
        env.warp(1);
        if s > W {
            let g = 10_000 + 30_000 * u128::try_from((s - W).min(120)).unwrap() / 120;
            if helpers::spot(&env, &cand) * 10_000 < p0 * g * 9_999 / 10_000 {
                helpers::move_to(&mut env, &cand, p0, g);
            }
        }
        if s == W {
            // I18 flags an L_slow lifted above the honest lows while the guard is set: the
            // keeper's crank integrates this second (after the attacker's deposit-wrapped one).
            let pre = invariants::snapshot(&env.svm, &[market]);
            let meta = ok(env.send(&[crank_price_ix(&rm2)], &[]));
            let ev = IxEvents::of(&meta, 0);
            ok(freeze(&mut env));
            let m = env.market(&market);
            let root = helpers::root_xy(&env, &cand);
            println!(
                "M4X-2 at the ramp: L_slow/√(X·Y) {:.3}, L_ema/√(X·Y) {:.3}, guard {}",
                m.liq_slow.get() as f64 / root as f64,
                m.ema_liq.get() as f64 / root as f64,
                m.ema_guard
            );
            assert_eq!(m.ema_guard, 1, "the freeze keeps the guard set");
            assert!(m.ema_liq.get() > 2 * root, "L_ema frozen high");
            assert!(m.liq_slow.get() <= root, "L_slow at the lows");
            let i18 = |env: &Env| {
                invariants::confirm::check_ix(
                    &env.svm,
                    &pre.positions,
                    &invariants::confirm::ConfirmFacts {
                        created: &ev.created,
                        switched: &ev.switched,
                        venue_changed: &ev.venue_changed,
                        opens: &ev.opens,
                        liquidations: &ev.liquidations,
                        expiries: &ev.expiries,
                        settlements: &ev.settlements,
                        armed: &ev.armed,
                        disarmed: &ev.disarmed,
                    },
                )
                .map_err(|v| v.invariant)
            };
            assert_eq!(i18(&env), Ok(()));
            let good = env.svm.get_account(&market).unwrap();
            let mut acc = good.clone();
            acc.data[1_672..1_688].copy_from_slice(&m.ema_liq.get().to_le_bytes());
            env.svm.set_account(market, acc).unwrap();
            assert_eq!(i18(&env), Err("I18"), "L_slow lifted to the frozen L_ema");
            env.svm.set_account(market, good).unwrap();
        } else {
            env.crank(&rm2);
            ok(freeze(&mut env));
        }
        if s <= W {
            continue;
        }
        let res = env.send(
            &[liquidate_ix(&rm2, &keeper.pubkey(), &b.key(), nonce)],
            &[&keeper],
        );
        if let Ok(meta) = res {
            let e: Liquidate = events::one(&meta);
            println!(
                "M4X-2: liquidated at +{s} s ({:.3}×), buyback {:.3} SOL, borrower {:.3} SOL",
                helpers::spot(&env, &cand) as f64 / p0 as f64,
                e.buyback_cost as f64 / 1e9,
                e.borrower_payout as f64 / 1e9
            );
            assert!(e.borrower_payout > 0);
            return;
        }
        let settle = sol_settle_ix(&env, &rm2, &keeper.pubkey(), &b.key(), nonce);
        if let Ok(meta) = env.send(&[settle], &[&keeper])
            && let Some(e) = events::of::<SolSettle>(&meta).first()
        {
            let owed = u128::from(q) * helpers::spot(&env, &cand) / shortwin_math::num::WAD;
            panic!(
                "S{} at +{s} s: lenders {:.3} SOL for q worth {:.3} SOL, borrower {:.3} SOL",
                e.condition,
                e.lender_amount as f64 / 1e9,
                owed as f64 / 1e9,
                e.borrower_amount as f64 / 1e9
            );
        }
    }
    panic!("the position is still open after the ramp");
}

/// **M4X-3** (documentation; O18 residual 4): the window floor is judged at `min(P_ema, P_cand)`
/// by the proposals only. A genuine 4× dump 5 s into the collection window — after the keeper's
/// proposal opened it at the honest price — is judged by the execution at the stale `P_ema` and
/// adopted: the positions move onto a pool with ≈ 15 SOL of locked SOL at its live price (floor
/// 25), never looser than M4 as built. The warm-up then re-seeds `P_ema` towards the live price —
/// since M6-SEC-1 (owner decision of 2026-09-30) by at most one v0.4.1 step per integration, so at
/// the gate the carried `P_ema` still lies above the dumped pool (≈ 1.7×: `(6/7)·(11/12)⁷` of the
/// carried 4×, then one steady step; as built it read the live price at once) — the safe side: no
/// exit is forced (the spot leg reads the live price) and the market takes no open: an open at the
/// gate is refused `PoolListWindow` (the adopted pool's listing window). A minute of steady
/// integrations then brings `P_ema` onto the pool, so the pool is seen as it is: once the new
/// pool's listing window has run, a new open is refused `PoolTooThin`.
#[test]
fn m4x3_a_dump_inside_the_window_is_adopted_at_p_ema_then_re_seeded() {
    use common::*;
    use shortwin_litesvm_tests::pool_select::passes_window_floor;
    use support::*;

    let (mut env, rm, _b, _nonce, cand) = halted_with_candidate(30_000_000_000);
    let p_ema = env.market(&rm.market()).ema_price.get();
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    assert!(
        env.market(&rm.market()).fp_t0 > 0,
        "window open at the honest price"
    );
    env.warp(5);
    let p0 = helpers::spot(&env, &cand);
    helpers::move_to(&mut env, &cand, p0, 2_500);
    let p_cand = helpers::spot(&env, &cand);
    crank_every(&mut env, &rm, 30, 2);
    let floor = u128::from(env.config().params.pool_min_real_sol);
    let d = d_of(&env, &cand);
    let locked_live = shortwin_math::tier::locked_sol(d, p_cand);
    let locked_ema = shortwin_math::tier::locked_sol(d, p_ema);
    println!(
        "M4X-3: locked SOL {:.2} at P_ema, {:.2} live (floor {:.0})",
        locked_ema as f64 / 1e9,
        locked_live as f64 / 1e9,
        floor as f64 / 1e9
    );
    assert!(locked_ema >= floor && locked_live < floor);
    assert!(!passes_window_floor(&env, &cand, Some(p_ema.min(p_cand))));
    execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    let rm2 = rm.on_pool(&env.svm, cand);
    let carried = env.market(&rm.market()).ema_price.get();
    crank_every(&mut env, &rm2, 1, 10);
    let m = env.market(&rm.market());
    assert_eq!(m.ema_warm, 0);
    let ratio = m.ema_price.get() as f64 / p_cand as f64;
    println!(
        "M4X-3: adopted at P_ema ({:.3}× the pool); after the warm-up P_ema/P_live {ratio:.3}",
        carried as f64 / p_cand as f64
    );
    // M6-SEC-1: eight warm-up integrations fall by at most ×(6/7)·(11/12)⁷ ≈ 0.466 of the carried
    // P_ema, and the one steady step after the gate by at most ×11/12 more.
    let fell = m.ema_price.get() as f64 / carried as f64;
    assert!((0.42..0.47).contains(&fell), "{fell}");
    assert!(ratio > 1.5, "{ratio}: the seed lags the dump");
    // An open meanwhile waits out the adopted pool's listing window (`PoolListWindow`, checked
    // before the spot/EMA deviation the lagging seed would also fail).
    let mut b1 = env.borrower();
    let q = q_of(&env, &cand);
    let (res, _) = env.open(&rm2, &mut b1, q);
    assert_err(&res, E::PoolListWindow);
    crank_every(&mut env, &rm2, 1, 60);
    let ratio = env.market(&rm.market()).ema_price.get() as f64 / p_cand as f64;
    println!("M4X-3: a minute later P_ema/P_live {ratio:.3}");
    assert!((0.99..=1.01).contains(&ratio), "{ratio}");
    // The listing window of the adopted pool (opens wait `pool_list_window_s`), observed.
    crank_every(&mut env, &rm2, 60, 61);
    let mut b2 = env.borrower();
    let q = q_of(&env, &cand);
    let (res, _) = env.open(&rm2, &mut b2, q);
    assert_err(&res, E::PoolTooThin);
}

// ================================================================================================
// M4F-1: the capital deposit freeze under the keeper's own cadence
// ================================================================================================

/// The keeper's coverage rules (`services/keeper`: `coverage::MarketTrack`, `decide::market_needs`),
/// modelled second by second for the M4F-1 regressions. Each second the keeper reads the pool first
/// (its own view of the spot and the depth), checks the market's open and last completed buckets
/// for a foreign observation (duty 2: a price deviation starts a per-slot burst; on a market with
/// open positions a liquidity deviation starts 10 s of per-second coverage, FITS-2 (b)), then
/// observes if due; the attacker's bundle lands after it in the same second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cadence {
    /// As shipped: per second while the post-change warm-up runs **or the reference guard is set**
    /// (market with positions, M4F-1); duty 2's per-slot bursts on the **price** alone (M4F-1's
    /// liquidity bursts reverted: M4G-2's keeper-cost DoS) and, on a market with open positions,
    /// **per-second** coverage for 10 s after a foreign bucket liquidity off the keeper's own
    /// `√(X·Y)`, extended while it recurs (FITS-2 (b), `liq_cover_s`).
    Shipped,
    /// The keeper before M4F-1 (the finding's model): per second only while `ema_warm > 0`.
    PreM4f,
}

/// A range `(lo, hi)`.
type Range = (u128, u128);

struct KeeperModel {
    cadence: Cadence,
    /// Own views per second: the price and the depth range read in it.
    views: std::collections::HashMap<i64, (Range, Range)>,
    seen: std::collections::HashSet<i64>,
    burst_until: i64,
    last_obs: Option<i64>,
    bursts: Vec<i64>,
    /// FITS-2 (b): the liquidity-triggered per-second window and its trigger seconds.
    liq_seen: std::collections::HashSet<i64>,
    liq_until: i64,
    liq_covers: Vec<i64>,
}

impl KeeperModel {
    fn new(cadence: Cadence) -> Self {
        Self {
            cadence,
            views: Default::default(),
            seen: Default::default(),
            burst_until: i64::MIN,
            last_obs: None,
            bursts: Vec::new(),
            liq_seen: Default::default(),
            liq_until: i64::MIN,
            liq_covers: Vec::new(),
        }
    }

    /// The keeper's read of second `now`: its own view, then duty 2 on the market as read.
    fn read(&mut self, now: i64, m: &shortwin::state::Market, spot: u128, depth: u128) {
        let v = self
            .views
            .entry(now)
            .or_insert(((spot, spot), (depth, depth)));
        v.0 = (v.0.0.min(spot), v.0.1.max(spot));
        v.1 = (v.1.0.min(depth), v.1.1.max(depth));
        let out = |lo: u128, hi: u128, (vlo, vhi): (u128, u128)| {
            lo * 10_000 < vlo * 9_800 || hi * 10_000 > vhi * 10_200
        };
        let o = m.observation();
        // The last completed bucket's liquidity extremes: the ring entry that integrated it.
        let lb_liq = o
            .ring
            .newest()
            .filter(|e| e.ts > o.last.sec && e.lo == o.last.lo && e.hi == o.last.hi)
            .map(|e| (e.llo, e.lhi));
        let buckets = [
            (
                o.bucket.sec,
                (o.bucket.lo, o.bucket.hi),
                Some((o.bucket.llo, o.bucket.lhi)),
            ),
            (o.last.sec, (o.last.lo, o.last.hi), lb_liq),
        ];
        let open = m.open_positions > 0;
        for (sec, (lo, hi), liq) in buckets {
            if lo == 0 {
                continue;
            }
            let Some(&(vp, vl)) = self.views.get(&sec) else {
                continue;
            };
            if !self.seen.contains(&sec) && out(lo, hi, vp) {
                self.seen.insert(sec);
                self.burst_until = self.burst_until.max(sec + 60);
                self.bursts.push(sec);
            }
            let shipped = self.cadence == Cadence::Shipped;
            if let Some((llo, lhi)) = liq.filter(|(llo, _)| *llo != 0)
                && shipped
                && open
                && !self.liq_seen.contains(&sec)
                && out(llo, lhi, vl)
            {
                self.liq_seen.insert(sec);
                self.liq_until = self.liq_until.max(sec + 10);
                self.liq_covers.push(sec);
            }
        }
    }

    /// Whether the keeper observes at `now` (`near`: a position near its threshold, here the
    /// genuine ramp), on the market's state as the next instruction's prologue will see it.
    fn due(&self, now: i64, m: &shortwin::state::Market, spot: u128, near: bool) -> bool {
        let mut o = *m.observation();
        o.roll(now, &m.ema_params()).unwrap();
        let ema_dev = spot.abs_diff(o.ema.price) * 10_000 > o.ema.price * 200;
        let per_second = o.ema.warm > 0
            || (self.cadence == Cadence::Shipped && o.ema.guard != 0)
            || ema_dev
            || near;
        now < self.burst_until
            || per_second
            || now < self.liq_until
            || self.last_obs.is_none_or(|t| now - t >= 30)
    }
}

/// One M4F-1 run and what it printed.
#[derive(Debug)]
struct FreezeRun {
    report: String,
    /// (second after the adoption, borrower payout) of the liquidation.
    liquidated: Option<(i64, u64)>,
    /// A settlement instead (S1 / S2).
    settled: Option<i64>,
    /// Seconds after the warm-up whose reference liquidity the Raydium depth cap lowered
    /// (`bounds::ref_liquidity` with and without it on the pool's depth after the second).
    capped_seconds: usize,
    /// The first second with `ema_warm = 0` and the guard clear.
    guard_clear: Option<i64>,
    /// Seconds whose foreign observation started a burst.
    bursts: Vec<i64>,
    /// Seconds whose foreign liquidity started or extended the per-second coverage (FITS-2 (b)).
    liq_covers: Vec<i64>,
    /// Seconds after the adoption the keeper observed.
    observed: Vec<i64>,
    /// At the ramp start, over the pool's `√(X·Y)`: `L_ema`, `L_mlo`, `L_b` (the reference
    /// liquidity as `bounds::ref_liquidity` computes it with the market's Raydium depth cap),
    /// and the cap itself; the guard.
    at_ramp: (f64, f64, f64, f64, u8),
}

/// The M4F-1 scenario (the M4X-2 setup): a 2,500-SOL λ 50 % AMM v4 incumbent (Tier A) with a
/// position at the size cap, halted; an honest window adopts a CPMM candidate of `cand_sol` SOL at
/// locked share `cand_lam`. From the adoption the keeper follows `cadence` ([`KeeperModel`]) and,
/// from second `from`, the attacker sends `[deposit k·LP counter, crank_price, withdraw]` every
/// second after it; the price is flat until `w`, then genuinely ramps 4×/120 s, the keeper
/// observing every second (a near position) and trying `liquidate` and `sol_settle` every second.
fn cadence_freeze_run(
    cand_sol: u64,
    cand_lam: u64,
    w: i64,
    k: u64,
    from: Option<i64>,
    cadence: Cadence,
) -> FreezeRun {
    use common::*;
    use shortwin::events::{Liquidate, SolSettle};
    use shortwin_litesvm_tests::engine::{crank_price_ix, liquidate_ix, sol_settle_ix};
    use support::*;

    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let inc = sized(&mut env, Plant::V4, &base, 5_000, 2_500_000_000_000);
    ok(env.list_ray(&inc, VENUE_HINT_V4));
    let q0 = q_of(&env, &inc);
    let rm = env.ray_market_listed(RaySpec::new(FART, q0.saturating_mul(60)), inc);
    env.crank(&rm);
    crank_every(&mut env, &rm, 1, 5);
    let cand = sized(
        &mut env,
        Plant::Cpmm,
        &base,
        cand_lam,
        cand_sol * 1_000_000_000,
    );
    let AnyPool::Cpmm(cp) = cand else {
        unreachable!()
    };
    // Switch only if all fit: 95 % of the candidate's per-position cap.
    let fit_q = shortwin_litesvm_tests::pool_select::carried_caps(&env, &cand).0 / 100 * 95;
    let (x_inc, _) = inc
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .unwrap();
    let mut b = env.borrower();
    let nonce = OPEN_BPS
        .into_iter()
        .find_map(|bps| {
            let q = (u128::from(x_inc) * u128::from(bps) / 10_000) as u64;
            let (res, nonce) = env.open(&rm, &mut b, q.min(fit_q));
            res.is_ok().then_some(nonce)
        })
        .expect("an open");
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    let (x_c, _) = cand
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .unwrap();
    let q = env.position(&rm, &b.key(), nonce).q;
    crank_every(&mut env, &rm, 30, 5);
    window_ok(&mut env, &rm.pool, &cand);
    crank_every(&mut env, &rm, 30, 2);
    execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
    let market = rm.market();
    let rm2 = rm.on_pool(&env.svm, cand);
    let keeper = env.funded_keypair();
    let (sol, tok) = lp_needed(&env, &cp, k * cp.state(&env.svm).lp_supply);
    let who = lp::provider(&mut env, &cp, 6 * sol, 6 * tok, 0);
    let signer = who.owner.insecure_clone();
    // AIR-14: the reference cap is `D` itself (`⌊D·10⁴/λ⌋` at λ = 100 %), as the program's.
    let cap_lambda = 10_000;
    let p0 = helpers::spot(&env, &cand);
    let adopted_at = env.now();
    let mut model = KeeperModel::new(cadence);
    let mut run = FreezeRun {
        report: format!("q/x {:.2} %", q as f64 * 100.0 / x_c as f64),
        liquidated: None,
        settled: None,
        capped_seconds: 0,
        guard_clear: None,
        bursts: Vec::new(),
        liq_covers: Vec::new(),
        observed: Vec::new(),
        at_ramp: (0.0, 0.0, 0.0, 0.0, 0),
    };
    for s in 1..=(w + 400) {
        env.warp(1);
        let now = env.now();
        if s > w {
            let g = 10_000 + 30_000 * u128::try_from((s - w).min(120)).unwrap() / 120;
            if helpers::spot(&env, &cand) * 10_000 < p0 * g * 9_999 / 10_000 {
                helpers::move_to(&mut env, &cand, p0, g);
            }
        }
        // The keeper: its read, duty 2, then its observation if due.
        let (spot, root) = (helpers::spot(&env, &cand), helpers::root_xy(&env, &cand));
        model.read(now, &env.market(&market), spot, root);
        if model.due(now, &env.market(&market), spot, s > w) {
            env.crank(&rm2);
            model.last_obs = Some(now);
            run.observed.push(s);
        }
        // The attacker, after the keeper in the same second.
        if from.is_some_and(|f| s >= f) {
            let add = k * cp.state(&env.svm).lp_supply;
            ok(env.send(
                &[
                    lp::deposit_ix(&cp, &who, add),
                    crank_price_ix(&rm2),
                    lp::withdraw_ix(&cp, &who, add),
                ],
                &[&signer],
            ));
        }
        let m = env.market(&market);
        {
            let o = m.observation();
            let root = helpers::root_xy(&env, &cand);
            let cap = shortwin_math::bounds::ref_depth_cap(m.pool_d.get(), cap_lambda);
            let l_b = |c| shortwin_math::bounds::ref_liquidity(root, &o.ema, &o.ring, c).unwrap();
            // Only where an exit can read it: during the warm-up the anchor `⌊D·10⁴/λ_min⌋` sits
            // above the pool and the cap at `D` lowers it, but every exit waits for the gate.
            if m.ema_warm == 0 {
                run.capped_seconds += usize::from(l_b(Some(cap)) != l_b(None));
            }
        }
        if run.guard_clear.is_none() && m.ema_warm == 0 && m.ema_guard == 0 {
            run.guard_clear = Some(s);
        }
        if s == w {
            let o = m.observation();
            let root = helpers::root_xy(&env, &cand);
            let l_mlo = shortwin_math::observe::ring_liq_lower_median(&o.ring).unwrap();
            let cap = shortwin_math::bounds::ref_depth_cap(m.pool_d.get(), cap_lambda);
            let l_b =
                shortwin_math::bounds::ref_liquidity(root, &o.ema, &o.ring, Some(cap)).unwrap();
            let r = |x: u128| x as f64 / root as f64;
            run.at_ramp = (r(o.ema.liq), r(l_mlo), r(l_b), r(cap), o.ema.guard);
            run.report += &format!(
                "; guard clear at +{:?} s; at the ramp L_ema {:.3}, L_mlo {:.3}, L_slow {:.3}, L_b {:.3} \
                 (cap {:.3}) × √(X·Y), guard {}",
                run.guard_clear,
                r(o.ema.liq),
                r(l_mlo),
                r(o.ema.liq_slow),
                r(l_b),
                r(cap),
                o.ema.guard
            );
        }
        if s <= w || m.ema_warm > 0 {
            continue;
        }
        let res = env.send(
            &[liquidate_ix(&rm2, &keeper.pubkey(), &b.key(), nonce)],
            &[&keeper],
        );
        if let Ok(meta) = res {
            let e: Liquidate = events::one(&meta);
            run.liquidated = Some((s, e.borrower_payout));
            run.report += &format!(
                "; LIQUIDATED at +{s} s ({:.3}×), buyback {:.3} SOL, borrower {:.3} SOL",
                helpers::spot(&env, &cand) as f64 / p0 as f64,
                e.buyback_cost as f64 / 1e9,
                e.borrower_payout as f64 / 1e9
            );
            break;
        }
        let settle = sol_settle_ix(&env, &rm2, &keeper.pubkey(), &b.key(), nonce);
        if let Ok(meta) = env.send(&[settle], &[&keeper])
            && let Some(e) = events::of::<SolSettle>(&meta).first()
        {
            let owed = u128::from(q) * helpers::spot(&env, &cand) / shortwin_math::num::WAD;
            run.settled = Some(s);
            run.report += &format!(
                "; S{} at +{s} s ({:.3}×): lenders {:.3} SOL for q worth {:.3} SOL, borrower {:.3} SOL",
                e.condition,
                helpers::spot(&env, &cand) as f64 / p0 as f64,
                e.lender_amount as f64 / 1e9,
                owed as f64 / 1e9,
                e.borrower_amount as f64 / 1e9
            );
            break;
        }
    }
    run.bursts = model.bursts.iter().map(|t| t - adopted_at).collect();
    run.liq_covers = model.liq_covers.iter().map(|t| t - adopted_at).collect();
    run.report += &format!(
        "; {} burst second(s) from +{:?} s; {} liquidity-coverage trigger(s) from +{:?} s; the \
         keeper observed {} s; the depth cap bound in {} s",
        run.bursts.len(),
        run.bursts.first(),
        run.liq_covers.len(),
        run.liq_covers.first(),
        run.observed.len(),
        run.capped_seconds
    );
    run
}

/// **M4F-1**, restated by "switch only if all fit" (owner decision of 2026-09-29; keeper duties 2
/// and 5). As found, after a switch's warm-up the keeper dropped a healthy carried position to the
/// 30-s idle cadence, and an attacker filled the idle seconds with `[deposit 7·LP counter,
/// crank_price, withdraw]`: the guard cleared at ≈ +15 s, `L_ema` froze at 8× the pool's depth and
/// the genuine 4× pump ended in S1 (lenders −10.056 SOL) — on a position of ≈ 3.1 % of the new
/// pool's `X`, which the fit rule no longer carries
/// (`m4f1_an_oversized_carried_position_is_not_adopted_and_settles_in_s3`). A position that fits
/// the candidate's caps (here 95 % of its per-position cap) is liquidated whatever the freeze:
///
/// - with the shipped keeper ([`Cadence::Shipped`]: per second while the guard is set, per-slot
///   bursts on prices only, per-second coverage after a foreign liquidity on a market with
///   positions), the freeze from the adoption keeps the guard set and the reference at the lows;
///   from +40 s (after the honest clear) nothing starts a per-slot burst, but the foreign
///   liquidity starts per-second coverage (FITS-2 (b)): after the first attacker-only second
///   every second carries the keeper's honest extreme, so `L_ema` stays where that one second
///   left it, which the reference no longer reads: its depth cap is `D` (AIR-14; `⌊D·10⁴/λ_min⌋`
///   as built), so `L_b = L_live`;
/// - under the finding's cadence ([`Cadence::PreM4f`]) too.
///
/// Every run is liquidated with the lenders' `q` bought back; the pushes only delay the exit
/// (printed: the seconds after the control's liquidation).
#[test]
fn m4f1_a_deposit_freeze_on_a_fitting_carried_position_only_delays_the_liquidation() {
    const W: i64 = 120;
    let control = cadence_freeze_run(320, 9_978, W, 7, None, Cadence::Shipped);
    println!("M4F-1 control (no freeze): {}", control.report);
    let (c_at, _) = control.liquidated.expect("control liquidated");
    assert!(control.bursts.is_empty(), "{}", control.report);
    // An honest switch: after the warm-up the depth cap never lowers the reference (the exits are
    // unchanged).
    assert_eq!(control.capped_seconds, 0, "{}", control.report);

    let early = cadence_freeze_run(320, 9_978, W, 7, Some(1), Cadence::Shipped);
    println!(
        "M4F-1 freeze from the adoption, shipped keeper: {}",
        early.report
    );
    let (_, _, l_b, _, guard) = early.at_ramp;
    assert_eq!(guard, 1, "the guard is still set: {}", early.report);
    assert!(l_b <= 1.01, "the reference at the lows: {}", early.report);

    let late = cadence_freeze_run(320, 9_978, W, 7, Some(40), Cadence::Shipped);
    println!("M4F-1 freeze from +40 s, shipped keeper: {}", late.report);
    // The honest warm-up cleared the guard; no burst: only a price deviation starts one. The
    // foreign liquidity starts per-second coverage (FITS-2 (b)), kept while the freeze recurs.
    assert!(late.guard_clear.is_some_and(|t| t < 40), "{}", late.report);
    let first = *late.liq_covers.first().expect("liquidity coverage");
    assert!((40..=41).contains(&first), "{}", late.report);
    assert!(
        (first + 1..=W).all(|s| late.observed.contains(&s)),
        "per second from the trigger: {}",
        late.report
    );
    assert!(
        control.liq_covers.is_empty() && early.liq_covers.iter().all(|&t| t >= 1),
        "honest depths start nothing"
    );
    // The depth cap at `D` (AIR-14): the reference is the live pool, the freeze reaches none of it.
    let (_, _, l_b, _, _) = late.at_ramp;
    assert!(l_b <= 1.001, "L_b = L_live: {}", late.report);

    let found = cadence_freeze_run(320, 9_978, W, 7, Some(1), Cadence::PreM4f);
    println!(
        "M4F-1 freeze from the adoption, the finding's cadence: {}",
        found.report
    );

    for (tag, run) in [("early", &early), ("late", &late), ("found", &found)] {
        assert!(run.bursts.is_empty(), "{tag}: no burst: {}", run.report);
        assert!(
            run.settled.is_none(),
            "{tag}: no settlement: {}",
            run.report
        );
        let (at, payout) = run
            .liquidated
            .unwrap_or_else(|| panic!("{tag}: {}", run.report));
        assert!(
            payout > 0,
            "{tag}: the borrower keeps equity: {}",
            run.report
        );
        println!("M4F-1 {tag}: liquidated +{} s after the control", at - c_at);
        assert!(
            at >= c_at && at - c_at <= 30,
            "{tag}: a delay only: {}",
            run.report
        );
    }
}

/// **M4F-1** (on-chain defense in depth, `bounds::ref_liquidity`): the reference liquidity's EMA
/// terms are capped at the pool's locked depth `D` (AIR-14; `⌊D·10⁴/λ_min⌋`, the most depth
/// sizing credits the pool with, as built), so under the keeper cadence of the finding
/// ([`Cadence::PreM4f`]: the guard cleared by attacker-only seconds, `L_ema` frozen at 8× the
/// pool's depth) the reference reads the live pool, `L_b = L_live` — the freeze neutralised on the
/// λ 99.78 % candidate as on a λ 50 % one (the cap at `λ/λ_min` let it credit ≈ 2.0× on the
/// first). With the carried position within the candidate's caps (switch only if all fit) both
/// pumps are liquidated. (The I18 hook checks every liquidation's `b_ref` at the capped `L_b`.)
#[test]
fn m4f1_the_depth_cap_bounds_the_frozen_reference_at_the_live_pool() {
    const W: i64 = 120;
    for (sol, lam) in [(320u64, 9_978u64), (640, 5_000)] {
        let bound = lam as f64 / 10_000.0;
        let run = cadence_freeze_run(sol, lam, W, 7, Some(1), Cadence::PreM4f);
        println!(
            "M4F-1 finding's cadence, candidate {sol} SOL λ {lam}: {}",
            run.report
        );
        let (l_ema, l_mlo, l_b, cap, guard) = run.at_ramp;
        assert_eq!(
            guard, 0,
            "attacker-only seconds cleared the guard: {}",
            run.report
        );
        assert!(
            run.bursts.is_empty(),
            "price-only bursts see nothing: {}",
            run.report
        );
        assert!(
            l_ema > 7.9 && l_mlo > 7.9,
            "the freeze as found: {}",
            run.report
        );
        assert!(cap <= bound * 1.001, "cap = D = λ·√(X·Y): {}", run.report);
        assert!(l_b <= 1.001, "L_b = L_live: {}", run.report);
        assert!(run.liquidated.is_some(), "{}", run.report);
    }
}

/// **M4F-1 / M4G-3**, restated by "switch only if all fit" (owner decision of 2026-09-29): the
/// finding's market — a 2,500-SOL λ 50 % AMM v4 incumbent (Tier A) with a position at its size
/// cap, ≈ 3.1 % of the 320-SOL λ 99.78 % CPMM candidate's `X` — is never adopted. The incumbent
/// halts 900 s before the position's deadline; once the fast path is armed every proposal of the
/// candidate, and of a 400-SOL AMM v4 sibling, is refused `CarriedPositionsTooLarge`; an LP's real
/// 7× deposit held in the candidate (M4G-3) changes nothing (its locked depth `D`, which bounds
/// `X₀`, does not move); no window opens. The keeper keeps the S3 chain (a crank every 30 s), and at
/// the deadline `sol_settle` settles the position in S3 at fair value on the failed pool: the
/// lenders get `FV(P_ema, L_ema)` in full (below `V − K_s`, so uncapped: at least `q·P_ema` plus
/// the expiry fee and the rent), the borrower the rest.
#[test]
fn m4f1_an_oversized_carried_position_is_not_adopted_and_settles_in_s3() {
    use common::*;
    use shortwin::events::SolSettle;
    use shortwin_litesvm_tests::engine::sol_settle_ix;
    use support::*;

    let mut env = Env::ray_fork();
    let base = env.load_ray_pool(FART);
    let inc = sized(&mut env, Plant::V4, &base, 5_000, 2_500_000_000_000);
    ok(env.list_ray(&inc, VENUE_HINT_V4));
    let q0 = q_of(&env, &inc);
    let rm = env.ray_market_listed(RaySpec::new(FART, q0.saturating_mul(60)), inc);
    env.crank(&rm);
    crank_every(&mut env, &rm, 1, 5);
    // AIR-13: a Raydium listing starts `L_slow` at the pool's locked depth `D` — half this λ 50 %
    // pool's √(X·Y) — so the size cap waits for it: four hours of the keeper's 60-s observations
    // bring `L_slow` within 2 % of the pool (τ_slow 1 h) before the open at the cap.
    crank_every(&mut env, &rm, 60, 240);
    env.fresh_oracle();
    let (x_inc, _) = inc
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .unwrap();
    let mut b = env.borrower();
    let mut refused = Vec::new();
    let nonce = OPEN_BPS
        .into_iter()
        .find_map(|bps| {
            let q = (u128::from(x_inc) * u128::from(bps) / 10_000) as u64;
            let (res, nonce) = env.open(&rm, &mut b, q);
            if let Err(e) = &res {
                refused.push((bps, format!("{:?}", e.err)));
            }
            res.is_ok().then_some(nonce)
        })
        .unwrap_or_else(|| panic!("an open at the size cap: {refused:?}"));
    crank_every(&mut env, &rm, 1, 3);
    let pos = env.position(&rm, &b.key(), nonce);
    assert_eq!(env.market(&rm.market()).max_q_hwm, pos.q, "the mark");
    // Idle until 900 s before the deadline, then the incumbent halts.
    let wait = pos.deadline - 900 - env.now();
    env.warp(wait);
    env.crank(&rm);
    crank_every(&mut env, &rm, 1, 3);
    halt(&mut env, &rm.pool);
    let cand = sized(&mut env, Plant::Cpmm, &base, 9_978, 320_000_000_000);
    let AnyPool::Cpmm(cp) = cand else {
        unreachable!()
    };
    let v4 = sized(&mut env, Plant::V4, &base, 9_978, 400_000_000_000);
    let (x_c, _) = cand
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .unwrap();
    let (cap, _) = shortwin_litesvm_tests::pool_select::carried_caps(&env, &cand);
    println!(
        "M4F-1 oversized: q {:.2} % of the candidate's X, its per-position cap {:.2} %",
        pos.q as f64 * 100.0 / x_c as f64,
        cap as f64 * 100.0 / x_c as f64
    );
    assert!(pos.q > cap);
    let market = rm.market();
    let halted_at = env.now();
    let keeper = env.funded_keypair();
    let mut refused = 0u32;
    let mut settled = None;
    for s in 1..=1_400i64 {
        env.warp(1);
        if s % 30 == 0 {
            env.crank(&rm);
        }
        if s == 150 || s == 300 {
            // The fast path is armed (the chain ≥ 120 s): the candidates are refused.
            for c in [&cand, &v4] {
                assert_err(&env.propose(Some(&rm.pool), c), E::CarriedPositionsTooLarge);
                refused += 1;
            }
        }
        if s == 200 {
            // M4G-3: an LP's real deposit of 7× the counter, held.
            let add = 7 * cp.state(&env.svm).lp_supply;
            let (sol, tok) = lp_needed(&env, &cp, add);
            let who = lp::provider(&mut env, &cp, sol, tok, 0);
            ok(env.send(&[lp::deposit_ix(&cp, &who, add)], &[&who.owner]));
        }
        let m = env.market(&market);
        assert_eq!(m.fp_t0, 0, "+{s} s: no window");
        assert_eq!(
            m.pool_cache[0],
            shortwin_litesvm_tests::pool_select::pool_key(&rm.pool),
            "+{s} s: not adopted"
        );
        if env.now() >= pos.deadline && s % 30 == 1 {
            let settle = sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), nonce);
            let meta = ok(env.send(&[settle], &[&keeper]));
            if let Some(e) = events::of::<SolSettle>(&meta).into_iter().next() {
                settled = Some((s, e));
                break;
            }
        }
    }
    let (s, e) = settled.expect("an S3 settlement at the deadline");
    let p_ema = env.market(&market).ema_price.get();
    let owed = u128::from(pos.q) * p_ema / shortwin_math::num::WAD;
    println!(
        "M4F-1 oversized: {refused} proposals refused; S{} at +{s} s after the halt (deadline \
         +{} s): lenders {:.3} SOL (FV {:.3}; q at P_ema {:.3}), borrower {:.3} SOL",
        e.condition,
        pos.deadline - halted_at,
        e.lender_amount as f64 / 1e9,
        e.fv as f64 / 1e9,
        owed as f64 / 1e9,
        e.borrower_amount as f64 / 1e9
    );
    assert_eq!(e.condition, 3, "S3");
    assert!(
        e.fv > 0 && e.lender_amount == e.fv,
        "the lenders get FV in full"
    );
    assert!(u128::from(e.lender_amount) >= owed, "at least q at P_ema");
    assert!(e.borrower_amount > 0);
    assert_eq!(e.lender_amount + e.borrower_amount + e.k_s, pos.v);
    let m = env.market(&market);
    assert_eq!(
        (m.open_positions, m.max_q_hwm),
        (0, 0),
        "the mark reset at 0"
    );
}

/// The AIR-14 / AIR-23 shape, on the program's own exit-bound functions (`engine::bounds`, as
/// `liquidate::plan`'s expiry branch and the keeper call them): a 1 % Raydium CPMM pool (100 SOL,
/// `X` = 10¹⁵) whose LP is wholly burned (λ 100 %: `D = √(X·Y)`), a position at 95 % of the
/// 2.47 % impact cap (q/X 2.35 %), and the observation state a deposit of twice the pool, held for
/// the last 62 min before the deadline (a crank every 60 s) and pulled in the deadline second,
/// leaves: `L_slow` ≈ 2.28 D, the live pool back at `D`. Returns the config (`pool_lock_min_bps`
/// at its default 5,000), the market, the position, the pool and the first second after the
/// deadline.
mod held_deposit {
    use anchor_lang::AccountDeserialize;
    use bytemuck::Zeroable;
    use shortwin::engine::bounds;
    use shortwin::state::{GlobalConfig, Market, ShortPosition, VenueKind};
    use shortwin::venues::VenueState;
    use shortwin_math::fees::CpmmFees;
    use shortwin_math::num::WAD;
    use shortwin_math::observe::{EmaParams, Observation};
    use shortwin_math::quote::Pricing;

    pub const X: u64 = 1_000_000_000_000_000;
    pub const Y: u64 = 100_000_000_000;
    /// `√(X·Y)`: the pool's depth, all of it locked.
    pub const D: u128 = 10_000_000_000_000;

    pub fn setup() -> (GlobalConfig, Market, ShortPosition, VenueState, i64) {
        let p = u128::from(Y) * WAD / u128::from(X);
        let prm = EmaParams::DEFAULT;
        let mut obs = Observation::new(0, p, D).unwrap();
        let crank = |obs: &mut Observation, t: i64, l: u128| {
            obs.record(t, p, l).unwrap();
            obs.roll(t + 1, &prm).unwrap();
        };
        // Two honest hours at the pool's depth, then the deposit (3·D) for 62 cranks 60 s apart.
        let mut t = 60;
        while t < 7_200 {
            crank(&mut obs, t, D);
            t += 60;
        }
        for _ in 0..62 {
            crank(&mut obs, t, 3 * D);
            t += 60;
        }
        // The deadline second: the deposit is pulled, the keeper observes the drained pool.
        let deadline = t;
        crank(&mut obs, deadline, D);
        let mut market = Market::zeroed();
        market.venue = VenueKind::RaydiumCpmm as u8;
        market.pool_d.set(D);
        market.set_observation(&obs);
        let mut config = GlobalConfig::zeroed();
        config.params.pool_lock_min_bps = 5_000;
        let mut pos = ShortPosition::try_deserialize_unchecked(&mut &[0u8; 384][..]).unwrap();
        pos.q = X / 10_000 * 235;
        pos.v = 5_000_000_000;
        pos.deadline = deadline;
        pos.delta_exp_bps = 100;
        pos.exec_tol_bps = 10;
        pos.expiry_fee_bps = 200;
        pos.venue_fee_hard_cap_bps = 500;
        pos.ref_max_age_s = 2;
        pos.kappa_growth_n = 3;
        pos.kappa_liq_bps = 2_500;
        let fee = CpmmFees {
            trade: 10_000,
            creator: 0,
            creator_on_input: false,
            protocol: 0,
            fund: 0,
        };
        let st = VenueState {
            kind: VenueKind::RaydiumCpmm,
            executable: true,
            x: X,
            y: Y,
            r: Y,
            pricing: Some(Pricing::Cpmm {
                buy: fee,
                sell: fee,
            }),
            fee_ppm: 10_000,
            locked_depth: D,
            curve: None,
            pool: None,
        };
        (config, market, pos, st, deadline + 1)
    }

    /// The expiry's `(L_b, B̂_ref, max_in)` and the live buyback `B̂_live`, as `liquidate::plan`
    /// computes them (`R_due` and `K_s` 0 here).
    pub fn expiry(
        config: &GlobalConfig,
        market: &Market,
        pos: &ShortPosition,
        st: &VenueState,
        now: i64,
    ) -> (u128, u64, u64, u64) {
        let obs = market.observation();
        let cap = bounds::ref_depth_cap(config, market, VenueKind::RaydiumCpmm);
        let bound = bounds::expiry_reference(pos, &obs, st, now, cap)
            .unwrap()
            .expect("a fresh bucket");
        let b_ref = bound.b_ref.expect("B_ref");
        let max_in = bounds::max_in_exp(pos, b_ref, 0, 0)
            .unwrap()
            .expect("max_in");
        let b_live = st.buy(pos.q).expect("B_live").cost;
        (bound.l_b, b_ref, max_in, b_live)
    }
}

/// **AIR-14** (the review's I-14; the Raydium side of M6E-1): the exit reference capped its EMA
/// terms at `⌊D·10⁴/pool_lock_min_bps⌋` = 2D on Raydium, so a deposit held into a position's
/// deadline and pulled there left `L_b` at twice the drained pool — the buyback ≈ 1.2 % above the
/// reference, over `δ_exp` = 1 % — and the expiry was refused `ExitBoundNotMet` until `L_slow`
/// decayed (tens of minutes). The cap is now `D` on Raydium as on PumpSwap (`D ≤ L_live`, so
/// `L_b = L_live`): the expiry executes in the deadline's next second.
#[test]
fn air14_a_deposit_pulled_at_the_deadline_does_not_hold_a_raydium_expiry() {
    use held_deposit::*;
    let (config, market, pos, st, now) = setup();
    let obs = market.observation();
    assert!(obs.ema.liq_slow > 2 * D, "L_slow {}", obs.ema.liq_slow);
    let l_live = st.liquidity().unwrap();
    assert_eq!(l_live, D);
    let (l_b, b_ref, max_in, b_live) = expiry(&config, &market, &pos, &st, now);
    assert!(
        b_live <= max_in,
        "ExitBoundNotMet: B_live {b_live} > max_in {max_in} (B_ref {b_ref} at L_b/L_live {:.3})",
        l_b as f64 / l_live as f64
    );
    assert_eq!(l_b, l_live, "the reference reads the drained pool");
}

/// **AIR-23** (the review's lead L-7): the exit cap read `config.params.pool_lock_min_bps` at every
/// liquidation and expiry, so a Governance change (bounds 2,000–10,000) moved the reference of
/// positions already open — 2D → 5D at 2,000 — though their exit terms are locked at open (A26).
/// The cap reads no governed parameter now (`D` itself): the same position's exit bound is the
/// same whatever `pool_lock_min_bps` is.
#[test]
fn air23_a_governed_pool_lock_min_bps_moves_no_open_positions_exit_bound() {
    use held_deposit::*;
    let (mut config, market, pos, st, now) = setup();
    let at_open = expiry(&config, &market, &pos, &st, now);
    for lam in [2_000u32, 3_500, 7_500, 10_000] {
        config.params.pool_lock_min_bps = lam;
        assert_eq!(
            expiry(&config, &market, &pos, &st, now),
            at_open,
            "pool_lock_min_bps {lam}: (L_b, B_ref, max_in, B_live)"
        );
    }
}
