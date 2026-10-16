//! Shared scenario machinery of the `sol_settle` test binaries (`settle_it`, `route_it`,
//! `extras_it`, `attacks_settle_it`; the settle-o6 track of M2). Each of them includes this
//! file as `mod support` (`#[path = "settle_support.rs"]`); compiled on its own it is an empty
//! test binary (no tests of its own).
//!
//! - [`Sc`]: one mock market with one borrower's position, a keeper, and a whale that trades
//!   the pool (exogenous moves and attacker bundles), over the harness's real instructions
//!   (`Env::send`: the invariant hooks run after every instruction).
//! - Keeper seconds ([`Sc::second`]: one `crank_price` and one `sol_settle` per second, the
//!   in-house keeper of ECON §5.8 duty 4), attacker seconds ([`Sc::attacker_only_second`],
//!   [`Sc::frozen_second`]) and bundles ([`Sc::bundle_ixs`]) in the attack scripts' models
//!   (`docs/design/sim/attacks/common.py`: the atomic `[push → crank → trade back]`, the
//!   attacker-only second, the push-then-freeze second).
//! - [`Sc::s1_boundary`]: the spot multiple at which the S1 live leg starts to hold (the
//!   attack scripts' `s1_boundary`).

#![allow(dead_code, clippy::result_large_err)]

#[allow(unused_imports)]
#[path = "engine_it/common.rs"]
pub mod common;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use litesvm::types::TransactionMetadata;
use shortwin::events::{EpochRolled, SettleArmed, SettleDisarmed, SolSettle};
use shortwin::state::{Market, ShortPosition};
use shortwin_litesvm_tests::drive::{Actor, swap_in_ix};
use shortwin_litesvm_tests::engine::sol_settle_ix;
use shortwin_math::num::WAD;
use shortwin_math::quote::cpmm;
use shortwin_math::settle::ArmState;
use solana_keypair::Keypair;
use solana_signer::Signer;

pub use common::*;

/// The mock's fee (25 bps, the Raydium CPMM example of ECON's `RC`).
pub const FEE_PPM: u32 = 2_500;

/// An `RC`-like pool (ECON §11 markets: a CPMM pool of a fresh pump graduate, 206.9 M tokens
/// against 84.99 SOL, 25 bps fee, Tier C): what the mock can reproduce of ECON's market set
/// (the mock is a CPMM with the fee on the input; a new listing is Tier C by age, as `RC`).
pub fn rc_spec() -> MarketSpec {
    MarketSpec {
        fee_ppm: FEE_PPM,
        base: 206_900_000_000_000,
        quote: 84_990_000_000,
        lend: 2_000_000_000_000,
        ..MarketSpec::default()
    }
}

/// A position size inside every cap of an [`rc_spec`] market for one wallet (the wallet cap
/// binds: 25 % of the per-market cap): 0.4 % of the pool's tokens, ≈ 0.34 SOL of notional.
pub const RC_Q: u64 = 827_600_000_000;

/// What one `sol_settle` did.
pub struct Outcome {
    pub settled: Option<SolSettle>,
    pub armed: Option<SettleArmed>,
    pub disarmed: Option<SettleDisarmed>,
    pub rolled: Option<EpochRolled>,
    /// Instruction-trace entries of the transaction's first instruction (ARCH §6.2).
    pub trace: usize,
    /// Compute units of the transaction.
    pub cu: u64,
}

impl Outcome {
    /// Decodes the events of a successful `sol_settle` transaction (or a bundle containing
    /// one).
    pub fn of(meta: &TransactionMetadata) -> Self {
        Self {
            settled: events::of::<SolSettle>(meta).pop(),
            armed: events::of::<SettleArmed>(meta).pop(),
            disarmed: events::of::<SettleDisarmed>(meta).pop(),
            rolled: events::of::<EpochRolled>(meta).pop(),
            trace: trace_entries(meta, 0),
            cu: meta.compute_units_consumed,
        }
    }
}

/// One mock market with one open position (module docs).
pub struct Sc {
    pub env: Env,
    pub mm: MockMarket,
    pub b: Borrower,
    pub nonce: u64,
    pub q: u64,
    pub keeper: Keypair,
    /// Trades the pool: exogenous moves, pushes, sandwiches.
    pub whale: Actor,
}

impl Sc {
    /// A test-venue environment, a mock market per `spec`, one position of `q`, a keeper and a
    /// whale holding ten times the pool on each side.
    pub fn new(spec: MarketSpec, q: u64) -> Self {
        let (mut env, mm) = setup_with(spec);
        let mut b = env.borrower();
        let (_open, nonce, _) = open_ok(&mut env, &mm, &mut b, q);
        let keeper = env.funded_keypair();
        let whale = env.actor(
            &mm,
            spec.base.saturating_mul(10),
            spec.quote.saturating_mul(10),
        );
        Self {
            env,
            mm,
            b,
            nonce,
            q,
            keeper,
            whale,
        }
    }

