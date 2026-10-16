//! The curve's fee buckets on pump's 2026-10-02 redeploy (M10 pump revalidation; `fork::
//! PumpDeploy::Oct2026`; R-PUMP Update 2026-09-30 K2–K5, which ran the same release's unreleased
//! artifacts from `pump-rust-client` 0.2.0):
//!
//! - **the release is deployed:** a third party's `buy_v3` / `sell_v3` (17 accounts, no fee
//!   recipient, no creator vault, no pump-fees CPI) pay the buyback slice in the trade and keep
//!   the rest of the protocol fee and the creator fee in the curve account, counted in
//!   `BondingCurve.creator_fee` @125 and `protocol_fees` @133 and **outside** `rQ`; each costs or
//!   pays the v2 quote (a buy one lamport less: v3 credits `n(q)` without v2's ε₁); the 2026-09
//!   build has no such instruction;
//! - **short.win's curve trades price on `rQ`:** it reads `rQ` from the curve data, never the
//!   curve's lamports, so an open and a close on a curve carrying buckets execute to the lamport
//!   of `quote::sol_leg` (`flows::round_trip`, the hooks run); since pump-final §6.1 they are
//!   `sell_v3` / `buy_v3` themselves, so their fees add to the buckets;
//! - **graduation moves `rQ`, not the buckets:** after v3 fees, the completing buy and
//!   `migrate_v2` seed the canonical pool with the `X_p` and at most the `Y_p` `B̂_str` prices (ARCH
//!   §4.3), and the migrated curve keeps its rent plus the buckets for a later sweep; the redeploy's
//!   pool is 287 bytes (the 2026-09 build made 271).

