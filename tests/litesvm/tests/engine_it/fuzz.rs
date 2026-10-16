//! A seeded random walk over the engine on the mock (ARCH §8.2 "invariants after every
//! instruction"; §8.4's stateful fuzzing in miniature): opens, closes and margin top-ups by
//! several borrowers, arbitrage trades that move the price both ways and price shocks, lender
//! deposits and withdrawals, cranks and time steps up to and past deadlines, a keeper that tries
//! `liquidate`, `expire` and `sol_settle` on a random position (or, after shocks and deadline
//! jumps, on every position every second, as the in-house keeper does), and I10's fuzz action ("pause
//! and flag everything"): for a few actions, opens paused, the Emergency route-broken flag set
//! and the venue disabled, in any combination — with every M1 and M2 invariant hook running
//! after each instruction. Opens may be refused (caps, deviation, notional; always under a
//! pause, the flag or a halted venue), closes may be (the vault cannot pay after a pump; no
//! route), and keeper exits are refused unless their conditions hold: those are ordinary
//! outcomes; an invariant violation panics. The walk ends by closing every position that is
//! left (pushing the price back if needed) and checks that the market returns to no position,
//! no receivable and no rent aggregate; over all walks, keepers must have exited positions.
//!
//! `ENGINE_FUZZ_SEEDS` (default 4) and `ENGINE_FUZZ_STEPS` (default 120) scale it.

use shortwin::PauseScope;
use shortwin::errors::ShortwinError as E;
use shortwin::events::{Expire, Liquidate, SolSettle};
use shortwin_litesvm_tests::drive::Actor;
use shortwin_litesvm_tests::engine::{
    add_margin_ix, expire_ix, liquidate_ix, set_route_broken_ix, sol_settle_ix,
};
use shortwin_litesvm_tests::ix;
use shortwin_litesvm_tests::mock::{Knobs, update_knobs};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::common::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn is_one_of(code: Option<u32>, allowed: &[E]) -> bool {
    code.is_some_and(|c| allowed.iter().any(|e| u32::from(*e) == c))
}

/// Program codes a random open, close or withdrawal may legitimately end with.
const REFUSALS: [E; 14] = [
    E::CapPositionImpact,
    E::CapMarketOi,
    E::CapUtilization,
    E::CapWallet,
    E::BelowMinNotional,
    E::MarketNotionalCap,
    E::PriceDeviatesFromEma,
    E::PriceAboveOpenMax,
    E::InsufficientVault,
    E::ProceedsBelowMin,
    E::InsufficientFreeInventory,
    E::AboveProRataAllowance,
    E::LastSharesBorrowed,
    E::NoRoute,
];

/// Program codes a keeper's `liquidate` / `expire` may legitimately end with.
const EXIT_REFUSALS: [E; 8] = [
    E::NotLiquidatable,
    E::NotExpired,
    E::ReferenceStale,
    E::UseSolSettle,
    E::UseExpire,
    E::UseLiquidate,
    E::ExitBoundNotMet,
    E::NoRoute,
];

/// The gates of I10's fuzz action.
#[derive(Clone, Copy, Default, Debug)]
struct Gates {
    paused: bool,
    flagged: bool,
    disabled: bool,
}

struct Walk {
    env: Env,
    mm: MockMarket,
    rng: Rng,
    borrowers: Vec<Borrower>,
    open: Vec<(usize, u64)>,
    arb: Actor,
    shocker: Actor,
    lender: Keypair,
    keeper: Keypair,
    emergency: Keypair,
    opened: u32,
    closed: u32,
    exited: u32,
    /// Executed keeper exits: liquidations, expiries, settlements (S1, S2, S3).
    kinds: [u32; 5],
}

