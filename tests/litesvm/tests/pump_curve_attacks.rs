//! ECON §12's attack patterns that apply on the bonding curve, reproduced on the fork against the
//! release short.win and the dumped pump ELFs (ARCH §8.2 "adversarial suite … on every real venue
//! as it lands"; M5): the attacker trades the real curve (`buy_v2` / `sell_v2`) and lands its
//! bundles as single transactions, the invariant hooks check every instruction of every bundle.
//!
//! | ECON | Test |
//! |---|---|
//! | §12.2 `sandwich.py`, expiry, atomic: D@50% honest `0.707 / 0.0226 / 0.0141 / 2.168`, attacked `0.713 / … / 2.161`, attacker `+0.003`, borrower `−0.006`, lenders `+0.0000` | [`sandwich_expiry_d50`] |
//! | §12.2 the same, D@90%: `1.331 → 1.343`, attacker `+0.004`, borrower `−0.012` | [`sandwich_expiry_d90`] |
//! | §12.2 `sandwich.py` / `fixed_delta.py` (d), liquidation, atomic, keeper at once and after 4 s: D@50% `1.678 → 1.709` (`+0.064`, extra `+0.014`, borrower `−0.031`) and `1.713` after 4 s (`+0.066`, `+0.016`, `−0.035`; under the minimum growth rule before AIR-22 `1.711`, `+0.065`, `+0.015`, `−0.033`) | [`sandwich_liquidation_d50`] |
//! | §12.4 `ema_atomic.py`, atomic `[pump, crank, dump]` every second against a fresh position: "not liquidatable within 60 s at any push level" (D@50%) | [`atomic_pushes_never_liquidate_a_fresh_position`] |
//! | §12.4 the held model (every observation of the second pumped): D@50% `x3, 11 s`, push cost `9.439`, final sandwich `+0.663`, attacker net `−8.776` (AIR-22; `+0.655` / `−8.785` under the minimum) | [`held_pushes_liquidate_a_fresh_position_at_a_loss`] |
//! | §12.5 `ema_atomic_multi.py` (a), atomic, the OI-cap basket: "no exit forced within 120 s at any push level" (D@50%) | [`atomic_pushes_force_no_exit_on_the_oi_basket`] |
//! | §12.5 `ema_atomic_multi.py` (b), atomic, S1 forcing on the OI-cap basket at ×4.4 / ×5.2 / ×6.4 with `sol_settle` on every position inside the pumped transaction each second: "no exit forced within 120 s" (D@50%) — no clock arms | [`atomic_pushes_cannot_force_s1_on_the_oi_basket`] (M10) |
//! | §12.5 `ema_atomic_multi.py` (b), held: D@50% `4 / 7.11 | x4.40 | 58 | 72.31 | −51.12 | … | 0 / 1 | −123.04`; on the program's basket (its fourth position smaller, [`oi_basket`]) the bundles start at 18 s, the first S1 at 58 s, best net `−123.14` | [`held_pushes_forcing_s1_on_the_oi_basket_lose`] (M10) |
//! | §12.3 `ema_near.py`, atomic, D@50% (the script's functions on `D@50%`): after honest rises ×1.82 … ×2.17 (`CR_ema` 1.29 … 1.08 × `L`, the sim's to the bps) "not reached within 30 s" at every push ×1.02 … ×2; the final bundle `NotLiquidatable` | [`atomic_pushes_never_liquidate_a_near_threshold_position`] (M10) |
//! | §12.3 `ema_near.py`, held, D@50% ×2.17: `x1.15, 7 s | 0.875 | +0.182 | −0.693` (AIR-22; `+0.180 | −0.694` under the minimum) | [`held_pushes_near_the_threshold_do_not_pay`] (M10) |
//! | §12.6 / `sandwich.py` held liquidation, D@50%: best plan "none" (`+0.064`); the plans 1 s at +2.5 % (`−0.017` push, `+0.069`, net `+0.052`) and 3 s at +30 % (`−0.790`, `+0.441`, net `−0.350`; AIR-22, `+0.068` / `+0.427` under the minimum) to the lamport, the lenders' legs above the honest ones | [`held_pushes_before_a_liquidation_sandwich_do_not_pay`] (M10) |
//!
//! The sandwich and held-push figures are the scripts' own (`sandwich.setup_expiry`,
//! `sandwich.setup_liq`, `sandwich.attack(…, "atomic", wait)`, `common.sandwich_exit`,
//! `ema_atomic.attack("D@50%", model)`, `common.push_second`) on ECON's `D@50%` / `D@90%`
//! markets, replayed step for step (the same cranks, the same exact trade sizes, the scripts'
//! optimal front-run `dX`); the liquidation legs below the rent move by the documented `ΔR`
//! (`pump_curve_support.rs`: `R_min` at 80 % in the sim, the pre-open 0 % in the program), which
//! the attacker's P&L does not contain. The expiry rows' rent is the accrued one: exact.
//!
//! The M10 rows' figures were printed by the scripts' own functions on `D@50%` (`ema_near.setup`
//! / `attack`, `sandwich.attack(…, "held")`'s plans, `ema_atomic_multi.fill` / `rolling`), whose
//! tables print other markets; the CR rows compare `CR_ema` at the sim's rent (`V − R_min`).
//!
//! Not on the curve in M5: §12.2's D@90% liquidation rows (the +0.1 %/s ramp completes the curve
//! at 1.65× before the position is liquidatable; the sim then liquidates on the migrated pool,
//! M6) and every D@0% row (no short can open on a curve without real SOL,
//! `pump_curve_scenarios.rs`).

#![allow(clippy::result_large_err)]

#[path = "pump_curve_support.rs"]
mod support;

use shortwin_litesvm_tests::engine::with_cu_limit;
use shortwin_litesvm_tests::fork::PumpV2;
use shortwin_math::{quote, risk};
use support::*;

/// ECON's `D@50%` position (`max_open_q()`), its `V` and the sim's `R_min`.
const D50_Q: u64 = 10_071_769_920_663;
const D50_V: u64 = 2_912_898_540;
const D50_RMIN: u64 = 1_294_111;
/// ECON's `D@90%` position and `V`.
const D90_Q: u64 = 5_348_333_909_677;
const D90_V: u64 = 5_485_454_676;

