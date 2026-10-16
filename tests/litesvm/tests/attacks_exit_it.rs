//! ECON §12's exit attack scripts reproduced against the program on the mock venue (ARCH §8.2
//! "adversarial suite", §9 M2 exit criteria): `sandwich.py` (the exit sandwich, atomic and held
//! models, liquidation and expiry, fresh and aged positions), `fixed_delta.py` (the fixed bound:
//! extraction does not grow with waiting; liveness on genuine paths), `ema_atomic.py` /
//! `ema_atomic_multi.py` (atomic `[pump, crank, dump]` bundles every second against a fresh
//! position and the OI-cap basket; the basket attack's held model and its S1-forcing variant),
//! `ptf_sandwich.py` (push-then-freeze cannot lift the liquidation reference, R4B-2),
//! `fvcap.py` (the fair-value cap binds in S2, never in S1; drained-pool model cases),
//! `ema_near.py` (near-threshold positions), `cascade.py` (an honest cascade on a market
//! filled to the OI cap) and `block_spot.py` (b), the borrower's paid veto of its own liquidation
//! (V1-2, M10 G-E2: `a_borrower_veto_defers_its_liquidation_at_the_sims_cost_ra`).
//!
//! **ECON's `RA` market, exactly.** The mock prices like `shortwin_sim.RaydiumPool` with
//! `fee_ppm = 2,500` and `out_share_ppm = 120,000` (the mock's fixed protocol share), so
//! `RA = RaydiumPool(1.5·10¹⁴, 2,500 SOL)` at Tier A (fresh oracle, age ≥ 7 days) is the sim's
//! market to the lamport: the sim's own steps (`step_to` = one crank, `exo_move` = the same
//! exact-out buy or exact-in sale of `move_size` tokens, the same float price paths) replayed
//! here give the sim's `V`, `M`, `S`, every buyback `B`, `B̂_ref`, `max_in`, penalty and
//! attacker P&L exactly. The constants below were printed by the scripts' own functions
//! (`sandwich.setup_liq("RA")`, `setup_expiry`, `attack`, `sandwich_exit`, `push_second`,
//! `shortwin_sim.run(new_market("RA"), 1.0, Scenario(..), keeper_latency=1)`,
//! `cascade.run_basket` / `run_single`) and match `docs/design/sim/out/attacks/*.out` — except
//! where AIR-22 (the sustained growth's lower median) moved them, which the `.out` files and
//! ECON §12 follow when the docs are regenerated.
//!
//! **One modelled difference: `R_min`.** The sim prices every position's minimum rent at its
//! fixed 80 % utilisation; the program prices `r_min` at the pre-open utilisation (ARCH §4.5
//! "pre-instruction counters"), 0 % for a market's first position. Where the sim's rent due is
//! `R_min` (1,069,992 lamports on RA) the program's is the accrued rent (a few 10⁵ lamports), and
//! the waterfall legs below the rent move by exactly that difference: [`assert_sim_liquidation`]
//! compares `B`, `B̂_ref` and `max_in` exactly and the rest after that adjustment.

#![allow(clippy::result_large_err, clippy::too_many_arguments)]

#[path = "liquidate_support.rs"]
mod support;

use anchor_lang::solana_program::instruction::Instruction;
use litesvm::types::TransactionMetadata;
use shortwin::events::{Liquidate, SettleArmed, SolSettle};
use shortwin::state::ShortPosition;
use shortwin_litesvm_tests::drive::swap_in_ix;
use shortwin_litesvm_tests::engine::sol_settle_ix;
use shortwin_litesvm_tests::mock::Leg;
use shortwin_math::num::{Rounding, mul_div, sqrt_u128};
use shortwin_math::rent;
use solana_signer::Signer;
use support::*;

/// ECON's RA position: `max_open_q()` of the RA market (the fee-scaled per-position cap).
const RA_Q: u64 = 1_124_774_998_898;
/// The sim's RA `R_min` (1 h at 80 % utilisation).
const SIM_RMIN: u64 = 1_069_992;

/// The RA environment: the wallet cap and the beta notional cap lifted (the sim has neither;
/// both are within their compiled bounds).
fn ra_env() -> Env {
    env_with(|a| {
        a.params.wallet_cap_bps = 10_000;
        a.params.market_max_notional = 100_000_000_000_000;
    })
}

/// A sim market on the chain: `t0` is the chain time of the sim's `t = 0` (the open).
struct Sim {
    env: Env,
    mm: MockMarket,
    t0: i64,
    actor: Actor,
}

impl Sim {
    /// ECON's RA market lending `ΣQ + ⌈ΣQ/4⌉`: exactly 80 % utilisation (`⌈ΣB_Q·10⁴/(T +
    /// ΣB_Q)⌉ = 8,000`, the sim's `util_bps`) once `ΣQ` is borrowed.
    fn ra(sum_q: u64) -> Self {
        Self::ra_lending(sum_q + sum_q.div_ceil(4))
    }

    fn ra_lending(lend: u64) -> Self {
        let mut env = ra_env();
        let mm = ra_market(&mut env, lend);
        let actor = env.actor(&mm, 1_000_000_000_000_000, 40_000_000_000_000);
        let t0 = env.now();
        Self { env, mm, t0, actor }
    }

    /// The sim's clock at `t` (never backwards).
    fn at(&mut self, t: i64) {
        let d = self.t0 + t - self.env.now();
        assert!(d >= 0, "time runs forward");
        if d > 0 {
            self.env.warp(d);
        }
    }

    /// `Market.step_to(t)`: the honest keeper's crank at `t`.
    fn step_to(&mut self, t: i64) {
        self.at(t);
        self.env.crank(&self.mm);
    }

    fn reserves(&self) -> (u64, u64) {
        let r = mock::reserves(&self.env.svm, self.mm.mock());
        (r.base, r.quote)
    }

    /// `move_size(v, target)`: tokens to buy (> 0) or sell (< 0) to bring the pool to `target`.
    fn move_size(&self, target: u128) -> i128 {
        let (x, y) = self.reserves();
        let k = u128::from(x) * u128::from(y);
        let tx = sqrt_u128(
            mul_div(k, W, target.max(1), Rounding::Floor).unwrap(),
            Rounding::Floor,
        );
        i128::from(x) - i128::try_from(tx).unwrap()
    }

    fn buy_ix(&self, who: &Actor, tokens: u64) -> Instruction {
        let leg = Leg {
            dir: Dir::QuoteIn,
            authority: who.key(),
            source: who.wsol,
            destination: who.base,
        };
        mock::swap_exact_out_ix(self.mm.mock(), &leg, u64::MAX, tokens, &[])
    }

    fn sell_ix(&self, who: &Actor, tokens: u64) -> Instruction {
        swap_in_ix(&self.mm, who, Dir::BaseIn, tokens, 0)
    }

    /// `Venue.move_to_price(p)` by other traders (the sim's `exo_move`). A sale whose output
    /// rounds to 0 lamports (a token or two) is skipped: the mock refuses it (`ZeroOutput`)
    /// where the sim would add the dust to `X`.
    fn exo_move(&mut self, p: u128) {
        let d = self.move_size(p);
        let ix = match d {
            0 => return,
            d if d > 0 => self.buy_ix(&self.actor, u64::try_from(d).unwrap()),
            d => {
                let tokens = u64::try_from(-d).unwrap();
                let out = mock::quote_exact_in(&self.env.svm, self.mm.mock(), Dir::BaseIn, tokens);
                if out.is_none_or(|o| o.proceeds == 0) {
                    return;
                }
                self.sell_ix(&self.actor, tokens)
            }
        };
        let kp = self.actor.kp.insecure_clone();
        ok(self.env.send(&[ix], &[&kp]));
    }

    fn spot(&self) -> u128 {
        spot(&self.env, &self.mm)
    }

    fn open(&mut self, q: u64) -> (Borrower, u64) {
        let (b, nonce, pos) = open(&mut self.env, &self.mm, q);
        assert_eq!(pos.tier, 0, "Tier A");
        (b, nonce)
    }

    /// The position's legs now (right after this second's crank: the post-roll state).
    fn status(&mut self, b: &Borrower, nonce: u64) -> Predicted {
        predict(&view(&mut self.env, &self.mm, &b.key(), nonce))
    }
}

/// `sandwich.setup_liq("RA")` (and `block_spot.setup_liq_S`): open at the cap, 120 s of honest
/// cranks, then an honest +0.1 %/s rise until the first second the position is liquidatable on
/// both legs. Returns the sim, the borrower, the nonce and `t`.
fn setup_liq_ra(age: i64) -> (Sim, Borrower, u64, i64) {
    let mut s = Sim::ra(RA_Q);
    let (b, nonce) = s.open(RA_Q);
    let spot_post = s.spot();
    for t in 1..=120 {
        s.step_to(t);
    }
    let mut t = 120;
    if age > 0 {
        s.step_to(age);
        t = age;
        for tt in age + 1..=age + 30 {
            s.step_to(tt);
        }
        t += 30;
    }
    let mut k = 1.0f64;
    while k < 5.0 {
        k *= 1.001;
        t += 1;
        s.at(t);
        s.exo_move((spot_post as f64 * k) as u128);
        s.step_to(t);
        if s.status(&b, nonce).cr_liquidatable() {
            return (s, b, nonce, t);
        }
    }
    panic!("never liquidatable");
}

/// The sim's result of a keeper exit (lamports).
#[derive(Clone, Copy, Debug)]
struct SimExit {
    b: u64,
    rent: u64,
    pen: u64,
    borrower: u64,
    b_ref: u64,
    max_in: u64,
}

/// Compares a liquidation with the sim's: `B`, `B̂_ref`, `max_in` exactly; the penalty and the
/// borrower's remainder after the rent difference (module docs); the waterfall conserves `V`.
fn assert_sim_liquidation(ev: &Liquidate, sim: &SimExit, v: u64) {
    assert_eq!(ev.buyback_cost, sim.b, "B");
    assert_eq!(ev.b_ref, sim.b_ref, "B̂_ref");
    assert_eq!(ev.max_in, sim.max_in, "max_in");
    assert!(ev.rent <= sim.rent);
    let d = sim.rent - ev.rent;
    if sim.borrower > 0 {
        assert_eq!(ev.penalty, sim.pen, "penalty");
        assert_eq!(
            ev.borrower_payout,
            sim.borrower + d,
            "borrower (rent adjusted)"
        );
    } else {
        assert_eq!(ev.borrower_payout, 0);
        assert_eq!(ev.penalty, sim.pen + d, "truncated penalty (rent adjusted)");
    }
    assert_eq!(
        ev.buyback_cost + ev.rent + ev.penalty + ev.keeper_topup + ev.borrower_payout,
        v
    );
}

/// A keeper's liquidation sandwich `[buy dX exact-out, liquidate, sell dX exact-in]` by
/// `attacker` (also the keeper): the event and the attacker's P&L (WSOL delta + keeper legs).
fn sandwich_exit(s: &mut Sim, attacker: &Actor, b: &Borrower, nonce: u64, dx: u64) -> (Exit, i128) {
    let wsol0 = i128::from(read::token_amount(&s.env.svm, &attacker.wsol).unwrap());
    let lam0 = i128::from(read::lamports(&s.env.svm, &attacker.key()));
    let front = s.buy_ix(attacker, dx);
    let back = s.sell_ix(attacker, dx);
    let kp = attacker.kp.insecure_clone();
    let ex = exit_checked(
        &mut s.env,
        &s.mm,
        Which::Liquidate,
        &kp,
        &b.key(),
        nonce,
        &[front],
        &[back],
        &[],
    );
    let wsol1 = i128::from(read::token_amount(&s.env.svm, &attacker.wsol).unwrap());
    let lam1 = i128::from(read::lamports(&s.env.svm, &attacker.key()));
    (ex, wsol1 - wsol0 + lam1 - lam0)
}