    /// The default mock market (1,000 SOL deep) with a [`Q`] position.
    pub fn default_market() -> Self {
        Self::new(MarketSpec::default(), Q)
    }

    /// The [`rc_spec`] market with an [`RC_Q`] position.
    pub fn rc() -> Self {
        Self::new(rc_spec(), RC_Q)
    }

    /// The position's key.
    pub fn position_key(&self) -> Pubkey {
        pda::position(&self.mm.market(), &self.b.key(), self.nonce).0
    }

    /// The position, if it is still open.
    pub fn pos_opt(&self) -> Option<ShortPosition> {
        read::borsh(&self.env.svm, &self.position_key())
    }

    /// The position (panics once it is closed).
    pub fn pos(&self) -> ShortPosition {
        self.pos_opt().expect("the position is closed")
    }

    /// The position's arming clock.
    pub fn arm(&self) -> ArmState {
        self.pos().arm_state()
    }

    /// The market.
    pub fn market(&self) -> Market {
        self.env.market(&self.mm.market())
    }

    /// `sol_settle` by the keeper.
    pub fn settle_ix(&self) -> Instruction {
        sol_settle_ix(
            &self.env,
            &self.mm,
            &self.keeper.pubkey(),
            &self.b.key(),
            self.nonce,
        )
    }

    /// Sends `sol_settle` by the keeper now.
    pub fn settle(&mut self) -> TxResult {
        let ix = self.settle_ix();
        let kp = self.keeper.insecure_clone();
        self.env.send(&[ix], &[&kp])
    }

    /// `sol_settle` that must succeed; its outcome.
    pub fn settle_ok(&mut self) -> Outcome {
        Outcome::of(&ok(self.settle()))
    }

    /// `crank_price` now.
    pub fn crank(&mut self) {
        let mm = &self.mm;
        let ix = crank_price_ix(mm);
        ok(self.env.send(&[ix], &[]));
    }

    /// One keeper second: the clock advances by 1, then one `crank_price` (the observation of
    /// the second) and one `sol_settle` (ECON §5.8 duties 1 and 4; the sims' `step_to(t)`
    /// then `try_exit(t, settle_only = True)`).
    pub fn second(&mut self) -> Outcome {
        self.env.warp(1);
        self.crank();
        self.settle_ok()
    }

    /// Keeper seconds until a settlement executes or `max_s` seconds pass. Returns the second
    /// (counted from 1) and the settlement.
    pub fn run_until_settled(&mut self, max_s: u32) -> Option<(u32, SolSettle)> {
        for s in 1..=max_s {
            if let Some(ev) = self.second().settled {
                return Some((s, ev));
            }
        }
        None
    }

    /// Keeper seconds until a settlement executes or `max_s` seconds pass: the second and the
    /// whole outcome (with the `EpochRolled` event if the settlement rolled the epoch).
    pub fn run_until_settled_out(&mut self, max_s: u32) -> Option<(u32, Outcome)> {
        for s in 1..=max_s {
            let out = self.second();
            if out.settled.is_some() {
                return Some((s, out));
            }
        }
        None
    }

    /// A genuine gap to `beyond` × the S1 boundary (an exogenous move), then keeper seconds
    /// until the clock arms. Returns the second it armed (counted from the gap).
    pub fn gap_and_arm(&mut self, beyond: f64) -> u32 {
        let kb = self.s1_boundary();
        self.move_by(kb * beyond);
        for s in 1..=60 {
            if self.second().armed.is_some() {
                return s;
            }
        }
        panic!("never armed");
    }

    /// The pool's spot price (WAD).
    pub fn spot(&self) -> u128 {
        self.env.spot(&self.mm)
    }

    /// An exogenous trade by the whale that moves the pool to about `target` (WAD): other
    /// market participants (the sims' `exo_move`).
    pub fn move_to(&mut self, target: u128) {
        let (whale, mm) = (&self.whale, &self.mm);
        let ix = self
            .env
            .push_price_ix(mm, whale, target)
            .map(|(_, _, ix)| ix);
        if let Some(ix) = ix {
            let kp = whale.kp.insecure_clone();
            ok(self.env.send(&[ix], &[&kp]));
        }
    }

    /// Moves the pool by `mult` (a factor on the current spot).
    pub fn move_by(&mut self, mult: f64) {
        let target = scale(self.spot(), mult);
        self.move_to(target);
    }

