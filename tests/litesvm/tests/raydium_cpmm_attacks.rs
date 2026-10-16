//! ECON §12's attack patterns on Raydium **CPMM** (M10 G-C-CPMM, the reduced port; G-D-real
//! and G-E4 on CPMM): the scripts' structure — `sandwich.py` / `fixed_delta.py`, the held model
//! of §12.6, `ema_atomic.py` (atomic and held), `ema_near.py`, `ema_atomic_multi.py` (a) and
//! (b), `fv_push.py`'s pushed seconds and flips and its `p′` veto, and V2-4's `[deposit LP → sol_settle → withdraw LP]` every 20 s —
//! against the release short.win on the **dumped** CPMM program (its 2026-10 redeploy at slot
//! 452,376,460) and the dumped USELESS pool `Q2sPHPdU…` (SPL, fee on the input), the liquidation
//! sandwich also on the creator-fee pool `FQd8z3…`. The attacker trades the real pool
//! (`swap_base_output` / `swap_base_input`) and lands its bundles as single transactions; every
//! instruction of every bundle goes through `Env::send`, so the invariant hooks run after each.
//!
//! **A reduced port.** CPMM has no differential oracle (AMM v4's `raydium_v4_attacks.rs` and the
//! curve's `pump_curve_attacks.rs` have one): the program's own decision is read on a clone of the
//! VM (`venue_attacks_support.rs`), and the claims are structural — the I11 spend bound, the
//! lenders' and protocol's legs against the honest run's, the attacker's extra against the
//! borrower's loss, `NotLiquidatable` on the final bundles, settlements within `fv_dev_bps` of the
//! honest one and never an S1 the honest run does not reach. No ECON sim market is a CPMM pool, so
//! no figure is compared to the lamport; `-- --nocapture` prints them (`tests/litesvm/README.md`).
//!
//! The market lifts the per-market sizing limits within their compiled bounds
//! (`market_max_notional`, `wallet_cap_bps`), so the victim holds one position at the SPEC
//! per-position cap — the size at which ECON prices the attacks.
//!
//! | Test | ECON script | What holds on the real venue |
//! |---|---|---|
//! | [`exit_sandwich_on_a_liquidation`] | `sandwich.py`, `fixed_delta.py` (d) (atomic) | on USELESS and the creator-fee pool: the attacker-keeper's best `[buy dX, liquidate, sell dX]` spends at most `⌊B̂_ref·(1 + δ)⌋`; lenders and protocol get the honest legs; the attacker's extra comes out of the borrower's remainder only, at most `δ·B̂_ref`; waiting 4 s keeps δ |
//! | [`exit_sandwich_on_an_expiry`] | `sandwich.py` (expiry, atomic) | rent senior, the expiry fee within 0.1 % of the honest one, the spend within `δ_exp`, the extra out of the borrower only |
//! | [`held_pushes_before_a_liquidation_take_from_the_borrower_only`] | `sandwich.py` held, §12.6 | 1 s at +2.5 % and 3 s at +15 % held before the sandwich: I11 over the lifted reference, the lenders' legs at least the honest ones while the borrower keeps a remainder, the extra out of the borrower only (O19's residual) |
//! | [`atomic_bundles_cannot_liquidate_a_fresh_position`] | `ema_atomic.py` (atomic) | `[pump, crank_price, dump]` every second at ×1.25 … ×3 next to the honest crank: the EMA never rises above the honest evidence, the honest keeper's and the attacker's own `[pump, liquidate, dump]` are refused `NotLiquidatable` every second |
//! | [`held_pushes_on_a_fresh_position_do_not_pay`] | `ema_atomic.py` (held) | attacker-only seconds holding the pool at ×2.5 until the fresh position is liquidatable, then the best sandwich: I11, and the held seconds cost more than the sandwich and the keeper legs bring (net < 0) |
//! | [`atomic_bundles_cannot_push_near_threshold_positions`] | `ema_near.py` (atomic) | the same after honest rises with the EMA caught up 5–15 % short of the price that liquidates |
//! | [`atomic_bundles_cannot_force_the_oi_cap_basket`] | `ema_atomic_multi.py` (a), (b) (atomic) | the market filled to its OI cap: no basket position is liquidated by `[pump, liquidate, dump]`; `[pump past the S1 boundary, sol_settle × every position, dump]` arms nothing |
//! | [`pushed_seconds_cannot_flip_the_drained_s2`] | `fv_push.py` (R3A-1, R4B-2, R4A-9) | on `raydium_cpmm_e2e.rs`'s drained pool one second before its honest S2: a push up or down leaves an S2 within `fv_dev_bps`; the flip bundle after 1–2 pushed seconds, after push-then-freeze, after 1–4 consecutive attacker-only seconds and after pushes with honest seconds between: no S1 |
//! | [`a_contrary_observation_does_not_change_pprime`] | `fv_push.py` (V), the `p′` veto (R4A-3) | the Emergency flag and a genuine ×2.3 move: one contrary low observation in the second before the keeper's S3 leaves `p′` at the penalty on both corners and the lenders' amount, lump and borrower's remainder unchanged to the lamport |
//! | [`lp_around_settle_every_20_s_changes_nothing`] | V2-4 / R4B-2 (G-E4) | `[deposit LP, sol_settle, withdraw LP]` (the real CPMM `deposit` / `withdraw`) from the gap past the S1 boundary every 20 s and in each of the 5 s before the honest S1, until it: no wrapped settlement settles or disarms; S1 in the honest second with the honest box (`L_hi`, `P_hi`) and amounts |

