//! The M5 security review's findings on the pump-curve venue as regressions (M5-SEC-1, -3, -4,
//! -5, -6), on ECON's curve markets (`pump_curve_support.rs`): the release short.win on the
//! dumped pump / pump-fees / PumpSwap ELFs. Every instruction goes through `Env::send` (invariant
//! hooks after each one). Each test reproduced its finding on the pre-fix build (the audit's
//! reproducers, `m5_sec_*` of the same names before the fix); after the fix:
//!
//! - **M5-SEC-1** — a full wipe on a live curve no longer locks deposits: `sweep_trader` sets the
//!   venue-reward bit when the trader's pump UVA holds no claimable cashback, and a cashback
//!   coin's lenders get the pre-wipe cashback through `[pump claim_cashback(user = trader),
//!   sweep_trader]` (T-CASHBACK, pump part: `claim_cashback` needs no signature of the trader).
//! - **M5-SEC-3** — the curve caps read the curve at `P_ema` when it holds fewer tokens than the
//!   live one: a sale in the same transaction lifts neither the Tier D guard's `rT` nor `X₀`.
//! - **M5-SEC-4** — M5 could not enable the curve venue; M6 empties that gate: the curve bit is
//!   enableable, and the atomic complete + migrate that froze an M5 market now graduates it onto
//!   its pool (`→ PumpSwap`, the ring rewrite, the post-change warm-up) and the position is
//!   liquidated there (the M5 tripwire, inverted).
//! - **M5-SEC-5** — a canonical-pool account that does not read as a valid pool is not "graduated
//!   elsewhere": the migrated market stays `PumpMigrating` (the list-only D1 side is in
//!   `ray_it::listing` and `pool_select_it::listing`).
//! - **M5-SEC-6** — a mint ground for a low `ATA(trader, WSOL)` bump costs no more per trade.

#![allow(clippy::result_large_err, dead_code, unused_imports)]

#[path = "pump_curve_support.rs"]
mod support;

use shortwin::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID, VENUE_BIT_PUMP_CURVE, VENUE_BITS_UNFINISHED,
    WSOL_MINT,
};
use shortwin::events::TraderSwept;
use shortwin::state::{
    ConfigChange, MARKET_FLAG_GRADUATED_ELSEWHERE, MARKET_FLAG_WIPE_LIVE,
    MARKET_FLAG_WIPE_TRADER_SWEPT,
};
use shortwin_litesvm_tests::fork::PumpV2;
use shortwin_litesvm_tests::ix::LenderKeys;
use shortwin_math::pool::{WIPE_REWARDS_CLAIMED, WIPE_TRADER_SWEPT};
use support::*;

/// ECON's `D@50%` position (`max_open_q()`).
const D50_Q: u64 = 10_071_769_920_663;
/// ECON's `D@80%` market and position.
const D80_SOLD: u64 = 634_480_000_000_000;
const D80_Q: u64 = 6_529_192_912_424;

/// The seeding lender withdraws free tokens down to `keep` (security_m2's helper, on a pump
/// market).
fn withdraw_down_to(s: &mut Sim, keep: u64) {
    let m = s.env.market(&s.pm.market());
    let pool = m.pool();
    let want = m.tokens_free - keep;
    let lender = s
        .env
        .lender(&s.pm.market(), &s.pm.lender.pubkey())
        .expect("lender");
    let (mut lo, mut hi) = (0u128, lender.shares);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if shortwin_math::pool::tokens_for_shares(&pool, mid).unwrap() <= u128::from(want) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let kp = s.pm.lender.insecure_clone();
    ok(s.env
        .send(&[ix::withdraw(&s.pm.lender_keys(), lo, want)], &[&kp]));
    assert_eq!(s.env.market(&s.pm.market()).tokens_free, keep);
}

/// A new lender with `amount` real tokens bought on the curve.
fn new_lender(s: &mut Sim, amount: u64) -> (CurveUser, LenderKeys) {
    let whale = CurveUser::new(&mut s.env, &s.pm.curve);
    ok(whale.buy(&mut s.env, &s.pm.curve, amount, u64::MAX));
    let k = LenderKeys {
        owner: whale.kp.pubkey(),
        mint: s.pm.keys.mint,
        token_program: s.pm.keys.token_program,
        owner_token_account: whale.ata,
    };
    (whale, k)
}

/// `deposit(amount)` of `k` with the share-epoch records it may need.
fn deposit_ix(env: &Env, pm: &PumpMarket, k: &LenderKeys, amount: u64) -> Instruction {
    let market = pm.market();
    let m = env.market(&market);
    let recs = ix::epoch_records(&market, 0, m.share_epoch);
    ix::with_remaining(ix::deposit(k, amount, 0), &recs)
}

/// ECON's `D@50%` market, one capped position, the only lender takes out every free token and a
/// 5x gap settles the position S1: a full wipe on a live curve.
fn full_wipe_on_a_live_curve(s: &mut Sim) {
    let p = s.open(D50_Q);
    withdraw_down_to(s, 0);
    let r = s.run(&p, &Scenario::gap(5.0));
    let Outcome::Settled(ev) = &r.outcome else {
        panic!("expected S1, got {}", r.outcome.kind())
    };
    assert_eq!(ev.condition, 1, "S1");
    let m = s.env.market(&s.pm.market());
    assert_eq!(
        m.venue_kind(),
        Some(VenueKind::PumpCurve),
        "the curve is live"
    );
    assert_eq!(m.total_shares.get(), 0);
    assert!(m.has_flag(MARKET_FLAG_WIPE_LIVE), "a full wipe");
}

