//! ★T-PAMM-LOCK (ARCH §8.2, changelog 56; ECON #105, §12.12 (12); SPEC §14 M6R-1, the owner's
//! decision of 2026-09-30): third-party LP on canonical PumpSwap pools (R-VEN Update H11) against
//! the Raydium liquidity-push protections, on the fork with the `pamm_state` pools and pAMM's own
//! `deposit` / `withdraw` ([`PammLp`]).
//!
//! The pool's **locked depth** is Raydium's metric on its burned creation LP: `D =
//! ⌊λ_bps·isqrt(X·(Y + VQR))/10⁴⌋`, `λ_bps = ⌊(Pool.lp_supply − lp_mint.supply)·10⁴/lp_supply⌋`,
//! read with the LP mint as the ReadSet's P6 and written into `pool_d` by every executable
//! observation. It bounds the size cap (`X₀ ≤ D/√P_ema`), the tier (`R_eff ≤ locked SOL −
//! VQR⁺`) and the exit reference (`L_b`'s EMA terms `≤ D`, so `L_b = L_live` on a canonical pool)
//! at `λ_min` = `PUMPSWAP_LOCK_MIN_BPS` = 100 % — **PumpSwap credits its burned LP only**
//! (OWNER-M6D-1, the owner's decision of 2026-09-30; ARCH changelog 57, A82; ECON #106; as built
//! at changelog 56 all three divided `D` by `pool_lock_min_bps`, 50 %; row 57 first kept the
//! reference cap at `2D`, which M6E-1 found holding up expiries: part 3 (f)).
//!
//! | Test | What (`attacks/pumpswap_lp.py`) |
//! |---|---|
//! | [`u33_pool_d_is_the_burned_lp_depth_on_every_pool_role`] | U33: `pool_d = D` recomputed from the pool and its LP mint on every pool role (e/acc's `mature` at λ 92.98 %; the others 100 %, `D = √k`), equal to `pool_select::locked_depth` |
//! | [`the_lp_mint_p6_is_key_checked`] | a crank or an open naming another account at P6 fails `VenueAccountMismatch`; without P6 `BadRemainingAccounts` |
//! | [`a_deposit_and_its_withdrawal_move_neither_d_nor_the_bounds`] | a deposit of 3× the pool drops λ to 25 % and leaves `D`, the size cap's `D` term, the tier's bound and the reference cap (`D`) within 1 bp; the withdrawal restores λ |
//! | [`part_3c_a_deposit_wrapped_around_the_open_inflates_nothing`] | the deposit-at-open shape: `[deposit 3×, open_short, withdraw]` opens the honest cap (1.00×, within 1 bp: the EMA state's own reserve in place of `X_now`) |
//! | [`part_3b_a_held_deposit_lifts_no_open_the_burned_lp_sizes_it`] | a deposit of 3× held 1 h: the next open's cap is the honest one (1.00×: `X₀`'s `D` term at 100 %, OWNER-M6D-1; without the `D` term 2.84×, as built at row 56 2.00×), whatever `pool_lock_min_bps` is |
//! | [`part_3a_a_held_deposit_pulled_at_a_pump_does_not_hold_up_the_liquidation`] | M4F-1's shape for a position sized **before** the deposit (95 % of the honest cap): a deposit of 3× held 1 h and pulled as a genuine 3×/120 s pump starts; `L_b` is the drained pool's depth (the cap `D`; row 56's `2D` capped it at 2×), the position is liquidated in the control's second (as built after M6 it stayed open) |
//! | [`part_3e_a_held_deposit_sizes_nothing_and_the_pulled_pump_is_liquidated`] | **M6D-1, decided (OWNER-M6D-1, option (a))**: the deposit held 1 h no longer sizes the open (the held cap 1.000× the honest one); pulled in the first second of a genuine 5×/120 s pump, the position at 95 % of that cap is liquidated — the lenders get `Q` back and SOL on top, above `Q`'s value at the spot — like the control (the deposit kept); on a config at `pool_lock_min_bps` 2,000 in the same second (the governed `λ_min` reaches no PumpSwap term). As built at row 56 (`λ_min` 50 % in sizing) the held cap was 2.00× and the pulled pump gapped into S1: the lenders −37.9 % of `Q`, the borrower zeroed |
//! | [`part_3f_a_deposit_pulled_at_the_deadline_does_not_hold_up_the_expiry`] | **M6E-1**: a position sized on `D` (95 % of its cap) at its deadline; a deposit of 3× (or 2×) held for the last ≈ 62 min and pulled at the deadline (or 10 min before) — expired at +1 s, the control's second, with the reference at the drained pool (`L_b = L_live`); the reference cap `2D` row 57 first kept would have credited the deposit (`L_b/L_live` 2.00) and put the buyback above every `max_in` (`δ_exp` 1 %): that build refused 3,346 expiries and expired at +3,347 s |
//! | [`capacity_on_the_canonical_pool_fixtures`] | what OWNER-M6D-1 costs: every `pamm_state` pool role (and the synthetic negative-VQR pool), the program's largest open measured after the warm-up and after 2 h, against the size caps on `X₀` at `λ_min` = 100 % (equal to the program's) and at `pool_lock_min_bps` (row 56's rule); the pools whose creation LP is wholly burned lose nothing; e/acc's (λ 92.98 %) −0.12 % after the warm-up (whose `L_slow ≤ D` already sized it on `D`) and −6.13 % after 2 h (`X₀` 0.939×, tending to λ as `L_slow` reaches the live depth) |
//! | [`part_2_a_deposit_before_the_transition_is_capped_at_d_by_the_first_observation`] | `[migrate_v2, deposit 3×, crank_price]`: the transition's `lp_supply` anchor is inflated, `L_slow` keeps the curve's (≈ 1.35× the depth); the first PumpSwap observation caps it at `D` (1.000; as built 1.353 for an hour) |
//! | [`part_3d_fits2_a_withdraw_wrapped_liquidation_forces_nothing`] | FITS-2 (a) on a pool with 50 % third-party LP: a position just below its threshold, a genuine spike and retreat (the EMA leg true, the spot leg's veto); the attacker's `[withdraw all, liquidate, deposit]` each second is refused `NotLiquidatable`, although at the drained `L_live` alone (#100) the spot leg was true. FITS-2 (b), the keeper's per-second coverage against the withdraw drag, is the keeper's (`services/keeper-litesvm`, `pamm_lp::fits2_b_*`) |
//! | [`fitx1_a_held_drain_then_a_freeze_forces_what_a_withdrawal_for_good_does`] | FITX-1 (inherited residual, ECON §12.12 residual 16): an LP that withdraws for real for 20 s and then withdraw-wraps a crank and the liquidation every second forces exactly the liquidation the same LP's withdrawal for good forces (the same second and buyback cost) |

use shortwin::constants::PUMPSWAP_LOCK_MIN_BPS;
use shortwin_litesvm_tests::invariants::venue_view;
use shortwin_litesvm_tests::pamm::PammLp;
use shortwin_math::caps;
use shortwin_math::pool_select;
use shortwin_math::tier::{self, OraclePrice, Tier, TierInputs, TierVenue};
use solana_keypair::Keypair;

use crate::common::*;

const SPL261: PammCoin = PammCoin::Legacy261;

/// `pool_lock_min_bps` of the harness's config: on PumpSwap the `λ_min` of every `D` term as built
/// at ARCH row 56, and of the reference cap and a price start's anchor until M6E-1; since then no
/// PumpSwap term reads it (`PUMPSWAP_LOCK_MIN_BPS`, OWNER-M6D-1).
fn lambda_min(env: &Env) -> u32 {
    env.config().params.pool_lock_min_bps
}

/// The `legacy261` market (λ 100 %, SPL Token) on a config with the per-market notional limit and
/// the wallet cap lifted within their compiled bounds (`market_max_notional` 10¹³, `wallet_cap_bps`
/// 10⁴, as `raydium_v4_support::lift_notional`: the beta's 25-SOL notional limit binds first
/// otherwise, and these tests compare the size caps on `X₀`), `400·q` lent, warmed up, the boost
/// waited out, and the pool grown from 261 bytes by a small trade (to 287 bytes since the
/// 2026-10-02 redeploy, 271 before; pAMM's `deposit` serializes the whole pool: on a shorter one
/// it fails until a trade grows it, R-VEN Update H8). Returns it and `q`.
fn lock_setup() -> (Env, PammMarket, u64) {
    lock_setup_lambda(None)
}