#![allow(clippy::result_large_err)]

#[path = "venue_attacks_support.rs"]
mod support;

use shortwin_litesvm_tests::lp;
use support::*;

const USELESS: RayFixture = RayFixture::Cpmm(CpmmFixture::Useless);
const CREATOR_FEE: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorFee);

/// The market on USELESS with the sizing limits lifted and 10 % of the pool's tokens lent.
fn useless() -> Lab {
    Lab::cpmm(USELESS, |x, _| x / 10, lift_notional)
}

/// The same on the creator-fee pool.
fn creator_fee() -> Lab {
    Lab::cpmm(CREATOR_FEE, |x, _| x / 10, lift_notional)
}

#[test]
fn exit_sandwich_on_a_liquidation() {
    liquidation_sandwich(&useless, "USELESS");
    liquidation_sandwich(&creator_fee, "creator-fee pool");
}

#[test]
fn exit_sandwich_on_an_expiry() {
    expiry_sandwich(&useless, "USELESS");
}

#[test]
fn held_pushes_before_a_liquidation_take_from_the_borrower_only() {
    held_sandwich(&useless, "USELESS");
}

#[test]
fn atomic_bundles_cannot_liquidate_a_fresh_position() {
    atomic_fresh(&useless, "USELESS");
}

#[test]
fn held_pushes_on_a_fresh_position_do_not_pay() {
    held_fresh(&useless, "USELESS");
}

#[test]
fn atomic_bundles_cannot_push_near_threshold_positions() {
    let (_, _, _, k_liq) = setup_liq(&useless, "USELESS");
    atomic_near(&useless, "USELESS", k_liq);
}

#[test]
fn atomic_bundles_cannot_force_the_oi_cap_basket() {
    basket_attacks(
        &|| Lab::cpmm(USELESS, |x, _| x / 5, lift_notional),
        "USELESS",
    );
}

// ---- fv_push.py on a drained pool (G-D-real) ----------------------------------------------------

/// Out-of-model drain (`raydium_cpmm_e2e.rs`): both pricing reserves scaled so the token reserve
/// is `x_target`, at an unchanged price; the fee counters stay excluded as before.
fn drain_to(lab: &mut Lab, x_target: u64) {
    let rm = lab.rm();
    let p = *rm.cpmm();
    let st = p.state(&lab.env.svm);
    let (x, y) = rm.reserves(&lab.env.svm).expect("reserves");
    let y_target = (u128::from(y) * u128::from(x_target) / u128::from(x)) as u64;
    let (tok_vault, sol_vault) = rm.vaults();
    let (c_sol, c_tok) = if p.sol_is_token0 {
        (
            st.protocol_fees_token_0 + st.fund_fees_token_0 + st.creator_fees_token_0,
            st.protocol_fees_token_1 + st.fund_fees_token_1 + st.creator_fees_token_1,
        )
    } else {
        (
            st.protocol_fees_token_1 + st.fund_fees_token_1 + st.creator_fees_token_1,
            st.protocol_fees_token_0 + st.fund_fees_token_0 + st.creator_fees_token_0,
        )
    };
    fork::set_token_amount(&mut lab.env.svm, &tok_vault, x_target + c_tok);
    fork::set_token_amount(&mut lab.env.svm, &sol_vault, y_target + c_sol);
    assert_eq!(lab.rm().reserves(&lab.env.svm), Some((x_target, y_target)));
}

