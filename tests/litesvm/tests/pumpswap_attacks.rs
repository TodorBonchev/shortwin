//! ECON §12's attack patterns on **PumpSwap** (M10 G-C-PSwap, the full table; G-D-real and G-E4
//! on PumpSwap): `sandwich.py` / `fixed_delta.py`, the held model of §12.6, `ema_atomic.py`,
//! `ema_near.py`, `ema_atomic_multi.py` (a) and (b), a liquidation sandwiched while a third-party
//! LP withdraws inside the bundle (FITS-2's shape), round 3 — `arming.py`, `refute_push.py` (a),
//! (b), (c) and `fv_push.py`'s pushes, flips and 1–6 attacker-only seconds around the Emergency
//! flag's S3 — and V2-4's `[deposit LP → sol_settle → withdraw LP]` every 20 s, against the
//! release short.win on the **dumped** PumpSwap, pump and pump-fees programs and the canonical
//! `mature` pool of `tests/fixtures/` (e/acc, Token-2022, its boost spent, 7.02 % third-party LP;
//! `shortwin_litesvm_tests::pamm`). The attacker trades the real pool (`buy` / `sell`) and lands
//! its bundles as single transactions; every instruction of every bundle goes through `Env::send`,
//! so the invariant hooks run after each — on PumpSwap markets with the harness's venue view (I13
//! re-quotes, I16 route rules, I18's PumpSwap clauses: the exit reference capped at the burned
//! depth, the spot leg at `max(L_live, L_ema')`).
//!
//! **No oracle, no sim market.** As on CPMM (`raydium_cpmm_attacks.rs`), the program's own
//! decision is read on a clone of the VM (`venue_attacks_support.rs`) and the claims are
//! structural: the I11 spend bound, the lenders' and protocol's legs against the honest run's, the
//! attacker's extra against the borrower's loss, `NotLiquidatable` on the final bundles,
//! settlements within `fv_dev_bps` of the honest one and never an S1 the honest run does not
//! reach, S1 delays of seconds. ECON's sims have no PumpSwap market with these attacks, so no
//! figure is compared to the lamport; `-- --nocapture` prints them (`tests/litesvm/README.md`).
//! A graduated `D@80 %` market (`graduation_support.rs`) is not ported: its pool is a fresh
//! PumpSwap pool of the same program, and the graduation E2E already exits on it.
//!
//! The market lifts the per-market sizing limits within their compiled bounds
//! (`market_max_notional`, `wallet_cap_bps`), so the victim holds one position at the SPEC
//! per-position cap on the burned LP's depth (OWNER-M6D-1).
//!
//! | Test | ECON script | What holds on PumpSwap |
//! |---|---|---|
//! | [`exit_sandwich_on_a_liquidation`] | `sandwich.py`, `fixed_delta.py` (d) (atomic) | the best `[buy dX, liquidate, sell dX]` spends at most `⌊B̂_ref·(1 + δ)⌋`; lenders and protocol get the honest legs; the attacker's extra comes out of the borrower's remainder only, at most `δ·B̂_ref`; waiting 4 s keeps δ |
//! | [`exit_sandwich_on_an_expiry`] | `sandwich.py` (expiry, atomic) | rent senior, the expiry fee within 0.1 % of the honest one, the spend within `δ_exp` |
//! | [`held_pushes_before_a_liquidation_take_from_the_borrower_only`] | `sandwich.py` held, §12.6 | 1 s at +2.5 % and 3 s at +15 % held before the sandwich: I11 over the lifted reference, the lenders' legs at least the honest ones while the borrower keeps a remainder, the extra out of the borrower only (O19's residual) |
//! | [`atomic_bundles_cannot_liquidate_a_fresh_position`] | `ema_atomic.py` (atomic) | `[pump, crank_price, dump]` every second at ×1.25 … ×3: the EMA never rises above the honest evidence; the honest keeper's and the attacker's `[pump, liquidate, dump]` refused `NotLiquidatable` every second |
//! | [`held_pushes_on_a_fresh_position_do_not_pay`] | `ema_atomic.py` (held) | attacker-only seconds holding the pool at ×2.5 until the fresh position is liquidatable, then the best sandwich: I11, and the held seconds cost more than the sandwich and the keeper legs bring (net < 0) |
//! | [`atomic_bundles_cannot_push_near_threshold_positions`] | `ema_near.py` (atomic) | the same after honest rises with the EMA caught up 5–15 % short of the price that liquidates |
//! | [`atomic_bundles_cannot_force_the_oi_cap_basket`] | `ema_atomic_multi.py` (a), (b) (atomic) | the market filled to its OI cap: no basket position is liquidated by `[pump, liquidate, dump]`; `[pump past the S1 boundary, sol_settle × every position, dump]` arms nothing |
//! | [`a_withdraw_wrapped_sandwich_stays_within_the_bound`] | FITS-2's shape (ECON §12.12 (5b)) | a third-party LP holding half the pool's depth withdraws inside the attacker's `[withdraw LP, buy dX, liquidate, sell dX, deposit LP]`: the spend within `⌊B̂_ref·(1 + δ)⌋`, the reference not below the honest one, the lenders' legs at least the honest ones (to 10 lamports), the extra — SOL, tokens and LP marked at the honest spot — out of the borrower only |
//! | [`round_3_settlement_attacks_delay_s1_by_seconds_only`] | `arming.py`, `refute_push.py` (a), (b), (c) | after a gap 5 % past the S1 boundary: a borrower bundle every 20 s changes nothing; a refutation push delays S1 ≤ 4 s, push-then-freeze ≤ 8 s, a spot-leg veto of 9–20 s ≤ 6 s |
//! | [`emergency_flag_s3_amount_pushes_and_flips`] | `fv_push.py` (E) (R3A-1, R4B-2, R4A-9) | the Emergency flag set 30 min before a healthy position's deadline; one second before the honest S3: a push up or down leaves an S3 within `fv_dev_bps`; the flip bundle after 1–2 pushed seconds, push-then-freeze, 1–4 consecutive attacker-only seconds or pushes with honest seconds between: no S1 |
//! | [`a_contrary_observation_does_not_change_pprime`] | `fv_push.py` (V), the `p′` veto (R4A-3) | the Emergency flag and a genuine ×2.3 move: one contrary low observation in the second before the keeper's S3 leaves `p′` at the penalty on both corners and the lenders' amount, lump and borrower's remainder unchanged to the lamport |
//! | [`lp_around_settle_every_20_s_changes_nothing`] | V2-4 / R4B-2 (G-E4) | `[deposit LP, sol_settle, withdraw LP]` (pAMM's own `deposit` / `withdraw`) from the gap past the S1 boundary every 20 s and in each of the 5 s before the honest S1, until it: no wrapped settlement settles or disarms; S1 in the honest second with the honest box and amounts |