/// The push levels of `ema_atomic.py` / `ema_atomic_multi.py`.
const MULTS: [f64; 6] = [1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// A CU limit that covers a bundle `[buy, exit, sell]` on the curve.
const BUNDLE_CU: u32 = 1_000_000;

/// An attacker: a curve user (base ATA and pump UVA created) that is also the keeper of the
/// exits it sandwiches.
struct Attacker {
    user: CurveUser,
}

impl Attacker {
    fn new(s: &mut Sim) -> Self {
        let user = CurveUser::new(&mut s.env, &s.pm.curve);
        Self { user }
    }

    fn key(&self) -> Pubkey {
        self.user.kp.pubkey()
    }

    fn kp(&self) -> Keypair {
        self.user.kp.insecure_clone()
    }

    fn trade_ix(&self, s: &Sim, kind: PumpV2, q: u64, bound: u64) -> Instruction {
        let fr = fork::pump_fee_recipient(&s.env.svm, 2);
        let bb = fork::pump_buyback_fee_recipient(&s.env.svm, 2);
        s.pm.curve.v2_ix(
            &s.env.svm,
            kind,
            &self.key(),
            &self.user.ata,
            &fr,
            &bb,
            q,
            bound,
        )
    }

    /// `buy_v2(q, u64::MAX)`.
    fn buy_ix(&self, s: &Sim, q: u64) -> Instruction {
        self.trade_ix(s, PumpV2::Buy, q, u64::MAX)
    }

    /// `sell_v2(q, 0)`.
    fn sell_ix(&self, s: &Sim, q: u64) -> Instruction {
        self.trade_ix(s, PumpV2::Sell, q, 0)
    }

    fn lamports(&self, s: &Sim) -> i128 {
        i128::from(lamports(&s.env, &self.key()))
    }
}

/// `move_size(v, target)` of `common.py` on the curve: `move_to_price`'s size, a buy capped at
/// `rT − 1`.
fn move_size(s: &Sim, target: u128) -> i128 {
    let d = s.move_size(target);
    let rt = i128::try_from(curve(&s.env, &s.pm).rt).unwrap();
    if d > 0 { d.min(rt - 1) } else { d }
}

/// The attacker's sandwich `[buy dX, exit, sell dX]` in one transaction: the exit's event
/// payload and the attacker's P&L (its lamports; the tokens round-trip), which includes the
/// keeper's pay.
fn sandwich(
    s: &mut Sim,
    a: &Attacker,
    p: &Pos,
    dx: u64,
    expiry: bool,
) -> (TransactionMetadata, i128) {
    let kp = a.kp();
    let exit = if expiry {
        expire_ix(&s.pm, &a.key(), &p.key(), p.nonce)
    } else {
        liquidate_ix(&s.pm, &a.key(), &p.key(), p.nonce)
    };
    let ixs = [
        pump::cu_limit_ix(BUNDLE_CU),
        a.buy_ix(s, dx),
        exit,
        a.sell_ix(s, dx),
    ];
    let l0 = a.lamports(s);
    let meta = ok(s.env.send(&ixs, &[&kp]));
    (meta, a.lamports(s) - l0)
}

/// The honest keeper's exit at this second.
fn honest(s: &mut Sim, p: &Pos, expiry: bool) -> TransactionMetadata {
    let kp = s.keeper.insecure_clone();
    let exit = if expiry {
        expire_ix(&s.pm, &kp.pubkey(), &p.key(), p.nonce)
    } else {
        liquidate_ix(&s.pm, &kp.pubkey(), &p.key(), p.nonce)
    };
    let cu = pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU);
    ok(s.env.send(&[cu, exit], &[&kp]))
}

// ---- §12.2: the exit sandwich --------------------------------------------------------------

/// `sandwich.setup_expiry(key)`: the position at the cap, a crank every 30 s until 120 s before
/// the deadline, then every second until it; returns the sim second (the deadline).
fn setup_expiry(sold: u64, q: u64) -> (Sim, Pos, i64) {
    let mut s = Sim::d_market(sold, lend_for_80(q));
    let p = s.open(q);
    let deadline = s.position(&p).deadline - s.t0;
    let mut t = 0;
    while t < deadline - 120 {
        t = (t + 30).min(deadline - 120);
        s.step_to(t);
    }
    while t < deadline {
        t += 1;
        s.step_to(t);
    }
    (s, p, t)
}

/// An expiry as the sim computed it.
struct SimExp {
    b: u64,
    rent: u64,
    fee: u64,
    keeper: u64,
    borrower: u64,
    b_ref: u64,
    max_in: u64,
}

fn assert_exp(ev: &Expire, sim: &SimExp) {
    assert_eq!(
        (
            ev.buyback_cost,
            ev.rent,
            ev.expiry_fee,
            ev.keeper_reward,
            ev.borrower_payout
        ),
        (sim.b, sim.rent, sim.fee, sim.keeper, sim.borrower),
        "B, rent, expiry fee, K_e, borrower: the sim's"
    );
    assert_eq!(
        (ev.b_ref, ev.max_in),
        (sim.b_ref, sim.max_in),
        "B̂_ref, max_in"
    );
}

/// `sandwich.py`, expiry, model atomic, `D@50%` (ECON §12.2): the honest keeper's expiry at the
/// deadline and the attacker-keeper's best sandwich (`dX` = the script's grid optimum), to the
/// lamport. The expiry bound (`B̂_ref·(1 + δ_exp)` with rent and the expiry fee senior) lets the
/// attacker raise the buyback by 0.9 % of `B̂_ref`: attacker `+0.0027` SOL including `K_e`
/// (`+0.001` over an honest keeper), borrower `−0.0065`, and the rent and expiry fee — the
/// lenders' and the protocol's legs — are paid in full.
#[test]
fn sandwich_expiry_d50() {
    let (mut s, p, t) = setup_expiry(SOLD_D50, D50_Q);
    assert_eq!(t, 259_200);
    let saved = s.env.svm.clone();
    let meta = honest(&mut s, &p, true);
    let h: Expire = events::one(&meta);
    let honest_exp = SimExp {
        b: 706_857_371,
        rent: 22_615_871,
        fee: 14_137_148,
        keeper: 1_706_857,
        borrower: 2_167_581_293,
        b_ref: 706_857_386,
        max_in: 713_925_959,
    };
    assert_exp(&h, &honest_exp);
    s.env.svm = saved;
    let a = Attacker::new(&mut s);
    let (meta, pnl) = sandwich(&mut s, &a, &p, 3_089_347_964_642, true);
    let ev: Expire = events::one(&meta);
    assert_exp(
        &ev,
        &SimExp {
            b: 713_310_296,
            borrower: 2_161_128_368,
            ..honest_exp
        },
    );
    assert_eq!(pnl, 2_701_644, "the attacker's net incl. K_e (ECON +0.003)");
    assert_eq!(sol4(pnl - i128::from(h.keeper_reward)), "+0.0010");
    assert_eq!(
        sol4(i128::from(h.borrower_payout - ev.borrower_payout)),
        "+0.0065",
        "the borrower's loss (ECON 0.006)"
    );
    assert_eq!(
        (ev.rent, ev.expiry_fee),
        (h.rent, h.expiry_fee),
        "lenders and protocol unharmed (+0.0000)"
    );
    assert!(ev.buyback_cost <= ev.max_in);
}

