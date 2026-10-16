//! T-SYNTH, T-SIGMA and T-MIG-HALT (pump-final §14; §5.2, §6.1.7, §6.7): short.win's own exits
//! past the curve's `rT` on the release build, against the **dumped** pump (2026-10-13), PumpSwap
//! and pump-fees ELFs, with the invariant hooks after every instruction (`Env::send`; I13 allows
//! §5.2's bound between a synthetic exit's charge and its quote).
//!
//! Since pump's 2026-10-08 redeploy a `buy_v3` of `q > rT` buys `rT` on the curve, which
//! completes, and `q − rT` from the would-be PumpSwap pool (`X_s = curve_base_ata − rT`, `Y_s = rQ +
//! n₁ − pool_migration_fee`), both parts at the curve's fee, in one instruction. The curve adapter
//! trades every leg on v3, so a straddled position — the 3-transaction Jito bundle and the
//! `complete_curve` helper it needed are retired (owner D1 (a), D2) — exits like any other: the
//! close, the keeper's liquidation and expiry land in one instruction, quoted `B̂_v3` (owner D3),
//! and the venue sync moves the market to `PumpMigrating` after the leg.
//!
//! | Test | What |
//! |---|---|
//! | [`a_straddled_close_completes_the_curve_in_one_instruction`] | T-SYNTH-1 on `near166` (166 bytes, no growth) and on the same curve cut back to a 141-byte account (the exit pays its growth): `B − venue_rent` = pump's charge (its events) `≤ B̂_v3 ≤` charge + 7; `q` into the lending vault; the trader clean; `PumpMigrating` with `migration_seen_at = now`; `pcB = q − rT` |
//! | [`a_straddled_liquidation_lands_on_the_curve`], [`a_straddled_expiry_lands_on_the_curve`], [`a_self_made_straddle_is_liquidated_at_once`] | T-SYNTH-2: the keeper's liquidation (ECON's D@80 %, the crowd buys `rT` down to `q/2`; and M5-SEC-4's self-made straddle, `rT = q − 1`) and expiry land on the curve with the bound from `B̂_ref` (`B̂_v3` on the ring's states), no warm-up wait, the lenders' `q` back |
//! | [`the_keeper_migrates_and_transitions_after_a_completing_exit`] | T-SYNTH-3: after a synthetic close `migrate_v2` creates exactly the pool short.win prices the pause on (`X₁` = the live `curve_base_ata`, `Y₁ = rQ + pcQ − fee`), the FV of the other position during the pause is §5.4's ρ-scaled one, the transition follows, and the other position closes on the pool |
//! | [`a_curve_buy_before_the_exit_enlarges_the_synthetic_part`], [`max_in_bounds_the_v3_exit`], [`a_third_party_completion_before_the_exit_refuses_venue_migrating`] | T-SYNTH-4: a curve buy between the read and the close only enlarges the pool part (re-quoted at execution; a bound set on the stale quote refuses all or nothing); `max_buyback_cost` bounds the exit; a third party's completion (exact `rT`, or synthetic) first refuses `VenueMigrating` and changes nothing |
//! | [`a_buyback_beyond_the_synthetic_pool_refuses_curve_straddle`] | T-SYNTH-5 on **forged** state (the would-be pool cut to `X_s = q − rT`, C8: unreachable under the per-market cap): the close refuses `CurveStraddle`, the liquidation `UseSolSettle`, `sol_settle` waits (no S1, no S2, no S3 on an executable venue) |
//! | [`a_donated_curve_base_ata_prices_the_synthetic_exit_on_the_live_pool`] | T-SYNTH-6: a token donation to `curve_base_ata` before the close: `B̂_v3` and pump both price the pool part on the donation-inclusive `X_s` |
//! | [`a_straddle_above_sigma_max_goes_to_sol_settle`], [`a_straddle_at_sigma_max_is_liquidated_on_the_curve`] | T-SIGMA (C5): the position's `σ_max` forged around σ(`B̂_v3`) (≈ 2–3 %, below the parameter's 500 bps floor; σ on the retired `B̂_str` differs): at σ(`B̂_v3`) − 1 the liquidation refuses `UseSolSettle` and `sol_settle` settles S2; at σ(`B̂_v3`) it lands on the curve — the program judges σ on `B̂_v3`, to the basis point |
//! | [`a_synthetic_close_under_a_create_pool_halt_freezes_the_rest_until_s3`] | T-MIG-HALT: PumpSwap's `create_pool` halt; opens refuse `GraduationHalted`; short.win's own synthetic close still lands and completes the curve; `migrate_v2` fails 6017; the other position waits (`VenueMigrating`) and settles S3 at §5.4's FV on the live `(X₁, Y₁)`; the halt lifted, `migrate_v2` creates exactly that pool |

#![allow(clippy::result_large_err)]

#[path = "graduation_support.rs"]
mod support;

use anchor_lang::{AccountDeserialize, AccountSerialize};
use shortwin_math::fees::FeeBps;
use shortwin_math::quote::{self as mq, MigrationParams};
use shortwin_math::{rent, risk};
use shortwin_venue_state::pump as vpump;
use support::*;

const D80_SOLD: u64 = 634_480_000_000_000;
const D80_Q: u64 = 6_529_192_912_424;
const D97_SOLD: u64 = 769_307_000_000_000;
const D97_Q: u64 = 4_521_732_607_755;

// ---- helpers --------------------------------------------------------------------------------------

/// A curve market on `coin` (`create_market(Auto)`, `40·q` lent, 60 s of warm-up) with a funded
/// crowd and a keeper; `q = vT/200`, within every default cap.
fn sim_on(coin: PumpCoin) -> (Sim, u64) {
    let (mut env, c) = Env::fork_pump_coin(coin);
    let crowd = CurveUser::new(&mut env, &c);
    env.fund(&crowd.kp.pubkey(), 2_000 * SOL);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(coin, q * 40), c);
    let keeper = env.funded_keypair();
    let t0 = env.now();
    (
        Sim {
            env,
            pm,
            crowd,
            keeper,
            t0,
        },
        q,
    )
}