/// [`lock_setup`] with `pool_lock_min_bps` set to `lambda_min` (within its compiled bounds
/// [2,000, 10,000]). Since OWNER-M6D-1 / M6E-1 it moves no PumpSwap term — the sizing, the tier,
/// the reference cap and a price start's anchor all take `λ_min` = 100 % (parts 3 (b), (e)).
fn lock_setup_lambda(lambda_min: Option<u32>) -> (Env, PammMarket, u64) {
    let (mut env, p) = Env::fork_pamm_coin_with(SPL261, |a| {
        a.params.market_max_notional = 10_000_000_000_000;
        a.params.wallet_cap_bps = 10_000;
        if let Some(l) = lambda_min {
            a.params.pool_lock_min_bps = l;
        }
    });
    let q = q_for(&env, &p);
    let pm = list_on(&mut env, &p, q.saturating_mul(400));
    wait_out_boost(&mut env, &pm);
    let u = PammUser::new(&mut env, &pm.pool, 10 * SOL);
    ok(u.buy(&mut env, &pm.pool, q / 1_000, u64::MAX));
    assert!(
        env.svm.get_account(&pm.pool.pool).expect("pool").data.len()
            >= shortwin_venue_state::pump_amm::POOL_LEN_CREATE
    );
    env.warp(1);
    crank(&mut env, &pm);
    (env, pm, q)
}

/// Whether `ixs` (with a 1.4 M CU limit) lands on a clone of the VM — nothing is kept, no hook runs.
fn lands(env: &Env, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), Option<u32>> {
    let mut all = vec![cu_limit_ix(1_400_000)];
    all.extend_from_slice(ixs);
    let tx = env.transaction(&all, signers);
    let mut svm = env.svm.clone();
    svm.send_transaction(tx)
        .map(|_| ())
        .map_err(|f| match f.err {
            solana_transaction_error::TransactionError::InstructionError(
                _,
                solana_instruction::error::InstructionError::Custom(c),
            ) => Some(c),
            _ => None,
        })
}

/// The open of `q` by `b` at its next nonce, bounds of its own off.
fn open_ix(pm: &PammMarket, b: &Borrower, q: u64) -> Instruction {
    open_short_ix(
        pm,
        &b.key(),
        b.next_nonce,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    )
}

/// The largest `q` a first open of `b` lands with now — alone, or wrapped in `[deposit lp_out,
/// open, withdraw lp_out]` by `lp` — by bisection on clones of the VM; every refusal above it is a
/// size cap.
fn max_open(env: &Env, pm: &PammMarket, b: &Borrower, wrap: Option<(&PammLp, u64)>) -> u64 {
    let attempt = |q: u64| {
        let mut ixs = Vec::new();
        let mut signers: Vec<&Keypair> = vec![&b.kp];
        if let Some((lp, n)) = wrap {
            ixs.push(lp.deposit_ix(&pm.pool, n));
            signers.push(&lp.kp);
        }
        ixs.push(open_ix(pm, b, q));
        if let Some((lp, n)) = wrap {
            ixs.push(lp.withdraw_ix(&pm.pool, n));
        }
        lands(env, &ixs, &signers)
    };
    let caps = [
        code(E::CapPositionImpact),
        code(E::CapMarketOi),
        code(E::CapWallet),
        code(E::CapUtilization),
        code(E::InsufficientInventory),
    ];
    // From 0.2 SOL of notional (the default `min_notional` is 0.1 SOL) to a quarter of the pool.
    let r = pm.pool.reserves(&env.svm).expect("reserves");
    let lo = u64::try_from(u128::from(r.base) * 200_000_000 / r.effective_quote).expect("u64");
    let (mut lo, mut hi) = (lo, r.base / 4);
    assert!(attempt(lo).is_ok(), "a tiny open lands");
    let over = attempt(hi).expect_err("a quarter of the pool is over the caps");
    assert!(over.is_some_and(|c| caps.contains(&c)), "refused {over:?}");
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        match attempt(mid) {
            Ok(()) => lo = mid,
            Err(c) => {
                assert!(
                    c.is_some_and(|c| caps.contains(&c)),
                    "q {mid}: refused {c:?}"
                );
                hi = mid;
            }
        }
    }
    lo
}

/// The size cap's conservative reserve now from the market's stored state (`L_slow`, `P_ema`,
/// `pool_d`) and the pool's live `X`: with the `D` term at `λ_min` = `lambda` (the program's
/// `PUMPSWAP_LOCK_MIN_BPS` since OWNER-M6D-1; `pool_lock_min_bps` as built at row 56), and without
/// it.
fn x0_now(env: &Env, pm: &PammMarket, lambda: u32) -> (u64, u64) {
    let m = market(env, pm);
    let x = pm.pool.reserves(&env.svm).expect("reserves").base;
    let (slow, p) = (m.liq_slow.get(), m.ema_price.get());
    let with = caps::conservative_x0(x, slow, p, Some((m.pool_d.get(), lambda))).expect("X0");
    let without = caps::conservative_x0(x, slow, p, None).expect("X0");
    (with, without)
}

// ---- U33 and the ReadSet ---------------------------------------------------------------------------

/// U33 (ARCH §12; R-VEN Update H11): the program's `pool_d` after a crank is the pool's locked
/// depth recomputed here from `Pool.lp_supply` and the LP mint's supply on `(X, Y + VQR)` — and the
/// math crate's `pool_select::locked_depth` — on every pool role: e/acc's `mature` pool with
/// third-party LP at λ 92.98 %, every other role's creation LP wholly burned (`D = √k`).
#[test]
fn u33_pool_d_is_the_burned_lp_depth_on_every_pool_role() {
    for coin in PammCoin::ALL {
        let (mut env, pm, _) = setup(coin);
        env.warp(1);
        crank(&mut env, &pm);
        let (lam, d) = pm.pool.locked_depth(&env.svm);
        let st = pm.pool.state(&env.svm);
        let r = pm.pool.reserves(&env.svm).expect("reserves");
        let y = u64::try_from(r.effective_quote).expect("u64");
        let root = (u128::from(r.base) * r.effective_quote).isqrt();
        assert_eq!(
            pool_select::locked_depth(st.lp_supply, pm.pool.lp_mint_supply(&env.svm), r.base, y),
            Ok(d),
            "{coin:?}"
        );
        assert_eq!(market(&env, &pm).pool_d.get(), d, "{coin:?}: pool_d");
        match coin {
            PammCoin::Mature => assert_eq!(lam, 9_298, "e/acc"),
            _ => assert_eq!((lam, d), (10_000, root), "{coin:?}: all burned"),
        }
        eprintln!("U33 {coin:?}: λ {lam} bps, D {d}, √k {root}");
    }
}

/// P6 is key-checked against the live `pool.lp_mint` (ARCH §3.12): another account there —
/// another mint, WSOL — fails `VenueAccountMismatch` on a crank and on an open; a ReadSet without
/// it is `BadRemainingAccounts`.
#[test]
fn the_lp_mint_p6_is_key_checked() {
    let (mut env, pm, q) = setup(SPL261);
    env.warp(1);
    let p6 = |ix: &mut Instruction, key: Pubkey| {
        let at = ix
            .accounts
            .iter()
            .position(|a| a.pubkey == pm.pool.lp_mint)
            .expect("P6 in the ReadSet");
        ix.accounts[at].pubkey = key;
    };
    for other in [
        pm.keys.mint,
        shortwin::constants::WSOL_MINT,
        Pubkey::new_unique(),
    ] {
        let mut ix = crank_price_ix(&pm);
        p6(&mut ix, other);
        assert_err(&env.send(&[ix], &[]), E::VenueAccountMismatch);
        let b = env.borrower();
        let mut ix = open_ix(&pm, &b, q);
        p6(&mut ix, other);
        let kp = b.kp.insecure_clone();
        assert_err(
            &env.send(&[cu_limit_ix(pamm::PAMM_TRADE_CU), ix], &[&kp]),
            E::VenueAccountMismatch,
        );
    }
    let mut ix = crank_price_ix(&pm);
    let at = ix
        .accounts
        .iter()
        .position(|a| a.pubkey == pm.pool.lp_mint)
        .expect("P6");
    ix.accounts.remove(at);
    assert_err(&env.send(&[ix], &[]), E::BadRemainingAccounts);
    crank(&mut env, &pm);
}

// ---- deposits and withdrawals ----------------------------------------------------------------------