/// `sandwich.py`, expiry, model atomic, `D@90%` (ECON §12.2): honest `1.331 / 0.0426 / 0.0266 /
/// 4.083`, attacked `1.343 / … / 4.071`, attacker `+0.004`, borrower `−0.012`, lenders `−0.0000`
/// (the fee base rises with the reference, by 3 lamports).
#[test]
fn sandwich_expiry_d90() {
    let (mut s, p, t) = setup_expiry(SOLD_D90, D90_Q);
    assert_eq!(t, 259_200);
    assert_eq!(s.position(&p).v, D90_V);
    let saved = s.env.svm.clone();
    let meta = honest(&mut s, &p, true);
    let h: Expire = events::one(&meta);
    let honest_exp = SimExp {
        b: 1_331_125_715,
        rent: 42_589_310,
        fee: 26_622_515,
        keeper: 2_331_125,
        borrower: 4_082_786_011,
        b_ref: 1_331_125_885,
        max_in: 1_344_437_143,
    };
    assert_exp(&h, &honest_exp);
    s.env.svm = saved;
    let a = Attacker::new(&mut s);
    let (meta, pnl) = sandwich(&mut s, &a, &p, 1_640_512_502_593, true);
    let ev: Expire = events::one(&meta);
    assert_exp(
        &ev,
        &SimExp {
            b: 1_343_277_608,
            fee: 26_622_518,
            borrower: 4_070_634_115,
            ..honest_exp
        },
    );
    assert_eq!(pnl, 4_204_470, "the attacker's net incl. K_e (ECON +0.004)");
    assert_eq!(
        sol4(i128::from(h.borrower_payout - ev.borrower_payout)),
        "+0.0122",
        "the borrower's loss (ECON 0.012)"
    );
}

/// `sandwich.setup_liq(key)`: the position at the cap, a crank every second for 120 s, then an
/// honest +0.1 %/s rise (other users' real buys) with a crank every second until the first
/// second the position is CR-liquidatable on both legs.
fn setup_liq() -> (Sim, Pos, i64) {
    let mut s = Sim::d_market(SOLD_D50, lend_for_80(D50_Q));
    let p = s.open(D50_Q);
    let spot_post = s.spot();
    for t in 1..=120 {
        s.step_to(t);
    }
    let mut t = 120;
    let mut k = 1.0f64;
    while k < 5.0 {
        k *= 1.001;
        t += 1;
        s.at(t);
        assert!(s.exo_move((spot_post as f64 * k) as u128).is_none());
        s.crank();
        if s.status(&p).cr_liq() {
            return (s, p, t);
        }
    }
    panic!("never liquidatable");
}

/// A liquidation as the sim computed it (`rent` the sim's `R_min`).
struct SimLiq {
    b: u64,
    pen: u64,
    keeper: u64,
    borrower: u64,
    b_ref: u64,
    max_in: u64,
}

/// `B`, `B̂_ref`, `max_in`, the penalty and the keeper's share exactly; the borrower's remainder
/// moves by `ΔR` (module docs). Returns `ΔR`.
fn assert_liq(ev: &Liquidate, sim: &SimLiq, v: u64) -> u64 {
    assert_eq!(ev.buyback_cost, sim.b, "B");
    assert_eq!(
        (ev.b_ref, ev.max_in),
        (sim.b_ref, sim.max_in),
        "B̂_ref, max_in"
    );
    assert_eq!(ev.penalty, sim.pen, "penalty");
    assert_eq!(ev.penalty_keeper + ev.keeper_topup, sim.keeper, "keeper");
    assert!(ev.rent < D50_RMIN);
    let d = D50_RMIN - ev.rent;
    assert_eq!(
        ev.borrower_payout,
        sim.borrower + d,
        "borrower (rent adjusted)"
    );
    assert_eq!(
        ev.buyback_cost + ev.rent + ev.penalty + ev.keeper_topup + ev.borrower_payout,
        v
    );
    d
}

/// `sandwich.py` liquidation rows and `fixed_delta.py` (d), `D@50%`, model atomic: the honest
/// keeper at the first liquidatable second, and the attacker-keeper's best sandwich at once and
/// after waiting 4 s with nobody exiting. The fixed 2 % bound over the reference caps the
/// front-run: the extraction over an honest keeper is `+0.014` SOL at once and `+0.016` after
/// 4 s (the sim; v0.3's Dutch clock widened with waiting), the borrower loses `0.031` / `0.035`,
/// and the penalty — the lenders' and protocol's legs — is the honest one (`+0.0000`; after 4 s
/// the reference moved up, so the penalty slightly more). AIR-22: the sustained growth is the
/// lower median of the newest steps, so after the ramp `P_ref` keeps rising a little longer than
/// under the minimum (ECON: `+0.015` / `0.033` after 4 s).
#[test]
fn sandwich_liquidation_d50() {
    let (mut s, p, t) = setup_liq();
    assert_eq!(t, 977, "the sim's first liquidatable second (k = 2.355)");
    let saved = s.env.svm.clone();
    let meta = honest(&mut s, &p, false);
    let h: Liquidate = events::one(&meta);
    let honest_liq = SimLiq {
        b: 1_678_058_339,
        pen: 167_704_608,
        keeper: 50_311_382,
        borrower: 1_065_841_482,
        b_ref: 1_677_046_080,
        max_in: 1_710_587_001,
    };
    let d = assert_liq(&h, &honest_liq, D50_V);

    // The attacker-keeper at t: the script's grid optimum dX.
    s.env.svm = saved.clone();
    let a = Attacker::new(&mut s);
    let (meta, pnl) = sandwich(&mut s, &a, &p, 4_026_193_326_522, false);
    let ev: Liquidate = events::one(&meta);
    assert_eq!(
        assert_liq(
            &ev,
            &SimLiq {
                b: 1_709_033_596,
                borrower: 1_034_866_225,
                ..honest_liq
            },
            D50_V
        ),
        d
    );
    assert_eq!(
        pnl, 64_184_611,
        "attacker net incl. the keeper share (ECON +0.064)"
    );
    let extra = pnl - i128::from(h.penalty_keeper);
    assert_eq!(
        sol4(extra),
        "+0.0139",
        "extra over an honest keeper (ECON +0.014)"
    );
    assert_eq!(
        sol4(i128::from(h.borrower_payout - ev.borrower_payout)),
        "+0.0310",
        "borrower loss (ECON 0.031)"
    );
    assert_eq!(
        (ev.penalty_lender, ev.penalty_protocol, ev.rent),
        (h.penalty_lender, h.penalty_protocol, h.rent),
        "lenders + protocol: +0.0000"
    );
    assert!(u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * 10_200);

    // After 4 s of waiting (cranks only, nobody exits): the same fixed δ.
    s.env.svm = saved;
    for tt in t + 1..=t + 4 {
        s.step_to(tt);
    }
    let a = Attacker::new(&mut s);
    let (meta, pnl4) = sandwich(&mut s, &a, &p, 4_473_548_140_580, false);
    let ev4: Liquidate = events::one(&meta);
    assert_liq(
        &ev4,
        &SimLiq {
            b: 1_712_528_146,
            pen: 167_907_691,
            keeper: 50_372_307,
            borrower: 1_031_168_592,
            b_ref: 1_679_076_902,
            max_in: 1_712_658_440,
        },
        D50_V,
    );
    assert_eq!(pnl4, 65_820_219, "the sim: +0.066 after 4 s");
    assert_eq!(ev4.delta_bps, 200, "the fixed δ");
    let extra4 = pnl4 - i128::from(h.penalty_keeper);
    assert_eq!(sol4(extra4), "+0.0155", "the sim: +0.016 after 4 s");
    // The δ stays 200 bps; the extra grows only with the reference, which rises with the EMA's
    // catch-up after the ramp (+11.8 % over these 4 s since AIR-22, +5.9 % under the minimum).
    assert!(
        extra4 * 100 <= extra * 120,
        "waiting does not widen the bound"
    );
}

