//! FITS-2 (b) on PumpSwap (M6R-1, the owner's decision of 2026-09-30; ARCH §7.1 duty 2, changelog
//! 56; ECON #105, §5.8 duty 2, §12.12 (12); `attacks/pumpswap_lp.py` part 3 (d)): canonical PumpSwap
//! pools take third-party LP (R-VEN Update H11), so the keeper's **per-second liquidity-anomaly
//! coverage** (duty 2: a foreign bucket liquidity off the keeper's own `√(X·(Y + VQR))` of that
//! second, on a market **with open positions**, starts `liq_cover_s` = 10 s of per-second
//! observations, extended while it recurs) runs on PumpSwap markets exactly as on Raydium — and
//! never on a market without positions. On the fork: the release short.win, the dumped PumpSwap
//! ELF, the `legacy261` pool with as much third-party LP again (λ 50 %) held by the attacker, pAMM's
//! own `deposit` / `withdraw` (`shortwin_litesvm_tests::pamm::PammLp`).
//!
//! - **The drag** (FITS-2 (b)): after each keeper tick, from the first second its idle cadence
//!   skips (the adversarial timing), the attacker sends `[withdraw all, crank_price, deposit]`. Its
//!   first wrap is the only attacker-only second: the keeper answers it with per-second coverage,
//!   every later second carries its honest `√k`, and `L_ema` holds at one v0.4.1 step
//!   (`dt/(τ + dt)` of the way to the drained depth once, ≈ ×0.92 of the pool's), at one keeper
//!   crank per attacker wrap. With the trigger off (`liq_cover_s = 0`, the keeper of #100) the
//!   same drag pulls `L_ema` far down in the idle cadence's attacker-only seconds.
//! - **No position, no coverage**: the same wraps on a PumpSwap market without positions trigger
//!   nothing and the keeper keeps its idle cadence.
//!
//! The withdraw-wrapped liquidation (FITS-2 (a), the spot leg at `max(L_live, L_ema')`) and FITX-1
//! are the program tests' (`pamm_it::lock`); the invariant hooks check every PumpSwap liquidation's
//! spot leg (I18 (e)).

use shortwin_litesvm_tests::engine::crank_price_ix;
use shortwin_litesvm_tests::pamm::{PammCoin, PammLp, PammMarket, PammUser};

use crate::common::*;

const SOL: u64 = 1_000_000_000;

/// `legacy261` listed on a config with the notional limit and the wallet cap lifted
/// (`market_max_notional` 10¹³, `wallet_cap_bps` 10⁴, as the program tests' `lock_setup`), `400·q` lent, warmed up, the boost waited out, the pool grown
/// by a small trade (to 287 bytes since the 2026-10-02 redeploy, 271 before; pAMM's `deposit` needs it), then a deposit of 1× the pool by the
/// attacker's LP held 10 min (λ 50 %: `L_ema` and `L_slow` follow the deeper pool). With
/// `position`, a position of 1 % of the pool is open.
fn market(position: bool) -> (Env, PammMarket, PammLp) {
    let (mut env, p) = Env::fork_pamm_coin_with(PammCoin::Legacy261, |a| {
        a.params.market_max_notional = 10_000_000_000_000;
        a.params.wallet_cap_bps = 10_000;
    });
    let r = p.reserves(&env.svm).expect("reserves");
    let q = r.base / 100;
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, q * 400, 60);
    let params = env.config().params;
    loop {
        let m = env.market(&pm.market());
        if pm.pool.boost_amount(&env.svm) <= params.boost_dust_lamports
            || env.now() >= m.graduated_at + i64::from(params.boost_window_s)
        {
            break;
        }
        env.warp(30);
        env.crank(&pm);
    }
    let u = PammUser::new(&mut env, &pm.pool, 10 * SOL);
    ok(u.buy(&mut env, &pm.pool, q / 1_000, u64::MAX));
    let lp = PammLp::funded_for(&mut env, &pm.pool, 10_000);
    lp.deposit(&mut env, &pm.pool, 10_000);
    for _ in 0..10 {
        env.warp(60);
        env.crank(&pm);
    }
    assert_eq!(pm.pool.locked_depth(&env.svm).0, 5_000, "λ 50 %");
    if position {
        let mut b = env.borrower();
        let (res, _) = env.open(&pm, &mut b, q);
        ok(res);
    }
    env.warp(1);
    env.crank(&pm);
    (env, pm, lp)
}

/// `L_ema / √(X·(Y + VQR))` of the market now.
fn l_ema_ratio(env: &Env, pm: &PammMarket) -> f64 {
    let m = env.market(&pm.market());
    let r = pm.pool.reserves(&env.svm).expect("reserves");
    let root = (u128::from(r.base) * r.effective_quote).isqrt();
    m.ema_liq.get() as f64 / root as f64
}