fn sandwich_expiry(
    s: &mut Sim,
    attacker: &Actor,
    b: &Borrower,
    nonce: u64,
    dx: u64,
) -> (Exit, i128) {
    let wsol0 = i128::from(read::token_amount(&s.env.svm, &attacker.wsol).unwrap());
    let lam0 = i128::from(read::lamports(&s.env.svm, &attacker.key()));
    let front = s.buy_ix(attacker, dx);
    let back = s.sell_ix(attacker, dx);
    let kp = attacker.kp.insecure_clone();
    let ex = exit_checked(
        &mut s.env,
        &s.mm,
        Which::Expire,
        &kp,
        &b.key(),
        nonce,
        &[front],
        &[back],
        &[],
    );
    let wsol1 = i128::from(read::token_amount(&s.env.svm, &attacker.wsol).unwrap());
    let lam1 = i128::from(read::lamports(&s.env.svm, &attacker.key()));
    (ex, wsol1 - wsol0 + lam1 - lam0)
}

fn attacker(s: &mut Sim) -> Actor {
    s.env.actor(&s.mm, 100_000_000_000_000, 5_000_000_000_000)
}

// ---- sandwich.py / fixed_delta.py (d): the atomic exit sandwich ------------------------------

/// `sandwich.py`, liquidation, model atomic, RA, keeper waits 0 s and 4 s after arming
/// (`fixed_delta.py` (d), v0.4.1 row): the honest keeper's exit and the attacker-keeper's best
/// sandwich, to the lamport. The sim: honest `30.566 / 0.0011 / 1.5283 / 5.775`, attacked
/// `31.159 / … / 5.182`, extra over an honest keeper `+0.439`, borrower loss `0.593`, lenders
/// `+0.0000`; after 4 s `+0.463 / 0.625` — the fixed δ does not widen with waiting (v0.3's
/// Dutch clock: `+3.855` for Tier A after 4 s). AIR-22 (the sustained growth's lower median)
/// lifted `P_ref` a little on this rise; ECON's table has the minimum's `1.5274 / 5.776`,
/// `31.127 / … / 5.215`, `+0.415 / 0.561`, `+0.439 / 0.594`.
#[test]
fn sandwich_liquidation_atomic_ra() {
    let (mut s, b, nonce, t) = setup_liq_ra(0);
    assert_eq!(t, 612, "the sim's first liquidatable second");
    let v = s.env.position(&s.mm, &b.key(), nonce).v;
    assert_eq!(v, 37_870_437_682, "the sim's V");
    let saved = s.env.svm.clone();

    // The honest keeper at t.
    let k = keeper(&mut s.env);
    let honest = liquidate_checked(&mut s.env, &s.mm, &k, &b.key(), nonce);
    let h = honest.liquidated().clone_event();
    assert_sim_liquidation(
        &h,
        &SimExit {
            b: 30_566_119_890,
            rent: SIM_RMIN,
            pen: 1_528_305_995,
            borrower: 5_774_941_805,
            b_ref: 30_566_120_174,
            max_in: 31_177_442_577,
        },
        v,
    );
    assert_eq!(h.penalty_keeper, 458_491_798);

    // The attacker-keeper at t: the sim's grid optimum dX = X·19/2000.
    s.env.svm = saved.clone();
    let a = attacker(&mut s);
    let (ex, pnl) = sandwich_exit(&mut s, &a, &b, nonce, 1_123_033_214_218);
    let ev = ex.liquidated().clone_event();
    assert_sim_liquidation(
        &ev,
        &SimExit {
            b: 31_158_781_576,
            rent: SIM_RMIN,
            pen: 1_528_305_855,
            borrower: 5_182_280_259,
            b_ref: 30_566_117_098,
            max_in: 31_177_439_439,
        },
        v,
    );
    // The sim's attacker net 897,190,041 includes a keeper share of 458,491,756.
    assert_eq!(
        pnl - i128::from(ev.penalty_keeper),
        897_190_041 - 458_491_756
    );
    let extra = pnl - i128::from(h.penalty_keeper);
    assert_eq!(extra, 438_698_243, "extra over an honest keeper (+0.439)");
    assert_eq!(
        h.borrower_payout - ev.borrower_payout,
        592_661_546,
        "borrower loss (0.593)"
    );
    let lenders = |e: &Liquidate| e.penalty_lender + e.penalty_protocol + e.rent;
    assert!(
        lenders(&h) - lenders(&ev) < 1_000,
        "lenders + protocol: +0.0000"
    );
    // The largest front-run is bounded by the fixed 2 % over the reference.
    assert!(u128::from(ev.buyback_cost) * 10_000 <= u128::from(ev.b_ref) * 10_200);

    // After 4 s of waiting (nobody exits): the same fixed δ.
    s.env.svm = saved;
    for tt in t + 1..=t + 4 {
        s.step_to(tt);
    }
    let a = attacker(&mut s);
    let (ex, pnl) = sandwich_exit(&mut s, &a, &b, nonce, 1_182_140_225_492);
    let ev = ex.liquidated().clone_event();
    assert_sim_liquidation(
        &ev,
        &SimExit {
            b: 31_190_450_224,
            rent: SIM_RMIN,
            pen: 1_529_227_236,
            borrower: 5_149_690_230,
            b_ref: 30_584_544_715,
            max_in: 31_196_235_609,
        },
        v,
    );
    assert_eq!(
        pnl - i128::from(ev.penalty_keeper),
        921_026_219 - 458_768_170
    );
    assert_eq!(ev.delta_bps, 200);
    let extra4 = pnl - i128::from(h.penalty_keeper);
    // The sim: +0.463 after 4 s against +0.439 at once.
    assert!(extra4 * 100 <= extra * 110, "{extra4} vs {extra}");
}

/// `sandwich.py`, liquidation, model held, RA, keeper waits 0 s: the attacker holds the pool at
/// `P_ema·1.15` for 2 whole seconds (every observation sees it, no arbitrage), then sandwiches
/// the liquidation the next second. The sim: attacked `36.412 / 0.0011 / 1.4577 / 0.000`,
/// attacker net `+2.839`, extra `+2.381`, borrower loss `5.775` (everything), lenders + protocol
/// lose `+0.0495` — v0.4.1's no-reserve bound lets the held attacker buy the vault down to
/// `V − K_s − ert` (ECON §12.6; ARCH §8.5 residual risk, O19). AIR-22: the sustained growth is
/// the lower median of the newest 3 steps, so two held steps open `κ_eff` — the best plan was
/// 3 s at +15 % under the minimum (ECON: `37.156 / 0.0011 / 0.7135 / 0.000`, net `+1.981`, extra
/// `+1.523`, lenders `+0.5697`), a push 1.22 SOL dearer.
#[test]
fn sandwich_liquidation_held_ra() {
    let (mut s, b, nonce, t) = setup_liq_ra(0);
    let v = s.env.position(&s.mm, &b.key(), nonce).v;
    let a = attacker(&mut s);
    let kp = a.kp.insecure_clone();
    let mut pre: i128 = 0;
    let mut tt = t;
    for _ in 0..2 {
        tt += 1;
        let target = s.env.market(&s.mm.market()).ema_price.get() * 11_500 / 10_000;
        let d = s.move_size(target);
        assert!(d > 0);
        let d = u64::try_from(d).unwrap();
        s.at(tt);
        let w0 = i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
        let ixs = [s.buy_ix(&a, d), crank_price_ix(&s.mm), s.sell_ix(&a, d)];
        ok(s.env.send(&ixs, &[&kp]));
        pre += w0 - i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
    }
    assert_eq!(pre, 2_070_437_831, "the sim's held-pump cost");
    s.at(tt + 1);
    let (ex, pnl) = sandwich_exit(&mut s, &a, &b, nonce, 9_811_382_041_799);
    let ev = ex.liquidated().clone_event();
    assert_sim_liquidation(
        &ev,
        &SimExit {
            b: 36_411_710_437,
            rent: SIM_RMIN,
            pen: 1_457_657_253,
            borrower: 0,
            b_ref: 35_706_129_862,
            max_in: 36_420_252_459,
        },
        v,
    );
    // The sandwich's trade P&L equals the sim's (its keeper share moves with the rent).
    let trade = pnl - i128::from(ev.penalty_keeper + ev.keeper_topup);
    assert_eq!(trade, 4_909_750_103 - 437_297_175);
    assert_eq!(ev.borrower_payout, 0, "the borrower loses everything");
    assert!(
        ev.penalty < 1_528_305_995,
        "lenders and protocol lose part of the penalty"
    );
    assert_eq!(ev.max_in, 36_420_252_459);
}

/// `sandwich.py`'s last table, RA aged 25 days at 90 % utilisation, rule "none" (v0.4.1): the
/// rent due (1.898 SOL) exceeds `K_s`, and the held attacker's buyback leaves only part of it —
/// the sim: lenders + protocol `+1.257` SOL, borrower loss `5.512` (the held-model disclosure on
/// aged positions, ECON §12.6, O19). AIR-22: the best plan holds 2 s at +20 % (attacker net
/// `+2.892`); under the minimum it held 3 s (ECON: net `+2.150`, lenders + protocol `+2.272`).
#[test]
fn sandwich_liquidation_held_aged_ra() {
    // Exactly 90 %: T = ⌈Q/9⌉.
    let mut s = Sim::ra_lending(RA_Q + RA_Q.div_ceil(9));
    let (b, nonce) = s.open(RA_Q);
    let spot_post = s.spot();
    for t in 1..=120 {
        s.step_to(t);
    }
    let age = 25 * 86_400;
    s.step_to(age);
    for tt in age + 1..=age + 30 {
        s.step_to(tt);
    }
    let mut t = age + 30;
    let mut k = 1.0f64;
    loop {
        k *= 1.001;
        t += 1;
        s.at(t);
        s.exo_move((spot_post as f64 * k) as u128);
        s.step_to(t);
        if s.status(&b, nonce).cr_liquidatable() {
            break;
        }
    }
    assert_eq!(t, 2_160_470, "the sim's first liquidatable second");
    let v = s.env.position(&s.mm, &b.key(), nonce).v;
    let p = s.status(&b, nonce);
    assert_eq!(
        p.r_due, 1_897_970_613,
        "the sim's rent due (accrued, above R_min)"
    );
    let saved = s.env.svm.clone();
    let kk = keeper(&mut s.env);
    let honest = liquidate_checked(&mut s.env, &s.mm, &kk, &b.key(), nonce);
    let h = honest.liquidated().clone_event();
    assert_sim_liquidation(
        &h,
        &SimExit {
            b: 29_010_912_848,
            rent: 1_897_970_613,
            pen: 1_449_676_474,
            borrower: 5_511_877_747,
            b_ref: 28_993_529_470,
            max_in: 29_573_400_059,
        },
        v,
    );
    s.env.svm = saved;
    // The sim's best plan: 2 s held at +20 %, then the sandwich.
    let a = attacker(&mut s);
    let kp = a.kp.insecure_clone();
    let mut tt = t;
    let mut pre: i128 = 0;
    for _ in 0..2 {
        tt += 1;
        let target = s.env.market(&s.mm.market()).ema_price.get() * 12_000 / 10_000;
        let d = u64::try_from(s.move_size(target)).unwrap();
        s.at(tt);
        let w0 = i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
        let ixs = [s.buy_ix(&a, d), crank_price_ix(&s.mm), s.sell_ix(&a, d)];
        ok(s.env.send(&ixs, &[&kp]));
        pre += w0 - i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
    }
    assert_eq!(pre, 2_632_800_861, "the sim's held-pump cost");
    s.at(tt + 1);
    // The sim's grid optimum is the largest front-run that still executes.
    let dx = best_dx(&mut s, &b, nonce);
    assert_eq!(dx, 12_616_935_838_115, "the sim's dX");
    let (ex, pnl) = sandwich_exit(&mut s, &a, &b, nonce, dx);
    let ev = ex.liquidated().clone_event();
    assert_eq!(
        pnl, 5_524_795_782,
        "the sim's sandwich P&L (keeper share 0)"
    );
    assert_eq!(ev.buyback_cost, 36_214_682_559, "B");
    assert_eq!(ev.rent, 1_655_755_123, "rent truncated to V − B");
    assert_eq!((ev.penalty, ev.keeper_topup, ev.borrower_payout), (0, 0, 0));
    let lenders = |e: &Liquidate| e.penalty_lender + e.penalty_protocol + e.rent;
    assert_eq!(
        lenders(&h) - lenders(&ev),
        (2_338_113_260 + 574_630_885) - (1_407_391_855 + 248_363_268),
        "lenders + protocol lose 1.257 SOL"
    );
    assert_eq!(h.borrower_payout - ev.borrower_payout, 5_511_877_747);
}