/// ECON §12.11 `fv_push.py` on `raydium_cpmm_e2e::settlements_on_every_fixture`'s S2 on USELESS
/// (G-D-real): a 2-SOL position (default config), the pool drained to `5·q` tokens, 120 s, a real
/// pump to 1.8× the entry — the normal buyback refused `UseSolSettle` (its slippage above
/// `σ_max`) — then keeper seconds until one second before the honest S2; there
/// `support::amount_pushes_and_flips`: pushes up and down settle S2 within `fv_dev_bps`, no flip
/// bundle after 1–2 pushed seconds, push-then-freeze, 1–4 consecutive attacker-only seconds or
/// pushes with honest seconds between makes it S1.
#[test]
fn pushed_seconds_cannot_flip_the_drained_s2() {
    let mut lab = Lab::cpmm(
        USELESS,
        |x, y| (u128::from(x) * 80 * u128::from(SOL) / u128::from(y)) as u64,
        |_| {},
    );
    let (x, y) = lab.reserves();
    let q = u64::try_from(x * 2 * u128::from(SOL) / y).unwrap();
    let p_entry = lab.spot();
    let (b, nonce, _) = lab.open(q);
    lab.tick(5);
    drain_to(&mut lab, q * 5);
    lab.tick(120);
    lab.env.warp(1);
    lab.move_to(scale(p_entry, 1.8));
    lab.crank();
    let refusal = (0..60)
        .find_map(|_| {
            lab.second();
            match lab.exit_probe(Which::Liquidate, &b.key(), nonce) {
                Err(Some(c)) if c == u32::from(E::NotLiquidatable) => None,
                r => Some(r),
            }
        })
        .expect("liquidatable within 60 s");
    assert_eq!(
        refusal,
        Err(Some(u32::from(E::UseSolSettle))),
        "the normal buyback refused"
    );
    let mut ready = None;
    for _ in 0..300 {
        let saved = lab.env.svm.clone();
        if let Some(ev) = lab.keeper_second(&b.key(), nonce).ev {
            ready = Some((saved, ev));
            break;
        }
    }
    let (saved, honest) = ready.expect("settles");
    assert_eq!(honest.condition, 2, "the honest outcome is S2");
    amount_pushes_and_flips(
        &mut lab,
        &b.key(),
        nonce,
        &saved,
        &honest,
        "USELESS drained S2",
    );
}

/// The `p′` veto (R4A-3; G-D-real) on USELESS: `support::pprime_veto`.
#[test]
fn a_contrary_observation_does_not_change_pprime() {
    pprime_veto(&useless, "USELESS");
}

// ---- V2-4 with liquidity: LP around the settlement every 20 s (G-E4) -----------------------------

/// V2-4's liquidity variant on USELESS (G-E4): `[deposit LP, sol_settle, withdraw LP]` with the
/// real CPMM `deposit` / `withdraw` of the pool's whole LP counter (its depth doubled inside the
/// transaction) from the gap past the S1 boundary, every 20 s and in each of the 5 seconds before
/// the honest S1, until it (`support::lp_around_settle`): S1 in the honest second with the honest
/// box and amounts.
#[test]
fn lp_around_settle_every_20_s_changes_nothing() {
    lp_around_settle(&useless, "USELESS", &|lab, settle| {
        let p = *lab.rm().cpmm();
        let counter = p.state(&lab.env.svm).lp_supply;
        let (sol_need, tok_need) = lp::lp_needed(&lab.env, &p, counter);
        let who = lp::provider(&mut lab.env, &p, sol_need * 2, tok_need * 2, 0);
        let ixs = vec![
            cu_limit_ix(BUNDLE_CU),
            lp::deposit_ix(&p, &who, counter),
            settle,
            lp::withdraw_ix(&p, &who, counter),
        ];
        (ixs, who.owner)
    });
}