impl Walk {
    fn new(seed: u64, program: TokenProgram) -> Self {
        let (mut env, mm) = setup_with(spec_on(program));
        let borrowers: Vec<Borrower> = (0..4).map(|_| env.borrower()).collect();
        let arb = env.actor(&mm, 200_000_000_000_000, 300_000_000_000);
        // Price shocks: enough WSOL to move the 1,000-SOL pool several-fold.
        let shocker = env.actor(&mm, 400_000_000_000_000, 5_000_000_000_000);
        let lender = mm.lender.insecure_clone();
        let keeper = env.funded_keypair();
        let emergency = env.pause_authority.insecure_clone();
        Self {
            env,
            mm,
            rng: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1),
            borrowers,
            open: Vec::new(),
            arb,
            shocker,
            lender,
            keeper,
            emergency,
            opened: 0,
            closed: 0,
            exited: 0,
            kinds: [0; 5],
        }
    }

    fn pick(&mut self) -> Option<usize> {
        (!self.open.is_empty()).then(|| self.rng.below(self.open.len() as u64) as usize)
    }

    fn try_open(&mut self, gates: Gates) {
        let i = self.rng.below(self.borrowers.len() as u64) as usize;
        let q = 200_000_000_000 + self.rng.below(4_000_000_000_000);
        let (res, nonce) = self.env.open(&self.mm, &mut self.borrowers[i], q);
        let got = custom_code(&res);
        match res {
            Ok(_) => {
                assert!(
                    !(gates.paused || gates.flagged || gates.disabled),
                    "an open was accepted through {gates:?}"
                );
                self.open.push((i, nonce));
                self.opened += 1;
            }
            Err(_) if gates.paused => assert_eq!(got, Some(code(E::OpensPaused))),
            Err(_) if gates.flagged => assert_eq!(got, Some(code(E::RouteBroken))),
            Err(_) => assert!(is_one_of(got, &REFUSALS), "open: {got:?}"),
        }
    }

    fn try_close(&mut self) {
        let Some(k) = self.pick() else { return };
        let (i, nonce) = self.open[k];
        let res = self.env.close(&self.mm, &self.borrowers[i], nonce);
        match res {
            Ok(_) => {
                self.open.swap_remove(k);
                self.closed += 1;
            }
            Err(_) => assert!(
                is_one_of(custom_code(&res), &REFUSALS),
                "close: {:?}",
                custom_code(&res)
            ),
        }
    }

    fn add_margin(&mut self) {
        let Some(k) = self.pick() else { return };
        let (i, nonce) = self.open[k];
        let amount = 1_000_000 + self.rng.below(1_000_000_000);
        let b = &self.borrowers[i];
        let ix = add_margin_ix(&self.mm, &b.key(), nonce, amount);
        let kp = b.kp.insecure_clone();
        // No gate refuses a top-up (ARCH §3.5).
        ok(self.env.send(&[ix], &[&kp]));
    }

    /// A keeper tries every exit on one position: `liquidate`, `expire`, then `sol_settle` (an
    /// `Ok` no-op unless it arms, disarms or settles).
    fn keeper_exits(&mut self) {
        if let Some(k) = self.pick() {
            self.keeper_exit_at(k);
        }
    }

    /// [`Walk::keeper_exits`] on position `k`; `true` if it exited.
    fn keeper_exit_at(&mut self, k: usize) -> bool {
        let (i, nonce) = self.open[k];
        let b = self.borrowers[i].key();
        let keeper = self.keeper.insecure_clone();
        let mut gone = false;
        for which in 0..2 {
            let ix = if which == 0 {
                liquidate_ix(&self.mm, &keeper.pubkey(), &b, nonce)
            } else {
                expire_ix(&self.mm, &keeper.pubkey(), &b, nonce)
            };
            let res = self.env.send(&[ix], &[&keeper]);
            match &res {
                Ok(meta) => {
                    let (l, e) = (
                        events::of::<Liquidate>(meta).len(),
                        events::of::<Expire>(meta).len(),
                    );
                    gone = l + e == 1;
                    assert!(gone, "an executed keeper exit emits its event");
                    self.kinds[which] += 1;
                    break;
                }
                Err(_) => assert!(
                    is_one_of(custom_code(&res), &EXIT_REFUSALS),
                    "keeper exit {which}: {:?}",
                    custom_code(&res)
                ),
            }
        }
        if !gone {
            let ix = sol_settle_ix(&self.env, &self.mm, &keeper.pubkey(), &b, nonce);
            let meta = ok(self.env.send(&[ix], &[&keeper]));
            let settled = events::of::<SolSettle>(&meta);
            gone = settled.len() == 1;
            if let Some(ev) = settled.first() {
                self.kinds[1 + usize::from(ev.condition)] += 1;
            }
        }
        if gone {
            self.open.swap_remove(k);
            self.exited += 1;
        }
        gone
    }

    fn trade(&mut self) {
        let dir = if self.rng.below(2) == 0 {
            Dir::BaseIn
        } else {
            Dir::QuoteIn
        };
        let amount = match dir {
            Dir::BaseIn => 1_000_000_000 + self.rng.below(20_000_000_000_000),
            Dir::QuoteIn => 1_000_000 + self.rng.below(20_000_000_000),
        };
        // A trade the arbitrageur cannot fund simply fails.
        let _ = self.env.trade(&self.mm, &self.arb, dir, amount);
    }

    /// A price shock by ×1.2 … ×3 (up two times in three), then keeper seconds.
    fn shock(&mut self) {
        let r = mock::reserves(&self.env.svm, self.mm.mock());
        let spot = u128::from(r.quote) * shortwin_math::num::WAD / u128::from(r.base);
        let m = 1.2 + self.rng.below(180) as f64 / 100.0;
        let target = if self.rng.below(3) != 0 {
            (spot as f64 * m) as u128
        } else {
            (spot as f64 / m) as u128
        };
        self.env.warp(1);
        // A push the shocker cannot fund simply fails.
        if let Some((_, _, push)) = self.env.push_price_ix(&self.mm, &self.shocker, target) {
            let kp = self.shocker.kp.insecure_clone();
            let _ = self.env.send(&[push], &[&kp]);
        }
        let secs = 1 + self.rng.below(40) as u32;
        if self.rng.below(2) == 0 {
            self.env.tick(&self.mm, secs);
        } else {
            self.keeper_seconds(secs);
        }
    }

    /// The in-house keeper for `secs` seconds (ARCH §7.1 duties 1, 3, 4): each second one
    /// `crank_price`, then every exit on every open position (a settlement clock needs one
    /// evaluation per second to persist).
    fn keeper_seconds(&mut self, secs: u32) {
        for _ in 0..secs {
            self.env.warp(1);
            self.env.crank(&self.mm);
            let mut k = 0;
            while k < self.open.len() {
                if !self.keeper_exit_at(k) {
                    k += 1;
                }
            }
        }
    }

    fn lender_step(&mut self) {
        let lender = self.lender.insecure_clone();
        if self.rng.below(2) == 0 {
            let amount = 1_000_000 + self.rng.below(1_000_000_000_000);
            let payer = self.env.payer.insecure_clone();
            shortwin_litesvm_tests::tokens::mint_to(
                &mut self.env.svm,
                &payer,
                &self.mm.mint(),
                &self.mm.lender_account,
                &self.mm.pool.mint_authority,
                amount,
                self.mm.spec.program,
            );
            let ix = ix::deposit(&self.mm.lender_keys(), amount, 0);
            ok(self.env.send(&[ix], &[&lender]));
        } else if let Some(l) = self.env.lender(&self.mm.market(), &lender.pubkey()) {
            let shares = l.shares / (2 + u128::from(self.rng.below(20)));
            let ix = ix::withdraw(&self.mm.lender_keys(), shares, 0);
            let res = self.env.send(&[ix], &[&lender]);
            if res.is_err() {
                assert!(
                    is_one_of(custom_code(&res), &REFUSALS),
                    "withdraw: {:?}",
                    custom_code(&res)
                );
            }
        }
    }

    fn set_gates(&mut self, g: Gates) {
        let emergency = self.emergency.insecure_clone();
        let market = Some(self.mm.market());
        ok(self.env.send(
            &[
                ix::set_pause(&emergency.pubkey(), PauseScope::Market, g.paused, market),
                set_route_broken_ix(
                    &emergency.pubkey(),
                    PauseScope::Market,
                    0,
                    g.flagged,
                    market,
                ),
            ],
            &[&emergency],
        ));
        update_knobs(&mut self.env, &self.mm.pool, |k: &mut Knobs| {
            k.disabled = g.disabled;
        });
    }

    /// I10's fuzz action: pause opens, flag the route and / or disable the venue, run a few
    /// actions under those gates (opens must be refused; closes, top-ups and keeper exits
    /// ignore the pause and the flag), then clear them.
    fn gated_actions(&mut self) {
        let bits = 1 + self.rng.below(7);
        let g = Gates {
            paused: bits & 1 != 0,
            flagged: bits & 2 != 0,
            disabled: bits & 4 != 0,
        };
        self.set_gates(g);
        for _ in 0..1 + self.rng.below(4) {
            match self.rng.below(5) {
                0 => self.try_open(g),
                1 => self.try_close(),
                2 => self.add_margin(),
                3 => self.keeper_exits(),
                _ => {
                    let secs = 1 + self.rng.below(30) as u32;
                    self.env.tick(&self.mm, secs);
                }
            }
        }
        self.set_gates(Gates::default());
    }

    fn step(&mut self) {
        match self.rng.below(20) {
            0..=4 => self.try_open(Gates::default()),
            5..=6 => self.try_close(),
            7 => self.trade(),
            8 => self.lender_step(),
            9 | 18..=19 => {
                let secs = 1 + self.rng.below(3) as u32;
                self.env.tick(&self.mm, secs);
            }
            10 => {
                self.env.warp(1 + self.rng.below(600) as i64);
                self.env.crank(&self.mm);
            }
            11 => self.add_margin(),
            12..=14 => self.keeper_exits(),
            15 => self.shock(),
            16 => self.gated_actions(),
            // Toward and past deadlines: a long jump, then the keeper's seconds.
            _ => {
                self.env.warp(3_600 + self.rng.below(4 * 86_400) as i64);
                let secs = 1 + self.rng.below(10) as u32;
                self.keeper_seconds(secs);
            }
        }
    }

    /// Restores the price if a pump made closes unaffordable, then closes every position; one
    /// whose vault can no longer pay its buyback and rent (rent accrued over the long jumps)
    /// is exited by the keeper instead, which may take a few seconds (fresh reference, the
    /// EMA legs, the settlement clock).
    fn wind_down(&mut self) {
        let spec = self.mm.spec;
        let target = u128::from(spec.quote) * shortwin_math::num::WAD / u128::from(spec.base);
        let fixer = self.env.actor(&self.mm, 10 * spec.base, 10 * spec.quote);
        self.env.warp(1);
        self.env.push_price(&self.mm, &fixer, target);
        self.env.tick(&self.mm, 12);
        for _ in 0..600 {
            if self.open.is_empty() {
                break;
            }
            let mut k = 0;
            while k < self.open.len() {
                let (i, nonce) = self.open[k];
                let res = self.env.close(&self.mm, &self.borrowers[i], nonce);
                if res.is_ok() {
                    self.open.swap_remove(k);
                    self.closed += 1;
                    continue;
                }
                assert_eq!(custom_code(&res), Some(code(E::InsufficientVault)));
                if !self.keeper_exit_at(k) {
                    k += 1;
                }
            }
            self.env.tick(&self.mm, 1);
        }
        assert!(self.open.is_empty(), "positions left after the wind-down");
        let m = self.env.market(&self.mm.market());
        assert_eq!(
            (m.open_positions, m.borrowed_q, m.total_notional_open),
            (0, 0, 0)
        );
        assert_eq!(m.receivable, 0);
        assert!(m.rent_aggr.iter().all(|a| a.get() == 0));
        assert!(tokens(&self.env, &self.mm.keys.pool_vault) >= m.tokens_free);
        assert_eq!(self.opened, self.closed + self.exited);
    }
}

fn walk(seed: u64, steps: u64, program: TokenProgram) -> u32 {
    let mut w = Walk::new(seed, program);
    for _ in 0..steps {
        w.step();
    }
    w.wind_down();
    let k = w.kinds;
    println!(
        "seed {seed} ({program:?}): {} positions opened, {} closed by their borrowers, {} exited by a keeper (liquidate {}, expire {}, S1 {}, S2 {}, S3 {}) over {steps} steps",
        w.opened, w.closed, w.exited, k[0], k[1], k[2], k[3], k[4]
    );
    assert!(w.opened > 0, "the walk opened nothing");
    w.exited
}

#[test]
fn random_walks_keep_every_invariant() {
    let seeds = env_num("ENGINE_FUZZ_SEEDS", 4);
    let steps = env_num("ENGINE_FUZZ_STEPS", 120);
    let mut exits = 0;
    for seed in 0..seeds {
        let program = if seed % 2 == 0 {
            TokenProgram::Spl
        } else {
            TokenProgram::Token2022
        };
        exits += walk(seed + 1, steps, program);
    }
    assert!(exits > 0, "no keeper exit executed in any walk");
}