/// The sim's grid (`sandwich_exit`: `X·i/2000`, `i < 200`, then `i` by 4 to 900) searched for
/// the largest front-run that still executes — the attacker's optimum (its P&L grows with
/// `dX` while the exit executes).
fn best_dx(s: &mut Sim, b: &Borrower, nonce: u64) -> u64 {
    let (x, _) = s.reserves();
    let grid: Vec<u64> = (1..200)
        .chain((200..900).step_by(4))
        .map(|i| u64::try_from(u128::from(x) * i / 2000).unwrap())
        .collect();
    let a = attacker(s);
    let ok_at = |s: &mut Sim, dx: u64| {
        let front = s.buy_ix(&a, dx);
        let v = view_after(&mut s.env, &s.mm, &b.key(), nonce, &[front], &[&a.kp]);
        predict(&v).liquidation().is_ok()
    };
    let (mut lo, mut hi) = (0usize, grid.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if ok_at(s, grid[mid]) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    assert!(lo > 0);
    grid[lo - 1]
}

/// `sandwich.py`, expiry, model atomic, RA (position at the cap, 80 % utilisation, 30 days):
/// ECON: honest `18.654 / 0.7590 / 0.3731 / 18.065`, attacked `18.824 / 0.7590 / 0.3731 /
/// 17.895`, extra `+0.113`, borrower loss `0.170`, lenders `+0.0000` — rent and the fee are
/// senior; the sandwich takes at most δ_exp = 1 %.
#[test]
fn sandwich_expiry_atomic_ra() {
    let mut s = Sim::ra(RA_Q);
    let (b, nonce) = s.open(RA_Q);
    let pos = s.env.position(&s.mm, &b.key(), nonce);
    let deadline = pos.deadline - s.t0;
    assert_eq!(deadline, 2_592_000);
    // The sim's calm steps (30 s) until the EMA sits at its fixed point, then daily cranks
    // (a flat EMA accrues the same rent whatever the step), then 1 s steps for the last 120 s.
    let mut t = 0;
    for _ in 0..400 {
        t += 30;
        s.step_to(t);
    }
    let ema = s.env.market(&s.mm.market()).ema_price.get();
    while t + 86_400 < deadline - 120 {
        t += 86_400;
        s.step_to(t);
    }
    assert_eq!(
        s.env.market(&s.mm.market()).ema_price.get(),
        ema,
        "EMA at its fixed point"
    );
    t = deadline - 120;
    s.step_to(t);
    while t < deadline {
        t += 1;
        s.step_to(t);
    }
    let saved = s.env.svm.clone();
    let k = keeper(&mut s.env);
    let honest = expire_checked(&mut s.env, &s.mm, &k, &b.key(), nonce);
    let h = honest.expired();
    assert_eq!(
        (
            h.buyback_cost,
            h.rent,
            h.expiry_fee,
            h.keeper_reward,
            h.borrower_payout
        ),
        (
            18_653_749_886,
            758_985_315,
            373_074_998,
            19_653_749,
            18_064_973_734
        ),
        "the sim's honest expiry"
    );
    assert_eq!((h.b_ref, h.max_in), (18_653_749_946, 18_840_287_445));
    let (h_keeper, h_borrower, h_rent, h_fee) =
        (h.keeper_reward, h.borrower_payout, h.rent, h.expiry_fee);
    s.env.svm = saved;
    let a = attacker(&mut s);
    let (ex, pnl) = sandwich_expiry(&mut s, &a, &b, nonce, 680_059_969_048);
    let ev = ex.expired();
    assert_eq!(
        (
            ev.buyback_cost,
            ev.rent,
            ev.expiry_fee,
            ev.keeper_reward,
            ev.borrower_payout
        ),
        (
            18_823_598_543,
            758_985_315,
            373_074_986,
            19_653_749,
            17_895_125_089
        ),
        "the sim's attacked expiry"
    );
    assert_eq!((ev.b_ref, ev.max_in), (18_653_749_253, 18_840_286_745));
    assert_eq!(pnl, 132_742_027, "the sim's attacker net");
    assert_eq!(
        pnl - i128::from(h_keeper),
        113_088_278,
        "extra over an honest keeper (+0.113)"
    );
    assert_eq!(
        h_borrower - ev.borrower_payout,
        169_848_645,
        "borrower loss (0.170)"
    );
    assert_eq!(ev.rent, h_rent, "rent senior");
    assert!(
        h_fee - ev.expiry_fee < 100,
        "fee senior (base moves by the front-run's L)"
    );
}

// ---- block_spot.py (a): blocking the spot leg cannot widen the fixed bound --------------------

/// The sim's `V_eff` (its rent due has `R_min` at 80 %: module docs).
fn sim_v_eff(p: &Predicted) -> u64 {
    let pos = &p.view.pos;
    let pr = shortwin_math::rent::PosRent {
        r_min: SIM_RMIN,
        ..pos.pos_rent()
    };
    let r = shortwin_math::rent::due(&pr, &p.view.market.basis()).unwrap();
    pos.v.saturating_sub(r)
}

/// `block_spot.liq_price_target`: the largest spot price at which the spot leg (live liquidity)
/// still fails, by the sim's 80-step bisection.
fn liq_price_target(p: &Predicted) -> u128 {
    let pos = &p.view.pos;
    let v_eff = sim_v_eff(p);
    let risky = live_pricing(p.view.fee_ppm).capped(u64::from(pos.venue_fee_hard_cap_bps));
    let lk = shortwin_math::quote::liquidity(u128::from(p.view.x), u128::from(p.view.y)).unwrap();
    let spot = u128::from(p.view.y) * W / u128::from(p.view.x);
    let (mut lo, mut hi) = (spot / 4, spot);
    for _ in 0..80 {
        let mid = (lo + hi) / 2;
        match risky.buy_repriced(pos.q, lk, mid) {
            Some(q)
                if u128::from(v_eff) * 10_000 / u128::from(q.cost)
                    >= u128::from(pos.cr_liq_bps) =>
            {
                lo = mid
            }
            _ => hi = mid,
        }
    }
    lo
}

/// `status()["ema_run"] ≥ 1` with the sim's `V_eff`: the newest ring entry is below `L`.
fn ema_run_started(p: &Predicted) -> bool {
    let pos = &p.view.pos;
    let v_eff = sim_v_eff(p);
    let risky = live_pricing(p.view.fee_ppm).capped(u64::from(pos.venue_fee_hard_cap_bps));
    let (x, y) = p.view.market.newest_ring_xy();
    let q = if x == 0 {
        None
    } else {
        risky.buy(pos.q, x, y).map(|b| b.cost)
    };
    shortwin_math::risk::cr_below(v_eff, q, pos.cr_liq_bps).unwrap()
}

/// `block_spot.offensive("RA", w)`, v0.4.1: during the honest +0.1 %/s rise, from the first
/// second the EMA leg starts, the attacker lands one atomic `[sell, crank_price, buy back]` a
/// second that leaves a price below the liquidation price in that second's bucket, so the robust
/// spot leg fails for every honest keeper; after `w` blocked seconds it lets one clean second
/// pass and sandwiches the liquidation. Asserts the sim's block cost, exit second, sandwich
/// (dX, `B`, `B̂_ref`, `max_in`, penalty, borrower) and the bound's δ at the exit.
fn block_spot_ra(
    w: u32,
    cost: i128,
    exit_t: i64,
    dx: u64,
    sim: SimExit,
    pnl_sim: i128,
    keeper_sim: u64,
) {
    let mut s = Sim::ra(RA_Q);
    let (b, nonce) = s.open(RA_Q);
    let v = s.env.position(&s.mm, &b.key(), nonce).v;
    let spot_post = s.spot();
    for t in 1..=120 {
        s.step_to(t);
    }
    let a = attacker(&mut s);
    let kp = a.kp.insecure_clone();
    let (mut t, mut k, mut paid, mut blocked) = (120i64, 1.0f64, 0i128, 0u32);
    loop {
        k *= 1.001;
        t += 1;
        s.at(t);
        s.exo_move((spot_post as f64 * k) as u128);
        s.env.crank(&s.mm);
        let p = predict(&read_view(&s.env, &s.mm, &b.key(), nonce));
        assert!(
            p.liquidation().is_err(),
            "the block holds: no honest liquidation at {t}"
        );
        if ema_run_started(&p) || blocked > 0 {
            if blocked >= w {
                break;
            }
            let tgt = liq_price_target(&p) * 995 / 1_000;
            let d = s.move_size(tgt);
            if d < 0 {
                let d = u64::try_from(-d).unwrap();
                let w0 = i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
                let ixs = [s.sell_ix(&a, d), crank_price_ix(&s.mm), s.buy_ix(&a, d)];
                ok(s.env.send(&ixs, &[&kp]));
                paid += w0 - i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
            }
            blocked += 1;
        }
        assert!(k < 5.0);
    }
    assert_eq!(paid, cost, "the sim's block cost");
    // The first clean second: the sim sandwiches at once (`wait = 0`).
    let tf = t + 1;
    k *= 1.001;
    s.at(tf);
    s.exo_move((spot_post as f64 * k) as u128);
    assert_eq!(tf, exit_t, "the sim's exit second");
    assert_eq!(best_dx(&mut s, &b, nonce), dx, "the sim's dX");
    let (ex, pnl) = sandwich_exit(&mut s, &a, &b, nonce, dx);
    let ev = ex.liquidated().clone_event();
    assert_sim_liquidation(&ev, &sim, v);
    assert_eq!(
        pnl - i128::from(ev.penalty_keeper),
        pnl_sim - i128::from(keeper_sim)
    );
    // δ at the exit: `⌊max_in·10⁴/B̂_ref⌋ − 10⁴` ≤ 200 — blocking never widens the bound.
    let delta = u128::from(ev.max_in) * 10_000 / u128::from(ev.b_ref) - 10_000;
    assert!(delta <= 200, "δ {delta} bps");
    assert_eq!(ev.delta_bps, 200);
}

/// `block_spot.py` (a), RA, v0.4.1 rows: blocking for 1 / 4 / 8 s costs 0.094 / 0.423 / 0.974
/// SOL and the exit bound stays the fixed 2 % (the sim: "delta at the exit 1.99 % / 1.99 % /
/// 1.99 %"; v0.2's clock reached 4.99 %): the extra over an honest keeper is +0.321 / −0.197 /
/// −0.816 SOL — the V1-1 attack class is gone. AIR-22 (the sustained growth's lower median)
/// moved the 1-s row's sandwich (ECON: +0.178 at 2.00 %, the minimum's).
#[test]
fn blocking_the_spot_leg_cannot_widen_the_bound_ra() {
    block_spot_ra(
        1,
        93_923_030,
        612,
        1_063_940_047_008,
        SimExit {
            b: 31_127_157_175,
            rent: SIM_RMIN,
            pen: 1_527_028_508,
            borrower: 5_215_182_007,
            b_ref: 30_540_570_159,
            max_in: 31_151_381_562,
        },
        873_292_718,
        458_108_552,
    );
    block_spot_ra(
        4,
        423_359_089,
        615,
        590_220_175_781,
        SimExit {
            b: 30_969_070_830,
            rent: SIM_RMIN,
            pen: 1_518_644_626,
            borrower: 5_381_652_234,
            b_ref: 30_372_892_513,
            max_in: 30_980_350_363,
        },
        685_192_926,
        455_593_387,
    );
    block_spot_ra(
        8,
        974_010_077,
        619,
        412_361_321_417,
        SimExit {
            b: 30_999_618_143,
            rent: SIM_RMIN,
            pen: 1_519_670_804,
            borrower: 5_350_078_743,
            b_ref: 30_393_416_077,
            max_in: 31_001_284_398,
        },
        616_888_103,
        455_901_241,
    );
}

/// The sim's run of the borrower's veto: `w`, its total cost, the last veto second's cost, and
/// the liquidation 2 s after the veto stops (`B`, `B̂_ref`, `max_in`, penalty, borrower).
struct Veto {
    w: u32,
    cost: i128,
    last: i128,
    liq: SimExit,
}

/// `block_spot.py` (b), V1-2 (M10 G-E2), RA, run with the script's functions (its
/// `liq_price_target`, `move_size`, `try_exit`): ECON §12.9 prices one veto second statically
/// (`block_spot.defensive`: 0.1098 SOL/s at the first liquidatable second of the rise, 53 s of
/// veto for the 5.776-SOL remainder); here the veto runs. The honest +0.1 %/s rise until the EMA
/// leg starts (t = 610), then the price held flat. Honestly the position is liquidated at 612.
/// Instead its borrower lands one atomic `[sell to 0.995 × the liquidation price, crank_price,
/// buy back]` every second after the keeper's crank, for `w` = 10, 60 and 300 s — the robust
/// spot leg fails for every honest keeper, whose checked `liquidate` is refused every second —
/// then stops. To the lamport: the veto costs the sim's 0.950 / 6.073 / 41.983 SOL (0.0939 SOL in its first second, 0.0961 / 0.1089 /
/// 0.1991 in its last: each round trip leaves its fees in the pool); the liquidation lands 2 s
/// after the last veto (`liq_persist_n` − 1 EMA updates), its buyback, reference, bound, penalty
/// and remainder the sim's (rent adjusted). The lenders get `Q` back and their legs; the low
/// observations lower the reference a little (penalty −0.5 % … −0.3 % against the unvetoed
/// liquidation), the borrower pays the rest. The figures: `python3 tests/litesvm/sim/ra_veto.py`.
#[test]
fn a_borrower_veto_defers_its_liquidation_at_the_sims_cost_ra() {
    let runs = [
        Veto {
            w: 10,
            cost: 949_902_505,
            last: 96_064_700,
            liq: SimExit {
                b: 30_513_932_642,
                rent: SIM_RMIN,
                pen: 1_517_697_017,
                borrower: 5_837_738_031,
                b_ref: 30_353_940_335,
                max_in: 30_961_019_141,
            },
        },
        Veto {
            w: 60,
            cost: 6_073_451_212,
            last: 108_890_251,
            liq: SimExit {
                b: 30_563_267_399,
                rent: SIM_RMIN,
                pen: 1_518_099_549,
                borrower: 5_788_000_742,
                b_ref: 30_361_990_968,
                max_in: 30_969_230_787,
            },
        },
        Veto {
            w: 300,
            cost: 41_983_331_023,
            last: 199_087_390,
            liq: SimExit {
                b: 30_909_247_721,
                rent: SIM_RMIN,
                pen: 1_520_922_909,
                borrower: 5_439_197_060,
                b_ref: 30_418_458_168,
                max_in: 31_026_827_331,
            },
        },
    ];
    let mut s = Sim::ra(RA_Q);
    let (b, nonce) = s.open(RA_Q);
    let v = s.env.position(&s.mm, &b.key(), nonce).v;
    let spot_post = s.spot();
    for t in 1..=120 {
        s.step_to(t);
    }
    let (mut t, mut k) = (120i64, 1.0f64);
    loop {
        k *= 1.001;
        t += 1;
        s.at(t);
        s.exo_move((spot_post as f64 * k) as u128);
        s.env.crank(&s.mm);
        let p = predict(&read_view(&s.env, &s.mm, &b.key(), nonce));
        assert!(p.liquidation().is_err(), "not yet liquidatable at {t}");
        if ema_run_started(&p) {
            break;
        }
    }
    assert_eq!(t, 610, "the sim's EMA-leg start");
    let start = t;
    let saved = s.env.svm.clone();
    let kk = keeper(&mut s.env);
    // The honest control: the price held, the keeper liquidates at 612.
    let mut tt = start;
    let honest = loop {
        let ex = liquidate_checked(&mut s.env, &s.mm, &kk, &b.key(), nonce);
        if ex.executed() {
            break ex.liquidated().clone_event();
        }
        tt += 1;
        s.step_to(tt);
    };
    assert_eq!(tt, 612, "honestly liquidated at 612");
    assert_sim_liquidation(
        &honest,
        &SimExit {
            b: 30_504_786_809,
            rent: SIM_RMIN,
            pen: 1_525_239_341,
            borrower: 5_839_341_540,
            b_ref: 30_535_438_220,
            max_in: 31_146_146_984,
        },
        v,
    );
    for run in &runs {
        s.env.svm = saved.clone();
        let a = attacker(&mut s);
        let kp = a.kp.insecure_clone();
        let mut tt = start;
        let (mut cost, mut first, mut last) = (0i128, 0i128, 0i128);
        for i in 0..run.w {
            if i > 0 {
                tt += 1;
                s.step_to(tt);
            }
            let p = predict(&read_view(&s.env, &s.mm, &b.key(), nonce));
            let tgt = liq_price_target(&p) * 995 / 1_000;
            let d = s.move_size(tgt);
            assert!(d < 0, "a low observation needs a sale");
            let d = u64::try_from(-d).unwrap();
            let w0 = i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
            let ixs = [s.sell_ix(&a, d), crank_price_ix(&s.mm), s.buy_ix(&a, d)];
            ok(s.env.send(&ixs, &[&kp]));
            let c = w0 - i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
            if i == 0 {
                first = c;
            }
            last = c;
            cost += c;
            let ex = liquidate_checked(&mut s.env, &s.mm, &kk, &b.key(), nonce);
            assert!(!ex.executed(), "w {}: vetoed at {tt}", run.w);
        }
        let stop = tt;
        let ev = loop {
            tt += 1;
            s.step_to(tt);
            let ex = liquidate_checked(&mut s.env, &s.mm, &kk, &b.key(), nonce);
            if ex.executed() {
                break ex.liquidated().clone_event();
            }
            assert!(tt - stop < 30, "liquidated after the veto");
        };
        eprintln!(
            "veto of {} s: cost {:.4} SOL ({:.4} → {:.4} SOL/s), liquidated {} s after it; \
             penalty {:.4} → {:.4} SOL, borrower {:.4} → {:.4} SOL",
            run.w,
            cost as f64 / 1e9,
            first as f64 / 1e9,
            last as f64 / 1e9,
            tt - stop,
            honest.penalty as f64 / 1e9,
            ev.penalty as f64 / 1e9,
            honest.borrower_payout as f64 / 1e9,
            ev.borrower_payout as f64 / 1e9
        );
        assert_eq!(
            first, 93_923_030,
            "the sim's first-second cost (block_spot.defensive)"
        );
        assert_eq!(
            (cost, last),
            (run.cost, run.last),
            "w {}: the sim's cost",
            run.w
        );
        assert_eq!(
            tt - stop,
            2,
            "w {}: liquidated 2 s after the last veto",
            run.w
        );
        assert_sim_liquidation(&ev, &run.liq, v);
        assert!(
            ev.penalty * 100 >= honest.penalty * 99,
            "the lenders' and protocol's penalty within 1 % of the unvetoed one"
        );
    }
}

// ---- ptf_sandwich.py: push-then-freeze cannot lift the liquidation reference (R4B-2) ------

/// `shortwin_sim.attacker_bundle`: `[move the pool to target, crank_price, trade back]` in one
/// transaction by `a`; returns its WSOL cost (the round trip's venue fees).
fn attacker_bundle(s: &mut Sim, a: &Actor, target: u128) -> i128 {
    let d = s.move_size(target);
    let w0 = i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
    let ixs = match d {
        0 => vec![crank_price_ix(&s.mm)],
        d if d > 0 => {
            let d = u64::try_from(d).unwrap();
            vec![s.buy_ix(a, d), crank_price_ix(&s.mm), s.sell_ix(a, d)]
        }
        d => {
            let d = u64::try_from(-d).unwrap();
            vec![s.sell_ix(a, d), crank_price_ix(&s.mm), s.buy_ix(a, d)]
        }
    };
    let kp = a.kp.insecure_clone();
    ok(s.env.send(&ixs, &[&kp]));
    w0 - i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap())
}