/// A deposit of 3× the pool (λ 100 % → 25 %) and its withdrawal leave the locked depth `D` — and
/// with it the size cap's `D` term, the tier's locked-SOL bound and the reference cap, all at
/// `λ_min` = `PUMPSWAP_LOCK_MIN_BPS` (the cap is `D` itself) — unchanged within 1 bp (λ's
/// whole-bps floor), while `√k` quadruples and comes back.
#[test]
fn a_deposit_and_its_withdrawal_move_neither_d_nor_the_bounds() {
    let (mut env, pm, _) = lock_setup();
    let lp = PammLp::funded_for(&mut env, &pm.pool, 30_000);
    let read = |env: &mut Env| {
        env.warp(1);
        crank(env, &pm);
        let m = market(env, &pm);
        let (lam, d) = pm.pool.locked_depth(&env.svm);
        assert_eq!(m.pool_d.get(), d);
        let r = pm.pool.reserves(&env.svm).expect("reserves");
        let root = (u128::from(r.base) * r.effective_quote).isqrt();
        let p = m.ema_price.get();
        let term = u128::from(
            caps::conservative_x0(u64::MAX, u128::MAX, p, Some((d, PUMPSWAP_LOCK_MIN_BPS)))
                .expect("X0"),
        );
        let locked = shortwin_math::tier::locked_sol(d, p);
        let cap = shortwin_math::bounds::ref_depth_cap(d, PUMPSWAP_LOCK_MIN_BPS);
        assert_eq!(cap, d, "the reference cap is D itself");
        (lam, d, root, term, locked, cap)
    };
    let within_bp = |a: u128, b: u128| a.abs_diff(b) * 10_000 <= b;
    let (lam0, d0, root0, term0, locked0, cap0) = read(&mut env);
    assert_eq!(lam0, 10_000);
    let minted = lp.deposit(&mut env, &pm.pool, 30_000);
    let (lam1, d1, root1, term1, locked1, cap1) = read(&mut env);
    eprintln!(
        "deposit 3x: λ {lam0} -> {lam1} bps, √k ×{:.4}, D ×{:.6}",
        root1 as f64 / root0 as f64,
        d1 as f64 / d0 as f64
    );
    assert_eq!(lam1, 2_500);
    assert!(root1 > 3 * root0);
    for (a, b, what) in [
        (d1, d0, "D"),
        (term1, term0, "X0's D term"),
        (locked1, locked0, "the tier's locked SOL"),
        (cap1, cap0, "the reference cap"),
    ] {
        assert!(within_bp(a, b), "{what}: {b} -> {a}");
    }
    assert_eq!(lp.lp_balance(&env.svm), minted);
    lp.withdraw_all(&mut env, &pm.pool);
    let (lam2, d2, _, _, _, _) = read(&mut env);
    assert_eq!(lam2, 10_000);
    assert!(within_bp(d2, d0), "D after the withdrawal: {d0} -> {d2}");
}

/// Part 3 (c), the deposit-at-open shape (ARCH §4.6 "not inflatable by a deposit at the open"):
/// `[deposit 3× the pool, open_short, withdraw]` in one transaction opens what the same borrower
/// opens without it (1.00×) — `X₀ = min(X_now, L_slow/√P_ema, D/√P_ema)` drops the
/// deposit-inflated `X_now`, and neither `L_slow` nor `D` moves within the transaction; what is
/// left is the EMA state's own reserve `L_slow/√P_ema`, within 1 bp of the live `X` here.
#[test]
fn part_3c_a_deposit_wrapped_around_the_open_inflates_nothing() {
    let (mut env, pm, _) = lock_setup();
    env.warp(1);
    crank(&mut env, &pm);
    let lp = PammLp::funded_for(&mut env, &pm.pool, 30_000);
    let n = PammLp::lp_for(&env.svm, &pm.pool, 30_000);
    let b = env.borrower();
    let honest = max_open(&env, &pm, &b, None);
    let wrapped = max_open(&env, &pm, &b, Some((&lp, n)));
    let x = pm.pool.reserves(&env.svm).expect("reserves").base;
    eprintln!(
        "3 (c): honest cap q/X {:.3} %, wrapped in a 3x deposit {:.3} % ({} more units: the \
         EMA reprojection L_slow/√P_ema in place of X_now)",
        honest as f64 * 100.0 / x as f64,
        wrapped as f64 * 100.0 / x as f64,
        wrapped.saturating_sub(honest)
    );
    // 1.00×: the deposit lifts `X_now` out of the minimum, leaving the EMA state's own reserve
    // `L_slow/√P_ema` (within 1 bp of the live `X` here), never the deposit.
    assert!(wrapped >= honest && (wrapped - honest) * 10_000 <= honest);
    // The wrapped open at the cap executes (the hooks after every instruction).
    let kp = b.kp.insecure_clone();
    let lk = lp.kp.insecure_clone();
    let meta = ok(env.send(
        &[
            cu_limit_ix(1_400_000),
            lp.deposit_ix(&pm.pool, n),
            open_ix(&pm, &b, wrapped),
            lp.withdraw_ix(&pm.pool, n),
        ],
        &[&kp, &lk],
    ));
    let open: Open = events::one(&meta);
    assert_eq!(open.q, wrapped);
}

/// Part 3 (b), OWNER-M6D-1 (ARCH row 57, A82; ECON #106 — the decided rule): a deposit of 3× the
/// pool held for an hour (a crank every minute) pushes `L_slow` (τ = 1 h) to ≈ 2.84× the pool's
/// depth, but an open while it is held is sized on the burned LP only: `X₀`'s `D` term at `λ_min` =
/// `PUMPSWAP_LOCK_MIN_BPS` = 100 % is `D/√P_ema`, the honest reserve — 1.00× the honest cap
/// (`attacks/pumpswap_lp.py` 3 (b): 1.00×; as built at row 56 `D/(λ_min·√P)` allowed 2.00× at
/// `λ_min` 50 %, without the `D` term 2.84×). The governed `pool_lock_min_bps` no longer reaches
/// PumpSwap sizing: at 2,000 (its compiled floor, `D/λ_min` = 5× the depth) the held cap is the
/// same.
#[test]
fn part_3b_a_held_deposit_lifts_no_open_the_burned_lp_sizes_it() {
    for lam in [None, Some(2_000)] {
        let (mut env, pm, _) = lock_setup_lambda(lam);
        env.warp(1);
        crank(&mut env, &pm);
        let b = env.borrower();
        let honest = max_open(&env, &pm, &b, None);
        let x_honest = pm.pool.reserves(&env.svm).expect("reserves").base;
        let lp = PammLp::funded_for(&mut env, &pm.pool, 30_000);
        lp.deposit(&mut env, &pm.pool, 30_000);
        for _ in 0..60 {
            env.warp(60);
            crank(&mut env, &pm);
        }
        let (with_d, without_d) = x0_now(&env, &pm, PUMPSWAP_LOCK_MIN_BPS);
        let (row56, _) = x0_now(&env, &pm, lambda_min(&env));
        let held = max_open(&env, &pm, &b, None);
        let ratio = held as f64 / honest as f64;
        eprintln!(
            "3 (b), pool_lock_min_bps {}: after 1 h held X0 {:.3}x the honest pool's X (without \
             the D term {:.3}x; at pool_lock_min_bps, row 56, {:.3}x); the cap q {:.4}x the honest \
             one",
            lambda_min(&env),
            with_d as f64 / x_honest as f64,
            without_d as f64 / x_honest as f64,
            row56 as f64 / x_honest as f64,
            ratio
        );
        // The D term binds at the burned LP's own reserve (within 1 bp of the honest X), and it is
        // what binds: the held deposit's L_slow credit is out of the minimum.
        assert!((0.999..=1.001).contains(&(with_d as f64 / x_honest as f64)));
        assert!(without_d as f64 / x_honest as f64 > 2.5, "the D term binds");
        assert!((0.999..=1.001).contains(&ratio), "{ratio}: the honest cap");
        let kp = b.kp.insecure_clone();
        let meta = ok(env.send(&[cu_limit_ix(1_400_000), open_ix(&pm, &b, held)], &[&kp]));
        let open: Open = events::one(&meta);
        assert_eq!(open.q, held);
        lp.withdraw_all(&mut env, &pm.pool);
    }
}

/// One run of part 3 (a) from a snapshot: the position `(b, nonce)` open, optionally a deposit of
/// 3× the pool held an hour (a crank a minute) and withdrawn in the second a genuine 3×/120 s pump
/// starts (other users' buys, a crank every second); the keeper tries `liquidate` every second.
/// Returns the liquidation's second after the pump's start and `L_b / √k` read at its first second
/// with the program's cap `D` (`λ_min` = 100 %) and with row 56's `⌊D·10⁴/pool_lock_min_bps⌋`.
fn pump_run(
    env: &mut Env,
    pm: &PammMarket,
    b: &Pubkey,
    nonce: u64,
    deposit: bool,
) -> (Option<i64>, f64, f64) {
    let lp = deposit.then(|| {
        let lp = PammLp::funded_for(env, &pm.pool, 30_000);
        lp.deposit(env, &pm.pool, 30_000);
        lp
    });
    for _ in 0..60 {
        env.warp(60);
        crank(env, pm);
    }
    if let Some(lp) = &lp {
        env.warp(1);
        lp.withdraw_all(env, &pm.pool);
    }
    let crowd = PammUser::new(env, &pm.pool, 100_000 * SOL);
    let kp = keeper(env);
    let t0 = env.now();
    let (mut l_b, mut l_b_2d) = (0.0, 0.0);
    for s in 1..=240i64 {
        env.warp(1);
        if s <= 120 {
            // 3^(1/120) ≈ 1.009197 a second.
            ok(crowd.push_price(env, &pm.pool, 1_009_197, 1_000_000));
        }
        crank(env, pm);
        if s == 1 {
            let m = market(env, pm);
            let obs = m.observation();
            let r = pm.pool.reserves(&env.svm).expect("reserves");
            let root = (u128::from(r.base) * r.effective_quote).isqrt();
            let lb = |cap: u128| {
                shortwin_math::bounds::ref_liquidity(root, &obs.ema, &obs.ring, Some(cap))
                    .expect("L_b") as f64
                    / root as f64
            };
            let d = m.pool_d.get();
            l_b = lb(shortwin_math::bounds::ref_depth_cap(
                d,
                PUMPSWAP_LOCK_MIN_BPS,
            ));
            l_b_2d = lb(d * 10_000 / u128::from(lambda_min(env)));
        }
        match custom_code(&liquidate(env, pm, &kp, b, nonce)) {
            None => return (Some(env.now() - t0), l_b, l_b_2d),
            Some(c) => assert!(
                c == code(E::NotLiquidatable) || c == code(E::ExitBoundNotMet),
                "{c}"
            ),
        }
    }
    (None, l_b, l_b_2d)
}