/// `n` seconds of the keeper's per-second crank.
fn crank_secs(s: &mut Sim, n: i64) {
    for _ in 0..n {
        s.env.warp(1);
        s.crank();
    }
}

/// The curve's `rT` now.
fn rt_now(s: &Sim) -> u64 {
    u64::try_from(curve(&s.env, &s.pm).rt).unwrap()
}

/// The crowd buys the curve down to `rt_left` (a real `buy_v2`).
fn buy_down_to(s: &mut Sim, rt_left: u64) {
    let rt = rt_now(s);
    s.crowd_buy(rt - rt_left);
    assert_eq!(rt_now(s), rt_left);
}

/// `Global.pool_migration_fee`.
fn migration_fee(env: &Env) -> u64 {
    let g = env
        .svm
        .get_account(&shortwin::constants::PUMP_GLOBAL)
        .unwrap();
    vpump::read_global(&g.owner.to_bytes(), &g.data)
        .unwrap()
        .pool_migration_fee()
}

/// `B̂_v3(q)` on the live curve from `shortwin-math` and the readers (pump-final §5.2), with the
/// figures a synthetic exit is checked against.
#[derive(Clone, Copy, Debug)]
struct Quote {
    /// `B̂_v3(q)`.
    b: u64,
    /// The live `X_s = curve_base_ata − rT`.
    xs: u128,
    /// The synthetic fee (the tier on the mint's live supply).
    fs: FeeBps,
    /// `rT` and the growth rent the exit's trade takes from the trader.
    rt: u64,
    rent: u64,
    /// The lending vault's tokens before the exit.
    vault: u64,
}

fn b_v3(s: &Sim, q: u64) -> Quote {
    let live = curve(&s.env, &s.pm);
    let fees = s.pm.curve.fees(&s.env.svm).expect("fees");
    let fs = s.pm.curve.synthetic_fees(&s.env.svm).expect("φ_s");
    let xs = u128::from(tokens(&s.env, &s.pm.curve.curve_base_ata)) - live.rt;
    let m = MigrationParams {
        migration_fee: migration_fee(&s.env),
        pool_tokens: xs,
        post_complete_quote_in: 0,
    };
    let b = mq::curve_synthetic(q, &live, &fees, &fs, &m).expect("B̂_v3");
    Quote {
        b: b.cost,
        xs,
        fs,
        rt: u64::try_from(live.rt).unwrap(),
        rent: s.pm.curve.growth_top_up(&s.env.svm),
        vault: tokens(&s.env, &s.pm.keys.pool_vault),
    }
}

/// §5.2's bound on `B̂_v3 − charge` for a pool part `q₂` on `X_s`: `(1 + Σ⌈bᵢ/10⁴⌉) + (Δ₂ +
/// Σ⌈Δ₂·bᵢ/10⁴⌉)`, `Δ₂ = 1 + ⌈q₂/(X_s − q₂)⌉` over the non-zero fee components.
fn v3_bound(q2: u128, xs: u128, fs: &FeeBps) -> u64 {
    let d2 = 1 + q2.div_ceil(xs - q2);
    let b: Vec<u128> = [fs.protocol, fs.creator]
        .into_iter()
        .filter(|b| *b > 0)
        .map(u128::from)
        .collect();
    let bound = (1 + b.iter().map(|b| b.div_ceil(10_000)).sum::<u128>())
        + (d2 + b.iter().map(|b| (d2 * b).div_ceil(10_000)).sum::<u128>());
    u64::try_from(bound).unwrap()
}

/// pump's charge of the buy in `meta`, from its events: the curve part's `TradeEvent` and the pool
/// part's `PostCompleteBuyEvent`, each input plus its protocol and creator fees. Returns the charge
/// and the tokens of each part.
fn pump_charge(meta: &TransactionMetadata) -> (u64, u64, u64) {
    let curve_part: Vec<_> = fork::pump_trade_events(meta)
        .into_iter()
        .filter(|e| e.is_buy)
        .collect();
    let pool_part = fork::pump_post_complete_buy_events(meta);
    let charge = curve_part
        .iter()
        .map(|e| e.sol_amount + e.fee + e.creator_fee)
        .sum::<u64>()
        + pool_part
            .iter()
            .map(|e| e.quote_in + e.fee + e.creator_fee)
            .sum::<u64>();
    (
        charge,
        curve_part.iter().map(|e| e.token_amount).sum(),
        pool_part.iter().map(|e| e.base_out).sum(),
    )
}

/// The checks every synthetic exit shares: pump delivered `rT` from the curve and `q − rT` from
/// the would-be pool; `B − venue_rent` is pump's charge; `charge ≤ B̂_v3 = b_live ≤ charge + §5.2's
/// bound` (≤ 7 here: `q₂ ≤ X_s/2`); the lending vault got `q`; the trader is clean; the curve
/// completed at 166 bytes with `pcB = q − rT`; the market is `PumpMigrating` since now, the
/// transition in the exit's own events.
fn check_synthetic_exit(
    s: &Sim,
    meta: &TransactionMetadata,
    q: u64,
    quote: &Quote,
    buyback_cost: u64,
    b_live: u64,
) {
    let (charge, curve_part, pool_part) = pump_charge(meta);
    assert_eq!(
        (curve_part, pool_part),
        (quote.rt, q - quote.rt),
        "rT + (q − rT)"
    );
    assert_eq!(
        b_live, quote.b,
        "the program's B̂_live = shortwin-math's B̂_v3"
    );
    assert_eq!(
        buyback_cost - quote.rent,
        charge,
        "B − venue_rent = pump's charge"
    );
    let bound = v3_bound(u128::from(q - quote.rt), quote.xs, &quote.fs);
    assert!(
        b_live >= charge && b_live - charge <= bound && bound <= 7,
        "B̂_v3 {b_live} vs charge {charge} (bound {bound})"
    );
    eprintln!(
        "synthetic exit of q {q} at rT {}: charge {charge}, B̂_v3 +{} (bound {bound}), growth {}, \
         {} CU",
        quote.rt,
        b_live - charge,
        quote.rent,
        meta.compute_units_consumed
    );
    assert_eq!(tokens(&s.env, &s.pm.keys.pool_vault) - quote.vault, q);
    assert_eq!(
        lamports(&s.env, &s.pm.keys.trader),
        0,
        "the trader holds nothing"
    );
    assert_eq!(tokens(&s.env, &s.pm.trader_base_ata), 0);
    let c = s.pm.curve.state(&s.env.svm);
    assert!(c.complete && c.real_token_reserves == 0, "completed");
    assert_eq!(c.post_complete_base_out, q - quote.rt, "pcB = q − rT");
    assert_eq!(
        c.post_complete_quote_in,
        fork::pump_post_complete_buy_events(meta)[0].quote_in,
        "pcQ"
    );
    assert_eq!(s.pm.curve.data_len(&s.env.svm), 166);
    venue_changed(meta, VenueKind::PumpCurve, VenueKind::PumpMigrating);
    let m = market(&s.env, &s.pm);
    assert_eq!(m.venue_kind(), Some(VenueKind::PumpMigrating));
    assert_eq!(m.migration_seen_at, s.env.now());
}