/// What one drag run saw.
#[derive(Debug, Default)]
struct Drag {
    /// Seconds (after the first wrap) the keeper observed, of `secs − 1`.
    observed_after_first: i64,
    /// The keeper's cranks and its liquidity-coverage triggers; the attacker's wraps.
    keeper_cranks: u64,
    liq_covers: usize,
    wraps: u64,
    /// `L_ema / √k` at the end.
    l_ema: f64,
}

/// `secs` seconds of the keeper ticking every second (its cadence decides whether it observes) and
/// the attacker's `[withdraw all, crank_price, deposit]` after each tick from the second the keeper
/// first skips (the adversarial timing).
fn drag(env: Env, pm: &PammMarket, lp: &PammLp, cover: Option<i64>, secs: i64) -> Drag {
    // The half-cap position is below the 1-SOL value-at-risk floor: covered all the same (duty 2's
    // liquidity-anomaly coverage runs on covered markets only; `common::covering_all`).
    let mut cfg = covering_all(KeeperConfig::default());
    if let Some(c) = cover {
        cfg.coverage.liq_cover_s = c;
    }
    let key = pm.market();
    let mut h = Harness::new(env, cfg);
    let mut d = Drag::default();
    let mut first: Option<i64> = None;
    for s in 1..=secs {
        h.idle(1);
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "+{s} s: {:?}", r.errors);
        let observed = r.observed(&key);
        d.liq_covers += r.liq_covers.iter().filter(|(m, _)| *m == key).count();
        if first.is_some() && observed {
            d.observed_after_first += 1;
        }
        if first.is_none() && observed {
            continue;
        }
        first.get_or_insert(s);
        h.with_env(|env| {
            let n = lp.lp_balance(&env.svm);
            let kp = lp.kp.insecure_clone();
            ok(env.send(
                &[
                    lp.withdraw_ix(&pm.pool, n),
                    crank_price_ix(pm),
                    lp.deposit_ix(&pm.pool, n),
                ],
                &[&kp],
            ));
        });
        d.wraps += 1;
    }
    d.keeper_cranks = h.keeper.metrics.markets.get(&key).map_or(0, |m| m.cranks);
    d.l_ema = h.with_env(|env| l_ema_ratio(env, pm));
    d
}

/// FITS-2 (b) on PumpSwap (module docs): on a market with a position the attacker's withdraw drag
/// gets one attacker-only second — `L_ema` holds at ≈ ×0.92 of the pool's depth — the keeper
/// observing every later second at one crank per wrap; with the trigger off the same drag pulls
/// `L_ema` below ×0.8 in the idle cadence's attacker-only seconds.
#[test]
fn fits2_b_the_keeper_covers_a_pumpswap_withdraw_drag_per_second() {
    const SECS: i64 = 120;
    let (env, pm, lp) = market(true);
    let covered = drag(env, &pm, &lp, None, SECS);
    let (env, pm, lp) = market(true);
    let uncovered = drag(env, &pm, &lp, Some(0), SECS);
    println!(
        "FITS-2 (b) on PumpSwap, a {SECS} s withdraw drag: with the coverage {covered:?}; the \
         trigger off {uncovered:?}"
    );
    assert!(
        covered.liq_covers > 0,
        "the foreign liquidity triggers the coverage"
    );
    assert!(
        covered.observed_after_first >= SECS - 3,
        "per second after the first wrap ({} of {})",
        covered.observed_after_first,
        SECS
    );
    assert!(
        covered.l_ema >= 0.9,
        "one attacker-only step: {}",
        covered.l_ema
    );
    assert!(
        covered.keeper_cranks <= covered.wraps + 12,
        "1:1 ({} cranks for {} wraps)",
        covered.keeper_cranks,
        covered.wraps
    );
    assert_eq!(uncovered.liq_covers, 0);
    assert!(
        uncovered.l_ema < 0.8,
        "the drag without the trigger: {}",
        uncovered.l_ema
    );
}

/// FITS-2 (b)'s scope: the liquidity trigger never runs on a PumpSwap market **without** open
/// positions (nothing there rests on the liquidity EMA); the same wraps start no coverage and the
/// keeper keeps its idle cadence.
#[test]
fn no_liquidity_coverage_on_a_pumpswap_market_without_positions() {
    const SECS: i64 = 120;
    let (env, pm, lp) = market(false);
    let d = drag(env, &pm, &lp, None, SECS);
    println!("FITS-2 (b) on PumpSwap without positions: {d:?}");
    assert_eq!(d.liq_covers, 0);
    assert!(
        d.observed_after_first < SECS / 4,
        "no per-second coverage ({} observed)",
        d.observed_after_first
    );
}