/// Part 3 (a), M4F-1's shape on PumpSwap: a position at 95 % of its cap, sized **before** the
/// deposit; a deposit of 3× the pool held an hour lifts `L_ema` and `L_slow` with it, and is pulled
/// as a genuine 3×/120 s pump starts. The reference `L_b = max(L_live, min(max(L_ema', L_slow),
/// ⌊D·10⁴/λ_min⌋))` at PumpSwap's `λ_min` = 100 % (M6E-1) credits the drained pool's own depth
/// (`L_b = L_live`; at row 56's `2D` it credited 2×, as built after M6 4×: the position stayed
/// open, ECON §12.12 (12)), and the position is liquidated in the control's second (no deposit).
/// Since OWNER-M6D-1 every PumpSwap position is sized on the burned LP's depth (part 3 (b)), so
/// this is every position's shape; one sized while the deposit is held is part 3 (e)'s.
#[test]
fn part_3a_a_held_deposit_pulled_at_a_pump_does_not_hold_up_the_liquidation() {
    let (mut env, pm, _) = lock_setup();
    env.warp(1);
    crank(&mut env, &pm);
    let mut b = env.borrower();
    let cap = max_open(&env, &pm, &b, None);
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, cap / 100 * 95);
    let snap = env.svm.clone();
    let (control, lb_control, _) = pump_run(&mut env, &pm, &b.key(), nonce, false);
    env.svm = snap;
    let (attacked, lb_attacked, lb_2d) = pump_run(&mut env, &pm, &b.key(), nonce, true);
    eprintln!(
        "3 (a): control liquidated +{control:?} s (L_b/depth {lb_control:.2}); deposit 3x held 1 h, \
         pulled at the pump: +{attacked:?} s (L_b/depth {lb_attacked:.2}; at row 56's 2D cap \
         {lb_2d:.2})"
    );
    let control = control.expect("the control is liquidated");
    let attacked = attacked.expect("liquidated under the pulled deposit");
    assert_eq!(attacked, control, "liquidated in the control's second");
    // The reference reads the drained pool itself; row 56's cap credited the pulled deposit.
    assert!((lb_attacked - 1.0).abs() < 1e-9, "{lb_attacked}");
    assert!(lb_2d > 1.9, "{lb_2d}");
}

/// Part 3 (e)'s outcome of one run.
#[derive(Debug)]
enum Gap {
    /// Liquidated `at` seconds after the pump started: the tokens the buyback returned to the
    /// lenders' pool, the lenders' SOL (`MarketVault.lender_sol`'s growth: the lenders' penalty
    /// share and rent), `Q`'s value at the pool's spot then, and the borrower's payout.
    Liquidated {
        at: i64,
        tokens_back: u64,
        lender_sol: u64,
        q_value: u64,
        borrower: u64,
    },
    /// S1 `at` seconds after the pump started: the lenders' amount, `Q`'s value at the pool's spot
    /// then, the borrower's amount, the position's `V`, and the liquidations refused
    /// `ExitBoundNotMet` before it.
    S1 {
        at: i64,
        lenders: u64,
        q_value: u64,
        borrower: u64,
        v: u64,
        bound_refusals: u32,
    },
    /// Neither within the horizon.
    Open,
}

impl Gap {
    /// The second of the liquidation, if it was liquidated.
    fn liquidated_at(&self) -> Option<i64> {
        match self {
            Self::Liquidated { at, .. } => Some(*at),
            _ => None,
        }
    }
}

/// `Q`'s value in lamports at the pool's spot now.
fn q_value(env: &Env, pm: &PammMarket, q: u64) -> u64 {
    let r = pm.pool.reserves(&env.svm).expect("reserves");
    u64::try_from(u128::from(q) * r.effective_quote / u128::from(r.base)).expect("u64")
}

/// Part 3 (e)'s position: the honest cap measured, then a deposit of 3× the pool by a third-party
/// LP held an hour (a crank a minute), then an open at 95 % of the cap it allows. Returns the cap
/// held / the honest cap, the borrower's key, the nonce and the LP.
fn sized_on_a_held_deposit(env: &mut Env, pm: &PammMarket) -> (f64, Pubkey, u64, PammLp) {
    env.warp(1);
    crank(env, pm);
    let mut b = env.borrower();
    let honest = max_open(env, pm, &b, None);
    let lp = PammLp::funded_for(env, &pm.pool, 30_000);
    lp.deposit(env, &pm.pool, 30_000);
    for _ in 0..60 {
        env.warp(60);
        crank(env, pm);
    }
    let held = max_open(env, pm, &b, None);
    let (_, nonce, _) = open_ok(env, pm, &mut b, held / 100 * 95);
    (held as f64 / honest as f64, b.key(), nonce, lp)
}

/// One run of part 3 (e) from the state [`sized_on_a_held_deposit`] leaves: a genuine 5×/120 s pump
/// (other users' buys, a crank every second); with `lp`, the provider withdraws every unit in the
/// pump's first second. The keeper tries `liquidate`, then `sol_settle`, every second for 300 s.
fn gap_run(env: &mut Env, pm: &PammMarket, b: &Pubkey, nonce: u64, lp: Option<&PammLp>) -> Gap {
    let pos = env.position(pm, b, nonce);
    let crowd = PammUser::new(env, &pm.pool, 100_000 * SOL);
    let kp = keeper(env);
    let mut bound_refusals = 0;
    for s in 1..=300i64 {
        env.warp(1);
        if let (1, Some(lp)) = (s, lp) {
            lp.withdraw_all(env, &pm.pool);
        }
        if s <= 120 {
            // 5^(1/120) ≈ 1.013503 a second.
            ok(crowd.push_price(env, &pm.pool, 1_013_503, 1_000_000));
        }
        crank(env, pm);
        let before = (
            market(env, pm).tokens_free,
            env.market_vault(&pm.market()).lender_sol,
            q_value(env, pm, pos.q),
        );
        let res = liquidate(env, pm, &kp, b, nonce);
        if let Ok(meta) = &res {
            let ev: shortwin::events::Liquidate = events::one(meta);
            return Gap::Liquidated {
                at: s,
                tokens_back: market(env, pm).tokens_free - before.0,
                lender_sol: env.market_vault(&pm.market()).lender_sol - before.1,
                q_value: before.2,
                borrower: ev.borrower_payout,
            };
        }
        // Not liquidatable yet, the exit bound at the reference, or (once `V` cannot buy back at
        // all) `UseSolSettle`.
        let c = custom_code(&res).expect("a program error");
        assert!(
            [E::NotLiquidatable, E::ExitBoundNotMet, E::UseSolSettle]
                .iter()
                .any(|&e| c == code(e)),
            "+{s} s: {c}"
        );
        bound_refusals += u32::from(c == code(E::ExitBoundNotMet));
        let meta = ok(settle(env, pm, &kp, b, nonce));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            assert_eq!(ev.condition, 1, "+{s} s: S{}", ev.condition);
            return Gap::S1 {
                at: s,
                lenders: ev.lender_amount,
                q_value: q_value(env, pm, pos.q),
                borrower: ev.borrower_amount,
                v: pos.v,
                bound_refusals,
            };
        }
    }
    Gap::Open
}