/// Cuts the curve account back to `len` bytes and its lamports by the rent the cut bytes held:
/// a dormant curve of an older layout (mainnet holds 141-byte curves of the 2026-10-02 build).
fn shrink_curve(env: &mut Env, c: &PumpCurve, len: usize) {
    let mut acc = env.svm.get_account(&c.curve).unwrap();
    let cut = env.svm.minimum_balance_for_rent_exemption(acc.data.len())
        - env.svm.minimum_balance_for_rent_exemption(len);
    acc.data.truncate(len);
    acc.lamports -= cut;
    env.svm.set_account(c.curve, acc).unwrap();
}

/// The keeper's `liquidate` of `p`, tried every second right after its crank until it lands; the
/// refusals before are `NotLiquidatable` (the EMA leg catching up) or `ExitBoundNotMet` (the
/// reference behind a fast move). Returns the event, the transaction and the quote of the second
/// it landed.
fn liquidate_on_the_curve(
    s: &mut Sim,
    p: &Pos,
    max_s: i64,
) -> (Liquidate, TransactionMetadata, Quote) {
    let kp = s.keeper.insecure_clone();
    let q = s.position(p).q;
    for _ in 0..max_s {
        s.env.warp(1);
        s.crank();
        if !s.status(p).cr_liq() {
            continue;
        }
        let quote = b_v3(s, q);
        let ix = liquidate_ix(&s.pm, &kp.pubkey(), &p.key(), p.nonce);
        let res = s
            .env
            .send(&[pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU), ix], &[&kp]);
        match res {
            Ok(meta) => return (events::one(&meta), meta, quote),
            Err(_) => {
                let code = custom_code(&res);
                assert!(
                    code == Some(u32::from(E::ExitBoundNotMet))
                        || code == Some(u32::from(E::NotLiquidatable)),
                    "the liquidation refused {code:?}"
                );
            }
        }
    }
    panic!("not liquidated within {max_s} s");
}

/// The stored position with `f` applied (a forged field; the account keeps its size).
fn forge_position(env: &mut Env, pm: &PumpMarket, p: &Pos, f: impl FnOnce(&mut ShortPosition)) {
    let key = pda::position(&pm.market(), &p.key(), p.nonce).0;
    let mut acc = env.svm.get_account(&key).unwrap();
    let mut pos = ShortPosition::try_deserialize(&mut &acc.data[..]).unwrap();
    f(&mut pos);
    let mut data = Vec::new();
    pos.try_serialize(&mut data).unwrap();
    acc.data[..data.len()].copy_from_slice(&data);
    env.svm.set_account(key, acc).unwrap();
}

// ---- T-SYNTH-1 ------------------------------------------------------------------------------------

/// T-SYNTH-1 (module docs): one `close_short` of `q = rT + q/2` completes the curve. On `near166`
/// the curve is already 166 bytes; then the same near-complete curve cut back to a 141-byte
/// account (the 2026-10-02 layout, which mainnet's dormant curves keep) after the crowd's buy, so
/// the exit's `buy_v3` grows it to 166 bytes and `B` carries that rent (`venue_rent`, out of
/// `ert`). (`grow141` itself is far from completion: buying it down to `q/2` would make the
/// position insolvent, `InsufficientVault`.)
#[test]
fn a_straddled_close_completes_the_curve_in_one_instruction() {
    for shrink in [false, true] {
        let (mut s, q) = sim_on(PumpCoin::Near166);
        let p = s.open(q);
        crank_secs(&mut s, 2);
        buy_down_to(&mut s, q / 2);
        if shrink {
            shrink_curve(&mut s.env, &s.pm.curve, 141);
            assert_eq!(s.pm.curve.data_len(&s.env.svm), 141);
        }
        s.env.warp(1);
        let quote = b_v3(&s, q);
        if shrink {
            assert!(quote.rent > 0, "the exit grows the curve");
        } else {
            assert_eq!(quote.rent, 0, "a 166-byte curve");
        }
        let meta = ok(s.env.close(&s.pm, &p.b, p.nonce));
        let close: Close = events::one(&meta);
        assert_eq!(close.q, q);
        check_synthetic_exit(&s, &meta, q, &quote, close.buyback_cost, close.b_live);
        assert!(
            read::account(
                &s.env.svm,
                &pda::position(&s.pm.market(), &p.key(), p.nonce).0
            )
            .is_none()
        );
        // The next close of anyone pauses; the next open is refused.
        let mut b2 = s.env.borrower();
        assert_err(&s.env.open(&s.pm, &mut b2, q).0, E::VenueMigrating);
    }
}

// ---- T-SYNTH-2 ------------------------------------------------------------------------------------