#![allow(clippy::result_large_err)]

#[path = "venue_attacks_support.rs"]
mod support;

use support::*;

/// The `mature` pool's market with the sizing limits lifted and 10 % of the pool's tokens lent.
fn mature() -> Lab {
    Lab::pswap(PammCoin::Mature, |x, _| x / 10, lift_notional)
}

#[test]
fn exit_sandwich_on_a_liquidation() {
    liquidation_sandwich(&mature, "PumpSwap mature");
}

#[test]
fn exit_sandwich_on_an_expiry() {
    expiry_sandwich(&mature, "PumpSwap mature");
}

#[test]
fn held_pushes_before_a_liquidation_take_from_the_borrower_only() {
    held_sandwich(&mature, "PumpSwap mature");
}

#[test]
fn atomic_bundles_cannot_liquidate_a_fresh_position() {
    atomic_fresh(&mature, "PumpSwap mature");
}

#[test]
fn held_pushes_on_a_fresh_position_do_not_pay() {
    held_fresh(&mature, "PumpSwap mature");
}

#[test]
fn atomic_bundles_cannot_push_near_threshold_positions() {
    let (_, _, _, k_liq) = setup_liq(&mature, "PumpSwap mature");
    atomic_near(&mature, "PumpSwap mature", k_liq);
}

#[test]
fn atomic_bundles_cannot_force_the_oi_cap_basket() {
    basket_attacks(
        &|| Lab::pswap(PammCoin::Mature, |x, _| x / 5, lift_notional),
        "PumpSwap mature",
    );
}