    /// The instructions of `[push the pool to target → middle → trade back]` by the whale
    /// (the sims' `attacker_bundle` with `middle = crank_price`; the flip bundle with `middle =
    /// sol_settle`). The push is an exact-in swap to about `target`; the trade back returns the
    /// pool's token reserve to where it was, as the sims' `buy(d)` / `sell(d)` pair does: a
    /// push up (tokens bought) sells the tokens it bought, a push down (tokens sold) buys the
    /// same tokens back exact-out. `None` if the pool is already there.
    pub fn bundle_ixs(&self, target: u128, middle: Vec<Instruction>) -> Option<Vec<Instruction>> {
        let (dir, amount, _) = self.env.push_price_ix(&self.mm, &self.whale, target)?;
        let s = self.env.sandwich_ixs(&self.mm, &self.whale, dir, amount);
        let back = match dir {
            Dir::QuoteIn => s.back,
            Dir::BaseIn => {
                let leg = mock::Leg {
                    dir: Dir::QuoteIn,
                    authority: self.whale.key(),
                    source: self.whale.wsol,
                    destination: self.whale.base,
                };
                let max_in = tokens(&self.env, &self.whale.wsol);
                mock::swap_exact_out_ix(self.mm.mock(), &leg, max_in, amount, &[])
            }
        };
        let mut ixs = vec![s.front];
        ixs.extend(middle);
        ixs.push(back);
        Some(ixs)
    }

    /// Sends a whale bundle (with the keeper co-signing when it includes `sol_settle`).
    pub fn send_bundle(&mut self, ixs: &[Instruction]) -> TxResult {
        let (w, k) = (self.whale.kp.insecure_clone(), self.keeper.insecure_clone());
        self.env.send(ixs, &[&w, &k])
    }

    /// The atomic `[push to target → crank_price → trade back]` in the current second (the
    /// sims' `attacker_bundle`): the observation it leaves is `target`'s. Returns the whale's
    /// SOL cost (round-trip fees; no arbitrage).
    pub fn attacker_bundle(&mut self, target: u128) -> i128 {
        let crank = crank_price_ix(&self.mm);
        let before = self.whale_sol();
        let ixs = self
            .bundle_ixs(target, vec![crank.clone()])
            .unwrap_or(vec![crank]);
        ok(self.send_bundle(&ixs));
        before - self.whale_sol()
    }

    /// An attacker-only second: the clock advances and the whale's bundle to `target` is the
    /// second's only observation (no honest crank; the sims' `push`, degraded mode). Returns
    /// the cost.
    pub fn attacker_only_second(&mut self, target: u128) -> i128 {
        self.env.warp(1);
        self.attacker_bundle(target)
    }

    /// A frozen second (R4B-2): the honest keeper observes the true price, and the whale adds an
    /// observation at `target` (just beyond `P_ema`, so the bucket straddles it). Returns the
    /// cost.
    pub fn frozen_second(&mut self, target: u128) -> i128 {
        self.env.warp(1);
        self.crank();
        self.attacker_bundle(target)
    }

    /// The whale's wrapped SOL.
    pub fn whale_sol(&self) -> i128 {
        i128::from(tokens(&self.env, &self.whale.wsol))
    }