/// T-SYNTH-2 (module docs): ECON's D@80 % position; the crowd buys the curve down to `rT = q/2`
/// (the price ×2.4, past Tier D's 175 % from 400 %); the keeper cranks every second and its
/// `liquidate` lands on the curve as soon as both legs hold and the reference bound covers
/// `B̂_v3` — the curve completes in the liquidation and the lenders get `q` back.
#[test]
fn a_straddled_liquidation_lands_on_the_curve() {
    let mut s = Sim::d_market(D80_SOLD, lend_for_80(D80_Q));
    let p = s.open(D80_Q);
    crank_secs(&mut s, 30);
    buy_down_to(&mut s, D80_Q / 2);
    let (ev, meta, quote) = liquidate_on_the_curve(&mut s, &p, 900);
    assert!(!ev.expired);
    assert!(
        ev.max_in >= ev.b_live && ev.b_ref > 0,
        "B̂_live within max_in"
    );
    check_synthetic_exit(&s, &meta, D80_Q, &quote, ev.buyback_cost, ev.b_live);
    assert_eq!(market(&s.env, &s.pm).ema_warm, 0, "no warm-up on the curve");
}

/// T-SYNTH-2: a straddled position held to its deadline on `near166` (`rT = q/2` an hour after
/// the open: the price ×1.8, healthy at 400 % → 220 %): the keeper's `expire` right after the
/// deadline lands on the curve with `B̂_v3` and completes it.
#[test]
fn a_straddled_expiry_lands_on_the_curve() {
    let (mut s, q) = sim_on(PumpCoin::Near166);
    let p = s.open(q);
    crank_secs(&mut s, 2);
    buy_down_to(&mut s, q / 2);
    let deadline = s.position(&p).deadline;
    let mut st = s.status(&p);
    while s.env.now() < deadline + 1 {
        let step = (deadline + 1 - s.env.now()).min(600);
        s.env.warp(step);
        s.crank();
        st = s.status(&p);
        assert!(!st.cr_liq(), "the straddle stays healthy");
    }
    assert!(st.expired);
    s.env.warp(1);
    s.crank();
    let quote = b_v3(&s, q);
    let kp = s.keeper.insecure_clone();
    let ix = expire_ix(&s.pm, &kp.pubkey(), &p.key(), p.nonce);
    let meta = ok(s
        .env
        .send(&[pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU), ix], &[&kp]));
    let ev: Expire = events::one(&meta);
    assert!(ev.max_in >= ev.b_live && ev.expiry_fee > 0);
    check_synthetic_exit(&s, &meta, q, &quote, ev.buyback_cost, ev.b_live);
}

/// T-SYNTH-2, M5-SEC-4's second vector: the D@80 % borrower buys `rT − q + 1` itself (`rT = q −
/// 1`). There is no bundle to wait for any more: the position prices its synthetic exit, is
/// liquidatable within a minute, and the keeper's liquidation lands on the curve, completing it.
#[test]
fn a_self_made_straddle_is_liquidated_at_once() {
    let mut s = Sim::d_market(D80_SOLD, lend_for_80(D80_Q));
    let p = s.open(D80_Q);
    crank_secs(&mut s, 30);
    let own = CurveUser::new(&mut s.env, &s.pm.curve);
    s.env.warp(1);
    let rt = rt_now(&s);
    ok(own.buy(&mut s.env, &s.pm.curve, rt - D80_Q + 1, u64::MAX));
    assert_eq!(rt_now(&s), D80_Q - 1);
    let t = s.env.now();
    let (ev, meta, quote) = liquidate_on_the_curve(&mut s, &p, 120);
    eprintln!(
        "self-made straddle liquidated on the curve {} s after the borrower's buy: B {:.4} SOL",
        s.env.now() - t,
        ev.buyback_cost as f64 / 1e9
    );
    check_synthetic_exit(&s, &meta, D80_Q, &quote, ev.buyback_cost, ev.b_live);
}

// ---- T-SYNTH-3 ------------------------------------------------------------------------------------

/// T-SYNTH-3 (module docs), on ECON's D@97 % market: positions A (`Q/2`) and B (`Q/4`); the crowd
/// buys `rT` down to `Q/4` and A's close completes the curve synthetically. During the pause B's
/// close refuses `VenueMigrating` and its FV is §5.4's: ρ-scaled on the pool `migrate_v2` will
/// create, which is exactly the pool it then creates (`X₁` the live `curve_base_ata`, `Y₁ = rQ +
/// pcQ − fee`); the keeper's crank moves the market onto it and B closes on the pool.
#[test]
fn the_keeper_migrates_and_transitions_after_a_completing_exit() {
    let (qa, qb) = (D97_Q / 2, D97_Q / 4);
    let mut g = Grad::d_market(D97_SOLD, lend_for_80(qa + qb));
    let pb = g.s.open(qb);
    let pa = g.s.open(qa);
    crank_secs(&mut g.s, 30);
    buy_down_to(&mut g.s, qb);
    g.s.env.warp(1);
    let quote = b_v3(&g.s, qa);
    let meta = ok(g.s.env.close(&g.s.pm, &pa.b, pa.nonce));
    let close: Close = events::one(&meta);
    check_synthetic_exit(&g.s, &meta, qa, &quote, close.buyback_cost, close.b_live);
    g.s.env.warp(1);
    assert_err(&g.s.env.close(&g.s.pm, &pb.b, pb.nonce), E::VenueMigrating);

    // B's FV in the pause: the program's quoter (host build) prices the pool migrate_v2 creates.
    let c = g.s.pm.curve.state(&g.s.env.svm);
    let a = u128::from(tokens(&g.s.env, &g.s.pm.curve.curve_base_ata));
    let live = curve(&g.s.env, &g.s.pm);
    let fee_m = migration_fee(&g.s.env);
    let m1 = MigrationParams {
        migration_fee: fee_m,
        pool_tokens: a,
        post_complete_quote_in: c.post_complete_quote_in,
    };
    let (x1, y1) = mq::migrated_pool(&live, &m1).unwrap();
    assert_eq!(x1 + u128::from(c.post_complete_base_out), STANDARD_TOKENS);
    let m = market(&g.s.env, &g.s.pm);
    let pos_b = g.s.position(&pb);
    let st = host_state(&g.s.env, &g.s.pm);
    let r = shortwin::venues::risk_quoter(&m, &st, &[], u64::from(pos_b.venue_fee_hard_cap_bps))
        .expect("the curve quoter");
    let p_ema = m.ema_price.get();
    let fv = r.fv_repriced(qb, m.ema_liq.get(), p_ema).expect("FV").cost;
    assert_eq!(fv, b_fv_migrating(&g.s.env, &g.s.pm, &pos_b, p_ema));
    let (x0, y0) = mq::migrated_pool(
        &live,
        &MigrationParams {
            migration_fee: fee_m,
            pool_tokens: STANDARD_TOKENS,
            post_complete_quote_in: 0,
        },
    )
    .unwrap();
    let rho_p = mq::migrating_fv_price(p_ema, x0, y0, x1, y1).unwrap();
    assert!(rho_p > p_ema, "ρ > 1 after a pool part");

    // migrate_v2 creates exactly (X₁, Y₁); the transition; B closes on the pool.
    g.migrate();
    let np = pump::NewPool::of(&g.s.pm.curve);
    let pr = np.reserves(&g.s.env.svm).expect("the canonical pool");
    assert_eq!(
        (u128::from(pr.base), pr.effective_quote),
        (x1, y1),
        "the pool migrate_v2 created is (X₁, Y₁)"
    );
    g.s.env.warp(1);
    let meta = g.crank();
    venue_changed(&meta, VenueKind::PumpMigrating, VenueKind::PumpSwap);
    g.s.env.warp(1);
    g.crank();
    let lent = market(&g.s.env, &g.s.pm).tokens_free;
    let close_b: Close = events::one(&ok(g.s.env.close(
        g.pam.as_ref().expect("graduated"),
        &pb.b,
        pb.nonce,
    )));
    assert_eq!(close_b.q, qb);
    let m = market(&g.s.env, &g.s.pm);
    assert_eq!(m.open_positions, 0);
    assert_eq!(m.tokens_free, lent + qb, "every lent token back");
}