use shortwin::constants::{
    PUMP_EVENT_AUTHORITY, PUMP_FEE_CONFIG, PUMP_GLOBAL, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
use shortwin_litesvm_tests::fork::{PUMP_BUY_V3, PumpDeploy, PumpProgram};
use shortwin_math::quote::{self as mq, MigrationParams};
use shortwin_venue_state::pump as vpump;

use crate::common::*;
use crate::flows::round_trip;

/// pump `sell_v3` (`1c92de77…`).
const PUMP_SELL_V3: [u8; 8] = [0x1c, 0x92, 0xde, 0x77, 0x26, 0xc4, 0x69, 0xd5];
/// `BondingCurve.creator_fee` (u64), the un-swept creator fee bucket.
const CURVE_OFF_CREATOR_FEE: usize = 125;
/// `BondingCurve.protocol_fees` (u64), the un-swept protocol fee bucket.
const CURVE_OFF_PROTOCOL_FEES: usize = 133;

/// The curve's buckets `(creator_fee, protocol_fees)` (0 on a curve shorter than 141 bytes).
fn curve_buckets(env: &Env, c: &PumpCurve) -> (u64, u64) {
    let d = env.svm.get_account(&c.curve).expect("curve").data;
    let at = |o: usize| {
        d.get(o..o + 8)
            .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    (at(CURVE_OFF_CREATOR_FEE), at(CURVE_OFF_PROTOCOL_FEES))
}

/// pump `buy_v3(amount, max_sol_cost, partial_fill = None)` or `sell_v3(amount, min_sol_output)`
/// by `u` (IDL `pump.json` of `pump-rust-client` 0.2.0, 17 accounts in its order), the second
/// buyback recipient of the live `Global` (as `CurveUser`'s v2 trades).
fn v3_ix(env: &Env, c: &PumpCurve, buy: bool, u: &CurveUser, a: u64, b: u64) -> Instruction {
    let user = u.kp.pubkey();
    let q = c.quote_mint;
    let qp = c.quote_token_program();
    let ata = |owner: &Pubkey| pda::ata(owner, &q, &qp);
    let buyback = fork::pump_buyback_fee_recipient(&env.svm, 1);
    let ro = AccountMeta::new_readonly;
    let w = |k: Pubkey| AccountMeta::new(k, false);
    let accounts = vec![
        ro(PUMP_GLOBAL, false),
        ro(c.mint, false),
        ro(q, false),
        ro(c.token_program.id(), false),
        ro(qp, false),
        w(c.curve),
        w(c.curve_base_ata),
        w(ata(&c.curve)),
        AccountMeta::new(user, true),
        w(u.ata),
        w(ata(&user)),
        w(fork::pump_user_volume_accumulator(&user)),
        ro(PUMP_FEE_CONFIG, false),
        w(buyback),
        ro(anchor_lang::system_program::ID, false),
        ro(PUMP_EVENT_AUTHORITY, false),
        ro(PUMP_PROGRAM_ID, false),
    ];
    let mut data = if buy { PUMP_BUY_V3 } else { PUMP_SELL_V3 }.to_vec();
    data.extend_from_slice(&a.to_le_bytes());
    data.extend_from_slice(&b.to_le_bytes());
    if buy {
        data.push(0); // partial_fill: None
    }
    assert_eq!(qp, TOKEN_PROGRAM_ID);
    Instruction {
        program_id: PUMP_PROGRAM_ID,
        accounts,
        data,
    }
}

fn send_v3(env: &mut Env, c: &PumpCurve, buy: bool, u: &CurveUser, a: u64, b: u64) -> TxResult {
    if !buy && tokens(env, &u.ata) < a {
        fork::set_token_amount(&mut env.svm, &u.ata, a);
    }
    let ix = v3_ix(env, c, buy, u, a, b);
    let kp = u.kp.insecure_clone();
    env.send(&[ix], &[&kp])
}

/// The curve's lamports above its rent-exempt minimum and `rQ`.
fn held_above_rq(env: &Env, c: &PumpCurve) -> u64 {
    let a = env.svm.get_account(&c.curve).unwrap();
    let rent = env.svm.minimum_balance_for_rent_exemption(a.data.len());
    a.lamports - rent - c.state(&env.svm).real_quote_reserves
}

/// A third party's `buy_v3` and `sell_v3` on the 151-byte `near` curve: each costs / pays the v2
/// quote (a buy one lamport less: no ε₁; the first trade also pays the curve's growth to 166
/// bytes on the 2026-10-08 build, 76,200), the fees stay in the curve outside `rQ` (its lamports
/// above rent and `rQ` grow by exactly the buckets), and the 2026-09 build has no `buy_v3` (101).
#[test]
fn v3_trades_keep_their_fees_in_the_curve() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near);
    let u = CurveUser::new(&mut env, &c);
    assert_eq!(curve_buckets(&env, &c), (0, 0), "none at the dump");
    assert_eq!(c.data_len(&env.svm), 151);
    let above0 = held_above_rq(&env, &c);
    assert_eq!(above0, 0, "the reserve is minimum_balance(151)");
    let growth = c.growth_top_up(&env.svm);
    assert_eq!(growth, 15 * 5_080, "151 → 166 bytes");
    let q = c.state(&env.svm).real_token_reserves / 10;

    let quote = c.quote_buy(&env.svm, q).expect("quote");
    let user0 = lamports(&env, &u.kp.pubkey());
    let rq0 = c.state(&env.svm).real_quote_reserves;
    let meta = ok(send_v3(&mut env, &c, true, &u, q, quote.cost));
    assert_eq!(c.data_len(&env.svm), 166, "grown by the trade");
    assert_eq!(tokens(&env, &u.ata), q);
    let (cf, pf) = curve_buckets(&env, &c);
    // v3 credits the curve `n(q)`, without the v2 buy's ε₁ = +1 lamport (`quote::sol_leg`'s
    // `net`): one lamport below the v2 quote, everything else as v2.
    let ev = fork::pump_trade_events(&meta).remove(0);
    assert_eq!(ev.ix_name, "buy");
    assert_eq!(ev.sol_amount + 1, quote.net, "n(q) without ε₁");
    assert_eq!(
        user0 - lamports(&env, &u.kp.pubkey()),
        quote.cost - 1 + growth,
        "v3 costs the v2 quote less ε₁ (and the growth)"
    );
    assert_eq!(c.state(&env.svm).real_quote_reserves, rq0 + ev.sol_amount);
    assert_eq!(held_above_rq(&env, &c) - above0, cf + pf, "outside rQ");
    let rq1 = c.state(&env.svm).real_quote_reserves;
    assert!(rq1 > rq0);

    let sale = c.quote_sell(&env.svm, q / 2).expect("quote");
    let user1 = lamports(&env, &u.kp.pubkey());
    ok(send_v3(&mut env, &c, false, &u, q / 2, sale.proceeds));
    let (cf2, pf2) = curve_buckets(&env, &c);
    assert!(cf2 > cf && pf2 > pf);
    assert_eq!(
        lamports(&env, &u.kp.pubkey()) - user1,
        sale.proceeds,
        "v3 pays the v2 quote"
    );
    assert_eq!(held_above_rq(&env, &c) - above0, cf2 + pf2);
    eprintln!(
        "buy_v3 {q}: cost {}, buckets {cf} + {pf}; sell_v3 {}: {}, buckets {cf2} + {pf2}",
        quote.cost,
        q / 2,
        sale.proceeds
    );

    fork::load_pump_deploy(&mut env.svm, PumpProgram::Pump, PumpDeploy::Sep2026);
    env.warp(1);
    let res = send_v3(&mut env, &c, true, &u, q, u64::MAX);
    assert_eq!(
        custom_code(&res),
        Some(101),
        "no buy_v3 in the 2026-09 build"
    );
}