// ---- §12.4 / §12.5: EMA pushes -------------------------------------------------------------

/// `common.would_liq`: the attacker's test that its own final transaction could liquidate — the
/// EMA leg on the ring now, a fresh bucket, and `CR < L` at the last bucket's low repriced on
/// the live `√(vT·vQ)` (the attacker pumps the live spot above that low inside the transaction).
fn would_liq(s: &Sim, p: &Pos) -> bool {
    let st = s.status(p);
    if !st.ema_leg || !st.fresh {
        return false;
    }
    let pos = s.position(p);
    let m = market(&s.env, &s.pm);
    let lo = m.observation().last.lo;
    let c = curve(&s.env, &s.pm);
    let l_live = quote::liquidity(c.vt, c.vq).unwrap();
    let ctx = CurveCtx::read(&s.env, &s.pm).unwrap();
    let b = ctx
        .buy_repriced(pos.q, l_live, lo, u64::from(pos.venue_fee_hard_cap_bps))
        .map(|b| b.cost);
    risk::cr_below(st.v_eff, b, pos.cr_liq_bps).unwrap()
}

/// One attacker second of `push_second(m, t, target, model)`: atomic — the honest keeper cranks
/// at the true price, then the attacker's `[buy d, crank_price, sell d]`; held — the attacker's
/// bundle is the second's only observation (every observation of the second pumped). Returns
/// the attacker's cost (lamports).
fn push_second(s: &mut Sim, a: &Attacker, t: i64, target: u128, held: bool) -> i128 {
    s.at(t);
    if !held {
        s.crank();
    }
    let d = move_size(s, target);
    if d <= 0 {
        if held {
            s.crank();
        }
        return 0;
    }
    let d = u64::try_from(d).unwrap();
    let kp = a.kp();
    let ixs = [
        pump::cu_limit_ix(BUNDLE_CU),
        a.buy_ix(s, d),
        crank_price_ix(&s.pm),
        a.sell_ix(s, d),
    ];
    let l0 = a.lamports(s);
    ok(s.env.send(&ixs, &[&kp]));
    l0 - a.lamports(s)
}

/// `ema_atomic.attack(key)`'s start: the position at the cap, a crank every second for 60 s;
/// returns the true price.
fn fresh_position() -> (Sim, Pos, u128) {
    let mut s = Sim::d_market(SOLD_D50, lend_for_80(D50_Q));
    let p = s.open(D50_Q);
    for t in 1..=60 {
        s.step_to(t);
    }
    let p_true = s.spot();
    assert_eq!(p_true, 68_298_686_219_984);
    (s, p, p_true)
}

/// `ema_atomic.py`, model atomic, `D@50%` (ECON §12.4: "not liquidatable within 60 s at any push
/// level"): at every push level the attacker's `[pump, crank, dump]` lands every second next to
/// the honest keeper's crank; the bucket straddles the EMA, which stays at the true price, so
/// the EMA leg never holds — `would_liq` is false for 60 s, the keeper's view never
/// CR-liquidatable, and the attacker's own final bundle `[pump, liquidate, dump]` is refused
/// `NotLiquidatable` every second.
#[test]
fn atomic_pushes_never_liquidate_a_fresh_position() {
    let (mut s, p, p_true) = fresh_position();
    let a = Attacker::new(&mut s);
    let saved = s.env.svm.clone();
    let t0 = s.t();
    for mult in MULTS {
        s.env.svm = saved.clone();
        let target = (p_true as f64 * mult) as u128;
        let mut cost = 0;
        for n in 1..=60 {
            let t = t0 + n;
            cost += push_second(&mut s, &a, t, target, false);
            assert!(!would_liq(&s, &p), "mult {mult}: liquidatable at +{n} s");
            assert!(!s.status(&p).cr_liq());
            let d = u64::try_from(move_size(&s, target)).unwrap();
            let ixs = [
                pump::cu_limit_ix(BUNDLE_CU),
                a.buy_ix(&s, d),
                liquidate_ix(&s.pm, &a.key(), &p.key(), p.nonce),
                a.sell_ix(&s, d),
            ];
            assert_err(&s.env.send(&ixs, &[&a.kp()]), E::NotLiquidatable);
        }
        let m = market(&s.env, &s.pm);
        eprintln!(
            "atomic ×{mult}: 60 s cost {} SOL, P_ema/P_true {:.4}",
            sol4(cost),
            m.ema_price.get() as f64 / p_true as f64
        );
        assert!(
            m.ema_price.get() * 1_000 <= p_true * 1_001,
            "the EMA stays at the true price"
        );
    }
    assert_eq!(market(&s.env, &s.pm).open_positions, 1);
}