/// `ptf_sandwich.py`, RA, v0.4.1 rows (R4B-2): on `sandwich.setup_liq`'s genuine +0.1 %/s rise
/// the attacker pushes the pool ×1.5 / ×2 in one attacker-only second (the honest keeper misses
/// it) `lead` seconds before the first liquidatable second, freezes `P_ema` with one bundle per
/// second at `P_ema·(1 + 5 bps)` (the honest keeper's observation is in each bucket too), then
/// sandwiches the liquidation as the keeper with the sim's best `dX`. v0.4.1 caps the reference
/// at `lo_x·(1 + κ_eff)` — the honest bucket low, extrapolated by a sustained growth a frozen
/// EMA does not have — so `B̂_ref` stays at the honest one (ratio 1.000) and the sandwich takes
/// what the atomic one does: ECON attacked `31.127 / 5.216`, borrower loss 0.560, lenders
/// −0.0004, attacker net −4.374 / −8.387 / −12.751 SOL after the push and freeze costs. The
/// sim's figures, to the lamport (the borrower's after the `R_min` difference, module docs).
#[test]
fn push_then_freeze_cannot_lift_the_liquidation_reference_ra() {
    // (mult, freeze seconds, lead, push cost, freeze costs, dX, B, B̂_ref, max_in, penalty,
    //  keeper share, borrower, sandwich P&L incl. the keeper share, net)
    #[allow(clippy::type_complexity)]
    let rows: [(
        f64,
        i64,
        i64,
        i128,
        &[i128],
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        i128,
        i128,
    ); 3] = [
        (
            1.5,
            4,
            6,
            2_949_072_740,
            &[585_593_763, 578_248_788, 570_897_120, 563_532_786],
            1_064_694_117_913,
            31_126_937_392,
            30_535_217_568,
            31_145_921_919,
            1_526_760_879,
            458_028_263,
            5_215_669_419,
            873_105_196,
            -4_374_240_001,
        ),
        (
            2.0,
            4,
            6,
            4_761_105_834,
            &[1_134_808_475, 1_128_108_690, 1_121_395_622, 1_114_668_921],
            1_065_273_255_389,
            31_126_767_562,
            30_535_052_333,
            31_145_753_379,
            1_526_752_617,
            458_025_785,
            5_215_847_511,
            873_020_352,
            -8_387_067_190,
        ),
        (
            2.0,
            8,
            10,
            4_751_587_426,
            &[
                1_132_539_765,
                1_125_853_374,
                1_119_153_727,
                1_112_440_475,
                1_105_713_619,
                1_098_973_166,
                1_092_219_116,
                1_085_451_471,
            ],
            1_065_916_705_006,
            31_126_585_962,
            30_534_875_957,
            31_145_573_476,
            1_526_743_798,
            458_023_139,
            5_216_037_930,
            872_926_290,
            -12_751_005_849,
        ),
    ];
    // The honest keeper at t_liq = 612 (sandwich_liquidation_atomic_ra): B̂_ref 30,547,802,182.
    let honest_b_ref: u64 = 30_547_802_182;
    let t_liq = 612;
    for (
        mult,
        freeze,
        lead,
        push_cost,
        freeze_costs,
        dx,
        b_,
        b_ref,
        max_in,
        pen,
        keeper_,
        borrower,
        pnl_sim,
        net_sim,
    ) in rows
    {
        let mut s = Sim::ra(RA_Q);
        let (b, nonce) = s.open(RA_Q);
        let spot_post = s.spot();
        let a = attacker(&mut s);
        for t in 1..=120 {
            s.step_to(t);
        }
        let mut k = 1.0f64;
        let mut t = 120;
        let exo = |s: &mut Sim, k: &mut f64, t: &mut i64| {
            *k *= 1.001;
            *t += 1;
            s.at(*t);
            s.exo_move((spot_post as f64 * *k) as u128);
        };
        while t < t_liq - lead {
            exo(&mut s, &mut k, &mut t);
            s.step_to(t);
        }
        // The attacker-only push second.
        exo(&mut s, &mut k, &mut t);
        let target = (s.spot() as f64 * mult) as u128;
        let mut cost = attacker_bundle(&mut s, &a, target);
        assert_eq!(cost, push_cost, "×{mult}: the push second's cost");
        // Freeze seconds: the honest keeper's crank, then the attacker's bundle at P_ema + 5 bps.
        let mut fz = 0;
        while t < t_liq - 1 {
            exo(&mut s, &mut k, &mut t);
            s.step_to(t);
            if fz < freeze {
                let ema = s.env.market(&s.mm.market()).ema_price.get();
                let c = attacker_bundle(&mut s, &a, ema * 10_005 / 10_000);
                assert_eq!(
                    c,
                    freeze_costs[usize::try_from(fz).unwrap()],
                    "×{mult}: freeze {fz}"
                );
                cost += c;
                fz += 1;
            }
        }
        // t_liq: nobody cranks first; the attacker-keeper's sandwich with the sim's best dX.
        exo(&mut s, &mut k, &mut t);
        assert_eq!(t, t_liq);
        let v = s.env.position(&s.mm, &b.key(), nonce).v;
        let (ex, pnl) = sandwich_exit(&mut s, &a, &b, nonce, dx);
        let ev = ex.liquidated().clone_event();
        assert_eq!(
            (ev.buyback_cost, ev.b_ref, ev.max_in, ev.penalty),
            (b_, b_ref, max_in, pen),
            "×{mult} / {freeze} s: the sim's B, B̂_ref, max_in, penalty"
        );
        assert_eq!(ev.penalty_keeper + ev.keeper_topup, keeper_);
        assert!(ev.rent <= SIM_RMIN);
        assert_eq!(ev.borrower_payout, borrower + (SIM_RMIN - ev.rent));
        assert_eq!(
            ev.buyback_cost + ev.rent + ev.penalty + ev.keeper_topup + ev.borrower_payout,
            v
        );
        assert!(
            ev.b_ref <= honest_b_ref,
            "the reference is not lifted (ratio 1.000)"
        );
        assert_eq!(pnl, pnl_sim, "the sandwich's P&L (trades + keeper share)");
        assert_eq!(
            pnl - cost,
            net_sim,
            "the attacker's net after push and freeze"
        );
        assert!(pnl - cost < 0, "push-then-freeze does not pay");
    }
}