/// The standard pool tokens (206.9e12).
const STANDARD_TOKENS: u128 = shortwin::venues::STANDARD_POOL_TOKENS;

/// The program's venue state of `pm` now (host build of `venues::read_state_with` on the ReadSet
/// accounts the svm holds, with the mint's supply).
fn host_state(env: &Env, pm: &PumpMarket) -> shortwin::venues::VenueState {
    use anchor_lang::prelude::AccountInfo;
    let mut accs: Vec<(Pubkey, u64, Vec<u8>, Pubkey)> = pm
        .read_set(false)
        .iter()
        .map(|a| {
            let acc = env.svm.get_account(&a.pubkey).unwrap_or_default();
            (a.pubkey, acc.lamports, acc.data, acc.owner)
        })
        .collect();
    let infos: Vec<AccountInfo> = accs
        .iter_mut()
        .map(|(k, l, d, o)| AccountInfo::new(k, false, false, l, d, o, false))
        .collect();
    let m = market(env, pm);
    let kind = m.venue_kind().expect("venue");
    let mint = env.svm.get_account(&pm.keys.mint).unwrap();
    let supply = shortwin_venue_state::mint::read_base(&mint.owner.to_bytes(), &mint.data)
        .unwrap()
        .supply;
    shortwin::venues::read_state_with(&m, kind, &infos, env.now(), Some(supply))
}

// ---- T-SYNTH-4 ------------------------------------------------------------------------------------

/// T-SYNTH-4: the close is built and its bound read on one state; a third party buys part of the
/// curve's `rT` first. The close is re-quoted at execution: with the borrower's open bound it lands
/// and the pool part is the larger `q − rT'`, all of it in one leg; with the bound set at the stale
/// quote it refuses `BuybackAboveMax` and nothing changes.
#[test]
fn a_curve_buy_before_the_exit_enlarges_the_synthetic_part() {
    let (mut s, q) = sim_on(PumpCoin::Near166);
    let p = s.open(q);
    crank_secs(&mut s, 2);
    buy_down_to(&mut s, q / 2);
    s.env.warp(1);
    let stale = b_v3(&s, q);
    // A third party buys a quarter of q first.
    buy_down_to(&mut s, q / 4);
    let fresh = b_v3(&s, q);
    assert!(fresh.b > stale.b, "a dearer exit");
    let tight = close_short_ix(&s.pm, &p.key(), p.nonce, stale.b, ExtrasSel::NONE, &[]);
    let before = s.env.svm.clone();
    assert_err(
        &s.env.send(&with_cu(&s.pm, tight), &[&p.b.kp]),
        E::BuybackAboveMax,
    );
    assert_eq!(
        s.env.svm.get_account(&s.pm.curve.curve).map(|a| a.data),
        before.get_account(&s.pm.curve.curve).map(|a| a.data),
        "nothing changed"
    );
    let meta = ok(s.env.close(&s.pm, &p.b, p.nonce));
    let close: Close = events::one(&meta);
    check_synthetic_exit(&s, &meta, q, &fresh, close.buyback_cost, close.b_live);
    assert_eq!(
        s.pm.curve.state(&s.env.svm).post_complete_base_out,
        q - q / 4
    );
}

/// `ix` behind the curve trades' CU limit (ARCH §6.1's PROPOSED 250 k).
fn with_cu(_pm: &PumpMarket, ix: Instruction) -> Vec<Instruction> {
    vec![pump::cu_limit_ix(pump::CURVE_TRADE_CU), ix]
}

/// T-SYNTH-4: the borrower's `max_buyback_cost` bounds the synthetic exit: one lamport under
/// `B̂_v3` refuses `BuybackAboveMax` (all or nothing), `B̂_v3` itself lands with `B ≤ B̂_v3`.
#[test]
fn max_in_bounds_the_v3_exit() {
    let (mut s, q) = sim_on(PumpCoin::Near166);
    let p = s.open(q);
    crank_secs(&mut s, 2);
    buy_down_to(&mut s, q / 2);
    s.env.warp(1);
    let quote = b_v3(&s, q);
    let under = close_short_ix(&s.pm, &p.key(), p.nonce, quote.b - 1, ExtrasSel::NONE, &[]);
    assert_err(
        &s.env.send(&with_cu(&s.pm, under), &[&p.b.kp]),
        E::BuybackAboveMax,
    );
    assert_eq!(rt_now(&s), q / 2, "the curve untouched");
    let exact = close_short_ix(&s.pm, &p.key(), p.nonce, quote.b, ExtrasSel::NONE, &[]);
    let meta = ok(s.env.send(&with_cu(&s.pm, exact), &[&p.b.kp]));
    let close: Close = events::one(&meta);
    assert!(close.buyback_cost <= quote.b);
    check_synthetic_exit(&s, &meta, q, &quote, close.buyback_cost, close.b_live);
}