/// `ema_atomic.py`, model held, `D@50%` (ECON §12.4: `x3, 11 s | 9.439 | +0.655 | -8.785`; the sim
/// since AIR-22's lower-median growth: `+0.663 | -8.776`): with
/// every observation of each second pumped to 3× (a degraded-mode market: no honest observer),
/// the EMA leg holds after 11 s; the attacker then sandwiches the liquidation in the next
/// second with the script's optimal `dX`. The push costs 9.439 SOL of round-trip fees (the
/// sim's, to the lamport), the final bundle earns 0.663 SOL including the keeper share: net
/// −8.776 SOL. The liquidation's legs are the sim's (rent adjusted).
#[test]
fn held_pushes_liquidate_a_fresh_position_at_a_loss() {
    let (mut s, p, p_true) = fresh_position();
    let a = Attacker::new(&mut s);
    let target = (p_true as f64 * 3.0) as u128;
    let t0 = s.t();
    let mut cost = 0;
    let mut n = 0;
    loop {
        n += 1;
        assert!(n <= 60, "never liquidatable");
        let d = move_size(&s, target);
        cost += push_second(&mut s, &a, t0 + n, target, true);
        if n == 1 {
            assert_eq!(d, 290_158_241_252_428, "the sim's first held push size");
        }
        if would_liq(&s, &p) {
            break;
        }
    }
    assert_eq!(n, 11, "11 held seconds (ECON)");
    assert_eq!(
        cost, 9_439_317_239,
        "push cost: 11 round trips of 0.858 SOL (ECON 9.439)"
    );
    s.at(t0 + n + 1);
    let (meta, pnl) = sandwich(&mut s, &a, &p, 299_323_491_685_409, false);
    let ev: Liquidate = events::one(&meta);
    let d = D50_RMIN - ev.rent;
    assert_eq!(
        (
            ev.buyback_cost,
            ev.b_ref,
            ev.max_in,
            ev.penalty,
            ev.penalty_keeper
        ),
        (
            2_248_025_988,
            2_211_506_763,
            2_255_736_898,
            221_150_677,
            66_345_203
        )
    );
    assert_eq!(
        ev.borrower_payout,
        442_427_764 + d,
        "borrower (rent adjusted)"
    );
    assert_eq!(pnl, 663_281_138, "final sandwich + keeper (the sim +0.663)");
    assert_eq!(sol4(pnl - cost), "-8.7760", "attacker net (the sim -8.776)");
}

/// `ema_atomic_multi.fill(m)` on ECON's `D@50%` market, printed by the script: positions at
/// `min(max_open_q, the OI cap left)`, one a second from the sim's `t = 0`, until the next is
/// refused (`python3 tests/litesvm/sim/curve_s1_forcing_d50.py`, its first section).
const D50_FILL: [u64; 4] = [
    10_071_769_920_663,
    10_071_769_205_477,
    10_071_769_205_544,
    1_811_466_694_782,
];