/// Part 3 (e) — **M6D-1, decided: OWNER-M6D-1 option (a)** (the owner's decision of 2026-09-30;
/// ARCH changelog 57, A82; ECON #106, §12.12 (12) (e), (f); `attacks/pumpswap_lp.py` 3 (e)).
/// Part 3 (b)'s held deposit (3× the pool, an hour) would size the open; PumpSwap sizes on its
/// burned LP only, so the cap it allows is the honest one (1.000×). The position at 95 % of it
/// meets a genuine 5×/120 s pump, and the LP leaves in the pump's first second (LPs pulling
/// liquidity as volatility starts is ordinary behaviour): `L_slow` (τ 1 h) still credits the
/// deposit, but the reference's cap is `D` (M6E-1: `L_b = L_live`, the drained pool) and a
/// position sized on `D` is liquidated — the buyback returns `Q` to the lenders with SOL on top
/// (above `Q`'s value at the spot) — like the control (the deposit kept). On a config at
/// `pool_lock_min_bps` = 2,000 (its compiled floor, `D/λ_min` = 5× the depth) the same pulled
/// pump is liquidated in the same second: the governed `λ_min` reaches no PumpSwap term. (With the
/// reference cap at `2D`, which row 57 first kept, this run was liquidated in the same second too,
/// +59 s; the cap's cost showed at the expiry, part 3 (f).) As built at ARCH row 56 (`λ_min` 50 %
/// in sizing) the held cap was 2.000× and the pulled pump gapped into S1 at +124 s — 20
/// liquidations refused `ExitBoundNotMet`, the lenders 4.189 SOL against `Q`'s 6.743 SOL
/// (−37.9 %), the borrower zeroed.
#[test]
fn part_3e_a_held_deposit_sizes_nothing_and_the_pulled_pump_is_liquidated() {
    let (mut env, pm, _) = lock_setup();
    let (ratio, key, nonce, lp) = sized_on_a_held_deposit(&mut env, &pm);
    let q = env.position(&pm, &key, nonce).q;
    let snap = env.svm.clone();
    let control = gap_run(&mut env, &pm, &key, nonce, None);
    env.svm = snap;
    let pulled = gap_run(&mut env, &pm, &key, nonce, Some(&lp));
    // pool_lock_min_bps at its compiled floor, 2,000: no PumpSwap term reads it.
    let (mut env_d, pm_d, _) = lock_setup_lambda(Some(2_000));
    let (ratio_d, key_d, nonce_d, lp_d) = sized_on_a_held_deposit(&mut env_d, &pm_d);
    let pulled_d = gap_run(&mut env_d, &pm_d, &key_d, nonce_d, Some(&lp_d));
    eprintln!(
        "3 (e) OWNER-M6D-1 (λ_min 100 %: sizing, tier, reference cap D): the held cap {ratio:.4}x \
         the honest one; deposit kept {control:?}; pulled at the pump {pulled:?}"
    );
    if let Gap::Liquidated {
        tokens_back,
        lender_sol,
        q_value,
        ..
    } = pulled
    {
        eprintln!(
            "3 (e): the lenders get Q back ({tokens_back} tokens) and {:.4} SOL on top: {:.4} SOL \
             against Q's value {:.4} SOL at the spot ({:+.2} %)",
            lender_sol as f64 / 1e9,
            (q_value + lender_sol) as f64 / 1e9,
            q_value as f64 / 1e9,
            lender_sol as f64 * 100.0 / q_value as f64
        );
    }
    eprintln!(
        "3 (e) at pool_lock_min_bps 2,000: the held cap {ratio_d:.4}x; pulled at the pump \
         {pulled_d:?}"
    );
    // The held deposit sizes nothing, whatever pool_lock_min_bps is.
    assert!((0.999..=1.001).contains(&ratio), "{ratio}: the honest cap");
    assert!((0.999..=1.001).contains(&ratio_d), "{ratio_d}");
    let control_s = control.liquidated_at().expect("the control is liquidated");
    let (at, tokens_back, lender_sol, q_value, borrower) = match pulled {
        Gap::Liquidated {
            at,
            tokens_back,
            lender_sol,
            q_value,
            borrower,
        } => (at, tokens_back, lender_sol, q_value, borrower),
        Gap::S1 {
            at,
            lenders,
            q_value,
            borrower,
            v,
            bound_refusals,
        } => panic!(
            "the pulled deposit gapped a position sized on D into S1 at +{at} s (row 56's M6D-1): \
             the lenders {lenders} against Q's {q_value}, the borrower {borrower} of V {v}, \
             {bound_refusals} liquidations refused ExitBoundNotMet"
        ),
        Gap::Open => panic!("neither liquidated nor settled in 300 s"),
    };
    eprintln!("3 (e): control +{control_s} s, pulled +{at} s, borrower paid {borrower}");
    // The lenders are made whole: every token of Q back, and SOL on top of Q's value.
    assert!(tokens_back >= q, "Q back: {tokens_back} < {q}");
    assert!(lender_sol > 0, "the lenders' SOL on top of Q ({q_value})");
    // pool_lock_min_bps reaches no PumpSwap exit either: the same second at 2,000.
    assert_eq!(
        pulled_d.liquidated_at(),
        Some(at),
        "the same exit second at pool_lock_min_bps 2,000"
    );
}

/// Part 3 (f)'s expiry run (M6E-1) from a state [`expiry_setup`] leaves (`deadline − 3,720 s`):
/// optionally a third-party deposit of `units` (× 10⁴ = the pool) is made there and held (a crank
/// a minute) until `pull_before` seconds before the deadline, when every unit is withdrawn; the
/// cranks go on each minute to the deadline. From the deadline the keeper cranks and tries `expire`
/// every second for up to `horizon` seconds.
#[derive(Debug)]
struct ExpiryRun {
    /// The expiry's second after the deadline.
    at: Option<i64>,
    /// `expire` attempts refused `ExitBoundNotMet` before it.
    bound_refusals: u32,
    /// `L_b / L_live` at the first attempt (the deadline + 1 s), with the reference cap at `D` (the
    /// program's since M6E-1) and at `⌊D·10⁴/pool_lock_min_bps⌋` = 2D (the cap row 57 first kept).
    lb_d: f64,
    lb_2d: f64,
    /// At the expiry: the execution quote `B̂_live` the program compared, its `B̂_ref`, the harness's
    /// `B̂_ref` recomputed at the cap `D` (equal to the program's) and at 2D, and the 2D cap's
    /// ceiling `⌊B̂_ref(2D)·(10⁴ + δ_exp)/10⁴⌋`, which bounds any `max_in` it allowed.
    b_live: u64,
    b_ref: u64,
    b_ref_d: u64,
    b_ref_2d: u64,
    ceiling_2d: u64,
}

/// `B̂_ref` of `(b, nonce)`'s expiry now, at the reference cap `cap` (the program's
/// `engine::bounds::expiry_reference` on the market's stored observation and the pool's live
/// reserves, at the position's risk fee), and `L_b / L_live`.
fn expiry_b_ref(env: &Env, pm: &PammMarket, b: &Pubkey, nonce: u64, cap: u128) -> (u64, f64) {
    let m = market(env, pm);
    let pos = env.position(pm, b, nonce);
    let obs = m.observation();
    let view = venue_view::view(&env.svm, &m).expect("a view");
    let (x, y) = view.reserves.expect("executable");
    let l_live = shortwin_math::quote::liquidity(u128::from(x), u128::from(y)).expect("L");
    let p_ref = shortwin_math::bounds::expiry_reference(
        &obs.ema,
        &obs.ring,
        &obs.last,
        env.now(),
        &pos.exit_params(),
    )
    .expect("P_ref")
    .expect("a fresh bucket");
    let l_b =
        shortwin_math::bounds::ref_liquidity(l_live, &obs.ema, &obs.ring, Some(cap)).expect("L_b");
    let b_ref = view
        .pricing
        .expect("pricing")
        .capped(u64::from(pos.venue_fee_hard_cap_bps))
        .buy_repriced(pos.q, l_b, p_ref)
        .expect("B_ref")
        .cost;
    (b_ref, l_b as f64 / l_live as f64)
}

/// The position part 3 (f) expires: [`lock_setup`], the honest cap measured, an open at 95 % of it
/// (q/X ≈ 2.29 %: sized on `D`, as every PumpSwap open since OWNER-M6D-1), then cranks every 10 min
/// to 3,720 s before its deadline. Returns the borrower's key and the nonce.
fn expiry_setup(env: &mut Env, pm: &PammMarket) -> (Pubkey, u64) {
    env.warp(1);
    crank(env, pm);
    let mut b = env.borrower();
    let cap = max_open(env, pm, &b, None);
    let (_, nonce, _) = open_ok(env, pm, &mut b, cap / 100 * 95);
    let deadline = env.position(pm, &b.key(), nonce).deadline;
    while env.now() + 600 <= deadline - 3_720 {
        env.warp(600);
        crank(env, pm);
    }
    env.warp(deadline - 3_720 - env.now());
    crank(env, pm);
    (b.key(), nonce)
}