/// The cashback pump's `claim_cashback` pays `user` now: its pump UVA's lamports above the
/// account's rent-exempt minimum (pump's `PUMP_CASHBACK_README`).
fn claimable_cashback(env: &Env, user: &Pubkey) -> u64 {
    let uva = fork::pump_user_volume_accumulator(user);
    let acc = env.svm.get_account(&uva).expect("the trader's pump UVA");
    acc.lamports - env.svm.minimum_balance_for_rent_exemption(acc.data.len())
}

// ---- M5-SEC-1: a full wipe on a live curve --------------------------------------------------

/// M5-SEC-1. A deposit into a live full-wipe record on a `PumpCurve` market waits for
/// `wipe_rewards_claimed` (R4A-7), which only M6's `claim_venue_rewards` was to set: an organic
/// full wipe on a live curve (the only lender withdraws every free token, the last position
/// settles S1 on a 5x gap, ECON §11.2 `D@50% gap 5x`) left the market — the only one its mint
/// can have — without deposits, and hence opens, for good (pre-fix: `WipeSweepPending` after 30
/// days of sweeps and cranks; opens 6060). Now the permissionless sweep sets both post-wipe bits
/// when the trader's pump UVA holds no claimable cashback: a new lender deposits, and the market
/// opens again.
#[test]
fn m5_sec_1_a_full_wipe_on_a_live_curve_reopens_after_the_sweep() {
    let mut s = Sim::d_market(SOLD_D50, lend_for_80(D50_Q));
    full_wipe_on_a_live_curve(&mut s);
    let market = s.pm.market();
    assert_eq!(
        claimable_cashback(&s.env, &s.pm.trader()),
        0,
        "not a cashback coin"
    );

    // Before the sweep: the deposit waits (R4A-7, unchanged).
    let amount = 5_000_000_000_000u64;
    let (whale, k) = new_lender(&mut s, amount);
    let d0 = deposit_ix(&s.env, &s.pm, &k, amount);
    assert_err(
        &s.env.send(std::slice::from_ref(&d0), &[&whale.kp]),
        E::WipeSweepPending,
    );

    // The permissionless sweep (with the token leg and the UVA) sets both bits.
    let meta = ok(s
        .env
        .send(&[pump::sweep_trader_pump_ix(&s.env, &s.pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    assert!(
        s.env
            .market(&market)
            .has_flag(MARKET_FLAG_WIPE_TRADER_SWEPT)
    );

    // The new lender deposits; the wipe record is final; opens resume.
    let d1 = deposit_ix(&s.env, &s.pm, &k, amount);
    ok(s.env.send(std::slice::from_ref(&d1), &[&whale.kp]));
    let m = s.env.market(&market);
    assert!(!m.has_flag(MARKET_FLAG_WIPE_LIVE));
    assert!(m.total_shares.get() > 0);
    for _ in 0..10 {
        s.env.warp(1);
        s.crank();
    }
    let mut b = s.env.borrower();
    let q = amount / 5;
    let (res, _) = s.env.open(&s.pm, &mut b, q);
    let open: Open = events::one(&ok(res));
    assert_eq!(open.q, q);
}

/// T-WIPE-GATE (pump-final §14, C6): the lamports seeded into the trader's pump UVA where a cashback
/// coin's trades used to pay its cashback. pump's v3 trades refuse a live cashback coin and
/// short.win no longer lists one (owner D1 (a)), but `claim_cashback` pays any balance above the
/// UVA's rent, donated lamports included, so the wipe gate stays needed and reachable. Above a
/// 0-byte account's rent-exempt minimum.
const SEEDED_CASHBACK: u64 = 1_000_000;

/// Seeds [`SEEDED_CASHBACK`] into the trader's pump UVA of `s`'s non-cashback curve (a System
/// transfer anyone can make); returns what `claim_cashback` pays now.
fn seed_uva(s: &mut Sim) -> u64 {
    assert!(
        !s.pm.curve.state(&s.env.svm).is_cashback_coin,
        "a non-cashback curve (D1 (a))"
    );
    let trader = s.pm.trader();
    let before = claimable_cashback(&s.env, &trader);
    s.env.fund(
        &fork::pump_user_volume_accumulator(&trader),
        SEEDED_CASHBACK,
    );
    let after = claimable_cashback(&s.env, &trader);
    assert_eq!(after - before, SEEDED_CASHBACK);
    after
}

/// M5-SEC-1 with cashback outstanding (as on a legacy cashback coin, where pump routes the creator
/// fee of the trader's own trades to its UVA): after the full wipe the sweep alone leaves the
/// venue-reward bit clear — the UVA still holds the pre-wipe cashback — so the deposit waits;
/// `[pump claim_cashback(user = trader), sweep_trader]`, signed by neither the trader nor anyone
/// short.win knows (T-CASHBACK, pump part, on the dumped ELF: `user` is not a signer), pays the
/// cashback to the trader, the sweep credits it to the wiped epoch's record (R4A-7) and sets both
/// bits, and the deposit lands. T-WIPE-GATE: since D1 (a) on a non-cashback curve, the claimable
/// balance seeded into the trader's pump UVA after the open ([`seed_uva`]) — every wipe-gate
/// assertion kept.
#[test]
fn m5_sec_1_pre_wipe_cashback_reaches_the_wiped_lenders() {
    let mut s = Sim::d_market(SOLD_D50, lend_for_80(D50_Q));
    full_wipe_on_a_live_curve(&mut s);
    let market = s.pm.market();
    let trader = s.pm.trader();
    let cashback = seed_uva(&mut s);
    assert!(cashback > 0, "a claimable balance in the trader's pump UVA");

    // The sweep alone: the trader bit, not the venue-reward bit; the deposit waits.
    let meta = ok(s
        .env
        .send(&[pump::sweep_trader_pump_ix(&s.env, &s.pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(ev.wipe_bits & WIPE_TRADER_SWEPT, WIPE_TRADER_SWEPT);
    assert_eq!(
        ev.wipe_bits & WIPE_REWARDS_CLAIMED,
        0,
        "cashback outstanding"
    );
    let amount = 1_000_000_000_000u64;
    let (whale, k) = new_lender(&mut s, amount);
    let d0 = deposit_ix(&s.env, &s.pm, &k, amount);
    assert_err(
        &s.env.send(std::slice::from_ref(&d0), &[&whale.kp]),
        E::WipeSweepPending,
    );

    // The unlocking bundle, signed by the fee payer only.
    let claim = pump::claim_cashback_ix(&trader);
    assert!(claim.accounts.iter().all(|a| !a.is_signer));
    let lender_sol0 = s.env.market_vault(&market).lender_sol;
    let meta = ok(s
        .env
        .send(&[claim, pump::sweep_trader_pump_ix(&s.env, &s.pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(
        ev.lamports, cashback,
        "the whole cashback, through the trader"
    );
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    assert_eq!(claimable_cashback(&s.env, &trader), 0);
    assert_eq!(
        s.env.market_vault(&market).lender_sol - lender_sol0,
        cashback,
        "credited to the wiped epoch's record"
    );
    let d1 = deposit_ix(&s.env, &s.pm, &k, amount);
    ok(s.env.send(std::slice::from_ref(&d1), &[&whale.kp]));
    assert!(!s.env.market(&market).has_flag(MARKET_FLAG_WIPE_LIVE));
}

/// AIR-08 (I-08), as on a live legacy cashback coin: the trader's own trades (the open's sale and
/// the close's buy) paid cashback into its pump UVA; the position closes and the only lender
/// withdraws everything (`S = 0`, no wipe record). Before the fix a deposit landed at once, so
/// `[deposit(dust), pump claim_cashback(user = trader), sweep_trader, withdraw]` vested the
/// cashback to the dust shares and the last-lender flush paid it back: SOL that KI-10 / R3A-10
/// give the protocol. Now a deposit into a pool every lender left waits for both bits, as after
/// a full wipe: the sweep alone sets only the trader bit while cashback is outstanding, and
/// `[claim_cashback, sweep_trader]` books the cashback to `protocol_fees` and sets both.
/// T-WIPE-GATE: since D1 (a) on a non-cashback curve, the claimable balance seeded into the
/// trader's pump UVA after the close ([`seed_uva`]) — every wipe-gate assertion kept.
#[test]
fn air_08_after_the_last_exit_the_cashback_goes_to_the_protocol() {
    let mut s = Sim::d_market(SOLD_D50, lend_for_80(D50_Q));
    let p = s.open(D50_Q);
    assert!(s.borrower_close(&p).is_some(), "the borrower closes");
    let market = s.pm.market();
    let trader = s.pm.trader();
    let cashback = seed_uva(&mut s);
    assert!(cashback > 0, "a claimable balance in the trader's pump UVA");
    let shares = s
        .env
        .lender(&market, &s.pm.lender.pubkey())
        .expect("lender")
        .shares;
    let kp = s.pm.lender.insecure_clone();
    ok(s.env
        .send(&[ix::withdraw(&s.pm.lender_keys(), shares, 0)], &[&kp]));
    let m = s.env.market(&market);
    assert_eq!(m.total_shares.get(), 0, "every lender left");
    assert!(!m.has_flag(MARKET_FLAG_WIPE_LIVE), "no wipe record");

    // The dust deposit first: refused.
    let amount = 1_000_000_000u64;
    let (whale, k) = new_lender(&mut s, amount);
    let d0 = deposit_ix(&s.env, &s.pm, &k, amount);
    assert_err(
        &s.env.send(std::slice::from_ref(&d0), &[&whale.kp]),
        E::WipeSweepPending,
    );
    // The sweep alone: the trader bit, not the venue-reward bit (cashback outstanding).
    let meta = ok(s
        .env
        .send(&[pump::sweep_trader_pump_ix(&s.env, &s.pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT
    );
    assert_err(
        &s.env.send(std::slice::from_ref(&d0), &[&whale.kp]),
        E::WipeSweepPending,
    );
    // The claim and the sweep: the cashback to the protocol, both bits.
    let fees0 = s.env.market_vault(&market).protocol_fees;
    let meta = ok(s.env.send(
        &[
            pump::claim_cashback_ix(&trader),
            pump::sweep_trader_pump_ix(&s.env, &s.pm),
        ],
        &[],
    ));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(
        (ev.lamports, ev.to),
        (cashback, shortwin::events::SWEEP_TO_PROTOCOL)
    );
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    assert_eq!(
        s.env.market_vault(&market).protocol_fees - fees0,
        cashback,
        "KI-10"
    );
    // Now the deposit lands, and there is no cashback left for it to take.
    let d1 = deposit_ix(&s.env, &s.pm, &k, amount);
    ok(s.env.send(std::slice::from_ref(&d1), &[&whale.kp]));
    assert_eq!(claimable_cashback(&s.env, &trader), 0);
    assert_eq!(s.env.market(&market).vest_bal, 0);
}

// ---- M5-SEC-3: the Tier D graduation guard and X₀ against a same-transaction sale ----------

/// A `buy_v2` / `sell_v2` instruction of a curve user.
fn v2(s: &Sim, u: &CurveUser, kind: PumpV2, q: u64, bound: u64) -> Instruction {
    let fr = fork::pump_fee_recipient(&s.env.svm, 2);
    let bb = fork::pump_buyback_fee_recipient(&s.env.svm, 2);
    s.pm.curve
        .v2_ix(&s.env.svm, kind, &u.kp.pubkey(), &u.ata, &fr, &bb, q, bound)
}

/// `[sell_v2(δ), open_short(q), buy_v2(δ)]` by `attacker` in one transaction.
fn sandwiched_open(s: &mut Sim, attacker: &CurveUser, nonce: u64, q: u64, delta: u64) -> TxResult {
    let open = open_short_ix(
        &s.pm,
        &attacker.kp.pubkey(),
        nonce,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let ixs = [
        pump::cu_limit_ix(1_000_000),
        v2(s, attacker, PumpV2::Sell, delta, 0),
        open,
        v2(s, attacker, PumpV2::Buy, delta, u64::MAX),
    ];
    let kp = attacker.kp.insecure_clone();
    s.env.send(&ixs, &[&kp])
}

/// M5-SEC-3. ARCH §4.6's Tier D graduation guard `ΣQ + q ≤ γ·(rT + q)` read the curve's live
/// `rT`, which the opener moves in the same transaction: `[sell_v2(δ), open_short(q),
/// buy_v2(δ)]` raised it by `δ` for the open only (the 10 % spot–EMA guard lets `δ` reach ≈ 5 %
/// of `vT`, the whole `rT` near completion). Pre-fix the bundle opened what the guard refuses
/// honestly and left `ΣQ/rT` 0.89 against γ = 0.50 (every position a straddle at once). Now the
/// guard reads `min(rT, rT` of the curve at `P_ema)`, which the sale does not move: the bundle is
/// refused `CapCurveGamma` like the honest open.
#[test]
fn m5_sec_3_the_curve_gamma_guard_holds_against_a_same_transaction_sale() {
    const D90_Q: u64 = 5_348_333_909_677;
    let mut s = Sim::d_market(SOLD_D90, 40_000_000_000_000);
    let attacker = CurveUser::new(&mut s.env, &s.pm.curve);
    let delta = 12_000_000_000_000u64;
    ok(attacker.buy(&mut s.env, &s.pm.curve, delta, u64::MAX));
    for _ in 0..120 {
        s.env.warp(1);
        s.crank();
    }
    let max_q = |s: &Sim| {
        let vt = u64::try_from(curve(&s.env, &s.pm).vt).unwrap();
        shortwin_math::caps::max_q_position(vt, 300).unwrap()
    };
    let qa = max_q(&s);
    let _a = s.open(qa);
    // The EMA follows the open's own sale down (a curve cap is read at `P_ema` while the EMA is
    // above the spot, M5-SEC-3): the next capped open waits for it.
    for _ in 0..300 {
        s.env.warp(1);
        s.crank();
    }
    let qb = max_q(&s);
    let _b = s.open(qb);
    for _ in 0..30 {
        let rt = u64::try_from(curve(&s.env, &s.pm).rt).unwrap();
        let target = 12_000_000_000_000u64;
        if rt <= target {
            break;
        }
        s.crowd_buy((rt - target).min(3_000_000_000_000));
        for _ in 0..30 {
            s.env.warp(1);
            s.crank();
        }
    }
    for _ in 0..300 {
        s.env.warp(1);
        s.crank();
    }
    let sum_q0 = s.env.market(&s.pm.market()).borrowed_q;
    let c = curve(&s.env, &s.pm);
    eprintln!(
        "ΣQ {} rT {} vT {} (ΣQ/rT {:.2})",
        sum_q0,
        c.rt,
        c.vt,
        sum_q0 as f64 / c.rt as f64
    );
    let q = 2_500_000_000_000u64;
    let mut borrower = Borrower {
        kp: attacker.kp.insecure_clone(),
        next_nonce: 0,
    };
    let (res, _) = s.env.open(&s.pm, &mut borrower, q);
    assert_err(&res, E::CapCurveGamma);
    // The bundle is refused the same way now (pre-fix: it opened).
    let res = sandwiched_open(&mut s, &attacker, borrower.next_nonce, q, delta);
    assert_err(&res, E::CapCurveGamma);
    assert_eq!(s.env.market(&s.pm.market()).borrowed_q, sum_q0);
    // A genuine sale the EMA has followed is real `rT`: the same size fits once it is there.
    ok(s.crowd_sell(delta));
    for _ in 0..60 {
        s.env.warp(1);
        s.crank();
    }
    let (res, _) = s.env.open(&s.pm, &mut borrower, q);
    let open: Open = events::one(&ok(res));
    assert_eq!(open.q, q);
}

/// M5-SEC-3, the size caps: `X₀ = vT` was the live reserve, which the same sale lifts by `δ` for
/// the open. At ECON's `D@50%` (the EMA at the spot) an open at the per-position cap on `vT`
/// opens honestly, one raw unit more is refused, and the bundle `[sell_v2(δ), open(cap + Δ),
/// buy_v2(δ)]` is refused `CapPositionImpact` too (pre-fix the sale's larger `vT` admitted it).
#[test]
fn m5_sec_3_a_same_transaction_sale_does_not_lift_the_size_cap() {
    let mut s = Sim::d_market(SOLD_D50, lend_for_80(4 * D50_Q));
    let attacker = CurveUser::new(&mut s.env, &s.pm.curve);
    let delta = 20_000_000_000_000u64;
    ok(attacker.buy(&mut s.env, &s.pm.curve, delta, u64::MAX));
    for _ in 0..120 {
        s.env.warp(1);
        s.crank();
    }
    let vt = u64::try_from(curve(&s.env, &s.pm).vt).unwrap();
    let cap = shortwin_math::caps::max_q_position(vt, 300).unwrap();
    let cap_after_sale = shortwin_math::caps::max_q_position(vt + delta, 300).unwrap();
    assert!(cap_after_sale > cap + 100_000_000_000);
    let mut b = s.env.borrower();
    let (res, _) = s.env.open(&s.pm, &mut b, cap + 1);
    assert_err(&res, E::CapPositionImpact);
    let res = sandwiched_open(&mut s, &attacker, 0, (cap + cap_after_sale) / 2, delta);
    assert_err(&res, E::CapPositionImpact);
    let (res, _) = s.env.open(&s.pm, &mut b, cap);
    ok(res);
}

// ---- M5-SEC-4: the curve venue is enableable again (M6), and an atomic completion graduates ---

/// M5-SEC-4's gate, emptied by M6. The M5 build refused to enable the curve venue
/// (`VENUE_BITS_UNFINISHED` = the curve bit): without the `→ PumpSwap` transition a curve that
/// completed and migrated froze its market. M6 has the transition, so `initialize_config` and
/// `propose_config(EnableVenues)` accept the curve bit (alone or with others), and a curve market
/// opens positions in a deployment of this build.
#[test]
fn m5_sec_4_m6_can_enable_the_curve_venue() {
    let mut env = Env::new();
    let args = env.default_init_args();
    assert_ne!(args.venue_enabled & VENUE_BIT_PUMP_CURVE, 0);
    assert_eq!(VENUE_BITS_UNFINISHED, 0, "M6 empties the set");
    ok(env.initialize_config_as_sent(args));
    assert_ne!(env.config().venue_enabled & VENUE_BIT_PUMP_CURVE, 0);
    let gov = env.governance.insecure_clone();
    for bits in [
        VENUE_BIT_PUMP_CURVE,
        VENUE_BIT_PUMP_CURVE | shortwin::constants::VENUE_BIT_RAYDIUM_V4,
    ] {
        ok(env.send(
            &[ix::propose_config(
                &gov.pubkey(),
                ConfigChange::EnableVenues { bits },
            )],
            &[&gov],
        ));
    }
    // A curve market lists, lends and opens.
    env.load_pump_programs();
    env.load_pump_statics();
    let c = env.load_pump_coin(PumpCoin::T22_115);
    env.set_pump_rent();
    env.set_pump_clock();
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::T22_115, 40 * q), c);
    assert_eq!(venue(&env, &pm), VenueKind::PumpCurve);
    ok(env.send(&[crank_price_ix(&pm)], &[]));
    let mut b = env.borrower();
    let (res, _) = env.open(&pm, &mut b, q);
    ok(res);
}

/// M5-SEC-4, **inverted by M6** (it was the tripwire
/// `m5_sec_4_tripwire_an_atomic_complete_and_migrate_freezes_an_m5_market`). Anyone — the borrower itself — completes a curve and runs
/// `migrate_v2` in **one transaction** (`buy_v2(rT)` + `migrate_v2`: ≈ 412k CU, self-financing),
/// so short.win never observes the completed curve, and the pool then trades at a multiple of
/// the last curve EMA (a buyer pushes it ×1.5 on top of the completion: ×3.79 the frozen EMA on
/// the fork). In M5 the market stayed `PumpMigrating` with that EMA for good. Now the next crank
/// moves it `PumpCurve → PumpSwap` (the curve's ReadSet shows the valid canonical pool): the ring
/// is rewritten to the pool's `lp_supply` and the state restarted (`ema_warm = 8`, `ema_guard =
/// 1`), `P_ema` carrying over. Every keeper exit fails `EmaWarmingUp` until eight integrations of
/// the pool's own buckets have re-seeded the EMA — by at most the largest v0.4.1 step each
/// (M4X-1), so the completion forces nothing before honest buckets exist — and the position,
/// underwater at the pool's price, is then liquidated **on the pool**.
#[test]
fn m5_sec_4_an_atomic_complete_and_migrate_graduates_the_market_onto_its_pool() {
    let mut s = Sim::d_market(D80_SOLD, lend_for_80(D80_Q));
    let a = s.open(D80_Q);
    for _ in 0..60 {
        s.env.warp(1);
        s.crank();
    }
    let p_ema0 = s.env.market(&s.pm.market()).ema_price.get();
    let attacker = CurveUser::new(&mut s.env, &s.pm.curve);
    let rt = u64::try_from(curve(&s.env, &s.pm).rt).unwrap();
    s.env.warp(1);
    let buy = v2(&s, &attacker, PumpV2::Buy, rt, u64::MAX);
    let mig = pump::migrate_v2_ix(&s.env.svm, &attacker.kp.pubkey(), &s.pm.curve);
    let meta = ok(s
        .env
        .send(&[pump::cu_limit_ix(1_400_000), buy, mig], &[&attacker.kp]));
    eprintln!(
        "complete + migrate in one tx: {} inner instructions, {} CU",
        meta.inner_instructions.iter().map(Vec::len).sum::<usize>(),
        meta.compute_units_consumed,
    );
    let c = curve(&s.env, &s.pm);
    assert_eq!((c.vt, c.rt), (0, 0), "migrated in the same transaction");
    // An exit on the curve's accounts refuses: its own prologue sees the graduation.
    let kp = s.keeper.insecure_clone();
    let liq = liquidate_ix(&s.pm, &kp.pubkey(), &a.key(), a.nonce);
    assert_err(
        &s.env.send(&[pump::cu_limit_ix(300_000), liq], &[&kp]),
        E::VenueMigrating,
    );
    // The pool trades on (a buyer pushes it ×1.5, the size of the boost's buy-and-burn).
    let np = pump::NewPool::of(&s.pm.curve);
    let r0 = np.reserves(&s.env.svm).unwrap();
    let buyer = s.env.funded_keypair();
    let payer = s.env.payer.pubkey();
    let program = s.pm.curve.token_program.id();
    ok(s.env.send(
        &[fork::create_ata_idempotent_ix(
            &payer,
            &buyer.pubkey(),
            &s.pm.keys.mint,
            &program,
        )],
        &[],
    ));
    let buyer_base = pda::ata(&buyer.pubkey(), &s.pm.keys.mint, &program);
    let wsol = fork::fund_wsol(&mut s.env, &buyer.pubkey(), 200 * SOL);
    let push = u64::try_from(u128::from(r0.base) - u128::from(r0.base) * 1000 / 1225).unwrap();
    ok(s.env.send(
        &[pump::pamm_buy_ix(
            &s.env.svm,
            &np,
            &buyer.pubkey(),
            &buyer_base,
            &wsol,
            push,
            200 * SOL,
        )],
        &[&buyer],
    ));
    let r1 = np.reserves(&s.env.svm).unwrap();
    let p_pool = r1.effective_quote * W / u128::from(r1.base);
    assert!(
        p_pool * 10 > p_ema0 * 35,
        "the pool at ≥ 3.5× the last curve EMA"
    );
    eprintln!("pool / frozen EMA = {:.2}", p_pool as f64 / p_ema0 as f64);

    // The next crank (the curve's ReadSet) graduates the market; P_ema carries over.
    s.env.warp(1);
    let meta = s.crank();
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpSwap);
    let m = s.env.market(&s.pm.market());
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1));
    assert!(
        m.ema_price.get().abs_diff(p_ema0) * 10_000 < p_ema0,
        "the completed curve was never observed: P_ema carries over"
    );
    let pam = shortwin_litesvm_tests::pamm::PammMarket::graduated(&s.env.svm, &s.pm);
    let ixs = shortwin_litesvm_tests::pamm::venue_account_ixs(&s.env.svm, &payer, &pam);
    ok(s.env.send(&ixs, &[]));

    // The keeper cranks the pool every second and tries the liquidation: refused while warming
    // up, executed on the pool once the EMA — rising at most one v0.4.1 step per second — shows
    // the position underwater.
    let mut liquidated = None;
    for sec in 1..=90 {
        s.env.warp(1);
        ok(s.env.send(&[crank_price_ix(&pam)], &[]));
        let warm = s.env.market(&s.pm.market()).ema_warm;
        let liq = liquidate_ix(&pam, &kp.pubkey(), &a.key(), a.nonce);
        let res = s.env.send(&[pump::cu_limit_ix(400_000), liq], &[&kp]);
        if warm > 0 {
            assert_err(&res, E::EmaWarmingUp);
            continue;
        }
        if let Ok(meta) = res {
            liquidated = Some((sec, events::one::<Liquidate>(&meta)));
            break;
        }
    }
    let (sec, ev) = liquidated.expect("liquidated on the pool");
    eprintln!(
        "liquidated on the pool {sec} s after the graduation: buyback {} lamports",
        ev.buyback_cost
    );
    assert!(sec >= 8, "never before the warm-up's eight integrations");
    assert_eq!(s.env.market(&s.pm.market()).open_positions, 0);
}

// ---- M5-SEC-5: an unreadable canonical pool is not "graduated elsewhere" --------------------

/// A `D80` market with a position open whose curve completed (`→ PumpMigrating` at the next
/// crank) and then migrated for real, not cranked since: the canonical pool, its account as
/// `migrate_v2` wrote it, and the market's `migration_seen_at`.
fn completed_and_migrated() -> (Sim, Pubkey, solana_account::Account, i64) {
    let mut s = Sim::d_market(D80_SOLD, lend_for_80(D80_Q));
    let _a = s.open(D80_Q);
    for _ in 0..10 {
        s.env.warp(1);
        s.crank();
    }
    let rt = u64::try_from(curve(&s.env, &s.pm).rt).unwrap();
    s.crowd_buy(rt);
    s.env.warp(1);
    venue_changed(&s.crank(), VenueKind::PumpCurve, VenueKind::PumpMigrating);
    let seen = s.env.market(&s.pm.market()).migration_seen_at;
    assert!(seen > 0);
    let migrator = s.env.funded_keypair();
    let mig = pump::migrate_v2_ix(&s.env.svm, &migrator.pubkey(), &s.pm.curve);
    ok(s.env.send(&[pump::cu_limit_ix(600_000), mig], &[&migrator]));
    let np = pump::NewPool::of(&s.pm.curve);
    let orig = s.env.svm.get_account(&np.pool).unwrap();
    (s, np.pool, orig, seen)
}

/// M5-SEC-5. A63: a correctly keyed venue-state account that no longer parses makes the venue
/// "not executable" and changes nothing else. Pre-fix `pump_curve::sync` read a migrated curve
/// (`vT = 0`) whose canonical pool did not parse as **graduated elsewhere** (`PumpMigrating →
/// None`, `graduated_elsewhere` set, the migration pause dropped from the S3 timer, the list-only
/// market open to a Raydium adoption against D1) and never came back. Now every way the pool
/// account can stop reading as a valid pool — a changed discriminator (a PumpSwap upgrade), a bool
/// byte other than 0/1, a shifted layout (the creator no longer where it was) — leaves the market
/// `PumpMigrating` (no transition, no flag, `migration_seen_at` kept); the pool reading again
/// moves it onto the pool (M6's `PumpMigrating → PumpSwap`; M5 kept it `PumpMigrating`).
#[test]
fn m5_sec_5_an_unreadable_canonical_pool_is_not_graduated_elsewhere() {
    let (mut s, pool, orig, seen) = completed_and_migrated();
    let market = s.pm.market();
    type Change<'a> = (&'a str, &'a dyn Fn(&mut Vec<u8>));
    let changes: [Change; 3] = [
        ("discriminator", &|d| d[0] ^= 0xff),
        ("a bool byte 2", &|d| {
            d[shortwin_venue_state::pump_amm::OFF_IS_MAYHEM_MODE] = 2
        }),
        ("a shifted layout", &|d| {
            d[shortwin_venue_state::pump_amm::OFF_CREATOR] ^= 1
        }),
    ];
    for (what, change) in changes {
        let mut acc = orig.clone();
        change(&mut acc.data);
        s.env.svm.set_account(pool, acc).unwrap();
        s.env.warp(1);
        let meta = s.crank();
        no_venue_change(&meta);
        let m = s.env.market(&market);
        assert_eq!(m.venue_kind(), Some(VenueKind::PumpMigrating), "{what}");
        assert!(!m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE), "{what}");
        assert_eq!(m.migration_seen_at, seen, "{what}: the pause is kept");
    }
    s.env.svm.set_account(pool, orig).unwrap();
    s.env.warp(1);
    venue_changed(&s.crank(), VenueKind::PumpMigrating, VenueKind::PumpSwap);
    let m = s.env.market(&market);
    assert!(!m.has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE));
    assert_eq!(
        m.pool_cache[shortwin::state::pump_swap_cache::POOL_BASE_VAULT],
        {
            let p = shortwin_venue_state::pump_amm::read_pool(
                &shortwin_venue_state::pump_amm::PROGRAM,
                &s.env.svm.get_account(&pool).unwrap().data,
            )
            .unwrap();
            Pubkey::new_from_array(p.pool_base_token_account)
        }
    );
}

/// M5-SEC-5's other side: a migrated curve whose canonical address holds nothing is still
/// graduated elsewhere (`PumpMigrating → None`, `graduated_elsewhere` set).
#[test]
fn m5_sec_5_an_empty_canonical_address_is_graduated_elsewhere() {
    let (mut s, pool, _, _) = completed_and_migrated();
    s.env
        .svm
        .set_account(pool, solana_account::Account::default())
        .unwrap();
    s.env.warp(1);
    let meta = s.crank();
    venue_changed(&meta, VenueKind::PumpMigrating, VenueKind::None);
    assert!(
        s.env
            .market(&s.pm.market())
            .has_flag(MARKET_FLAG_GRADUATED_ELSEWHERE)
    );
}

// ---- M5-SEC-6: a low-bump market pays no more per trade ------------------------------------

/// `ATA(trader(market(mint)), WSOL)`'s bump.
fn trader_wsol_ata_bump(mint: &Pubkey) -> u8 {
    let market = pda::market(mint).0;
    let trader = pda::trader(&market).0;
    Pubkey::find_program_address(
        &[
            trader.as_ref(),
            TOKEN_PROGRAM_ID.as_ref(),
            WSOL_MINT.as_ref(),
        ],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .1
}

/// Every occurrence of `from` in `d` replaced by `to`.
fn swap_key(d: &mut [u8], from: &Pubkey, to: &Pubkey) {
    let (f, t) = (from.to_bytes(), to.to_bytes());
    let mut i = 0;
    while i + 32 <= d.len() {
        if d[i..i + 32] == f {
            d[i..i + 32].copy_from_slice(&t);
            i += 32;
        } else {
            i += 1;
        }
    }
}

/// `from`'s coin copied under the mint `mint`: the mint account, the curve at `mint`'s curve
/// PDA and its base ATA (mint and owner rewritten). Same creator, same reserves.
fn relocate(env: &mut Env, from: &PumpCurve, mint: Pubkey) -> PumpCurve {
    let program = from.token_program.id();
    let curve = pda::bonding_curve(&mint);
    let base = pda::ata(&curve, &mint, &program);
    let mut m = env.svm.get_account(&from.mint).unwrap();
    swap_key(&mut m.data, &from.mint, &mint);
    env.svm.set_account(mint, m).unwrap();
    let c = env.svm.get_account(&from.curve).unwrap();
    env.svm.set_account(curve, c).unwrap();
    let mut a = env.svm.get_account(&from.curve_base_ata).unwrap();
    swap_key(&mut a.data, &from.mint, &mint);
    swap_key(&mut a.data, &from.curve, &curve);
    env.svm.set_account(base, a).unwrap();
    PumpCurve {
        mint,
        curve,
        curve_base_ata: base,
        ..*from
    }
}

/// M5-SEC-6. Every curve trade derived `ATA(trader, WSOL)` (T11) with `find_program_address`,
/// which charges 1,500 CU per bump it tries; the trader is the market's PDA, so a coin's
/// creator who grinds the mint (2^10 candidates here) gives it a low bump and adds
/// `1,500·(255 − bump)` CU to every open, close, liquidation and expiry of that market (pre-fix:
/// +13.5k CU and more on this pair; 25–30 extra bumps ≈ 37–45k, past ARCH §6.1's PROPOSED 250k
/// budget). Now the key is derived once at listing (`Market::trader_wsol_ata`) and the recipient
/// ATAs come from a table: the same open on the fixture coin and on its copy under a ground mint
/// costs the same.
#[test]
fn m5_sec_6_a_low_bump_market_costs_no_more_per_trade() {
    // A live curve with real SOL (the fresh fixture has 1 lamport: no sale fits).
    let (mut env, fresh) = Env::fork_pump_coin(PumpCoin::T22_115);
    let mut n = 0u32;
    let ground = loop {
        n += 1;
        let k = Pubkey::new_unique();
        if trader_wsol_ata_bump(&k) <= 245 {
            break k;
        }
    };
    let (b_fresh, b_ground) = (
        trader_wsol_ata_bump(&fresh.mint),
        trader_wsol_ata_bump(&ground),
    );
    eprintln!("trader WSOL ATA bumps: fixture {b_fresh}, ground {b_ground} ({n} candidates)");
    let low = relocate(&mut env, &fresh, ground);
    // The shared creator vault holds its rent already, so neither first trade tops it up.
    let vault = fresh.creator_vault(&env.svm);
    let rent0 = env.svm.minimum_balance_for_rent_exemption(0);
    env.fund(&vault, rent0);
    ok(env.list_pump(&fresh));
    ok(env.list_pump(&low));
    let q = fresh.state(&env.svm).virtual_token_reserves / 200;
    let pm_fresh = env.pump_market_listed(PumpSpec::new(PumpCoin::T22_115, 40 * q), fresh);
    let pm_low = env.pump_market_listed(PumpSpec::new(PumpCoin::T22_115, 40 * q), low);
    assert_eq!(
        env.market(&pm_low.market()).trader_wsol_ata,
        pump::wsol_ata(&pm_low.trader()),
        "cached at listing"
    );
    ok(env.send(&[crank_price_ix(&pm_fresh), crank_price_ix(&pm_low)], &[]));
    env.warp(1);
    // short.win's own CU of each open: its frame less pump's (pump derives its own PDAs of the
    // mint — the curve, its ATA, the trader's UVA — so its CU moves with the mint too, which
    // short.win cannot change; the residual is recorded in the review's design notes).
    let mut own = Vec::new();
    for pm in [&pm_fresh, &pm_low] {
        // The borrower's own PDAs (position, exposure) are derived at the open too; a borrower
        // whose both have bump 255 keeps that (its own) cost equal on both markets.
        let mut b = loop {
            let b = env.borrower();
            let (_, pb) = pda::position(&pm.market(), &b.key(), 0);
            let (_, eb) = pda::exposure(&pm.market(), &b.key());
            if pb == 255 && eb == 255 {
                break b;
            }
        };
        let (res, _) = env.open(pm, &mut b, q);
        let meta = ok(res);
        // The outermost frame of each program reports the most (its CPIs included).
        let sw = consumed(&meta, &shortwin::ID).into_iter().max().unwrap();
        let pumped = consumed(&meta, &shortwin::constants::PUMP_PROGRAM_ID)
            .into_iter()
            .max()
            .unwrap();
        eprintln!(
            "open: total {} CU, short.win {sw} CU of which pump {pumped} CU",
            meta.compute_units_consumed
        );
        assert!(
            meta.compute_units_consumed <= 250_000,
            "ARCH §6.1's PROPOSED open budget"
        );
        own.push(sw - pumped);
    }
    assert!(
        own[0].abs_diff(own[1]) < 1_500,
        "a trade no longer derives the trader's WSOL ATA ({} vs {} CU of short.win's own)",
        own[0],
        own[1]
    );
}

/// The CU each top-level or CPI frame of `program` reported consuming, in log order.
fn consumed(meta: &TransactionMetadata, program: &Pubkey) -> Vec<u64> {
    let head = format!("Program {program} consumed ");
    meta.logs
        .iter()
        .filter_map(|l| l.strip_prefix(&head))
        .filter_map(|r| r.split(' ').next()?.parse().ok())
        .collect()
}