/// T-SYNTH-4: a third party completes the curve before the close — with a `buy_v2` of exactly `rT`,
/// or with its own synthetic `buy_v3` past it. The close's prologue moves the market to
/// `PumpMigrating` and the close refuses `VenueMigrating`: no spend, the position as it was (the
/// next crank persists the transition).
#[test]
fn a_third_party_completion_before_the_exit_refuses_venue_migrating() {
    for synthetic in [false, true] {
        let (mut s, q) = sim_on(PumpCoin::Near166);
        let p = s.open(q);
        crank_secs(&mut s, 2);
        buy_down_to(&mut s, q / 2);
        s.env.warp(1);
        if synthetic {
            let u = CurveUser::new(&mut s.env, &s.pm.curve);
            s.env.fund(&u.kp.pubkey(), 1_000 * SOL);
            let ix = u.buy_v3_ix(&s.env.svm, &s.pm.curve, q / 2 + 1_000_000, u64::MAX);
            ok(u.send(&mut s.env, ix));
        } else {
            buy_down_to(&mut s, 0);
        }
        assert!(s.pm.curve.state(&s.env.svm).complete);
        let key = pda::position(&s.pm.market(), &p.key(), p.nonce).0;
        let before = s.env.svm.get_account(&key).unwrap();
        let v0 = lamports(&s.env, &p.key());
        assert_err(&s.env.close(&s.pm, &p.b, p.nonce), E::VenueMigrating);
        assert_eq!(s.env.svm.get_account(&key).unwrap(), before);
        assert_eq!(lamports(&s.env, &p.key()), v0);
        assert_eq!(
            venue(&s.env, &s.pm),
            VenueKind::PumpCurve,
            "reverted with it"
        );
        venue_changed(&s.crank(), VenueKind::PumpCurve, VenueKind::PumpMigrating);
    }
}

// ---- T-SYNTH-5 ------------------------------------------------------------------------------------

/// T-SYNTH-5 on forged state (pump-final C8, §6.7): ECON's D@80 % position made liquidatable as in
/// [`a_straddled_liquidation_lands_on_the_curve`], then the would-be pool cut to `X_s = q − rT`
/// (`curve_base_ata` set to `q`: a state the per-market cap keeps out of reach, `q ≤ ≈ 4.99e13 <
/// X_s/4`). Nothing can deliver `q`: the close refuses `CurveStraddle`; the liquidation's execution
/// quote is undefined, so it refuses `UseSolSettle`; `sol_settle` waits — no S1 (the robust spot
/// and box legs are affordable), no S2 (their σ is the curve's), no S3 (the venue executes).
#[test]
fn a_buyback_beyond_the_synthetic_pool_refuses_curve_straddle() {
    let mut s = Sim::d_market(D80_SOLD, lend_for_80(D80_Q));
    let p = s.open(D80_Q);
    crank_secs(&mut s, 30);
    buy_down_to(&mut s, D80_Q / 2);
    let mut liquidatable = false;
    for _ in 0..600 {
        s.env.warp(1);
        s.crank();
        if s.status(&p).cr_liq() {
            liquidatable = true;
            break;
        }
    }
    assert!(liquidatable);
    fork::set_token_amount(&mut s.env.svm, &s.pm.curve.curve_base_ata, D80_Q);
    let st = host_state(&s.env, &s.pm);
    assert!(st.executable, "still executable: the curve trades");
    assert!(st.buy(D80_Q).is_none(), "B̂_live undefined: q − rT ≥ X_s");
    s.env.warp(1);
    s.crank();
    assert_err(&s.env.close(&s.pm, &p.b, p.nonce), E::CurveStraddle);
    let kp = s.keeper.insecure_clone();
    let liq = liquidate_ix(&s.pm, &kp.pubkey(), &p.key(), p.nonce);
    assert_err(
        &s.env.send(
            &[pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU), liq],
            &[&kp],
        ),
        E::UseSolSettle,
    );
    for _ in 0..120 {
        s.env.warp(1);
        s.crank();
        let ix = sol_settle_ix(&s.env, &s.pm, &kp.pubkey(), &p.key(), p.nonce);
        let meta = ok(s.env.send(&[ix], &[&kp]));
        assert!(
            events::of::<SolSettle>(&meta).is_empty(),
            "the position waits on the executable curve"
        );
    }
    assert!(!s.pm.curve.state(&s.env.svm).complete);
}

// ---- T-SYNTH-6 ------------------------------------------------------------------------------------