// ---- fvcap.py: the fair-value cap binds only in S2 / S3, never in S1 (C0-7, D3) -------------

/// `fvcap.case("RA", drain_bps, k)`: the position at the cap at `t = 0`, 60 honest seconds, the
/// pool admin pulls `drain_bps` of both reserves (a model case: the LP lock broken, ECON §8.6),
/// 600 s for the liquidity EMA, an exogenous move by `k`, 120 s for the EMA, then the keeper's
/// seconds: one crank, then `liquidate`, and `sol_settle` when the liquidation is refused (the
/// sim's `try_exit`). Returns the sim second and the settlement (or `None` within 200 s), and
/// the rent due the program priced it with.
fn fvcap_ra(drain_bps: u64, k: f64) -> (Option<(i64, SolSettle)>, u64, ShortPosition) {
    let mut s = Sim::ra(RA_Q);
    let (b, nonce) = s.open(RA_Q);
    for t in 1..=60 {
        s.step_to(t);
    }
    let (x, y) = s.reserves();
    let pull = |r: u64| u64::try_from(u128::from(r) * u128::from(drain_bps) / 10_000).unwrap();
    ok(mock::remove_liquidity(
        &mut s.env,
        &s.mm.pool,
        pull(x),
        pull(y),
    ));
    for t in 61..=660 {
        s.step_to(t);
    }
    let target = (s.spot() as f64 * k) as u128;
    s.exo_move(target);
    for t in 661..=780 {
        s.step_to(t);
    }
    let pos = s.env.position(&s.mm, &b.key(), nonce);
    let kp = keeper(&mut s.env);
    for t in 781..980 {
        s.step_to(t);
        let r_due = rent::due(&pos.pos_rent(), &s.env.market(&s.mm.market()).basis()).unwrap();
        let liq = exit_ix(Which::Liquidate, &s.mm, &kp.pubkey(), &b.key(), nonce);
        assert!(
            s.env.send(&[liq], &[&kp]).is_err(),
            "the sim settles, it never liquidates here"
        );
        let ix = sol_settle_ix(&s.env, &s.mm, &kp.pubkey(), &b.key(), nonce);
        let meta = ok(s.env.send(&[ix], &[&kp]));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            return (Some((t, ev)), r_due, pos);
        }
    }
    (None, 0, pos)
}

/// `fvcap.py` on RA (ECON §8.6's drained-pool cases, C0-7 / D3): with 80 % of the LP pulled, a
/// ×1.9 move settles nothing within 200 s (nor do ×1.6 … ×1.8 in the sim) and ×2.0 settles
/// **S1** in second 821 — the lenders take exactly `V − K_s` = 37.620 SOL, never capped (FV at
/// EMA would be 41.069), of which the 0.658 SOL above `⌊Q·P_ema/WAD⌋ + R_due` (the drained
/// pool's impact) vests (AIR-21); with 95 % pulled, ×1.2 settles nothing (nor does ×1.0 in the
/// sim) and ×1.4 settles **S2** in second 821, capped at fair value: lenders 33.048 SOL (of
/// which 7.182 SOL vest as a lump), borrower 4.573 SOL. Every figure to the lamport once the
/// rent is the program's (the sim prices `R_min` at 80 % utilisation, module docs): FV and the
/// borrower's part move by that difference, and so does the S1 lump (a lower `R_due` leaves
/// more of the take above the immediate part); the S2 lump does not.
#[test]
fn the_fair_value_cap_binds_in_s2_never_in_s1_ra() {
    for (drain, k) in [(8_000, 1.9), (9_500, 1.2)] {
        assert!(
            fvcap_ra(drain, k).0.is_none(),
            "{drain} bps ×{k}: nothing settles"
        );
    }
    let v = 37_870_437_682u64;
    let (s1, r_due1, pos) = fvcap_ra(8_000, 2.0);
    let (t, ev) = s1.expect("S1");
    assert_eq!(pos.v, v);
    assert_eq!((t, ev.condition), (821, 1), "S1 in the sim's second");
    assert_eq!(ev.k_s, 250_000_000);
    assert_eq!(ev.lender_amount, 37_620_437_682, "S1: V − K_s, uncapped");
    assert_eq!(
        (ev.borrower_amount, ev.lump),
        (0, 658_250_579 + (SIM_RMIN - r_due1)),
        "S1's take above ⌊Q·P_ema/WAD⌋ + R_due vests (AIR-21; rent adjusted)"
    );
    let (s2, r_due, _) = fvcap_ra(9_500, 1.4);
    let (t, ev) = s2.expect("S2");
    assert_eq!((t, ev.condition), (821, 2), "S2 in the sim's second");
    let d = SIM_RMIN - r_due;
    assert_eq!(ev.fv, 33_047_677_770 - d, "FV (rent adjusted)");
    assert_eq!(ev.lender_amount, ev.fv, "capped at FV");
    assert_eq!(ev.lump, 7_181_698_087, "the vesting lump");
    assert_eq!(
        ev.borrower_amount,
        4_572_759_912 + d,
        "the borrower keeps V − K_s − FV"
    );
    assert_eq!(ev.k_s + ev.lender_amount + ev.borrower_amount, v);
}

// ---- ema_atomic.py / ema_near.py / ema_atomic_multi.py: atomic [pump, crank, dump] --------

/// One second of the atomic model (`common.push_second(.., "atomic")`): the in-house keeper's
/// honest crank at `t`, then the attacker's one-transaction bundle `[buy d, crank_price, sell
/// d]` that shows the pool at `target` to the same second's bucket.
fn atomic_second(s: &mut Sim, a: &Actor, t: i64, target: u128) {
    let d = s.move_size(target);
    s.step_to(t);
    if d > 0 {
        let d = u64::try_from(d).unwrap();
        let ixs = [s.buy_ix(a, d), crank_price_ix(&s.mm), s.sell_ix(a, d)];
        let kp = a.kp.insecure_clone();
        ok(s.env.send(&ixs, &[&kp]));
    }
}

/// The attacker's final bundle `[buy d, liquidate, sell d]` (it pumps the live spot in its own
/// transaction), checked with the oracle: it must be refused with `NotLiquidatable`.
fn final_bundle_refused(s: &mut Sim, a: &Actor, b: &Borrower, nonce: u64, target: u128) {
    let d = s.move_size(target);
    let (front, back) = if d > 0 {
        let d = u64::try_from(d).unwrap();
        (vec![s.buy_ix(a, d)], vec![s.sell_ix(a, d)])
    } else {
        (vec![], vec![])
    };
    let kp = a.kp.insecure_clone();
    let ex = exit_checked(
        &mut s.env,
        &s.mm,
        Which::Liquidate,
        &kp,
        &b.key(),
        nonce,
        &front,
        &back,
        &[],
    );
    assert_eq!(ex.refused(), E::NotLiquidatable);
}

/// `ema_atomic.py`, model atomic, RA: a fresh position (60 s after the open) under atomic
/// bundles every second at ×1.25 … ×3 of the true price for 60 s — ECON: "not liquidatable
/// within 60 s at any push level". The in-house keeper's honest observation in every second
/// keeps the bucket's low at the true price, so the EMA never rises (directional aggregation,
/// ECON §5.2) and the robust spot leg reads the honest low; every final `[pump, liquidate,
/// dump]` is refused.
#[test]
fn atomic_bundles_cannot_liquidate_a_fresh_position_ra() {
    let mut s = Sim::ra(RA_Q);
    let (b, nonce) = s.open(RA_Q);
    let a = attacker(&mut s);
    for t in 1..=60 {
        s.step_to(t);
    }
    let p_true = s.spot();
    let ema0 = s.env.market(&s.mm.market()).ema_price.get();
    let saved = s.env.svm.clone();
    for mult in [1.25, 1.5, 1.75, 2.0, 2.5, 3.0] {
        s.env.svm = saved.clone();
        let target = (p_true as f64 * mult) as u128;
        // The honest price the keeper observes (it drifts with the fees the round trips leave in
        // the pool); the EMA never rises above the honest evidence.
        let mut honest_max = p_true;
        for n in 1..=60 {
            honest_max = honest_max.max(s.spot());
            atomic_second(&mut s, &a, 60 + n, target);
            let p = s.status(&b, nonce);
            assert!(!p.would_liq() && !p.ema_leg, "×{mult}, second {n}");
            assert!(
                p.view.market.ema_price.get() <= ema0.max(honest_max),
                "×{mult}, second {n}: the pushes do not lift the EMA"
            );
            if n % 5 == 0 {
                final_bundle_refused(&mut s, &a, &b, nonce, target);
            }
        }
    }
}

/// `ema_near.py`, model atomic, RA rows: after an honest rise of ×1.30 … ×1.55 with the EMA
/// caught up (`CR_ema` = 1.248 … 1.045 × L, the sim's values), 30 s of atomic bundles at ×1.02
/// … ×2 never make the position liquidatable — ECON: "not reached within 30 s".
#[test]
fn atomic_bundles_cannot_push_near_threshold_positions_ra() {
    for (k0, cr_over_l) in [(1.30, 1.248), (1.40, 1.158), (1.50, 1.080), (1.55, 1.045)] {
        let mut s = Sim::ra(RA_Q);
        let (b, nonce) = s.open(RA_Q);
        let a = attacker(&mut s);
        for t in 1..=60 {
            s.step_to(t);
        }
        let p = (s.spot() as f64 * k0) as u128;
        s.exo_move(p);
        for t in 61..=360 {
            s.step_to(t);
        }
        let st = s.status(&b, nonce);
        assert!(!st.cr_liquidatable(), "×{k0}: not liquidatable honestly");
        let l = f64::from(st.view.pos.cr_liq_bps);
        let got = (st.cr_ema_bps as f64 / l * 1000.0).round() / 1000.0;
        assert_eq!(got, cr_over_l, "×{k0}: CR_ema / L as in ECON");
        let p_true = s.spot();
        let saved = s.env.svm.clone();
        for mult in [1.02, 1.05, 1.1, 1.15, 1.2, 1.3, 1.5, 1.75, 2.0] {
            s.env.svm = saved.clone();
            let target = (p_true as f64 * mult) as u128;
            for n in 1..=30 {
                atomic_second(&mut s, &a, 360 + n, target);
                assert!(
                    !s.status(&b, nonce).would_liq(),
                    "×{k0} push ×{mult}, second {n}"
                );
            }
            final_bundle_refused(&mut s, &a, &b, nonce, target);
        }
    }
}