/// An open of `q` by `b` tried on a clone of the VM (nothing is kept): `Ok` or the error code.
fn open_probe(s: &Sim, b: &Borrower, q: u64) -> Result<(), Option<u32>> {
    let ix = open_short_ix(
        &s.pm,
        &b.key(),
        b.next_nonce,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let tx = s.env.transaction(&with_cu_limit(&s.pm, ix), &[&b.kp]);
    s.env
        .svm
        .clone()
        .send_transaction(tx)
        .map(|_| ())
        .map_err(|f| match f.err {
            solana_transaction_error::TransactionError::InstructionError(
                _,
                solana_instruction::error::InstructionError::Custom(c),
            ) => Some(c),
            _ => None,
        })
}

/// The smallest `q` in `(lo, hi]` for which `refused(q)` is false, by bisection (`refused`
/// monotone: true below the answer).
fn first_not(mut lo: u64, mut hi: u64, refused: impl Fn(u64) -> bool) -> u64 {
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if refused(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

/// The OI-cap basket on `D@50%`: the sim's fill ([`D50_FILL`]) opened one a second from `t = 0`,
/// the fourth at the largest size the program's per-market cap still takes, then a crank every
/// second to `t` = 64 (`warm(m, t0, t0 + 60)`). Returns the market (`t = 0` at the first open),
/// the basket and the attack's start `t0` = 64.
///
/// The sim sizes each open on `curve_cap_reserves()` read **before** the open's own observation
/// (its EMA one roll behind); the program reads the state after its prologue's roll (M5-SEC-3).
/// The first three opens land as the sim sized them, but at `t` = 3 the program's OI cap is the
/// smaller (it refuses the sim's fourth open `CapMarketOi`): the fourth position is the largest
/// the program takes there, 1,552,563,673,080 tokens instead of the sim's 1,811,466,694,782 (the
/// minimum notional starts at 1,535,515,401,167), and a further open at `t` = 4 is refused
/// `CapMarketOi`.
fn oi_basket() -> (Sim, Vec<Pos>, i64) {
    let lend = lend_for_80(4 * D50_Q);
    let mut s = Sim::d_market(SOLD_D50, lend);
    let mut basket = Vec::new();
    let mut first = None;
    for (i, q) in D50_FILL.into_iter().enumerate() {
        if i == 3 {
            // The program's room under its OI cap: the largest q not refused `CapMarketOi`, and
            // the smallest the minimum notional takes.
            let probe = s.env.borrower();
            let code = |q| open_probe(&s, &probe, q).err().flatten();
            assert_eq!(
                code(q),
                Some(u32::from(E::CapMarketOi)),
                "the sim's fourth open"
            );
            let q_cap = first_not(0, q, |x| code(x) != Some(u32::from(E::CapMarketOi))) - 1;
            let q_min = first_not(0, q, |x| code(x) == Some(u32::from(E::BelowMinNotional)));
            eprintln!(
                "fourth open: the program takes at most {q_cap}, the minimum notional {q_min}"
            );
            if q_min > q_cap {
                break;
            }
            basket.push(s.open(q_cap));
            first.get_or_insert(s.t0);
            s.env.warp(1);
            continue;
        }
        basket.push(s.open(q));
        first.get_or_insert(s.t0);
        s.env.warp(1);
    }
    s.t0 = first.unwrap();
    s.at(4);
    eprintln!(
        "OI basket: {:?}, ΣQ {} ({:.3} % of vT)",
        basket.iter().map(|p| p.open.q).collect::<Vec<_>>(),
        basket.iter().map(|p| p.open.q).sum::<u64>(),
        basket.iter().map(|p| p.open.q).sum::<u64>() as f64 * 100.0
            / curve(&s.env, &s.pm).vt as f64
    );
    // A further open is refused: the per-market cap binds.
    let mut extra = s.env.borrower();
    let (res, _) = s.env.open(&s.pm, &mut extra, D50_Q / 2);
    assert_err(&res, E::CapMarketOi);
    for t in 5..=64 {
        s.step_to(t);
    }
    (s, basket, 64)
}

/// `ema_atomic_multi.py` (a), model atomic, `D@50%` (ECON §12.5: "no exit forced within 120 s at
/// any push level"): the market filled to its OI cap with positions at the per-position cap
/// ([`oi_basket`]), then at every push level the attacker's `[pump, crank, dump]` every second
/// next to the honest keeper's crank for 120 s: no position ever becomes CR-liquidatable (the
/// keeper's view of every basket position, every second), and the attacker's final bundle
/// `[pump, liquidate, dump]` is refused `NotLiquidatable` every second.
#[test]
fn atomic_pushes_force_no_exit_on_the_oi_basket() {
    let (mut s, basket, t0) = oi_basket();
    let n = basket.len();
    let p_true = s.spot();
    let a = Attacker::new(&mut s);
    let saved = s.env.svm.clone();
    for mult in MULTS {
        s.env.svm = saved.clone();
        let target = (p_true as f64 * mult) as u128;
        for k in 1..=120 {
            push_second(&mut s, &a, t0 + k, target, false);
            for p in &basket {
                assert!(!s.status(p).cr_liq(), "mult {mult}: forced at +{k} s");
            }
            let d = u64::try_from(move_size(&s, target)).unwrap();
            let mut ixs = vec![pump::cu_limit_ix(1_400_000), a.buy_ix(&s, d)];
            ixs.push(liquidate_ix(
                &s.pm,
                &a.key(),
                &basket[0].key(),
                basket[0].nonce,
            ));
            ixs.push(a.sell_ix(&s, d));
            assert_err(&s.env.send(&ixs, &[&a.kp()]), E::NotLiquidatable);
        }
    }
    assert_eq!(market(&s.env, &s.pm).open_positions as usize, n);
}

/// The position account of `p`.
fn pos_key(s: &Sim, p: &Pos) -> Pubkey {
    pda::position(&s.pm.market(), &p.key(), p.nonce).0
}

/// One attacker transaction `[cu, buy dX, sol_settle(each of alive), sell dX]` (the sims'
/// `exit_all` with `settle_only`): the metadata and the attacker's lamport change (trade and the
/// keeper rewards of what settled).
fn settle_bundle(
    s: &mut Sim,
    a: &Attacker,
    alive: &[&Pos],
    dx: u64,
) -> (TransactionMetadata, i128) {
    let mut ixs = vec![pump::cu_limit_ix(1_400_000)];
    if dx > 0 {
        ixs.push(a.buy_ix(s, dx));
    }
    for p in alive {
        ixs.push(sol_settle_ix(&s.env, &s.pm, &a.key(), &p.key(), p.nonce));
    }
    if dx > 0 {
        ixs.push(a.sell_ix(s, dx));
    }
    let l0 = a.lamports(s);
    let meta = ok(s.env.send(&ixs, &[&a.kp()]));
    (meta, a.lamports(s) - l0)
}

/// `ema_atomic_multi.py` (b), model atomic, `D@50%` (ECON §12.5: "no exit forced within 120 s at
/// any push level"): a lender owning the pool pushes the OI-cap basket every second for 120 s at
/// `cr_open × 1.1 / 1.3 / 1.6` (×4.4 / ×5.2 / ×6.4) next to the honest keeper's crank and calls
/// `sol_settle` on every basket position inside its own pumped transaction each second: the
/// honest observation keeps each bucket's low at the true price, so no settlement clock ever
/// arms and nothing settles.
#[test]
fn atomic_pushes_cannot_force_s1_on_the_oi_basket() {
    let (mut s, basket, t0) = oi_basket();
    let p_true = s.spot();
    let a = Attacker::new(&mut s);
    let saved = s.env.svm.clone();
    for mult in [4.4, 5.2, 6.4] {
        s.env.svm = saved.clone();
        let target = (p_true as f64 * mult) as u128;
        let mut cost = 0;
        for n in 1..=120 {
            cost += push_second(&mut s, &a, t0 + n, target, false);
            let dx = u64::try_from(move_size(&s, target).max(0)).unwrap();
            let alive: Vec<&Pos> = basket.iter().collect();
            let (meta, pnl) = settle_bundle(&mut s, &a, &alive, dx);
            cost -= pnl;
            assert!(
                events::of::<SolSettle>(&meta).is_empty()
                    && events::of::<SettleArmed>(&meta).is_empty(),
                "×{mult}, second {n}: nothing arms or settles"
            );
            for p in &basket {
                assert_eq!(s.position(p).s_armed, 0, "×{mult}, second {n}");
            }
        }
        eprintln!(
            "S1 forcing, atomic ×{mult}: 120 s cost the attacker {} SOL, nothing armed",
            sol4(cost)
        );
    }
    assert_eq!(market(&s.env, &s.pm).open_positions as usize, basket.len());
}

/// `ema_atomic_multi.py` (b), model held, `D@50%` (ECON §12.5 held row: `4 / 7.11 | x4.40 | 58 |
/// 72.31 | −51.12 | 0.02 | +0.38 | 0 / 1 | −123.04 | 0.40`): the attacker, a lender owning the
/// pool, holds the curve at the sim's best level (4.4× the true price) every second for 120 s
/// with no honest observation, and from the first second its own `[pump, sol_settle(every open
/// position), dump]` would arm a clock (tried on a clone, the sim's `would_s1`) lands that bundle
/// every second (`exit_all`). The clocks arm one by one and the positions settle **S1** at
/// `V − K_s` 40 EMA updates later; at every second the attacker's net — the S1 windfall over
/// `Q`'s true value plus the keeper rewards, minus the held pushes and the in-transaction round
/// trips — is negative, its best (the sim's optimal stop) far below zero. The basket differs from
/// the sim's in its fourth position ([`oi_basket`]), so the figures are the sim's row's shape,
/// not its lamports: the bundles start in the sim's second 18, the fourth position settles S1 in
/// its second 58 (the other three at 61), and the best net is −123.14 SOL at 58 s (sim −123.04).
/// The sim's row and its second-by-second trace:
/// `python3 tests/litesvm/sim/curve_s1_forcing_d50.py`.
#[test]
fn held_pushes_forcing_s1_on_the_oi_basket_lose() {
    let (mut s, basket, t0) = oi_basket();
    let p_true = s.spot();
    let a = Attacker::new(&mut s);
    let target = (p_true as f64 * 4.4) as u128;
    let (mut push, mut rt, mut keeper, mut windfall) = (0i128, 0i128, 0i128, 0i128);
    let mut alive: Vec<&Pos> = basket.iter().collect();
    let mut armed_at = vec![None; basket.len()];
    let mut settled = Vec::new();
    let mut started = None;
    let mut best: Option<(i128, i64)> = None;
    for n in 1..=120 {
        push += push_second(&mut s, &a, t0 + n, target, true);
        let dx = u64::try_from(move_size(&s, target).max(0)).unwrap();
        if started.is_none() {
            let saved = s.env.svm.clone();
            let (meta, _) = settle_bundle(&mut s, &a, &alive, dx);
            s.env.svm = saved;
            if events::of::<SettleArmed>(&meta).is_empty() {
                continue;
            }
            started = Some(n);
        }
        let (meta, dl) = settle_bundle(&mut s, &a, &alive, dx);
        for ev in events::of::<SettleArmed>(&meta) {
            let i = basket
                .iter()
                .position(|p| pos_key(&s, p) == ev.position)
                .unwrap();
            assert!(armed_at[i].is_none(), "armed once");
            armed_at[i] = Some(n);
        }
        let mut k_n = 0i128;
        for ev in events::of::<SolSettle>(&meta) {
            let i = basket
                .iter()
                .position(|p| pos_key(&s, p) == ev.position)
                .unwrap();
            let p = &basket[i];
            assert_eq!(ev.condition, 1, "S1");
            assert_eq!(ev.lender_amount + ev.k_s, p.open.v, "S1 takes V − K_s");
            k_n += i128::from(ev.k_s);
            windfall += i128::from(ev.lender_amount)
                - i128::try_from(u128::from(p.open.q) * p_true / W).unwrap();
            settled.push((n, i));
        }
        keeper += k_n;
        rt += dl - k_n;
        alive.retain(|p| {
            s.env
                .svm
                .get_account(&pos_key(&s, p))
                .is_some_and(|acc| acc.lamports > 0)
        });
        let net = rt + keeper + windfall - push;
        assert!(net < 0, "second {n}: S1 forcing does not pay ({net})");
        if !settled.is_empty() && best.is_none_or(|(b, _)| net > b) {
            best = Some((net, n));
        }
        if alive.is_empty() {
            break;
        }
    }
    let started = started.expect("the held pushes arm a clock");
    let (best_net, best_n) = best.expect("an S1 within 120 s");
    assert_eq!(started, 18, "the sim's first candidate second");
    assert_eq!(
        settled[0],
        (58, 3),
        "the fourth position settles first, in the sim's second"
    );
    eprintln!(
        "S1 forcing, held ×4.4 (sim: 4 positions, first candidate 18 s, 1 S1 at 58 s, net \
         −123.04 SOL): bundles from {started} s, armed at {armed_at:?}, S1 at {settled:?}; at the \
         end push {} SOL, round trips {} SOL, keeper {} SOL, windfall {} SOL; best net {} SOL \
         at {best_n} s",
        sol4(push),
        sol4(rt),
        sol4(keeper),
        sol4(windfall),
        sol4(best_net)
    );
    assert!(
        best_net < -50 * i128::from(SOL),
        "far below zero, as in the sim"
    );
}

// ---- §12.3: near-threshold positions (`ema_near.py`) ----------------------------------------

/// `ema_near.setup("D@50%", k)`: the position at the cap, a crank every second for 60 s, an honest
/// rise to `k ×` the spot (other users' real buy), then 300 s of cranks (the EMA caught up).
fn near_setup(k: f64) -> (Sim, Pos) {
    let mut s = Sim::d_market(SOLD_D50, lend_for_80(D50_Q));
    let p = s.open(D50_Q);
    for t in 1..=60 {
        s.step_to(t);
    }
    assert!(s.exo_move((s.spot() as f64 * k) as u128).is_none());
    for t in 61..=360 {
        s.step_to(t);
    }
    (s, p)
}

/// `CR_ema` (bps) on the newest ring entry with the sim's rent: `V − R_min` (the program's own
/// `V_eff` is larger by `ΔR`, module docs).
fn sim_cr_e(s: &Sim, p: &Pos) -> u64 {
    let st = s.status(p);
    risk::cr_bps(s.position(p).v - D50_RMIN, st.b_ema).unwrap()
}

/// `ema_near.py`'s honest rises on `D` markets (`k₀ × 1.4`) and the sim's `CR_ema` (bps) after
/// them (`st0["cr_e"]`; `L` = 17,500 at Tier D: 1.287, 1.194, 1.113, 1.077 × `L`;
/// `python3 tests/litesvm/sim/curve_ema_near_d50.py`).
const NEAR: [(f64, u64); 4] = [
    (1.30 * 1.4, 22_514),
    (1.40 * 1.4, 20_890),
    (1.50 * 1.4, 19_483),
    (1.55 * 1.4, 18_848),
];

/// `ema_near.py`, model atomic, `D@50%` (computed with the script's functions on `D@50%`; its
/// table prints `D@0%`): after an honest rise with the EMA caught up (`CR_ema` 1.08 … 1.29 ×
/// `L`, the sim's to the bps), 30 s of atomic `[pump, crank, dump]` at every push level of the
/// script (×1.02 … ×2) next to the honest keeper's crank never make the position liquidatable
/// (`would_liq` false every second — the sim: "not reached within 30 s" at every rise), and the
/// attacker's final bundle `[pump, liquidate, dump]` is refused `NotLiquidatable`.
#[test]
fn atomic_pushes_never_liquidate_a_near_threshold_position() {
    const PUSHES: [f64; 9] = [1.02, 1.05, 1.1, 1.15, 1.2, 1.3, 1.5, 1.75, 2.0];
    for (k, cr_e) in NEAR {
        let (mut s, p) = near_setup(k);
        let st = s.status(&p);
        assert!(!st.cr_liq(), "×{k:.2}: not liquidatable honestly");
        assert_eq!(sim_cr_e(&s, &p), cr_e, "×{k:.2}: the sim's CR_ema");
        let a = Attacker::new(&mut s);
        let p_true = s.spot();
        let t0 = s.t();
        let saved = s.env.svm.clone();
        for mult in PUSHES {
            s.env.svm = saved.clone();
            let target = (p_true as f64 * mult) as u128;
            for n in 1..=30 {
                push_second(&mut s, &a, t0 + n, target, false);
                assert!(!would_liq(&s, &p), "×{k:.2} push ×{mult}: +{n} s");
            }
            let d = u64::try_from(move_size(&s, target).max(1)).unwrap();
            let ixs = [
                pump::cu_limit_ix(BUNDLE_CU),
                a.buy_ix(&s, d),
                liquidate_ix(&s.pm, &a.key(), &p.key(), p.nonce),
                a.sell_ix(&s, d),
            ];
            assert_err(&s.env.send(&ixs, &[&a.kp()]), E::NotLiquidatable);
        }
        eprintln!(
            "ema_near ×{k:.2}: CR_ema/L {:.3}, no push liquidates within 30 s",
            cr_e as f64 / 17_500.0
        );
    }
}

/// `ema_near.py`, model held, `D@50%`, the closest rise (×2.17, `CR_ema` 1.077 × `L`) at the
/// script's best push (×1.15; the sim: `x1.15, 7 s | 0.875 | +0.182 | −0.693 | 0.901`; before AIR-22's
/// lower-median growth `+0.180 | −0.694 | 0.904`): seven
/// held seconds (every observation of each second pumped) make the position liquidatable, the
/// attacker sandwiches the liquidation in the next second with the script's optimal `dX`. The
/// pushes cost 0.875 SOL of round-trip fees, the final bundle earns 0.182 SOL including the
/// keeper share: net −0.693 SOL — and −1.09 … −2.46 SOL at the other rises (sim). The
/// liquidation's legs are the sim's (rent adjusted); the spend stays within the fixed δ. The
/// figures: `python3 tests/litesvm/sim/curve_ema_near_d50.py`.
#[test]
fn held_pushes_near_the_threshold_do_not_pay() {
    let (k, cr_e) = NEAR[3];
    let (mut s, p) = near_setup(k);
    assert_eq!(sim_cr_e(&s, &p), cr_e);
    let a = Attacker::new(&mut s);
    let p_true = s.spot();
    let target = (p_true as f64 * 1.15) as u128;
    let t0 = s.t();
    let mut cost = 0;
    let mut n = 0;
    loop {
        n += 1;
        assert!(n <= 30, "never liquidatable");
        let d = move_size(&s, target);
        if n == 1 {
            assert_eq!(d, 31_455_549_918_141, "the sim's first held push size");
        }
        cost += push_second(&mut s, &a, t0 + n, target, true);
        if would_liq(&s, &p) {
            break;
        }
    }
    assert_eq!(n, 7, "7 held seconds (sim)");
    assert_eq!(
        cost, 874_895_819,
        "push cost: 7 round trips of 0.125 SOL (sim 0.875)"
    );
    s.at(t0 + n + 1);
    let (meta, pnl) = sandwich(&mut s, &a, &p, 37_516_328_236_034, false);
    let ev: Liquidate = events::one(&meta);
    let d = D50_RMIN - ev.rent;
    assert_eq!(
        (
            ev.buyback_cost,
            ev.b_ref,
            ev.max_in,
            ev.penalty,
            ev.penalty_keeper + ev.keeper_topup
        ),
        (
            1_830_622_026,
            1_795_886_107,
            1_831_803_829,
            179_588_611,
            53_876_583
        ),
        "B, B̂_ref, max_in, penalty, keeper: the sim's"
    );
    assert_eq!(
        ev.borrower_payout,
        901_393_792 + d,
        "borrower (rent adjusted)"
    );
    assert!(u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * 10_200);
    assert_eq!(pnl, 181_503_377, "final sandwich + keeper (sim +0.182)");
    assert_eq!(sol4(pnl - cost), "-0.6934", "attacker net (sim −0.693)");
}

// ---- §12.6: the held model around a liquidation ---------------------------------------------

/// `sandwich.attack(m, pos, t, ("liquidation",), "held")`, `D@50%` (ECON §12.6 / `sandwich.py`'s
/// held liquidation row: best plan "none", `+0.064`): before its sandwich the attacker-keeper
/// holds the curve at `EMA × (1 + x)` for `h` whole seconds (every observation sees it) to raise
/// the reference the fixed bound is computed from. Two of the script's plans replayed to the
/// lamport — `h` = 1 s at +2.5 % (its best held plan) and 3 s at +30 % (its most aggressive):
/// each lifts `B̂_ref` and with it the penalty (the lenders' and protocol's legs **grow**: the
/// reference is senior to the attacker), the spend stays within `⌊B̂_ref·(1 + δ)⌋`, and the
/// attacker's net (sandwich and keeper share minus the held pushes) stays below the plain
/// atomic sandwich's `+0.0642` SOL — the held model does not pay on the curve. Every plan's
/// figures: `python3 tests/litesvm/sim/curve_held_sandwich_d50.py`.
#[test]
fn held_pushes_before_a_liquidation_sandwich_do_not_pay() {
    struct Plan {
        h: i64,
        x_bps: u128,
        pre: i128,
        dx: u64,
        pnl: i128,
        liq: SimLiq,
    }
    let plans = [
        Plan {
            h: 1,
            x_bps: 250,
            pre: 16_908_239,
            dx: 5_368_257_768_696,
            pnl: 69_213_955,
            liq: SimLiq {
                b: 1_719_549_478,
                pen: 168_654_582,
                keeper: 50_596_374,
                borrower: 1_023_400_369,
                b_ref: 1_686_545_815,
                max_in: 1_720_276_731,
            },
        },
        Plan {
            h: 3,
            x_bps: 3_000,
            pre: 790_464_615,
            dx: 74_260_899_133_630,
            pnl: 440_635_196,
            liq: SimLiq {
                b: 2_423_653_463,
                pen: 238_352_653,
                keeper: 71_505_795,
                borrower: 249_598_313,
                b_ref: 2_383_526_521,
                max_in: 2_431_197_051,
            },
        },
    ];
    /// The plain atomic sandwich's P&L at the same second (`sandwich_liquidation_d50`).
    const ATOMIC_PNL: i128 = 64_184_611;
    let (mut s, p, t) = setup_liq();
    let saved = s.env.svm.clone();
    let h: Liquidate = events::one(&honest(&mut s, &p, false));
    for plan in &plans {
        s.env.svm = saved.clone();
        let a = Attacker::new(&mut s);
        let mut pre = 0;
        let mut tt = t;
        for _ in 0..plan.h {
            tt += 1;
            let target = market(&s.env, &s.pm).ema_price.get() * (10_000 + plan.x_bps) / 10_000;
            pre += push_second(&mut s, &a, tt, target, true);
        }
        assert_eq!(
            pre, plan.pre,
            "{} s at +{} bps: the sim's held cost",
            plan.h, plan.x_bps
        );
        s.at(tt + 1);
        let (meta, pnl) = sandwich(&mut s, &a, &p, plan.dx, false);
        let ev: Liquidate = events::one(&meta);
        assert_liq(&ev, &plan.liq, D50_V);
        assert_eq!(pnl, plan.pnl, "the sim's sandwich + keeper");
        assert!(
            u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * 10_200
                && ev.buyback_cost <= ev.max_in,
            "I11: the spend within the fixed δ over the (lifted) reference"
        );
        assert!(
            ev.penalty_lender >= h.penalty_lender && ev.penalty_protocol >= h.penalty_protocol,
            "the lenders' and protocol's legs are at least the honest ones"
        );
        let net = pnl - pre;
        eprintln!(
            "held {} s at +{:.1} %: push {} SOL, sandwich {} SOL, net {} SOL (atomic {}); B̂_ref \
             {:.4} → {:.4} SOL, penalty to lenders + protocol {} → {} SOL",
            plan.h,
            plan.x_bps as f64 / 100.0,
            sol4(pre),
            sol4(pnl),
            sol4(net),
            sol4(ATOMIC_PNL),
            h.b_ref as f64 / 1e9,
            ev.b_ref as f64 / 1e9,
            sol4(i128::from(h.penalty_lender + h.penalty_protocol)),
            sol4(i128::from(ev.penalty_lender + ev.penalty_protocol)),
        );
        assert!(
            net < ATOMIC_PNL,
            "the held plan does not beat the atomic sandwich"
        );
    }
}