/// T-SYNTH-6: a token donation `D` = 1e12 to `curve_base_ata` before the close. pump prices the
/// pool part on the donation-inclusive `X_s = curve_base_ata − rT` (its event's
/// `pool_base_reserves_before`), and so does `B̂_v3` (the close's `b_live`, the live leg's
/// `live_pool_tokens`): pump's charge `≤ B ≤ B̂_v3 ≤` charge + 7, `pcB = q − rT`.
#[test]
fn a_donated_curve_base_ata_prices_the_synthetic_exit_on_the_live_pool() {
    let (mut s, q) = sim_on(PumpCoin::Near166);
    let p = s.open(q);
    crank_secs(&mut s, 2);
    buy_down_to(&mut s, q / 2);
    let a0 = tokens(&s.env, &s.pm.curve.curve_base_ata);
    let donation = 1_000_000_000_000;
    let holder = CurveUser::new(&mut s.env, &s.pm.curve);
    holder.plant(&mut s.env, donation);
    let decimals = s.env.svm.get_account(&s.pm.keys.mint).unwrap().data[44];
    let mut data = vec![12u8];
    data.extend_from_slice(&donation.to_le_bytes());
    data.push(decimals);
    let ix = Instruction {
        program_id: s.pm.keys.token_program,
        accounts: vec![
            AccountMeta::new(holder.ata, false),
            AccountMeta::new_readonly(s.pm.keys.mint, false),
            AccountMeta::new(s.pm.curve.curve_base_ata, false),
            AccountMeta::new_readonly(holder.kp.pubkey(), true),
        ],
        data,
    };
    ok(holder.send(&mut s.env, ix));
    s.env.warp(1);
    let quote = b_v3(&s, q);
    assert_eq!(quote.xs, u128::from(a0 + donation) - u128::from(quote.rt));
    let st = host_state(&s.env, &s.pm);
    let shortwin::venues::Quoter::Curve(cq) = st.quoter().unwrap() else {
        panic!("a curve quoter")
    };
    assert_eq!(
        cq.live_pool_tokens, quote.xs,
        "the live leg counts the donation"
    );
    let meta = ok(s.env.close(&s.pm, &p.b, p.nonce));
    let close: Close = events::one(&meta);
    let post = &fork::pump_post_complete_buy_events(&meta)[0];
    assert_eq!(
        u128::from(post.pool_base_reserves_before),
        quote.xs,
        "pump priced the pool part on the same X_s"
    );
    check_synthetic_exit(&s, &meta, q, &quote, close.buyback_cost, close.b_live);
}

// ---- T-SIGMA --------------------------------------------------------------------------------------

/// T-SIGMA's state: ECON's D@80 % position made liquidatable by the crowd's buy down to `rT = q/2`.
/// Returns the market, the position, σ(`B̂_v3`) — what the program judges, `risk::sigma_xy` of the
/// synthetic quote's fee-free input on the live `(vT, vQ)`, equal to the harness keeper view's
/// `sigma_live` — and σ on the retired straddle quote `B̂_str` (all of `q` on the migrated pool,
/// `X_p` the live `curve_base_ata − rT`) for the same state. The two differ, so a threshold set
/// between them tells which quote the program prices σ on.
fn sigma_setup() -> (Sim, Pos, u64, u64) {
    let mut s = Sim::d_market(D80_SOLD, lend_for_80(D80_Q));
    let p = s.open(D80_Q);
    crank_secs(&mut s, 30);
    buy_down_to(&mut s, D80_Q / 2);
    let mut st = s.status(&p);
    for _ in 0..600 {
        s.env.warp(1);
        s.crank();
        st = s.status(&p);
        if st.cr_liq() {
            break;
        }
    }
    assert!(st.cr_liq());
    let ctx = CurveCtx::read(&s.env, &s.pm).expect("a live curve");
    let v3 = ctx.buy_live(D80_Q, None).expect("B̂_v3");
    let mut straddle = ctx.pricing();
    straddle.synthetic = None;
    straddle.migration.pool_tokens = ctx.live_pool_tokens;
    let b_str = straddle.buy(D80_Q, &ctx.live).expect("B̂_str");
    let sigma = |n: u64| risk::sigma_xy(Some(n), D80_Q, ctx.live.vt, ctx.live.vq).expect("σ");
    let (sigma_v3, sigma_str) = (sigma(v3.net), sigma(b_str.net));
    assert_eq!(sigma_v3, st.sigma_live, "the keeper view's σ is σ(B̂_v3)");
    assert!(
        sigma_v3 > 0 && sigma_v3 < 500,
        "σ(B̂_v3) {sigma_v3} bps: below the parameter's floor"
    );
    assert_ne!(sigma_v3, sigma_str, "σ(B̂_v3) tells B̂_v3 from B̂_str");
    eprintln!(
        "T-SIGMA: σ(B̂_v3) {sigma_v3} bps (B̂_v3 {}), σ(B̂_str) {sigma_str} bps (B̂_str {})",
        v3.cost, b_str.cost
    );
    (s, p, sigma_v3, sigma_str)
}

/// T-SIGMA (C5; PUMP-2026-10-13-3): σ is judged on `B̂_v3` at once, to the basis point. ECON's
/// D@80 % position made liquidatable by the crowd's buy down to `rT = q/2` ([`sigma_setup`]) has
/// σ(`B̂_v3`) ≈ 2–3 % on its synthetic exit, below any `σ_max` the parameter's floor (500 bps)
/// allows, so the position's locked `σ_max` is forged to σ(`B̂_v3`) − 1: the liquidation refuses
/// `UseSolSettle` ("buyback allowed" fails on σ alone; it was liquidatable and the bound defined),
/// and the keeper's `sol_settle` arms the S2 legs and settles S2 at the box amount — no bundle, no
/// wait for the curve. With [`a_straddle_at_sigma_max_is_liquidated_on_the_curve`] (`σ_max` =
/// σ(`B̂_v3`) lands) this pins the threshold at σ(`B̂_v3`): σ(`B̂_str`) differs, so a program
/// judging σ on `B̂_str` would land here (σ(`B̂_str`) below) or refuse there (above).
#[test]
fn a_straddle_above_sigma_max_goes_to_sol_settle() {
    let (mut s, p, sigma_v3, _) = sigma_setup();
    let sigma_max = u16::try_from(sigma_v3 - 1).unwrap();
    forge_position(&mut s.env, &s.pm, &p, |pos| {
        pos.max_liq_slip_bps = sigma_max
    });
    let kp = s.keeper.insecure_clone();
    s.env.warp(1);
    s.crank();
    let st = s.status(&p);
    assert!(st.cr_liq() && !st.buyback_allowed);
    assert_eq!(st.sigma_live, sigma_v3, "σ held");
    let liq = liquidate_ix(&s.pm, &kp.pubkey(), &p.key(), p.nonce);
    assert_err(
        &s.env.send(
            &[pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU), liq],
            &[&kp],
        ),
        E::UseSolSettle,
    );
    let mut settled = None;
    for _ in 0..900 {
        let ix = sol_settle_ix(&s.env, &s.pm, &kp.pubkey(), &p.key(), p.nonce);
        let meta = ok(s.env.send(&[ix], &[&kp]));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            settled = Some(ev);
            break;
        }
        s.env.warp(1);
        s.crank();
    }
    let ev = settled.expect("S2 within 15 min");
    assert_eq!(ev.condition, 2, "S2");
    assert!(ev.lender_amount > 0 && ev.fv > 0);
    assert!(!s.pm.curve.state(&s.env.svm).complete, "no buyback ran");
}