/// The RA OI-cap basket: positions at the per-position cap until the market cap binds, opened
/// one a second from `t = 0`. `ema_atomic_multi.fill` evaluates the market cap at `X₀ + q_max`
/// (with the live `X` for `X₀`) and then opens less, so its third position (626,197,250,212)
/// exceeds the program's cap, which holds `ΣQ + q ≤ oi_cap(X₀ + q)` for the `q` opened on the
/// conservative `X₀` (ARCH §4.6; a sim inconsistency, see the track's notes). With the program's
/// rule the sim's own fill is these three positions and the scripts give the outcomes asserted
/// below (the sim's `fill` replaced by the ARCH §4.6 rule, nothing else changed).
const RA_FILL: [u64; 3] = [1_124_774_998_898, 1_124_784_207_520, 573_171_122_834];

/// The RA OI-cap basket (four borrowers), then 60 s of honest cranks (`cascade.setup`).
fn basket_ra() -> (Sim, Vec<(Borrower, u64)>, i64) {
    let mut s = Sim::ra(RA_FILL.iter().sum());
    let mut poss = Vec::new();
    for (t, q) in RA_FILL.iter().enumerate() {
        s.at(i64::try_from(t).unwrap());
        let mut b = s.env.borrower();
        let (res, nonce) = s.env.open(&s.mm, &mut b, *q);
        ok(res);
        poss.push((b, nonce));
    }
    let mut t = i64::try_from(RA_FILL.len()).unwrap();
    for _ in 0..60 {
        t += 1;
        s.step_to(t);
    }
    (s, poss, t)
}

/// `ema_atomic_multi.py` (a), model atomic, RA: the keeper pushes the OI-cap basket every second
/// for 120 s at ×1.5 … ×3 — ECON: "no exit forced within 120 s at any push level".
#[test]
fn atomic_bundles_cannot_force_the_oi_cap_basket_ra() {
    let (mut s, poss, t0) = basket_ra();
    let a = attacker(&mut s);
    let p_true = s.spot();
    let keys: Vec<(anchor_lang::prelude::Pubkey, u64)> =
        poss.iter().map(|(b, n)| (b.key(), *n)).collect();
    let saved = s.env.svm.clone();
    for mult in [1.5, 1.75, 2.0, 2.5, 3.0] {
        s.env.svm = saved.clone();
        let target = (p_true as f64 * mult) as u128;
        for n in 1..=120 {
            atomic_second(&mut s, &a, t0 + n, target);
            for v in views(&mut s.env, &s.mm, &keys) {
                assert!(!predict(&v).would_liq(), "×{mult}, second {n}");
            }
            if n % 20 == 0 {
                let (b, nonce) = &poss[0];
                final_bundle_refused(&mut s, &a, b, *nonce, target);
            }
        }
    }
}

// ---- ema_atomic_multi.py (a) held, (b) atomic and held: the OI-cap basket -----------------
//
// The sim's rows below are `ema_atomic_multi.rolling("RA", model, mode)` with the program's
// basket ([`RA_FILL`]: the sim's `fill` with the ARCH §4.6 market-cap rule, nothing else
// changed), printed second by second by the integration's helper
// (`ema_atomic_multi.rolling` replayed with a trace; ECON's RA rows have 4 positions, the
// program's basket 3). ECON's conclusions hold on the program: (a) held — the basket is forced
// and sandwiched in one transaction, marginally profitable on a 25 bps Raydium-like pool
// (ECON: "+0.5 – 8.5 SOL", RA +3.15 with 4 positions; here +2.258); (b) atomic — no exit is
// forced within 120 s at any push level; (b) held — S1 is forced on every position, but the
// 40-update persistence costs far more than the windfall (ECON: RA −338.35; here −331.75).

/// The keeper-attacker's one-transaction basket exit `[buy dX, exit₁ … exitₙ, sell dX]` (the
/// sim's `exit_all`): its metadata and the attacker's WSOL and lamport deltas.
fn basket_bundle(
    s: &mut Sim,
    a: &Actor,
    dx: u64,
    exits: Vec<Instruction>,
) -> (TransactionMetadata, i128, i128) {
    let w0 = i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
    let l0 = i128::from(read::lamports(&s.env.svm, &a.key()));
    let mut ixs = vec![s.buy_ix(a, dx)];
    ixs.extend(exits);
    ixs.push(s.sell_ix(a, dx));
    let kp = a.kp.insecure_clone();
    let meta = ok(s.env.send(&ixs, &[&kp]));
    let w1 = i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
    let l1 = i128::from(read::lamports(&s.env.svm, &a.key()));
    (meta, w1 - w0, l1 - l0)
}

/// `ema_atomic_multi.py` (a), model held, RA: the keeper holds the pool at ×2.5 of the true
/// price. The sim (program basket): no position is liquidatable for 6 s; in second 7 all three
/// are, and its best in-transaction pump `dX = 38,176,131,845,258` liquidates them in one
/// bundle. Push cost 32.743 SOL, in-transaction round trip +33.742, keeper shares 1.259,
/// attacker net **+2.258 SOL**, borrowers lose 42.076 SOL of their 48.858 SOL margin — to the
/// lamport here, with each position's rent at the program's accrued value instead of the
/// sim's `R_min` (module docs; the borrower keeps the difference).
#[test]
fn held_pushes_force_the_oi_cap_basket_marginally_ra() {
    let (mut s, poss, t0) = basket_ra();
    assert_eq!(t0, 63, "the sim's t0 after warm()");
    let a = attacker(&mut s);
    let p_true = s.spot();
    assert_eq!(p_true, 16_057_499_793_921_438, "the sim's p_true");
    let target = (p_true as f64 * 2.5) as u128;
    let sim_push: [i128; 7] = [
        4_676_848_241,
        4_677_124_075,
        4_677_381_246,
        4_677_619_762,
        4_677_839_632,
        4_678_040_865,
        4_678_223_467,
    ];
    let before: Vec<ShortPosition> = poss
        .iter()
        .map(|(b, n)| s.env.position(&s.mm, &b.key(), *n))
        .collect();
    let mut push = 0i128;
    for (n, want) in (1..=7).zip(sim_push) {
        let paid = held_second(&mut s, &a, t0 + n, target);
        assert_eq!(paid, want, "second {n}: the sim's push cost");
        push += paid;
        let liq: Vec<bool> = poss
            .iter()
            .map(|(b, nonce)| predict(&read_view(&s.env, &s.mm, &b.key(), *nonce)).would_liq())
            .collect();
        assert_eq!(liq, vec![n == 7; 3], "second {n}: the sim's candidates");
    }
    // Second 7: the sandwich over the whole basket, one transaction.
    let exits = poss
        .iter()
        .map(|(b, nonce)| exit_ix(Which::Liquidate, &s.mm, &a.key(), &b.key(), *nonce))
        .collect();
    let (meta, rt, keeper_lamports) = basket_bundle(&mut s, &a, 38_176_131_845_258, exits);
    let evs = events::of::<Liquidate>(&meta);
    assert_eq!(evs.len(), 3, "all three liquidated");
    // (B, rent, penalty, keeper, borrower) of the sim, per position.
    let sim = [
        (
            32_945_834_127u64,
            1_069_992u64,
            1_647_291_707u64,
            494_187_512u64,
            3_276_241_856u64,
        ),
        (
            33_606_922_434,
            1_070_001,
            1_680_346_122,
            504_103_836,
            2_582_411_515,
        ),
        (
            17_386_215_433,
            543_909,
            869_310_772,
            260_793_231,
            923_439_534,
        ),
    ];
    let mut keeper = 0i128;
    let mut lost = 0i128;
    for ((ev, (b, rent, pen, k, borrower)), p) in evs.iter().zip(sim).zip(&before) {
        assert_eq!(ev.buyback_cost, b, "B");
        assert_eq!(ev.penalty, pen, "penalty");
        assert_eq!(ev.penalty_keeper + ev.keeper_topup, k, "keeper share");
        assert!(ev.rent <= rent);
        assert_eq!(
            ev.borrower_payout,
            borrower + (rent - ev.rent),
            "borrower (rent adjusted)"
        );
        keeper += i128::from(ev.penalty_keeper + ev.keeper_topup);
        lost += i128::from(p.m) - i128::from(ev.borrower_payout);
    }
    assert_eq!(rt, 33_742_147_251, "the in-transaction round trip");
    assert_eq!(
        keeper_lamports, keeper,
        "the keeper legs reach the attacker"
    );
    assert_eq!(push, 32_743_077_288);
    assert_eq!(keeper, 1_259_084_579);
    let net = rt + keeper - push;
    assert_eq!(net, 2_258_154_542, "ECON: marginally profitable on RA");
    let rent_diff: i128 = evs
        .iter()
        .zip(sim)
        .map(|(ev, (_, rent, ..))| i128::from(rent - ev.rent))
        .sum();
    assert_eq!(lost, 42_075_967_468 - rent_diff, "borrower margin lost");
}

/// `ema_atomic_multi.py` (b), model atomic, RA: a lender owning the pool pushes the OI-cap
/// basket every second for 120 s at ×2.2 / ×2.6 / ×3.2 (`cr_open` × 1.1 / 1.3 / 1.6) and
/// calls `sol_settle` on every position inside its own pumped transaction each second — ECON:
/// "no exit forced within 120 s at any push level". The in-house keeper's honest observation
/// in every second keeps the bucket's low at the true price: the settlement clock never arms.
#[test]
fn atomic_bundles_cannot_force_s1_on_the_oi_cap_basket_ra() {
    let (mut s, poss, t0) = basket_ra();
    let a = attacker(&mut s);
    let p_true = s.spot();
    let saved = s.env.svm.clone();
    for mult in [2.2, 2.6, 3.2] {
        s.env.svm = saved.clone();
        let target = (p_true as f64 * mult) as u128;
        for n in 1..=120 {
            atomic_second(&mut s, &a, t0 + n, target);
            let d = s.move_size(target);
            let dx = u64::try_from(d.max(0)).unwrap();
            let settles = poss
                .iter()
                .map(|(b, nonce)| sol_settle_ix(&s.env, &s.mm, &a.key(), &b.key(), *nonce))
                .collect();
            let (meta, _, _) = basket_bundle(&mut s, &a, dx, settles);
            assert!(
                events::of::<SolSettle>(&meta).is_empty()
                    && events::of::<SettleArmed>(&meta).is_empty(),
                "×{mult}, second {n}: nothing arms or settles"
            );
            for (b, nonce) in &poss {
                assert_eq!(s.env.position(&s.mm, &b.key(), *nonce).s_armed, 0);
            }
        }
    }
}