    /// The spot multiple at which the S1 live leg starts to hold for the position: `B̂(q) + ert
    /// > V − K_s` on the pool moved to `spot·m` (constant product; the attack scripts'
    /// `s1_boundary`), by bisection on `m ∈ [1, 20]`.
    pub fn s1_boundary(&self) -> f64 {
        let p = self.pos();
        let k_s = k_s(&p);
        let room = u128::from(p.v - k_s);
        let r = mock::reserves(&self.env.svm, self.mm.mock());
        let (x, y) = (r.base as f64, r.quote as f64);
        let fees = mock::cpmm_fees(FEE_PPM);
        let cost = |m: f64| -> Option<u128> {
            let x1 = (x / m.sqrt()).floor();
            let y1 = (y * m.sqrt()).ceil();
            cpmm::buy_exact_out(p.q, y1 as u128, x1 as u128, &fees).map(|b| u128::from(b.cost))
        };
        let (mut lo, mut hi) = (1.0f64, 20.0f64);
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            match cost(mid) {
                Some(c) if c + u128::from(p.ert) <= room => lo = mid,
                _ => hi = mid,
            }
        }
        hi
    }

    /// The pool admin pulls `bps`/10⁴ of both pricing reserves out of the pool (an LP drain;
    /// the attack scripts' `remove_liquidity(bps, force = True)` model case, ECON §8.6).
    pub fn drain(&mut self, bps: u64) {
        let r = mock::reserves(&self.env.svm, self.mm.mock());
        let base = (u128::from(r.base) * u128::from(bps) / 10_000) as u64;
        let quote = (u128::from(r.quote) * u128::from(bps) / 10_000) as u64;
        ok(mock::remove_liquidity(
            &mut self.env,
            &self.mm.pool,
            base,
            quote,
        ));
    }

    /// The Emergency multisig sets or clears the market's route-broken flag (O6, A40).
    pub fn set_route_broken(&mut self, broken: bool) {
        let ea = self.env.pause_authority.insecure_clone();
        let ix = shortwin_litesvm_tests::engine::set_route_broken_ix(
            &ea.pubkey(),
            shortwin::PauseScope::Market,
            0,
            broken,
            Some(self.mm.market()),
        );
        ok(self.env.send(&[ix], &[&ea]));
    }

    /// The Emergency multisig sets or clears the mock venue's bit of `venue_route_broken`.
    pub fn set_venue_route_broken(&mut self, broken: bool) {
        let ea = self.env.pause_authority.insecure_clone();
        let bit = shortwin::constants::VENUE_BIT_MOCK.trailing_zeros() as u8;
        let ix = shortwin_litesvm_tests::engine::set_route_broken_ix(
            &ea.pubkey(),
            shortwin::PauseScope::Global,
            bit,
            broken,
            None,
        );
        ok(self.env.send(&[ix], &[&ea]));
    }

    /// Changes the mock pool's knobs (its admin signs).
    pub fn knobs(&mut self, f: impl FnOnce(&mut mock::Knobs)) {
        mock::update_knobs(&mut self.env, &self.mm.pool, f);
    }

    /// `n` cranks `step` seconds apart (the keeper's ≤ 30 s duty on a no-route market, ECON
    /// §5.8 duty 5).
    pub fn crank_every(&mut self, step: i64, n: u32) {
        for _ in 0..n {
            self.env.warp(step);
            self.crank();
        }
    }

    /// Warps to `deadline + after` (cranking once there).
    pub fn warp_to_deadline(&mut self, after: i64) {
        let to = self.pos().deadline + after;
        let now = self.env.now();
        if to > now {
            self.env.warp(to - now);
        }
        self.crank();
    }

    /// The position's `V − K_s` (its S1 take).
    pub fn take(&self) -> u64 {
        let p = self.pos();
        p.v - k_s(&p)
    }
}

/// `fv_push.py`'s `ready_model` on an [`rc_spec`] market: 60 s of honest cranks after the open,
/// `drain_bps` of the LP pulled, 600 s, an exogenous move by `mult`, 120 s; then keeper
/// seconds until the first one that would settle. Returns the state just before that second
/// and the settlement it would execute (the honest outcome).
pub fn fv_model_ready(drain_bps: u64, mult: f64) -> (Sc, SolSettle) {
    let mut sc = Sc::rc();
    sc.env.tick(&sc.mm, 60);
    sc.drain(drain_bps);
    sc.env.tick(&sc.mm, 600);
    sc.move_by(mult);
    sc.env.tick(&sc.mm, 120);
    for _ in 0..300 {
        let saved = sc.env.svm.clone();
        if let Some(ev) = sc.second().settled {
            sc.env.svm = saved;
            return (sc, ev);
        }
    }
    panic!("no settlement");
}

/// The flip bundle `[pump live to mult × spot → sol_settle → dump]` in the next second (the
/// sims' `attack_flip` tail): its settlement, if any.
pub fn flip_bundle(sc: &mut Sc, mult: f64) -> Option<SolSettle> {
    sc.env.warp(1);
    let settle = sc.settle_ix();
    let ixs = sc
        .bundle_ixs(scale(sc.spot(), mult), vec![settle])
        .expect("a pump");
    Outcome::of(&ok(sc.send_bundle(&ixs))).settled
}

/// One attacker-only second at `mult` × spot (the sims' `push`).
pub fn push(sc: &mut Sc, mult: f64) {
    let target = scale(sc.spot(), mult);
    sc.attacker_only_second(target);
}

/// `⌊p·m⌋` for a price `p` (WAD) and a factor `m`.
pub fn scale(p: u128, m: f64) -> u128 {
    ((p as f64) * m) as u128
}

/// `K_s = min(fixed + ⌊V·bps/10⁴⌋, cap, V)` with the position's locked terms.
pub fn k_s(p: &ShortPosition) -> u64 {
    let pct = u128::from(p.v) * u128::from(p.settle_keeper_bps) / 10_000;
    (u128::from(p.settle_keeper_fixed) + pct)
        .min(u128::from(p.settle_keeper_cap))
        .min(u128::from(p.v)) as u64
}

/// A price in SOL per whole token of 6 decimals, for messages.
pub fn price_f(p: u128) -> f64 {
    p as f64 / WAD as f64 * 1e6 / 1e9
}

/// A whale swap of `amount_in` in `dir` (no bound).
pub fn whale_swap(sc: &Sc, dir: Dir, amount_in: u64) -> Instruction {
    swap_in_ix(&sc.mm, &sc.whale, dir, amount_in, 0)
}