fn expiry_run(
    env: &mut Env,
    pm: &PammMarket,
    b: &Pubkey,
    nonce: u64,
    deposit: Option<(u64, i64)>,
    horizon: i64,
) -> ExpiryRun {
    let deadline = env.position(pm, b, nonce).deadline;
    let to = |env: &mut Env, t: i64| {
        while env.now() + 60 <= t {
            env.warp(60);
            crank(env, pm);
        }
        if env.now() < t {
            env.warp(t - env.now());
            crank(env, pm);
        }
    };
    if let Some((units, pull_before)) = deposit {
        let lp = PammLp::funded_for(env, &pm.pool, units);
        lp.deposit(env, &pm.pool, units);
        to(env, deadline - pull_before);
        lp.withdraw_all(env, &pm.pool);
    }
    to(env, deadline);
    let d = market(env, pm).pool_d.get();
    let cap_2d = d * 10_000 / u128::from(lambda_min(env));
    let delta = u128::from(env.position(pm, b, nonce).delta_exp_bps);
    let kp = keeper(env);
    let mut run = ExpiryRun {
        at: None,
        bound_refusals: 0,
        lb_d: 0.0,
        lb_2d: 0.0,
        b_live: 0,
        b_ref: 0,
        b_ref_d: 0,
        b_ref_2d: 0,
        ceiling_2d: 0,
    };
    for s in 1..=horizon {
        env.warp(1);
        crank(env, pm);
        let d = market(env, pm).pool_d.get();
        let ((b_ref_d, lb_d), (b_ref_2d, lb_2d)) = (
            expiry_b_ref(env, pm, b, nonce, d),
            expiry_b_ref(env, pm, b, nonce, cap_2d),
        );
        if s == 1 {
            (run.lb_d, run.lb_2d) = (lb_d, lb_2d);
        }
        let res = expire(env, pm, &kp, b, nonce);
        if let Ok(meta) = &res {
            let ev: shortwin::events::Expire = events::one(meta);
            run.at = Some(s);
            run.b_live = ev.b_live;
            run.b_ref = ev.b_ref;
            run.b_ref_d = b_ref_d;
            run.b_ref_2d = b_ref_2d;
            run.ceiling_2d =
                u64::try_from(u128::from(b_ref_2d) * (10_000 + delta) / 10_000).expect("u64");
            return run;
        }
        let c = custom_code(&res).expect("a program error");
        assert_eq!(c, code(E::ExitBoundNotMet), "+{s} s: {c}");
        run.bound_refusals += 1;
    }
    run
}

/// Part 3 (f) — **M6E-1** (the OWNER-M6D-1 verification; ECON #106 (2), §8.2, §12.12 (12) (f);
/// ARCH changelog 57; `attacks/pumpswap_lp.py` 3 (f) (iv)): the exit reference's cap on PumpSwap
/// is `D` itself, as every other term that credits `D` (`VenueKind::lock_min_bps`), not
/// `⌊D·10⁴/pool_lock_min_bps⌋` = 2D. A position sized on `D` at 95 % of its cap (q/X ≈ 2.29 %)
/// reaches its deadline; a third-party LP holds a deposit of 3× the pool for the last ≈ 62 min
/// (`L_slow`, τ 1 h, follows it) and withdraws it at the deadline. With the reference at `L_b =
/// L_live` (the cap `D ≤ L_live` on a canonical pool) the position expires at +1 s, as without the
/// deposit; so it does when the deposit is 2× the pool or is pulled 10 min early. At the cap 2D the
/// reference would read `L_b / L_live` = 2.00, and the live buyback, `(1 − c/2)/(1 − c) − 1` ≈
/// 1.2 % above a reference at twice its depth, fails `δ_exp` = 1 %: the harness's `B̂_ref` at 2D
/// (the program's function, checked equal to the event's at `D`) puts the buyback the program
/// executed above every `max_in` the 2D cap allowed. The build with the 2D cap (row 57 as first
/// written, before M6E-1) refused 3,346 expiries `ExitBoundNotMet` and expired at +3,347 s (pulled
/// 10 min early: 2,535 refusals, +2,536 s; 2× the pool: 1,909, +1,910 s); under this harness's I18
/// (d), which writes the PumpSwap cap out at `D`, that build now fails the hook at its first exit.
#[test]
fn part_3f_a_deposit_pulled_at_the_deadline_does_not_hold_up_the_expiry() {
    let (mut env, pm, _) = lock_setup();
    let (key, nonce) = expiry_setup(&mut env, &pm);
    let snap = env.svm.clone();
    let mut runs = Vec::new();
    for (what, deposit) in [
        ("no deposit (control)", None),
        ("3x pulled at the deadline", Some((30_000, 0))),
        ("3x pulled 600 s before", Some((30_000, 600))),
        ("2x pulled at the deadline", Some((20_000, 0))),
    ] {
        env.svm = snap.clone();
        let run = expiry_run(&mut env, &pm, &key, nonce, deposit, 4_000);
        eprintln!("3 (f) expiry, {what}: {run:?}");
        runs.push((what, deposit, run));
    }
    let control = runs[0].2.at.expect("the control expires");
    for (what, deposit, run) in &runs {
        let at = run.at.unwrap_or_else(|| {
            panic!(
                "{what}: not expired after {} ExitBoundNotMet refusals",
                run.bound_refusals
            )
        });
        assert_eq!(
            (at, run.bound_refusals),
            (control, 0),
            "{what}: expired in the control's second, no bound refusal"
        );
        // The harness's reference at the cap D is the program's.
        assert_eq!(run.b_ref_d, run.b_ref, "{what}: B_ref at D");
        // The reference at D is the live pool's: L_b = L_live.
        assert!(
            (run.lb_d - 1.0).abs() < 1e-9,
            "{what}: L_b/L_live {}",
            run.lb_d
        );
        if deposit.is_some_and(|(_, pull)| pull == 0) {
            // The deposit held into the deadline leaves L_slow at >= 2x the drained depth: a cap
            // at 2D would have credited it, and the executed buyback is above every max_in that
            // reference allowed (M6E-1's block).
            assert!(run.lb_2d > 1.9, "{what}: L_b/L_live at 2D {}", run.lb_2d);
            assert!(
                run.b_live > run.ceiling_2d,
                "{what}: B_live {} within the 2D cap's ceiling {}",
                run.b_live,
                run.ceiling_2d
            );
        }
    }
    assert_eq!(control, 1, "the control expires at the deadline + 1 s");
}

/// What OWNER-M6D-1 costs in PumpSwap open capacity on every canonical-pool fixture (the
/// `pamm_state` roles and the synthetic negative-VQR pool): each listed on a config with the
/// per-market notional limit and the wallet cap lifted (as [`lock_setup`]: the size caps on `X₀`
/// bind) and the boost waited out, the program's largest first open measured (by bisection on
/// clones of the VM) after the listing's warm-up and again after 2 h of cranks (`L_slow` at the
/// pool's live depth), against [`predicted_cap`] — the open's caps on `X₀` recomputed here — at
/// `λ_min` = `PUMPSWAP_LOCK_MIN_BPS` (which must equal the program's) and at `pool_lock_min_bps`
/// (ARCH row 56's rule). A pool whose creation LP is wholly burned (`D = √k`) loses nothing: its
/// `D/√P_ema` is its own reserve at the EMA price, which `L_slow/√P_ema` already is. e/acc's
/// `mature` pool (λ 92.98 %: 7.02 % third-party LP) is sized at λ of its live depth once `L_slow`
/// has integrated it (−6.13 % after 2 h, `X₀` 0.939×); right after a listing's warm-up the
/// warm-up's `L_slow ≤ D` already sized it nearly so (−0.12 %). The tier's `Λ` is compared at the same state for a 7-day-old market at
/// $121.20 (the listed markets are Tier C by age for 24 h).
#[test]
fn capacity_on_the_canonical_pool_fixtures() {
    let lift = |a: &mut shortwin::state::InitConfigArgs| {
        a.params.market_max_notional = 10_000_000_000_000;
        a.params.wallet_cap_bps = 10_000;
    };
    let mut rows = Vec::new();
    for (name, coin, neg) in PammCoin::ALL.iter().map(|&c| (c.role(), c, false)).chain([(
        "mature, VQR < 0 (synthetic)",
        PammCoin::Mature,
        true,
    )]) {
        let (mut env, p) = Env::fork_pamm_coin_with(coin, lift);
        if neg {
            env.plant_negative_vqr();
        }
        let q = q_for(&env, &p);
        let pm = list_on(&mut env, &p, q.saturating_mul(400));
        wait_out_boost(&mut env, &pm);
        env.warp(1);
        crank(&mut env, &pm);
        let b = env.borrower();
        for phase in ["warm-up", "+2 h"] {
            if phase == "+2 h" {
                for _ in 0..120 {
                    env.warp(60);
                    crank(&mut env, &pm);
                }
                env.warp(1);
                crank(&mut env, &pm);
            }
            let measured = max_open(&env, &pm, &b, None);
            let new = predicted_cap(&env, &pm, PUMPSWAP_LOCK_MIN_BPS);
            let old = predicted_cap(&env, &pm, lambda_min(&env));
            let (lam, _) = pm.pool.locked_depth(&env.svm);
            let row = format!(
                "{name:<28} {phase:<8} λ {:>6.2} %  X0 {:.4}x  q_cap {} → {} ({:+.2} %; measured \
                 {measured})  Λ(7 d) {:.1} → {:.1} SOL, tier {:?} → {:?}",
                lam as f64 / 100.0,
                new.x0 as f64 / old.x0 as f64,
                old.q,
                new.q,
                (new.q as f64 / old.q as f64 - 1.0) * 100.0,
                old.lambda as f64 / 1e9,
                new.lambda as f64 / 1e9,
                old.tier,
                new.tier,
            );
            eprintln!("capacity: {row}");
            rows.push((coin, neg, phase, lam, measured, new, old));
        }
    }
    for (coin, neg, phase, lam, measured, new, old) in rows {
        // The prediction is the program's cap.
        assert_eq!(
            measured, new.q,
            "{coin:?} {neg} {phase}: measured vs predicted"
        );
        assert!(new.q <= old.q && new.x0 <= old.x0, "{coin:?} {phase}");
        if lam == 10_000 {
            // All burned: nothing is lost.
            assert_eq!(new.q, old.q, "{coin:?} {neg} {phase}");
        } else {
            // e/acc: at most λ of the live depth, no less than λ − 1 %.
            let r = new.x0 as f64 / old.x0 as f64;
            assert!(
                r <= 1.0 && r >= lam as f64 / 1e4 - 0.01,
                "{coin:?} {phase}: {r}"
            );
        }
    }
}