/// T-SIGMA, the other side of the threshold ([`a_straddle_above_sigma_max_goes_to_sol_settle`]):
/// the same position with its `σ_max` forged to exactly σ(`B̂_v3`) — "buyback allowed" holds
/// (`σ ≤ σ_max`) — is liquidated on the curve, never refused `UseSolSettle`: the keeper's
/// `liquidate` lands as soon as the reference bound covers `B̂_v3` and completes the curve
/// synthetically, the lenders' `q` back.
#[test]
fn a_straddle_at_sigma_max_is_liquidated_on_the_curve() {
    let (mut s, p, sigma_v3, _) = sigma_setup();
    let sigma_max = u16::try_from(sigma_v3).unwrap();
    forge_position(&mut s.env, &s.pm, &p, |pos| {
        pos.max_liq_slip_bps = sigma_max
    });
    let st = s.status(&p);
    assert!(st.buyback_allowed && st.sigma_live == sigma_v3);
    let (ev, meta, quote) = liquidate_on_the_curve(&mut s, &p, 120);
    assert!(!ev.expired);
    check_synthetic_exit(&s, &meta, D80_Q, &quote, ev.buyback_cost, ev.b_live);
}

// ---- T-MIG-HALT -----------------------------------------------------------------------------------

/// T-MIG-HALT (pump-final §6.7; PUMP-2026-10-13-4): on `near166`, positions A and B, B 30 min from
/// its deadline. PumpSwap's `disable_flags` bit 0 halts `create_pool`: opens refuse
/// `GraduationHalted`. The crowd buys `rT` below A's `q`; **short.win's own** synthetic close of A
/// still lands and completes the curve (`PumpMigrating`). `migrate_v2` fails 6017; B's close
/// refuses `VenueMigrating`; B expires; after `max_migration_pause_s + s3_wait_s` the keeper's
/// `sol_settle` settles B S3 at §5.4's FV on the live `(X₁, Y₁)` (the expiry fee on top, rent
/// due); the halt lifted, `migrate_v2` creates exactly that pool.
#[test]
fn a_synthetic_close_under_a_create_pool_halt_freezes_the_rest_until_s3() {
    let (mut s, q) = sim_on(PumpCoin::Near166);
    let pb = s.open(q / 2);
    let pa = s.open(q);
    let deadline_b = s.position(&pb).deadline;
    let to = deadline_b - 1_800 - s.env.now();
    s.env.warp(to);
    s.crank();
    pamm::set_disable_flags(&mut s.env.svm, 0x01);
    let mut c = s.env.borrower();
    assert_err(&s.env.open(&s.pm, &mut c, q / 4).0, E::GraduationHalted);
    buy_down_to(&mut s, q / 2);
    s.env.warp(1);
    let quote = b_v3(&s, q);
    let meta = ok(s.env.close(&s.pm, &pa.b, pa.nonce));
    let close: Close = events::one(&meta);
    check_synthetic_exit(&s, &meta, q, &quote, close.buyback_cost, close.b_live);
    let t0 = s.env.now();
    let migrate = |env: &mut Env, pm: &PumpMarket| {
        let migrator = env.funded_keypair();
        let ix = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
        env.send(&[pump::cu_limit_ix(600_000), ix], &[&migrator])
    };
    assert_eq!(
        custom_code(&migrate(&mut s.env, &s.pm)),
        Some(6017),
        "DisabledCreatePool"
    );
    s.env.warp(1);
    assert_err(&s.env.close(&s.pm, &pb.b, pb.nonce), E::VenueMigrating);
    let pos = s.position(&pb);
    let due = t0 + i64::from(pos.max_migration_pause_s) + i64::from(pos.s3_wait_s);
    let kp = s.keeper.insecure_clone();
    let ev = loop {
        s.env.warp(30);
        s.crank();
        let ix = sol_settle_ix(&s.env, &s.pm, &kp.pubkey(), &pb.key(), pb.nonce);
        let meta = ok(s.env.send(&[ix], &[&kp]));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            break ev;
        }
        assert!(s.env.now() < due + 60, "S3 is due at the pause's end");
    };
    assert_eq!(ev.condition, 3, "S3");
    assert!(s.env.now() >= due);
    let m = market(&s.env, &s.pm);
    let p_ema = m.ema_price.get();
    let b_fv = b_fv_migrating(&s.env, &s.pm, &pos, p_ema);
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
    let fv = fair_value(&pos, s.env.now(), r_due, b_fv);
    assert_eq!(ev.fv, fv, "FV at §5.4's ρ-scaled pool price");
    assert_eq!(ev.lender_amount, fv.min(pos.v - k_s(&pos)));
    // The halt lifted: migrate_v2 creates exactly (X₁, Y₁).
    let live = curve(&s.env, &s.pm);
    let c = s.pm.curve.state(&s.env.svm);
    let (x1, y1) = mq::migrated_pool(
        &live,
        &MigrationParams {
            migration_fee: migration_fee(&s.env),
            pool_tokens: u128::from(tokens(&s.env, &s.pm.curve.curve_base_ata)),
            post_complete_quote_in: c.post_complete_quote_in,
        },
    )
    .unwrap();
    pamm::set_disable_flags(&mut s.env.svm, 0);
    ok(migrate(&mut s.env, &s.pm));
    let r = pump::NewPool::of(&s.pm.curve)
        .reserves(&s.env.svm)
        .expect("the canonical pool");
    assert_eq!((u128::from(r.base), r.effective_quote), (x1, y1));
}