/// `ema_atomic_multi.py` (b), model held, RA: a lender owning the pool holds it at ×2.2 (its
/// best level) and, from the first second the S1 legs hold (13), calls `sol_settle` on every
/// position inside its own pumped transaction each second (the sim's `exit_all` with the pump
/// `dX = move_size(target)`). The clocks arm in second 13 and every position settles **S1** in
/// second 53 (`settle_persist_n` = 40 EMA updates later), as in the sim; the S1 windfall to the
/// lenders (`V − K_s − Q·p_true`) is 48.893 SOL, but the held pushes cost 215.406 SOL and the
/// in-transaction pumps 165.941 SOL: attacker net **−331.753 SOL** (ECON RA, 4 positions:
/// −338.35). S1 forcing is unprofitable.
#[test]
fn held_pushes_forcing_s1_on_the_oi_cap_basket_lose_ra() {
    let (mut s, poss, t0) = basket_ra();
    let a = attacker(&mut s);
    let p_true = s.spot();
    let target = (p_true as f64 * 2.2) as u128;
    let before: Vec<ShortPosition> = poss
        .iter()
        .map(|(b, n)| s.env.position(&s.mm, &b.key(), *n))
        .collect();
    let (mut push, mut rt, mut keeper, mut windfall) = (0i128, 0i128, 0i128, 0i128);
    let mut settled = Vec::new();
    for n in 1..=53 {
        push += held_second(&mut s, &a, t0 + n, target);
        if n < 13 {
            continue;
        }
        let dx = u64::try_from(s.move_size(target)).unwrap();
        let settles = poss
            .iter()
            .map(|(b, nonce)| sol_settle_ix(&s.env, &s.mm, &a.key(), &b.key(), *nonce))
            .collect();
        let (meta, dw, dl) = basket_bundle(&mut s, &a, dx, settles);
        rt += dw;
        keeper += dl;
        let armed = events::of::<SettleArmed>(&meta);
        assert_eq!(
            armed.len(),
            if n == 13 { 3 } else { 0 },
            "second {n}: arming"
        );
        for ev in events::of::<SolSettle>(&meta) {
            settled.push((n, ev));
        }
        if n < 53 {
            assert!(
                settled.is_empty(),
                "second {n}: nothing settles before the sim's second"
            );
        }
    }
    assert_eq!(settled.len(), 3, "every position settled in second 53");
    for ((n, ev), p) in settled.iter().zip(&before) {
        assert_eq!(*n, 53);
        assert_eq!(ev.condition, 1, "S1");
        assert_eq!(ev.lender_amount, p.v - ev.k_s, "S1 takes V − K_s");
        windfall +=
            i128::from(ev.lender_amount) - i128::try_from(u128::from(p.q) * p_true / W).unwrap();
    }
    assert_eq!(
        settled.iter().map(|(_, e)| e.k_s).collect::<Vec<_>>(),
        vec![250_000_000, 250_000_000, 201_795_096],
        "the sim's K_s"
    );
    assert_eq!(push, 215_406_169_561, "the sim's held push cost");
    assert_eq!(rt, -165_941_174_480, "the sim's in-transaction round trips");
    assert_eq!(keeper, 701_795_096, "the keeper rewards reach the attacker");
    assert_eq!(windfall, 48_892_910_627, "the sim's S1 windfall");
    let net = rt + keeper + windfall - push;
    assert_eq!(net, -331_752_638_318, "ECON: S1 forcing does not pay");
}

// ---- ema_atomic.py / ema_near.py, model held: the forced liquidation does not pay -----------

/// One held second (`common.push_second(.., "held")`): the attacker holds the pool at `target`
/// for the whole second — `[buy d, crank_price, sell d]`, so every observation of the second
/// sees the push — and pays the round trip. Returns its cost.
fn held_second(s: &mut Sim, a: &Actor, t: i64, target: u128) -> i128 {
    let d = s.move_size(target);
    s.at(t);
    if d <= 0 {
        s.env.crank(&s.mm);
        return 0;
    }
    let d = u64::try_from(d).unwrap();
    let w0 = i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap());
    let ixs = [s.buy_ix(a, d), crank_price_ix(&s.mm), s.sell_ix(a, d)];
    let kp = a.kp.insecure_clone();
    ok(s.env.send(&ixs, &[&kp]));
    w0 - i128::from(read::token_amount(&s.env.svm, &a.wsol).unwrap())
}

/// A held push from `t0` until `would_liq` (the sim's `push_until`), then the sandwich the next
/// second with the sim's `dX`; asserts the sim's push seconds and cost, the liquidation, and the
/// sandwich's trade P&L. Returns the attacker's net (trade + keeper legs − push cost).
fn held_attack(
    s: &mut Sim,
    b: &Borrower,
    nonce: u64,
    t0: i64,
    target: u128,
    seconds: i64,
    cost: i128,
    dx: u64,
    sim: SimExit,
    sim_pnl: i128,
    sim_keeper: u64,
) -> i128 {
    let a = attacker(s);
    let v = s.env.position(&s.mm, &b.key(), nonce).v;
    let mut paid = 0;
    let mut t = t0;
    loop {
        t += 1;
        paid += held_second(s, &a, t, target);
        if predict(&read_view(&s.env, &s.mm, &b.key(), nonce)).would_liq() {
            break;
        }
        assert!(t - t0 < 60, "never liquidatable");
    }
    assert_eq!(t - t0, seconds, "the sim's push seconds");
    assert_eq!(paid, cost, "the sim's push cost");
    s.at(t + 1);
    let (ex, pnl) = sandwich_exit(s, &a, b, nonce, dx);
    let ev = ex.liquidated().clone_event();
    assert_sim_liquidation(&ev, &sim, v);
    let keeper_legs = i128::from(ev.penalty_keeper + ev.keeper_topup);
    assert_eq!(
        pnl - keeper_legs,
        sim_pnl - i128::from(sim_keeper),
        "the sandwich's trades"
    );
    pnl - paid
}

/// `ema_atomic.py`, model held, RA: forcing the liquidation of a fresh position (60 s after
/// the open) by holding the pool at ×2.5 for 7 s, then sandwiching it. ECON: push cost 33.110,
/// final sandwich + keeper +14.879, attacker net −18.231 — the borrower loses its margin but
/// the attack costs more than it takes.
#[test]
fn held_pushes_on_a_fresh_position_do_not_pay_ra() {
    let mut s = Sim::ra(RA_Q);
    let (b, nonce) = s.open(RA_Q);
    for t in 1..=60 {
        s.step_to(t);
    }
    let target = (s.spot() as f64 * 2.5) as u128;
    let net = held_attack(
        &mut s,
        &b,
        nonce,
        60,
        target,
        7,
        33_110_039_697,
        43_490_414_866_383,
        SimExit {
            b: 37_408_597_887,
            rent: SIM_RMIN,
            pen: 460_769_803,
            borrower: 0,
            b_ref: 47_860_197_318,
            max_in: 37_620_437_682,
        },
        14_879_055_141,
        138_230_940,
    );
    assert!(net < 0, "the held attack loses money: {net}");
    assert!(
        (net + 18_230_984_556).abs() < 1_000_000,
        "ECON's −18.231 SOL"
    );
}

/// `ema_near.py`, model held, RA rows: after an honest rise (×1.30 … ×1.55, EMA caught up) the
/// attacker holds the pool high until the position is liquidatable and sandwiches it. The sim
/// (RA): ×1.30 → ×1.5 for 7 s, net −8.023; ×1.40 → ×1.3 for 8 s, −7.198; ×1.50 → ×1.3 for 5 s,
/// −1.979; ×1.55 → ×1.15 for 5 s, −0.567 — never profitable on RA. AIR-22 (the sustained
/// growth's lower median) lifted the references a little (ECON, the minimum's: −7.200, −0.657).
#[test]
fn held_pushes_near_the_threshold_do_not_pay_ra() {
    #[allow(clippy::type_complexity)]
    let rows: [(f64, f64, i64, i128, u64, SimExit, i128, u64, i128); 4] = [
        (
            1.30,
            1.5,
            7,
            18_385_214_751,
            25_438_875_545_066,
            SimExit {
                b: 37_523_764_809,
                rent: SIM_RMIN,
                pen: 345_602_881,
                borrower: 0,
                b_ref: 37_453_561_978,
                max_in: 37_620_437_682,
            },
            10_362_168_547,
            103_680_864,
            -8_023_046_204,
        ),
        (
            1.40,
            1.3,
            8,
            14_504_060_708,
            17_109_933_179_287,
            SimExit {
                b: 35_111_760_112,
                rent: SIM_RMIN,
                pen: 1_726_736_614,
                borrower: 1_030_870_964,
                b_ref: 34_534_732_277,
                max_in: 35_225_426_922,
            },
            7_306_033_118,
            518_020_984,
            -7_198_027_590,
        ),
        (
            1.50,
            1.3,
            5,
            9_406_970_528,
            16_531_570_990_462,
            SimExit {
                b: 37_567_141_400,
                rent: SIM_RMIN,
                pen: 302_226_290,
                borrower: 0,
                b_ref: 37_450_569_102,
                max_in: 37_620_437_682,
            },
            7_427_752_820,
            90_667_887,
            -1_979_217_708,
        ),
        (
            1.55,
            1.15,
            5,
            5_224_020_770,
            9_952_654_682_893,
            SimExit {
                b: 34_480_349_840,
                rent: SIM_RMIN,
                pen: 1_690_455_200,
                borrower: 1_698_562_650,
                b_ref: 33_809_103_993,
                max_in: 34_485_286_072,
            },
            4_657_209_535,
            507_136_560,
            -566_811_235,
        ),
    ];
    for (k0, mult, secs, cost, dx, sim, pnl, keeper_sim, net_sim) in rows {
        let mut s = Sim::ra(RA_Q);
        let (b, nonce) = s.open(RA_Q);
        for t in 1..=60 {
            s.step_to(t);
        }
        let p = (s.spot() as f64 * k0) as u128;
        s.exo_move(p);
        for t in 61..=360 {
            s.step_to(t);
        }
        let target = (s.spot() as f64 * mult) as u128;
        let net = held_attack(
            &mut s, &b, nonce, 360, target, secs, cost, dx, sim, pnl, keeper_sim,
        );
        assert!(net < 0, "×{k0}: the held attack loses money ({net})");
        assert!(
            (net - net_sim).abs() < 1_000_000,
            "×{k0}: ECON's net {net_sim}, got {net}"
        );
    }
}

// ---- cascade.py -----------------------------------------------------------------------------

/// `cascade.run_basket` (`only = None`) / `run_single(i)` on RA: from `t0`, the path moves the
/// pool each second (other traders act first), the keeper cranks, then tries every alive
/// position (all of them, or only `i`) in ascending `cr_s`. Returns each position's exit second
/// (`t − t0`).
fn cascade_ra(
    path: &dyn Fn(i64) -> Option<f64>,
    only: Option<usize>,
    horizon: i64,
) -> Vec<Option<i64>> {
    let (mut s, poss, t0) = basket_ra();
    let p0 = s.spot();
    let k = keeper(&mut s.env);
    let mut out: Vec<Option<i64>> = vec![None; poss.len()];
    let mut alive: Vec<usize> = match only {
        Some(i) => vec![i],
        None => (0..poss.len()).collect(),
    };
    let mut t = t0;
    while t < t0 + horizon && !alive.is_empty() {
        t += 1;
        s.at(t);
        if let Some(m) = path(t - t0) {
            s.exo_move((p0 as f64 * m) as u128);
        }
        s.env.crank(&s.mm);
        let keys: Vec<_> = alive
            .iter()
            .map(|i| (poss[*i].0.key(), poss[*i].1))
            .collect();
        let vs = views(&mut s.env, &s.mm, &keys);
        let mut order: Vec<(u64, usize)> = vs
            .iter()
            .zip(alive.iter())
            .map(|(v, i)| (predict(v).cr_s(), *i))
            .collect();
        order.sort();
        for (_, i) in order {
            let (b, nonce) = &poss[i];
            if liquidate_checked(&mut s.env, &s.mm, &k, &b.key(), *nonce).executed() {
                out[i] = Some(t - t0);
                alive.retain(|j| *j != i);
            }
        }
    }
    out
}

fn gap_18(s: i64) -> Option<f64> {
    (s >= 1).then_some(1.8)
}

fn pump_2x_60s(s: i64) -> Option<f64> {
    (s >= 1).then(|| (2f64.ln() * (s.min(60) as f64) / 60.0).exp())
}

fn pump_10x_10min(s: i64) -> Option<f64> {
    (s >= 1).then(|| (10f64.ln() * (s.min(600) as f64) / 600.0).exp())
}

/// `cascade.py`, RA: an honest cascade on the OI-cap basket. ECON: every position liquidated in
/// the basket as in isolation, the largest delay of a basket exit 1 s (each exit's own buyback
/// lifts the next one's live cost against its one-second-old reference). The sim's seconds for
/// this basket, exactly: gap ×1.8 — basket 13 / 13 / 13, isolated 13 each; pump ×2/60 s —
/// basket 52 / 53 / 52, isolated 52 each; pump ×10/10 min — basket 140 / 141 / 140. Which of
/// the two equal-size positions goes first in a second follows `cr_s`, which the `R_min`
/// difference (module docs) can order the other way: the basket's seconds are compared as a
/// multiset.
#[test]
fn honest_cascade_on_the_oi_cap_basket_ra() {
    let sorted = |mut v: Vec<Option<i64>>| {
        v.sort();
        v
    };
    assert_eq!(sorted(cascade_ra(&gap_18, None, 1_800)), vec![Some(13); 3]);
    for i in 0..3 {
        assert_eq!(
            cascade_ra(&gap_18, Some(i), 1_800)[i],
            Some(13),
            "isolated {i}"
        );
    }
    assert_eq!(
        sorted(cascade_ra(&pump_2x_60s, None, 1_800)),
        vec![Some(52), Some(52), Some(53)]
    );
    for i in 0..3 {
        assert_eq!(
            cascade_ra(&pump_2x_60s, Some(i), 1_800)[i],
            Some(52),
            "isolated {i}"
        );
    }
    assert_eq!(
        sorted(cascade_ra(&pump_10x_10min, None, 1_800)),
        vec![Some(140), Some(140), Some(141)]
    );
}