/// The open's caps on `X₀` as [`capacity_on_the_canonical_pool_fixtures`] recomputes them.
#[derive(Clone, Copy, Debug)]
struct Capacity {
    /// `X₀`.
    x0: u64,
    /// The largest first open the caps allow.
    q: u64,
    /// `Λ = 2·R_eff` and the tier of a 7-day-old market at $121.20.
    lambda: u128,
    tier: Tier,
}

/// The largest first open on `pm` now under PumpSwap sizing at `λ_min` = `lambda` (the program's
/// `open_short` step 6, recomputed from the market's stored state, the pool and the vault): `X₀ =
/// conservative_x0(X_now, L_slow, P_ema, (pool_d, lambda))`; the per-position impact and fee-scaled
/// caps on `X₀`, the per-market impact and fee-scaled caps on `X₀ + q` (`ΣQ = 0`), utilisation,
/// the wallet cap (lifted) and the inventory, with the impact caps of the market's tier (Tier C:
/// the market is younger than a day), at the pool's live one-way fee. Also `Λ` and the tier at the
/// same state for a market 7 days old at a fresh $121.20 oracle.
fn predicted_cap(env: &Env, pm: &PammMarket, lambda: u32) -> Capacity {
    let m = market(env, pm);
    let config = env.config();
    let params = config.params;
    let r = pm.pool.reserves(&env.svm).expect("reserves");
    let (p, slow, d) = (m.ema_price.get(), m.liq_slow.get(), m.pool_d.get());
    let x0 = caps::conservative_x0(r.base, slow, p, Some((d, lambda))).expect("X0");
    let phi = pool_fees(env, &pm.pool).oneway_ppm();
    let tp = config.tiers[Tier::C.index()];
    let fits = |q: u64| {
        let oi = caps::oi_cap(x0 + q, tp.impact_mkt_bps, phi, params.fee_oi_cap_x100).expect("OI");
        q <= caps::max_q_position(x0, tp.impact_pos_bps).expect("pos")
            && caps::fee_size_cap(x0, phi, params.fee_size_cap_x100)
                .expect("fee cap")
                .is_none_or(|c| q <= c)
            && m.borrowed_q + q <= oi
            && caps::util_ok(m.tokens_free, m.borrowed_q, q, params.util_cap_bps).expect("util")
            && caps::wallet_ok(0, q, oi, params.wallet_cap_bps).expect("wallet")
            && q <= m.tokens_free
    };
    let (mut lo, mut hi) = (0u64, x0);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) { lo = mid } else { hi = mid }
    }
    let vqr = pm.pool.state(&env.svm).virtual_quote_reserves;
    let now = env.now();
    let inp = TierInputs {
        venue: TierVenue::PumpSwap {
            locked_depth: d,
            lambda_min_bps: lambda,
        },
        r_now: r
            .real_quote
            .min(u64::try_from(r.effective_quote).expect("u64")),
        y_slow: shortwin_math::quote::reprice(slow, p).map_or(0, |(_, y)| y),
        vqr: u64::try_from(vqr.max(0)).expect("u64"),
        p_ema: p,
        now,
        graduated_at: now - 7 * 86_400,
        oracle: OraclePrice {
            price: 12_120_000_000,
            conf: 0,
            expo: -8,
            publish_time: now,
        },
        oracle_stale_s: params.oracle_stale_s,
        theta_a_usd_e6: params.theta_a_usd_e6,
        theta_b_usd_e6: params.theta_b_usd_e6,
    };
    Capacity {
        x0,
        q: lo,
        lambda: tier::liquidity_lamports(&inp).expect("Λ").expect("a pool"),
        tier: tier::assign(&inp),
    }
}

// ---- the graduation ---------------------------------------------------------------------------------

/// Part 2 (ARCH §2.5): `[migrate_v2, deposit 3× the new pool, crank_price]` — the transition reads
/// the deposit-inflated `lp_supply` as its anchor, so `L_slow = min(L_slow, anchor)` keeps the
/// curve's value (≈ 1.35× the pool's depth: the curve's `√(vT·vQ)`); the first executable PumpSwap
/// observation (the keeper's crank on the pool) reads the LP mint and caps it at the burned LP's
/// depth `D` (1.000), where as built it stayed at 1.353 for an hour (τ_slow). The deposit then
/// leaves; `D` never moved.
#[test]
fn part_2_a_deposit_before_the_transition_is_capped_at_d_by_the_first_observation() {
    let (mut env, c) = Env::fork_pump_coin(fork::PumpCoin::Near);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(pump::PumpSpec::new(fork::PumpCoin::Near, q * 40), c);
    env.warp(1);
    let u = pump::CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(u.buy(&mut env, &pm.curve, rt, u64::MAX));
    env.warp(1);
    crank(&mut env, &pm);
    let slow_curve = env.market(&pm.market()).liq_slow.get();
    let migrator = env.funded_keypair();
    let mig = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
    ok(env.send(&[cu_limit_ix(600_000), mig], &[&migrator]));
    let pam = PammMarket::graduated(&env.svm, &pm);
    let payer = env.payer.pubkey();
    ok(env.send(&pamm::venue_account_ixs(&env.svm, &payer, &pam), &[]));
    let (_, d_honest) = pam.pool.locked_depth(&env.svm);
    let lp = PammLp::funded_for(&mut env, &pam.pool, 30_000);
    lp.deposit(&mut env, &pam.pool, 30_000);
    let anchor = u128::from(pam.pool.state(&env.svm).lp_supply);
    let (lam, d) = pam.pool.locked_depth(&env.svm);
    env.warp(1);
    let meta = ok(env.send(&[crank_price_ix(&pm)], &[]));
    venue_changed(&meta, VenueKind::PumpMigrating, VenueKind::PumpSwap);
    let m = env.market(&pm.market());
    let root = |env: &Env| {
        let r = pam.pool.reserves(&env.svm).expect("reserves");
        (u128::from(r.base) * r.effective_quote).isqrt()
    };
    // The transition: the anchor 4× the pool, L_slow the curve's, pool_d 0 (no LP mint read).
    assert_eq!(m.ema_liq.get(), anchor);
    assert_eq!(m.liq_slow.get(), slow_curve.min(anchor));
    assert_eq!(m.pool_d.get(), 0);
    let built = m.liq_slow.get() as f64 / d as f64;
    // The first PumpSwap observation caps L_slow at D.
    crank(&mut env, &pam);
    let m = env.market(&pm.market());
    assert_eq!(m.pool_d.get(), d);
    assert_eq!(m.liq_slow.get(), slow_curve.min(d));
    eprintln!(
        "part 2: deposit 3x between migrate_v2 and the transition (λ {lam} bps, anchor/√k {:.3}); \
         L_slow/D at the transition {built:.3} (as built for an hour), after the first PumpSwap \
         observation {:.3}",
        anchor as f64 / root(&env) as f64,
        m.liq_slow.get() as f64 / d as f64
    );
    assert!(built > 1.3, "{built}");
    assert!(m.liq_slow.get() <= d);
    // D is the burned creation LP's depth: the honest pool's (within 1 bp).
    assert!(d.abs_diff(d_honest) * 10_000 <= d_honest);
    lp.withdraw_all(&mut env, &pam.pool);
    crank_secs(&mut env, &pam, 9);
    let m = env.market(&pm.market());
    assert_eq!(m.ema_warm, 0);
    assert!(m.liq_slow.get() <= m.pool_d.get());
}

// ---- FITS-2 (a) and FITX-1 on a pool with third-party LP ---------------------------------------------

/// The keeper's view of a position (the program's legs on the harness's own venue view).
fn status(env: &Env, pm: &PammMarket, b: &Pubkey, nonce: u64) -> crate::support::Status {
    crate::support::pool_status(env, pm, b, nonce)
}

/// The liquidation spot leg as #100 judged it, at `L_live` alone (before FITS-2 (a)): `CR(V_eff,
/// B̂(Q; reprice(L_live, P_rs))) < L` at the position's risk fee.
fn spot_leg_at_live(env: &Env, pm: &PammMarket, b: &Pubkey, nonce: u64) -> bool {
    let st = status(env, pm, b, nonce);
    let m = market(env, pm);
    let pos = env.position(pm, b, nonce);
    let view = venue_view::view(&env.svm, &m).expect("a view");
    let (Some((x, y)), Some(pricing), Some(p_rs)) = (view.reserves, view.pricing, st.p_rs) else {
        return false;
    };
    let l_live = shortwin_math::quote::liquidity(u128::from(x), u128::from(y)).expect("L");
    let b_rs = pricing
        .capped(u64::from(pos.venue_fee_hard_cap_bps))
        .buy_repriced(pos.q, l_live, p_rs)
        .map(|b| b.cost);
    shortwin_math::risk::spot_leg(true, st.fresh, st.v_eff, b_rs, pos.cr_liq_bps).expect("leg")
}