/// short.win's open and close on a curve carrying buckets (a third party's `buy_v3` after the
/// listing) execute to the lamport of the readers' `rQ`-based quotes (`flows::round_trip`
/// accounts for the buckets), and its own v3 trades add their fees to both buckets.
#[test]
fn short_win_trades_a_curve_carrying_buckets() {
    let (mut env, pm, q) = setup(PumpCoin::Near);
    let u = CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(send_v3(&mut env, &pm.curve, true, &u, rt / 20, u64::MAX));
    let buckets = curve_buckets(&env, &pm.curve);
    assert!(buckets.0 > 0 && buckets.1 > 0);
    env.warp(1);
    crank(&mut env, &pm);
    round_trip(&mut env, &pm, q);
    let after = curve_buckets(&env, &pm.curve);
    assert!(
        after.0 > buckets.0 && after.1 > buckets.1,
        "v3 keeps short.win's fees in the buckets: {buckets:?} → {after:?}"
    );
}

/// Graduation after v3 fees: the completing `buy_v2` and `migrate_v2` seed the canonical pool
/// with `mq::migrated_pool`'s `X_p` and at most its `Y_p` — the pool `B̂_str` prices — on a curve
/// that holds buckets; the migrated curve keeps its rent and the buckets; the new pool is 287
/// bytes.
#[test]
fn migrate_v2_moves_rq_and_leaves_the_buckets() {
    let (mut env, pm, _) = setup(PumpCoin::Near);
    let u = CurveUser::new(&mut env, &pm.curve);
    let rt = pm.curve.state(&env.svm).real_token_reserves;
    ok(send_v3(&mut env, &pm.curve, true, &u, rt / 4, u64::MAX));
    let (cf, pf) = curve_buckets(&env, &pm.curve);
    assert!(cf > 0 && pf > 0);
    env.warp(1);
    crank(&mut env, &pm);

    buy_down_to(&mut env, &pm, 0);
    let live = pm.curve.curve_state(&env.svm);
    let g = env.svm.get_account(&PUMP_GLOBAL).unwrap();
    let migration_fee = vpump::read_global(&g.owner.to_bytes(), &g.data)
        .unwrap()
        .pool_migration_fee();
    let base_ata = u128::from(tokens(&env, &pm.curve.curve_base_ata));
    let (xp, yp) = mq::migrated_pool(
        &live,
        &MigrationParams {
            migration_fee,
            pool_tokens: base_ata - live.rt,
            post_complete_quote_in: 0,
        },
    )
    .unwrap();
    let migrator = env.funded_keypair();
    let mig = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
    ok(env.send(&[pump::cu_limit_ix(600_000), mig], &[&migrator]));
    let np = pump::NewPool::of(&pm.curve);
    let r = np.reserves(&env.svm).expect("the canonical pool");
    assert_eq!(u128::from(r.base), xp, "X_p");
    // Without v3 trades the pool is `Y_p − 1` (`straddle`); a v3 buy's missing ε₁ lifts it to
    // `Y_p` exactly — never above, so `B̂_str` (priced on `Y_p`) still covers a PumpSwap buy on it.
    assert!(
        r.effective_quote <= yp && yp - r.effective_quote <= 1,
        "the pool is Y_p or Y_p − 1: {} vs {yp}",
        r.effective_quote
    );
    assert_eq!(
        env.svm.get_account(&np.pool).unwrap().data.len(),
        shortwin_litesvm_tests::pamm::POOL_LEN_2026_10,
        "the redeploy's create_pool allocates 287 bytes"
    );
    let curve = env.svm.get_account(&pm.curve.curve).unwrap();
    let rent = env.svm.minimum_balance_for_rent_exemption(curve.data.len());
    assert_eq!(pm.curve.state(&env.svm).real_quote_reserves, 0);
    assert_eq!(
        curve.lamports,
        rent + cf + pf,
        "the buckets stay for a sweep"
    );
}