/// FITS-2's shape on a liquidation (ECON §12.12 (5b); the matrix's PumpSwap case): a third party
/// deposits half the pool's depth again (pAMM's own `deposit`) before the position is opened and
/// holds it; at the first liquidatable second the attacker-keeper, owning that LP, lands `[withdraw
/// all LP, buy dX, liquidate, sell dX, deposit it back]` — the exit judged on the thinned pool,
/// the front-run the largest that still lands. The spend stays within `⌊B̂_ref·(1 + δ)⌋` (I11),
/// the reference at least the honest keeper's (the withdrawal cannot lower it: the reference
/// reads `max(L_live, …)` and is capped at the burned depth), the lenders' and protocol's legs at
/// least the honest ones (`support::assert_lenders_keep`), and the attacker's extra over an honest keeper
/// comes out of the borrower's remainder only. The extra is marked to market: the attacker's SOL,
/// tokens and LP share of the pool's vaults, all valued at the honest run's post-liquidation spot,
/// against the same holdings after the honest keeper's liquidation, minus the honest keeper's
/// legs (its LP round trip ends with the same LP and fewer SOL but more tokens: the liquidation
/// bought tokens with SOL).
#[test]
fn a_withdraw_wrapped_sandwich_stays_within_the_bound() {
    let label = "PumpSwap mature, LP withdrawn inside the bundle";
    let lp_cell = std::cell::RefCell::new(None);
    let make = || {
        let mut lab = mature();
        let pool = lab.pm().pool;
        let lp = PammLp::funded_for(&mut lab.env, &pool, 5_000);
        lp.deposit(&mut lab.env, &pool, 5_000);
        lab.tick(5);
        *lp_cell.borrow_mut() = Some(lp);
        lab
    };
    let (mut lab, b, nonce, _) = setup_liq(&make, label);
    let lp = lp_cell.borrow_mut().take().unwrap();
    let pool = lab.pm().pool;
    let delta = lab.pos(&b.key(), nonce).delta_liq_bps;
    let penalty = lab.pos(&b.key(), nonce).penalty_bps;
    // The attacker is the LP's owner: its WSOL and base accounts are the LP's.
    let a = Actor {
        kp: lp.kp.insecure_clone(),
        wsol: lp.wsol,
        token: lp.base,
    };
    lab.fund(&a, 100_000 * SOL, 0);
    let held = lp.lp_balance(&lab.env.svm);
    // The attacker's holdings at a price `p` (WAD): SOL, tokens and its LP share of the vaults.
    let wealth = |lab: &Lab, p: u128| {
        let svm = &lab.env.svm;
        let pool = &lab.pm().pool;
        let supply = u128::from(pool.state(svm).lp_supply);
        let vb = u128::from(read::token_amount(svm, &pool.base_vault).unwrap_or(0));
        let vq = u128::from(read::token_amount(svm, &pool.quote_vault).unwrap_or(0));
        let toks = u128::from(lab.tokens(&a)) * p / WAD;
        let lp_val = u128::from(held) * (vq + vb * p / WAD) / supply;
        lab.sol_of(&a) + i128::try_from(toks + lp_val).unwrap()
    };
    let saved = lab.env.svm.clone();
    let h: Liquidate = events::one(&lab.exit(Which::Liquidate, &b.key(), nonce));
    let honest_keeper = i128::from(h.penalty_keeper + h.keeper_topup);
    let p_h = lab.spot();
    let w_h = wealth(&lab, p_h);
    lab.env.svm = saved.clone();
    let wrapped = |lab: &Lab, dx: u64| {
        let mut ixs = vec![cu_limit_ix(BUNDLE_CU), lp.withdraw_ix(&pool, held)];
        let inner = lab.sandwich_ixs(&a, Which::Liquidate, &b.key(), nonce, dx);
        ixs.extend(inner.into_iter().skip(1));
        ixs.push(lp.deposit_ix(&pool, held));
        ixs
    };
    // The largest front-run after which the wrapped bundle lands (bisection on clones).
    let lands = |lab: &Lab, dx: u64| lab.try_send(&wrapped(lab, dx), &[&a.kp]).is_ok();
    assert!(
        lands(&lab, 0),
        "the wrapped liquidation lands without a front-run"
    );
    let (x, _) = lab.reserves();
    let x = u64::try_from(x).unwrap();
    let (mut lo, mut hi) = (0u64, x / 200);
    while lands(&lab, hi) {
        lo = hi;
        hi += x / 200;
    }
    while hi - lo > (x / 1_000_000).max(1) {
        let mid = lo + (hi - lo) / 2;
        if lands(&lab, mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let ixs = wrapped(&lab, lo);
    let t0 = lab.tokens(&a);
    let (meta, pnl) = lab.send_as(&a, &ixs);
    let t1 = lab.tokens(&a);
    assert!(t1 >= t0, "the attacker keeps its tokens");
    assert_eq!(lp.lp_balance(&lab.env.svm), held, "the LP deposited back");
    let ev: Liquidate = events::one(&meta);
    assert_spend_bound(&ev, delta, label);
    assert!(
        ev.b_ref >= h.b_ref,
        "the withdrawal does not lower the reference"
    );
    assert_lenders_keep(&h, &ev, penalty, label);
    let (hl, al) = (lenders_take(&h), lenders_take(&ev));
    let extra = wealth(&lab, p_h) - w_h - honest_keeper;
    let loss = i128::from(h.borrower_payout) - i128::from(ev.borrower_payout);
    eprintln!(
        "{label}: dX = {lo}, B {:.4} → {:.4} SOL (B̂_ref {:.4} → {:.4}), the attacker's SOL {} \
         and tokens +{} (its LP round trip), extra marked at the honest spot {} SOL, borrower loss \
         {} SOL, lenders + protocol {hl} → {al} lamports",
        h.buyback_cost as f64 / 1e9,
        ev.buyback_cost as f64 / 1e9,
        h.b_ref as f64 / 1e9,
        ev.b_ref as f64 / 1e9,
        sol4(pnl),
        t1 - t0,
        sol4(extra),
        sol4(loss)
    );
    assert!(loss > 0, "the attacked liquidation spends more");
    assert!(
        extra < loss,
        "the attacker's extra comes out of the borrower only"
    );
}

#[test]
fn round_3_settlement_attacks_delay_s1_by_seconds_only() {
    round_3_s1(&mature, "PumpSwap mature");
}

/// `fv_push.py` (E) on PumpSwap (the mock's `emergency_flag_s3_amount_pushes`; G-D-real): a
/// position at the cap, the Emergency multisig sets the market's route-broken flag 30 min before
/// its deadline (O15), the keeper cranks every 30 s, then every second for the last minute; one
/// second before the honest S3 at the deadline (`fv_cap`: the lenders' take capped at FV),
/// `support::amount_pushes_and_flips`: a push up or down settles S3 within `fv_dev_bps`, and no
/// flip bundle — after 1–2 pushed seconds, push-then-freeze, 1–4 consecutive attacker-only seconds
/// or pushes with honest seconds between — makes it S1.
#[test]
fn emergency_flag_s3_amount_pushes_and_flips() {
    let (mut lab, b, nonce) = capped(&mature, "PumpSwap mature");
    lab.tick(5);
    let deadline = lab.pos(&b.key(), nonce).deadline;
    let to = deadline - 1_800 - lab.env.now();
    lab.env.warp(to);
    lab.crank();
    lab.set_route_broken(true);
    while lab.env.now() + 30 < deadline - 60 {
        lab.env.warp(30);
        lab.crank();
    }
    let to = deadline - 60 - lab.env.now();
    lab.env.warp(to);
    let mut ready = None;
    for _ in 0..120 {
        let saved = lab.env.svm.clone();
        if let Some(ev) = lab.keeper_second(&b.key(), nonce).ev {
            ready = Some((saved, ev));
            break;
        }
    }
    let (saved, honest) = ready.expect("the honest keeper's S3");
    assert_eq!(honest.condition, 3, "S3 under the flag");
    assert!(lab.env.now() >= deadline, "at the deadline");
    amount_pushes_and_flips(
        &mut lab,
        &b.key(),
        nonce,
        &saved,
        &honest,
        "PumpSwap Emergency-flag S3",
    );
}

#[test]
fn a_contrary_observation_does_not_change_pprime() {
    pprime_veto(&mature, "PumpSwap mature");
}

/// V2-4's liquidity variant on PumpSwap (G-E4): `[deposit LP, sol_settle, withdraw LP]` with
/// pAMM's own `deposit` / `withdraw` of the pool's whole LP supply (its depth doubled inside the
/// transaction) from the gap past the S1 boundary, every 20 s and in each of the 5 seconds before
/// the honest S1, until it (`support::lp_around_settle`): S1 in the honest second with the honest
/// box and amounts.
#[test]
fn lp_around_settle_every_20_s_changes_nothing() {
    lp_around_settle(&mature, "PumpSwap mature", &|lab, settle| {
        let pool = lab.pm().pool;
        let who = PammLp::funded_for(&mut lab.env, &pool, 10_000);
        let lp = PammLp::lp_for(&lab.env.svm, &pool, 10_000);
        let ixs = vec![
            cu_limit_ix(BUNDLE_CU),
            who.deposit_ix(&pool, lp),
            settle,
            who.withdraw_ix(&pool, lp),
        ];
        (ixs, who.kp)
    });
}