/// A position on `pm` near its threshold on a pool with as much third-party LP again (λ 50 %, held
/// by the returned provider): the deposit held 10 min, the position at half the honest cap, then a
/// genuine pump in `×1.005` steps (8 s each) to the last step at which the keeper's view (the
/// program's legs at the pool's depth) still finds it healthy after a 30-s hold there. Returns the
/// provider, the borrower (nonce 0) and a pool trader.
fn near_threshold(env: &mut Env, pm: &PammMarket) -> (PammLp, Borrower, PammUser) {
    let lp = PammLp::funded_for(env, &pm.pool, 10_000);
    lp.deposit(env, &pm.pool, 10_000);
    for _ in 0..10 {
        env.warp(60);
        crank(env, pm);
    }
    assert_eq!(pm.pool.locked_depth(&env.svm).0, 5_000, "λ 50 %");
    let mut b = env.borrower();
    let cap = max_open(env, pm, &b, None);
    let (_, nonce, _) = open_ok(env, pm, &mut b, cap / 2);
    assert_eq!(nonce, 0);
    let crowd = PammUser::new(env, &pm.pool, 100_000 * SOL);
    for _ in 0..30 {
        env.warp(1);
        crank(env, pm);
    }
    let key = b.key();
    let healthy = |env: &Env| !status(env, pm, &key, 0).cr_liq();
    // The states before each step (the newest last).
    let mut snaps = Vec::new();
    loop {
        snaps.push(env.svm.clone());
        env.warp(1);
        ok(crowd.push_price(env, &pm.pool, 1_005, 1_000));
        crank(env, pm);
        for _ in 0..8 {
            env.warp(1);
            crank(env, pm);
        }
        if !healthy(env) {
            break;
        }
    }
    // Back to the last step that stays healthy through the 30-s hold (the 8 s of a step leave the
    // EMA short of the step's price: the hold can still cross the threshold, a step further back
    // does not).
    loop {
        env.svm = snaps.pop().expect("a healthy step");
        for _ in 0..30 {
            env.warp(1);
            crank(env, pm);
        }
        if healthy(env) {
            break;
        }
    }
    (lp, b, crowd)
}

/// Part 3 (d), FITS-2 (a) on PumpSwap (ECON §12.12 (5b) (v)'s shape): on a pool with 50 %
/// third-party LP a position held just below its threshold (healthy at the pool's depth) meets a
/// genuine spike above the threshold (6 s) and its retreat. For some seconds after the retreat the
/// EMA leg (the ring's lagging prices) is true while the spot leg at the pool's depth is false —
/// the spot leg's veto. The attacker, the pool's other LP, then sends `[withdraw all, liquidate,
/// deposit]` each second after the keeper's crank: the spot leg is judged at `max(L_live, L_ema')`,
/// so every attempt is refused `NotLiquidatable`, although at the drained `L_live` alone (#100's
/// rule, checked on the drained state) the spot leg was true — the forced liquidation M6R-1 closes
/// on PumpSwap. The position stays open.
#[test]
fn part_3d_fits2_a_withdraw_wrapped_liquidation_forces_nothing() {
    let (mut env, pm, _) = lock_setup();
    let (lp, b, crowd) = near_threshold(&mut env, &pm);
    let (key, nonce) = (b.key(), 0u64);
    let kp = keeper(&mut env);
    let lk = lp.kp.insecure_clone();
    // The genuine spike (+5 %, 6 s) and its retreat (the same base sold back).
    let x0 = pm.pool.reserves(&env.svm).expect("reserves").base;
    env.warp(1);
    ok(crowd.push_price(&mut env, &pm.pool, 1_050, 1_000));
    let bought = x0 - pm.pool.reserves(&env.svm).expect("reserves").base;
    crank(&mut env, &pm);
    for _ in 0..6 {
        env.warp(1);
        crank(&mut env, &pm);
    }
    env.warp(1);
    ok(crowd.sell(&mut env, &pm.pool, bought, 0));
    crank(&mut env, &pm);
    let (mut vetoed, mut drained_true) = (0, 0);
    for _ in 0..10 {
        env.warp(1);
        crank(&mut env, &pm);
        let st = status(&env, &pm, &key, nonce);
        if !(st.ema_leg && !st.spot_leg) {
            continue;
        }
        vetoed += 1;
        let n = lp.lp_balance(&env.svm);
        // #100's rule on the drained pool (a clone): the spot leg at L_live alone.
        let drained = {
            let tx = env.transaction(&[lp.withdraw_ix(&pm.pool, n)], &[&lk]);
            let mut svm = env.svm.clone();
            svm.send_transaction(tx).expect("withdraw");
            let live = std::mem::replace(&mut env.svm, svm);
            let leg = spot_leg_at_live(&env, &pm, &key, nonce);
            env.svm = live;
            leg
        };
        drained_true += usize::from(drained);
        let res = env.send(
            &[
                cu_limit_ix(1_400_000),
                lp.withdraw_ix(&pm.pool, n),
                liquidate_ix(&pm, &kp.pubkey(), &key, nonce),
                lp.deposit_ix(&pm.pool, n),
            ],
            &[&kp, &lk],
        );
        assert_err(&res, E::NotLiquidatable);
    }
    eprintln!(
        "3 (d) FITS-2 (a): {vetoed} seconds of the spot leg's veto after the spike, each wrapped \
         liquidation refused; the spot leg at the drained L_live true in {drained_true}"
    );
    assert!(
        vetoed > 0,
        "the spike leaves the EMA leg true for some seconds"
    );
    assert!(
        drained_true > 0,
        "the drained spot leg is true: #100 forced the liquidation"
    );
    assert!(env.position(&pm, &key, nonce).q > 0, "open");
}

/// FITX-1 on PumpSwap (the inherited residual; ARCH §2.6 residual 10, ECON §12.12 residual 16): on
/// the same position, the pool's other LP withdraws **for real** for 20 s (nothing foreign: every
/// observation reads the drained pool, so `L_ema` decays to it). From there (1) it stays out: the
/// honest keeper liquidates when the program accepts it; or (2) it re-deposits and withdraw-wraps a
/// crank and a liquidation every second, freezing the genuinely reached low (the keeper's coverage
/// holds `L_ema`, it cannot lift it). The wrapped liquidation forces exactly what the withdrawal
/// for good forces: the same second and the same buyback cost, on the same drained pool.
#[test]
fn fitx1_a_held_drain_then_a_freeze_forces_what_a_withdrawal_for_good_does() {
    let (mut env, pm, _) = lock_setup();
    let (lp, b, _) = near_threshold(&mut env, &pm);
    let (key, nonce) = (b.key(), 0u64);
    let kp = keeper(&mut env);
    let lk = lp.kp.insecure_clone();
    let n = lp.lp_balance(&env.svm);
    lp.withdraw_all(&mut env, &pm.pool);
    for _ in 0..20 {
        env.warp(1);
        crank(&mut env, &pm);
    }
    let snap = env.svm.clone();
    // (1) For good.
    let mut for_good = None;
    for s in 1..=30i64 {
        env.warp(1);
        crank(&mut env, &pm);
        if let Ok(meta) = liquidate(&mut env, &pm, &kp, &key, nonce) {
            let ev: shortwin::events::Liquidate = events::one(&meta);
            for_good = Some((s, ev.buyback_cost));
            break;
        }
    }
    // (2) The freeze.
    env.svm = snap;
    ok(env.send(&[lp.deposit_ix(&pm.pool, n)], &[&lk]));
    let mut frozen = None;
    for s in 1..=30i64 {
        env.warp(1);
        crank(&mut env, &pm);
        let m = lp.lp_balance(&env.svm);
        ok(env.send(
            &[
                lp.withdraw_ix(&pm.pool, m),
                crank_price_ix(&pm),
                lp.deposit_ix(&pm.pool, m),
            ],
            &[&lk],
        ));
        let res = env.send(
            &[
                cu_limit_ix(1_400_000),
                lp.withdraw_ix(&pm.pool, m),
                liquidate_ix(&pm, &kp.pubkey(), &key, nonce),
                lp.deposit_ix(&pm.pool, m),
            ],
            &[&kp, &lk],
        );
        if let Ok(meta) = res {
            let ev: shortwin::events::Liquidate = events::one(&meta);
            frozen = Some((s, ev.buyback_cost));
            break;
        }
    }
    eprintln!("FITX-1 on PumpSwap: for good (second, B) {for_good:?}; the freeze {frozen:?}");
    let (f, z) = (
        for_good.expect("liquidated for good"),
        frozen.expect("the freeze forces it"),
    );
    assert_eq!(f, z, "the same second and buyback cost");
}