// ---- fixed_delta.py (a)–(c): liveness of the fixed bound on genuine paths --------------------

/// A price path of `shortwin_sim` (`t` → multiple of the post-open spot `P1`).
type PathFn = Box<dyn Fn(i64) -> Option<f64>>;

/// `shortwin_sim.step_path`.
fn step_path(te: i64, k: f64) -> PathFn {
    Box::new(move |t| (t >= te).then_some(k))
}

/// `shortwin_sim.ramp_path`.
fn ramp_path(te: i64, k: f64, dur: i64) -> PathFn {
    let g = k.ln() / dur as f64;
    Box::new(move |t| (t >= te).then(|| (g * (t - te).min(dur) as f64).exp()))
}

/// What the sim's `run()` ended with.
#[derive(Clone, Copy, Debug)]
enum Outcome {
    /// A liquidation at `t` (the sim's result).
    Liquidation(i64, SimExit),
    /// A SOL settlement (S1/S2) at `t`: on the chain the keeper's `liquidate` is refused with
    /// `UseSolSettle` there (the settlement itself is the `sol_settle` track's).
    Settle(i64),
}

/// `shortwin_sim.keeper_near`: the in-house keeper cranks every second near a threshold.
fn keeper_near(p: &Predicted, t_left_to_deadline: i64) -> bool {
    if t_left_to_deadline <= 60 || !p.view.executable {
        return true;
    }
    let spot = u128::from(p.view.y) * W / u128::from(p.view.x);
    let ema = p.view.market.ema_price.get().max(1);
    if (spot * 10_000 / ema).abs_diff(10_000) > 200 {
        return true;
    }
    match p.b_live {
        None => true,
        Some(q) => {
            u128::from(p.view.pos.v - p.r_due) * 10_000 / u128::from(q.cost)
                < u128::from(p.view.pos.cr_liq_bps) * 13 / 10
        }
    }
}

/// `shortwin_sim.run(new_market("RA"), 1.0, Scenario(path, t_event, window, events), keeper_latency
/// = 1)` on the chain, step for step (the sim's calm steps, the keeper's latency, the drain
/// event), with every keeper attempt checked by the oracle; asserts the sim's outcome.
fn run_ra(name: &str, path: PathFn, drain_at: Option<i64>, expect: Outcome) {
    let (t_event, window) = (3_600i64, 3_600i64);
    let mut s = Sim::ra(RA_Q);
    let (b, nonce) = s.open(RA_Q);
    let p1 = s.spot();
    let v = s.env.position(&s.mm, &b.key(), nonce).v;
    let deadline = s.env.position(&s.mm, &b.key(), nonce).deadline - s.t0;
    let horizon = (t_event + window).min(deadline + 3 * 3_600);
    let k = keeper(&mut s.env);
    let (mut t, mut act_at, mut near, mut drained) = (0i64, None::<i64>, true, false);
    while t < horizon {
        let in_window = t_event - 5 <= t && t <= t_event + window;
        if in_window || act_at.is_some() || near {
            t += 1;
        } else {
            let mut nxt = t + 30;
            if t < t_event - 5 {
                nxt = nxt.min(t_event - 5);
            }
            if t < deadline - 60 {
                nxt = nxt.min(deadline - 60);
            }
            t = nxt.max(t + 1);
        }
        s.at(t);
        if let Some(td) = drain_at
            && !drained
            && t >= td
        {
            let (x, y) = s.reserves();
            ok(mock::remove_liquidity(&mut s.env, &s.mm.pool, x / 2, y / 2));
            drained = true;
        }
        if let Some(m) = path(t) {
            s.exo_move((p1 as f64 * m) as u128);
        }
        s.env.crank(&s.mm);
        let st = predict(&read_view(&s.env, &s.mm, &b.key(), nonce));
        near = keeper_near(&st, deadline - t);
        let due = st.cr_liquidatable() || st.expired;
        if act_at.is_none() && due {
            act_at = Some(t + 1);
        }
        let Some(a) = act_at else { continue };
        if t < a {
            continue;
        }
        let ex = liquidate_checked(&mut s.env, &s.mm, &k, &b.key(), nonce);
        match expect {
            Outcome::Liquidation(te, sim) => {
                if ex.executed() {
                    assert_eq!(t, te, "{name}: liquidated at the sim's second");
                    assert_sim_liquidation(&ex.liquidated().clone_event(), &sim, v);
                    return;
                }
                assert!(t < te, "{name}: not liquidated at the sim's second {te}");
            }
            Outcome::Settle(te) => {
                assert!(
                    !ex.executed(),
                    "{name}: the sim settles in SOL; the chain liquidated at {t}"
                );
                if t == te {
                    assert_eq!(ex.refused(), E::UseSolSettle, "{name}");
                    return;
                }
            }
        }
        if !due {
            act_at = None;
        }
    }
    panic!("{name}: no outcome by the horizon");
}

fn liq(t: i64, b: u64, rent: u64, pen: u64, borrower: u64, b_ref: u64, max_in: u64) -> Outcome {
    Outcome::Liquidation(
        t,
        SimExit {
            b,
            rent,
            pen,
            borrower,
            b_ref,
            max_in,
        },
    )
}

/// `fixed_delta.py` (a), RA, keeper latency 1 s, v0.4.1 (fixed 2 %): the reviewer's eight
/// genuine pump / gap paths. ECON: no cell differs from v0.3's Dutch clock. The sim's outcome of
/// every path, to the second and the lamport.
#[test]
fn fixed_bound_on_genuine_paths_ra() {
    let te = 3_600;
    run_ra(
        "pump 10x/60s",
        ramp_path(te, 10.0, 60),
        None,
        Outcome::Settle(3_664),
    );
    run_ra(
        "pump 10x/3min",
        ramp_path(te, 10.0, 180),
        None,
        liq(
            3_647,
            34_121_293_275,
            1_072_144,
            1_705_368_755,
            2_042_703_508,
            34_107_375_098,
            34_789_522_599,
        ),
    );
    run_ra(
        "pump 10x/10min",
        ramp_path(te, 10.0, 600),
        None,
        liq(
            3_735,
            31_385_624_820,
            1_105_181,
            1_568_864_943,
            4_914_842_738,
            31_377_298_851,
            32_004_844_828,
        ),
    );
    run_ra("gap 5x", step_path(te, 5.0), None, Outcome::Settle(3_647));
    run_ra(
        "gap 2x",
        step_path(te, 2.0),
        None,
        liq(
            3_609,
            37_423_608_767,
            SIM_RMIN,
            445_758_923,
            0,
            38_439_528_132,
            37_620_437_682,
        ),
    );
    run_ra(
        "pump 3x/60s",
        ramp_path(te, 3.0, 60),
        None,
        liq(
            3_636,
            36_167_560_640,
            SIM_RMIN,
            1_701_807_050,
            0,
            36_155_014_340,
            36_878_114_626,
        ),
    );
    run_ra(
        "pump 2x/20s",
        ramp_path(te, 2.0, 20),
        None,
        liq(
            3_623,
            37_423_586_770,
            SIM_RMIN,
            445_780_920,
            0,
            38_473_372_894,
            37_620_437_682,
        ),
    );
    run_ra(
        "pump 2x/5s",
        ramp_path(te, 2.0, 5),
        None,
        liq(
            3_612,
            37_423_590_561,
            SIM_RMIN,
            445_777_129,
            0,
            38_541_112_728,
            37_620_437_682,
        ),
    );
}

/// `fixed_delta.py` (b), RA, latency 1 s: gaps near the liquidation band and 30 s ramps to
/// 1.3× the gap.
#[test]
fn fixed_bound_near_the_band_ra() {
    let te = 3_600;
    run_ra(
        "gap 1.7x",
        step_path(te, 1.7),
        None,
        liq(
            3_615,
            31_783_709_402,
            SIM_RMIN,
            1_589_185_471,
            4_496_472_817,
            31_994_514_843,
            32_634_405_139,
        ),
    );
    run_ra(
        "pump 2.21x/30s",
        ramp_path(te, 1.7 * 1.3, 30),
        None,
        Outcome::Settle(3_673),
    );
    run_ra(
        "gap 1.8x",
        step_path(te, 1.8),
        None,
        liq(
            3_612,
            33_662_892_201,
            SIM_RMIN,
            1_683_144_611,
            2_523_330_878,
            34_095_970_796,
            34_777_890_211,
        ),
    );
    run_ra(
        "pump 2.34x/30s",
        ramp_path(te, 1.8 * 1.3, 30),
        None,
        Outcome::Settle(3_671),
    );
    run_ra(
        "gap 1.9x",
        step_path(te, 1.9),
        None,
        liq(
            3_610,
            35_542_865_500,
            SIM_RMIN,
            1_777_143_275,
            549_358_915,
            36_278_900_966,
            37_004_478_985,
        ),
    );
    run_ra(
        "pump 2.47x/30s",
        ramp_path(te, 1.9 * 1.3, 30),
        None,
        Outcome::Settle(3_669),
    );
    run_ra("gap 2.1x", step_path(te, 2.1), None, Outcome::Settle(3_654));
    run_ra(
        "pump 2.73x/30s",
        ramp_path(te, 2.1 * 1.3, 30),
        None,
        Outcome::Settle(3_667),
    );
}

/// `fixed_delta.py` (c), RA: a 50 % LP drain (all unlocked LP at λ_min) then a pump. ECON:
/// liquidated at ×1.62 / ×1.69 / ×1.63 with the lenders up — a fixed 1 % bound would turn the
/// second into S1; the fixed 2 % liquidates. The mock has no locked depth, so the program caps no
/// reference here: the figures are the sim's with `Market.ref_depth_cap` lifted — exactly its
/// output while its Raydium cap was `2D`, the pool before the drain, which never bound. Since
/// AIR-14 the sim's RA (a λ 50 % pool) caps the reference at `D`, the drained pool: the same
/// seconds and prices, `B̂_ref` 0.9–1.0 % higher, the borrower's remainder 0.014–0.015 SOL lower.
#[test]
fn fixed_bound_after_a_drain_ra() {
    let te = 3_600;
    run_ra(
        "drain 50 % + 2x/600s",
        ramp_path(te + 60, 2.0, 600),
        Some(te),
        liq(
            4_079,
            30_623_388_452,
            1_228_833,
            1_516_978_221,
            5_728_842_176,
            30_339_564_420,
            30_946_355_708,
        ),
    );
    run_ra(
        "drain 50 % + 2x/120s",
        ramp_path(te + 60, 2.0, 120),
        Some(te),
        liq(
            3_751,
            31_936_950_264,
            1_106_022,
            1_580_715_624,
            4_351_665_772,
            31_614_312_478,
            32_246_598_727,
        ),
    );
    run_ra(
        "drain 50 % + 3x/600s",
        ramp_path(te + 60, 3.0, 600),
        Some(te),
        liq(
            3_928,
            30_830_206_602,
            1_172_314,
            1_527_083_864,
            5_511_974_902,
            30_541_677_279,
            31_152_510_824,
        ),
    );
}

// ---- trait helper ---------------------------------------------------------------------------

trait CloneEvent {
    fn clone_event(&self) -> Self;
}

impl CloneEvent for Liquidate {
    fn clone_event(&self) -> Self {
        let mut bytes = Vec::new();
        anchor_lang::AnchorSerialize::serialize(self, &mut bytes).unwrap();
        anchor_lang::AnchorDeserialize::deserialize(&mut bytes.as_slice()).unwrap()
    }
}
