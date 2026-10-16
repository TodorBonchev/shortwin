//! Shared support of the `liquidate` / `expire` tests (`tests/liquidate_it.rs`,
//! `tests/expire_it.rs`, `tests/attacks_exit_it.rs`; ARCH §3.6, §3.7, §8.2, §8.3; ECON §8, §12):
//! the other test binaries include it with `#[path]`. Built on its own it is an empty test
//! binary (it has no tests of its own).
//!
//! - **The differential oracle** ([`view`], [`predict`], [`Predicted`]): the state a keeper exit
//!   decides on (the market after a same-second `crank_price`, which runs exactly the exit's
//!   prologue — accrual, roll, record, chains — on a clone of the VM), and every decision and
//!   amount of ARCH §3.6 / §3.7 recomputed **directly from `shortwin-math`** (quotes, legs,
//!   references, bounds, waterfalls, `pool::{sweep, repay}`), independent of the program's glue
//!   (`instructions::liquidate`, `engine::bounds`). [`liquidate_checked`] / [`expire_checked`]
//!   send the exit (optionally inside a bundle) and assert the program did exactly that: the
//!   refusal code, or every event field, every lamport and token delta, the lender-pool state
//!   (`Pool`, `Vault`), the market aggregates, the closed position and exposure.
//! - **Environments**: [`env_with`] (the test-venue build with changed init parameters),
//!   [`ra_market`] (ECON's `RA` pool — 1.5·10¹⁴ raw tokens against 2,500 SOL, 25 bps, 12 %
//!   protocol share, which is the mock's fee split — at Tier A: fresh oracle, age ≥ 7 days).
//! - **Drivers**: price paths, holding a price, per-second keeper cranks.

#![allow(dead_code, clippy::result_large_err, clippy::too_many_arguments)]

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use litesvm::types::TransactionMetadata;
use shortwin::constants::VENUE_BIT_MOCK;
use shortwin::errors::ShortwinError;
use shortwin::events::{Expire, Liquidate, MintSafetyTripped, TraderSwept};
use shortwin::state::{InitConfigArgs, Market, OracleCache, ShortPosition, WalletExposure};
use shortwin::venues::ExtrasSel;
pub use shortwin_litesvm_tests::drive::Actor;
pub use shortwin_litesvm_tests::engine::{Borrower, MarketSpec, MockMarket, crank_price_ix};
pub use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
use shortwin_litesvm_tests::invariants::positions::venue_executable;
pub use shortwin_litesvm_tests::mock::{self, Dir};
pub use shortwin_litesvm_tests::{Env, TxResult, events, fixtures, pda, read};
use shortwin_math::bounds;
use shortwin_math::num::WAD;
use shortwin_math::observe;
use shortwin_math::persist;
use shortwin_math::pool::{self, Pool, Resolution, SweepSource, Vault};
use shortwin_math::quote::{self, BuyQuote, Pricing};
use shortwin_math::rent;
use shortwin_math::risk::{self, Budget};
use shortwin_math::waterfall::{self, ExitInputs, Payout};
use solana_keypair::Keypair;
use solana_signer::Signer;

pub type E = ShortwinError;

// ---- environments ------------------------------------------------------------------------

/// The test-venue environment (as `Env::test_venue`) with `f` applied to the init arguments.
pub fn env_with(f: impl FnOnce(&mut InitConfigArgs)) -> Env {
    let mut env = Env::new_test_venue_program();
    env.load_mock_cpamm();
    let mut args = env.default_init_args();
    args.venue_enabled |= VENUE_BIT_MOCK;
    f(&mut args);
    ok(env.initialize_config(args));
    env.install_mock_extras();
    env
}

/// The default mock market (Tier C: the oracle cache is empty).
pub fn setup() -> (Env, MockMarket) {
    let mut env = Env::test_venue();
    let mm = env.mock_market(MarketSpec::default());
    (env, mm)
}

/// A mock market per `spec` in `env`.
pub fn market(env: &mut Env, spec: MarketSpec) -> MockMarket {
    env.mock_market(spec)
}

/// Writes a fresh SOL/USD price into the oracle cache ($121.20, `expo = −8`, published now), as
/// `crank_sol_usd` would (ARCH §4.7; the tier's USD thresholds).
pub fn fresh_oracle(env: &mut Env) {
    let key = pda::oracle_cache();
    let mut c: OracleCache = env.oracle_cache();
    c.price = 12_120_000_000;
    c.conf = 1_000_000;
    c.expo = -8;
    c.publish_time = env.now();
    c.updated_slot = env.clock().slot;
    fixtures::set_borsh(&mut env.svm, &key, &c, OracleCache::SPACE, 0);
}

/// ECON's `RA` market on the mock: 1.5·10¹⁴ raw tokens (6 decimals) against 2,500 SOL, a 25 bps
/// fee with a 12 % protocol share (the mock's), `lend` tokens lent, listed 7 days + 60 s ago
/// with a fresh oracle, so an open now is Tier A (ECON §6.2: `Λ = 5,000 SOL ≈ $606 k ≥ θ_A`,
/// age ≥ 7 d).
pub fn ra_market(env: &mut Env, lend: u64) -> MockMarket {
    let mm = env.mock_market(MarketSpec {
        base: 150_000_000_000_000,
        quote: 2_500_000_000_000,
        fee_ppm: 2_500,
        lend,
        warmup: 60,
        ..MarketSpec::default()
    });
    env.warp(7 * 86_400);
    env.crank(&mm);
    env.warp(1);
    fresh_oracle(env);
    env.crank(&mm);
    mm
}

/// A keeper (a funded keypair).
pub fn keeper(env: &mut Env) -> Keypair {
    env.funded_keypair()
}

// ---- builders ----------------------------------------------------------------------------

fn build(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction {
        program_id: shortwin::ID,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    }
}

/// `liquidate(sel)` of `borrower`'s position `nonce` by `keeper`, with `ReadSet ++ TradeExtras
/// ++ extras`.
pub fn liquidate_ix(
    mm: &MockMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    let market = mm.market();
    let ix = build(
        shortwin::accounts::Liquidate {
            keeper: *keeper,
            borrower: *borrower,
            config: pda::config(),
            market,
            position: pda::position(&market, borrower, nonce).0,
            exposure: pda::exposure(&market, borrower).0,
            mint: mm.mint(),
            pool_vault: mm.keys.pool_vault,
            market_vault: mm.keys.market_vault,
            trader: mm.keys.trader,
            token_program: mm.keys.token_program,
            system_program: anchor_lang::system_program::ID,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::Liquidate { extras: sel },
    );
    shortwin_litesvm_tests::ix::with_remaining(ix, &mm.trade_remaining(extras))
}

/// `expire(sel)` of `borrower`'s position `nonce` by `keeper`.
pub fn expire_ix(
    mm: &MockMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
    sel: ExtrasSel,
    extras: &[AccountMeta],
) -> Instruction {
    let market = mm.market();
    let ix = build(
        shortwin::accounts::Expire {
            keeper: *keeper,
            borrower: *borrower,
            config: pda::config(),
            market,
            position: pda::position(&market, borrower, nonce).0,
            exposure: pda::exposure(&market, borrower).0,
            mint: mm.mint(),
            pool_vault: mm.keys.pool_vault,
            market_vault: mm.keys.market_vault,
            trader: mm.keys.trader,
            token_program: mm.keys.token_program,
            system_program: anchor_lang::system_program::ID,
            event_authority: pda::event_authority(),
            program: shortwin::ID,
        },
        shortwin::instruction::Expire { extras: sel },
    );
    shortwin_litesvm_tests::ix::with_remaining(ix, &mm.trade_remaining(extras))
}

/// Which keeper exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Liquidate,
    Expire,
}

/// The exit instruction of `which` with extras `none`.
pub fn exit_ix(
    which: Which,
    mm: &MockMarket,
    keeper: &Pubkey,
    borrower: &Pubkey,
    nonce: u64,
) -> Instruction {
    match which {
        Which::Liquidate => liquidate_ix(mm, keeper, borrower, nonce, ExtrasSel::NONE, &[]),
        Which::Expire => expire_ix(mm, keeper, borrower, nonce, ExtrasSel::NONE, &[]),
    }
}

// ---- the differential oracle ---------------------------------------------------------------

/// What a keeper exit sent now would decide on: the market after a same-second `crank_price`
/// (the exit's own prologue: accrual, roll, record, chains), the venue's live pre-trade state
/// and the position, read on a clone of the VM after `pre` (a bundle's front-run).
#[derive(Clone)]
pub struct View {
    pub now: i64,
    pub market: Market,
    pub pool: Pool,
    pub vault: Vault,
    pub pos: ShortPosition,
    pub pos_lamports: u64,
    pub exposure_q_open: u64,
    pub exposure_lamports: u64,
    /// Pricing reserves `(X, Y)` (0 when the venue cannot execute).
    pub x: u64,
    pub y: u64,
    pub fee_ppm: u32,
    pub executable: bool,
    /// Lamports the trader normalisation will sweep (`trader` + the `trader_wsol` address).
    pub donated: u64,
    /// The mock's `over_delivery` knob (the exit's expected `token_surplus`).
    pub over_delivery: u64,
}

/// The [`View`] after `pre` (signed by `signers`) on a clone of the VM; the VM is restored.
pub fn view_after(
    env: &mut Env,
    mm: &MockMarket,
    borrower: &Pubkey,
    nonce: u64,
    pre: &[Instruction],
    signers: &[&Keypair],
) -> View {
    let saved = env.svm.clone();
    if !pre.is_empty() {
        ok(env.send(pre, signers));
    }
    ok(env.send(&[crank_price_ix(mm)], &[]));
    let v = read_view(env, mm, borrower, nonce);
    env.svm = saved;
    v
}

/// The [`View`] now.
pub fn view(env: &mut Env, mm: &MockMarket, borrower: &Pubkey, nonce: u64) -> View {
    view_after(env, mm, borrower, nonce, &[], &[])
}

/// The [`View`]s of several positions now (one crank on one clone).
pub fn views(env: &mut Env, mm: &MockMarket, positions: &[(Pubkey, u64)]) -> Vec<View> {
    let saved = env.svm.clone();
    ok(env.send(&[crank_price_ix(mm)], &[]));
    let out = positions
        .iter()
        .map(|(b, n)| read_view(env, mm, b, *n))
        .collect();
    env.svm = saved;
    out
}

/// The [`View`] of the state as it is (no crank): valid right after a crank in this second.
pub fn read_view(env: &Env, mm: &MockMarket, borrower: &Pubkey, nonce: u64) -> View {
    let market = env.market(&mm.market());
    let executable = venue_executable(&env.svm, &market).unwrap_or(false);
    let (x, y) = if executable {
        let r = mock::reserves(&env.svm, mm.mock());
        (r.base, r.quote)
    } else {
        (0, 0)
    };
    let pos_key = pda::position(&mm.market(), borrower, nonce).0;
    let exposure_key = pda::exposure(&mm.market(), borrower).0;
    let exposure: Option<WalletExposure> = read::borsh(&env.svm, &exposure_key);
    let pool_state = mock::state(&env.svm, &mm.mock().pool);
    View {
        now: market.last_accrual_ts,
        pool: market.pool(),
        vault: *env.market_vault(&mm.market()).vault(),
        pos: env.position(mm, borrower, nonce),
        pos_lamports: read::lamports(&env.svm, &pos_key),
        exposure_q_open: exposure.map_or(0, |e| e.q_open),
        exposure_lamports: read::lamports(&env.svm, &exposure_key),
        x,
        y,
        fee_ppm: pool_state.fee_ppm,
        over_delivery: pool_state.over_delivery,
        executable,
        donated: read::lamports(&env.svm, &market.trader)
            + read::lamports(&env.svm, &market.trader_wsol),
        market,
    }
}

/// A reference and its bound, recomputed from `shortwin_math`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefPred {
    pub p_ref: Option<u128>,
    pub l_b: u128,
    pub b_ref: Option<u64>,
    pub max_in: Option<u64>,
    pub pi_ref: u64,
}

/// Every decision input of ARCH §3.6 / §3.7 on a [`View`], from `shortwin_math` alone.
#[derive(Clone)]
pub struct Predicted {
    pub view: View,
    pub r_due: u64,
    pub v_eff: u64,
    pub k_s: u64,
    pub fresh: bool,
    pub b_live: Option<BuyQuote>,
    pub b_live_risk: Option<u64>,
    pub sigma_live: u64,
    pub p_rs: Option<u128>,
    pub b_rs: Option<u64>,
    pub spot_leg: bool,
    pub ema_leg: bool,
    pub expired: bool,
    pub buyback_allowed: bool,
    pub cr_spot_bps: u64,
    pub cr_ema_bps: u64,
    pub liq: RefPred,
    pub exp: RefPred,
}

/// The pricing of the mock at its live fee (INTERFACE.md "short.win's view").
pub fn live_pricing(fee_ppm: u32) -> Pricing {
    let f = mock::cpmm_fees(fee_ppm);
    Pricing::Cpmm { buy: f, sell: f }
}

/// [`Predicted`] of `v` (ARCH §4.5; ECON §5.5, §8.2).
pub fn predict(v: &View) -> Predicted {
    let pos = &v.pos;
    let q = pos.q;
    let now = v.now;
    let obs = v.market.observation();
    let r_due = rent::due(&pos.pos_rent(), &v.market.basis()).unwrap();
    let v_eff = risk::v_eff(pos.v, r_due);
    let k_s = risk::k_s(pos.v, &pos.settle_keeper_terms()).unwrap();
    let live = live_pricing(v.fee_ppm);
    let risky = live.capped(u64::from(pos.venue_fee_hard_cap_bps));
    let (x, y) = (u128::from(v.x), u128::from(v.y));
    let b_live = if v.executable {
        live.buy(q, x, y)
    } else {
        None
    };
    let b_live_risk = if v.executable {
        risky.buy(q, x, y).map(|b| b.cost)
    } else {
        None
    };
    let sigma_live = b_live.map_or(u64::MAX, |b| risk::sigma_xy(Some(b.net), q, x, y).unwrap());
    let fresh = obs.last.is_fresh(now, pos.ref_max_age_s);
    let (p_live, l_live) = if v.executable {
        (
            Some(risk::price_wad(x, y).unwrap()),
            quote::liquidity(x, y).unwrap(),
        )
    } else {
        (None, obs.ema.liq)
    };
    let p_rs = p_live.and_then(|p| observe::robust_spot(p, &obs.last, now, pos.ref_max_age_s));
    let l_spot = spot_liquidity_of(l_live, &obs);
    let b_rs = p_rs.and_then(|p| risky.buy_repriced(q, l_spot, p).map(|b| b.cost));
    let l_bps = pos.cr_liq_bps;
    let spot_leg = risk::spot_leg(v.executable, fresh, v_eff, b_rs, l_bps).unwrap();
    let quote_at = |x: u128, y: u128| {
        if x == 0 {
            None
        } else {
            risky.buy(q, x, y).map(|b| b.cost)
        }
    };
    let ema_leg = persist::liq_ema_leg(
        &obs.ring,
        |x, y| risk::cr_below(v_eff, quote_at(x, y), l_bps),
        pos.liq_persist_n,
    )
    .unwrap();
    let budget = Budget {
        v: pos.v,
        k_s,
        ert: pos.ert,
    };
    let buyback_allowed = risk::buyback_allowed(
        &budget,
        b_live.map(|b| b.cost),
        sigma_live,
        u32::from(pos.max_liq_slip_bps),
    )
    .unwrap();
    let newest = obs.ring.newest().copied().unwrap_or_default();
    let cr_ema_bps = risk::cr_bps(v_eff, quote_at(newest.x, newest.y)).unwrap_or(u64::MAX);
    // The mock venue: no locked depth, no M4F-1 cap.
    let l_b = bounds::ref_liquidity(l_live, &obs.ema, &obs.ring, None).unwrap();
    let prm = pos.exit_params();
    let liq_p = bounds::liq_reference(&obs.ema, &obs.ring, &obs.last, now, &prm).unwrap();
    let liq_b = liq_p.and_then(|p| risky.buy_repriced(q, l_b, p).map(|b| b.cost));
    let liq = RefPred {
        p_ref: liq_p,
        l_b,
        b_ref: liq_b,
        max_in: liq_b.and_then(|b| bounds::max_in_liq(b, pos.delta_liq_bps, pos.v, k_s, pos.ert)),
        pi_ref: 0,
    };
    let exp_p = bounds::expiry_reference(&obs.ema, &obs.ring, &obs.last, now, &prm).unwrap();
    let exp_b = exp_p.and_then(|p| risky.buy_repriced(q, l_b, p).map(|b| b.cost));
    let pi_ref = exp_b.map_or(0, |b| bounds::pi_ref(b, pos.expiry_fee_bps).unwrap());
    let exp = RefPred {
        p_ref: exp_p,
        l_b,
        b_ref: exp_b,
        max_in: exp_b.and_then(|b| {
            bounds::max_in_exp(
                b,
                pos.delta_exp_bps,
                pos.exec_tol_bps,
                pos.v,
                k_s,
                r_due,
                pi_ref,
                pos.ert,
            )
        }),
        pi_ref,
    };
    Predicted {
        view: v.clone(),
        r_due,
        v_eff,
        k_s,
        fresh,
        b_live,
        b_live_risk,
        sigma_live,
        p_rs,
        b_rs,
        spot_leg,
        ema_leg,
        expired: now >= pos.deadline,
        buyback_allowed,
        cr_spot_bps: risk::cr_bps(v_eff, b_rs).unwrap_or(u64::MAX),
        cr_ema_bps,
        liq,
        exp,
    }
}

/// A keeper exit the oracle expects to execute.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub b_live: u64,
    pub b_live_risk: u64,
    pub r: RefPred,
    pub b_ref: u64,
    pub max_in: u64,
}

impl Predicted {
    /// `common.would_liq` of the attack scripts: liquidatable if the attacker pumps the live
    /// spot to at least the last bucket low inside its own final transaction — the EMA leg holds
    /// on the newest `liq_persist_n` entries, the bucket is fresh, and the robust-spot leg at
    /// `lb_lo` (on the live liquidity) is below `L`.
    pub fn would_liq(&self) -> bool {
        if !self.ema_leg || !self.fresh || !self.view.executable {
            return false;
        }
        let pos = &self.view.pos;
        let obs = self.view.market.observation();
        let risky = live_pricing(self.view.fee_ppm).capped(u64::from(pos.venue_fee_hard_cap_bps));
        let l = quote::liquidity(u128::from(self.view.x), u128::from(self.view.y)).unwrap();
        let q = risky.buy_repriced(pos.q, l, obs.last.lo).map(|b| b.cost);
        risk::cr_below(self.v_eff, q, pos.cr_liq_bps).unwrap()
    }

    /// `status()["cr_s"]`: `⌊V_eff·10⁴/B̂_live⌋` on the execution quote (0 if undefined).
    pub fn cr_s(&self) -> u64 {
        risk::cr_bps(self.v_eff, self.b_live.map(|b| b.cost)).unwrap_or(u64::MAX)
    }

    /// CR-liquidatable (spot ∧ EMA legs).
    pub fn cr_liquidatable(&self) -> bool {
        self.spot_leg && self.ema_leg
    }

    /// `liquidate`'s decision (ARCH §3.6 steps 1–6, in order).
    pub fn liquidation(&self) -> Result<Plan, E> {
        if !self.view.executable {
            return Err(E::NoRoute);
        }
        if self.view.market.ema_warm != 0 {
            return Err(E::EmaWarmingUp);
        }
        if !self.fresh {
            return Err(E::ReferenceStale);
        }
        if !(self.cr_liquidatable() || self.expired) {
            return Err(E::NotLiquidatable);
        }
        if !self.buyback_allowed {
            return Err(E::UseSolSettle);
        }
        if self.expired && !self.cr_liquidatable() {
            return Err(E::UseExpire);
        }
        self.plan(self.liq)
    }

    /// `expire`'s decision (ARCH §3.7, in the program's order).
    pub fn expiry(&self) -> Result<Plan, E> {
        if !self.view.executable {
            return Err(E::NoRoute);
        }
        if self.view.market.ema_warm != 0 {
            return Err(E::EmaWarmingUp);
        }
        if !self.expired {
            return Err(E::NotExpired);
        }
        if !self.fresh {
            return Err(E::ReferenceStale);
        }
        if self.cr_liquidatable() {
            return Err(E::UseLiquidate);
        }
        if !self.buyback_allowed {
            return Err(E::UseSolSettle);
        }
        self.plan(self.exp)
    }

    /// The decision of `which`.
    pub fn decision(&self, which: Which) -> Result<Plan, E> {
        match which {
            Which::Liquidate => self.liquidation(),
            Which::Expire => self.expiry(),
        }
    }

    fn plan(&self, r: RefPred) -> Result<Plan, E> {
        r.p_ref.ok_or(E::ReferenceStale)?;
        let b_ref = r.b_ref.ok_or(E::ExitBoundNotMet)?;
        let max_in = r.max_in.ok_or(E::ExitBoundNotMet)?;
        let b_live = self.b_live.ok_or(E::UseSolSettle)?.cost;
        if b_live > max_in {
            return Err(E::ExitBoundNotMet);
        }
        Ok(Plan {
            b_live,
            b_live_risk: self.b_live_risk.unwrap_or(b_live),
            r,
            b_ref,
            max_in,
        })
    }

    /// The waterfall of `which` with measured spend `spent`.
    pub fn payout(&self, which: Which, plan: &Plan, spent: u64) -> Payout {
        let pos = &self.view.pos;
        let x = ExitInputs {
            v: pos.v,
            b: spent,
            r_due: self.r_due,
            b_live: plan.b_live_risk,
            b_ref: plan.b_ref,
            rent_protocol_bps: u32::from(pos.rent_protocol_bps),
        };
        match which {
            Which::Liquidate => waterfall::liquidate(
                &x,
                u32::from(pos.penalty_bps),
                &pos.penalty_split(),
                self.k_s,
            )
            .unwrap(),
            Which::Expire => waterfall::expire(
                &x,
                u32::from(pos.expiry_fee_bps),
                &pos.expiry_keeper_terms(),
            )
            .unwrap(),
        }
    }

    /// The lender pool after the exit: the normalisation's sweep, then `pool::repay`.
    pub fn pool_after(&self, payout: &Payout, lump_vest_s: u32) -> (Pool, Vault) {
        let (mut p, mut v) = (self.view.pool, self.view.vault);
        if self.view.donated > 0 {
            pool::sweep(
                &mut p,
                &mut v,
                None,
                self.view.now,
                lump_vest_s,
                self.view.donated,
                SweepSource::Trader,
            )
            .unwrap();
        }
        let pos = &self.view.pos;
        let credited = rent::credited_for(&pos.pos_rent(), &self.view.market.basis()).unwrap();
        pool::repay(
            &mut p,
            &mut v,
            self.view.now,
            lump_vest_s,
            &Resolution {
                q: pos.q,
                credited,
                lump: payout.lump,
            },
            payout.rent,
            u32::from(pos.rent_protocol_bps),
        )
        .unwrap();
        v.protocol_fees += payout.protocol;
        (p, v)
    }
}

/// The state of `mm` just before an exit and just after it that [`check_exit`] compares.
struct Balances {
    keeper: u64,
    borrower: u64,
    market_vault: u64,
    pool_vault: u64,
}

fn balances(env: &Env, mm: &MockMarket, keeper: &Pubkey, borrower: &Pubkey) -> Balances {
    Balances {
        keeper: read::lamports(&env.svm, keeper),
        borrower: read::lamports(&env.svm, borrower),
        market_vault: read::lamports(&env.svm, &mm.keys.market_vault),
        pool_vault: read::token_amount(&env.svm, &mm.keys.pool_vault).unwrap_or(0),
    }
}

/// The result of a checked exit.
pub enum Exit {
    Liquidated(Box<Liquidate>, Box<Predicted>, TransactionMetadata),
    Expired(Box<Expire>, Box<Predicted>, TransactionMetadata),
    Refused(E, Box<Predicted>),
}

impl Exit {
    /// The `Liquidate` event (panics otherwise).
    pub fn liquidated(&self) -> &Liquidate {
        match self {
            Exit::Liquidated(e, ..) => e,
            _ => panic!("expected a liquidation, got {}", self.describe()),
        }
    }

    /// The `Expire` event (panics otherwise).
    pub fn expired(&self) -> &Expire {
        match self {
            Exit::Expired(e, ..) => e,
            _ => panic!("expected an expiry, got {}", self.describe()),
        }
    }

    /// The refusal (panics otherwise).
    pub fn refused(&self) -> E {
        match self {
            Exit::Refused(e, _) => *e,
            _ => panic!("expected a refusal, got {}", self.describe()),
        }
    }

    /// Whether the exit executed.
    pub fn executed(&self) -> bool {
        !matches!(self, Exit::Refused(..))
    }

    /// The oracle's prediction.
    pub fn predicted(&self) -> &Predicted {
        match self {
            Exit::Liquidated(_, p, _) | Exit::Expired(_, p, _) | Exit::Refused(_, p) => p,
        }
    }

    /// The transaction metadata of an executed exit.
    pub fn meta(&self) -> &TransactionMetadata {
        match self {
            Exit::Liquidated(_, _, m) | Exit::Expired(_, _, m) => m,
            Exit::Refused(..) => panic!("a refused exit has no metadata"),
        }
    }

    fn describe(&self) -> String {
        match self {
            Exit::Liquidated(..) => "a liquidation".into(),
            Exit::Expired(..) => "an expiry".into(),
            Exit::Refused(e, _) => format!("the refusal {e:?}"),
        }
    }
}

/// Sends `front ++ [exit] ++ back` (one transaction; `signers` sign the bundle) and asserts the
/// program decided and paid exactly what the oracle computes on the state after `front`
/// ([`view_after`]). The keeper must not be the fee payer and must not be touched by `front`
/// / `back` in lamports (an actor trades through its token accounts).
pub fn exit_checked(
    env: &mut Env,
    mm: &MockMarket,
    which: Which,
    keeper: &Keypair,
    borrower: &Pubkey,
    nonce: u64,
    front: &[Instruction],
    back: &[Instruction],
    signers: &[&Keypair],
) -> Exit {
    let exit = exit_ix(which, mm, &keeper.pubkey(), borrower, nonce);
    exit_checked_with(
        env, mm, which, keeper, borrower, nonce, exit, front, back, signers,
    )
}

/// [`exit_checked`] with a given exit instruction (extras sets).
pub fn exit_checked_with(
    env: &mut Env,
    mm: &MockMarket,
    which: Which,
    keeper: &Keypair,
    borrower: &Pubkey,
    nonce: u64,
    exit: Instruction,
    front: &[Instruction],
    back: &[Instruction],
    signers: &[&Keypair],
) -> Exit {
    let mut all: Vec<&Keypair> = signers.to_vec();
    all.push(keeper);
    let same_wallet = keeper.pubkey() == *borrower;
    let v = view_after(env, mm, borrower, nonce, front, &all);
    let pred = Box::new(predict(&v));
    let decision = pred.decision(which);
    let mut ixs: Vec<Instruction> = front.to_vec();
    let exit_index = ixs.len();
    ixs.push(exit);
    ixs.extend_from_slice(back);

    // Balances after `front` (on a clone), to compare the exit's own deltas.
    let saved = env.svm.clone();
    if !front.is_empty() {
        ok(env.send(front, &all));
    }
    let pre = balances(env, mm, &keeper.pubkey(), borrower);
    env.svm = saved;

    match decision {
        Err(e) => {
            let res = env.send(&ixs, &all);
            assert_err(&res, e);
            Exit::Refused(e, pred)
        }
        Ok(plan) => {
            // The exit alone (front applied) for exact deltas, then the whole bundle for real.
            let saved = env.svm.clone();
            if !front.is_empty() {
                ok(env.send(front, &all));
            }
            let alone = env.send(&ixs[exit_index..=exit_index], &all);
            let post_alone = balances(env, mm, &keeper.pubkey(), borrower);
            let market_after = env.market(&mm.market());
            let vault_after = *env.market_vault(&mm.market()).vault();
            let pos_gone = read::account(&env.svm, &pda::position(&mm.market(), borrower, nonce).0)
                .is_none_or(|a| a.lamports == 0);
            env.svm = saved;
            let alone = ok(alone);
            let res = env.send(&ixs, &all);
            let meta = match res {
                Ok(m) => m,
                Err(f) => panic!(
                    "the bundle failed although its exit alone succeeded: {:?}\n{}",
                    f.err,
                    f.meta.pretty_logs()
                ),
            };
            assert!(pos_gone, "the position is closed");
            let lump_vest_s = env.config().params.lump_vest_s;
            match which {
                Which::Liquidate => {
                    let ev: Liquidate = events::one(&alone);
                    let payout = pred.payout(which, &plan, ev.buyback_cost);
                    check_liquidation(&ev, &pred, &plan, &payout, &alone);
                    check_after(
                        env,
                        mm,
                        &pred,
                        &payout,
                        &pre,
                        &post_alone,
                        &market_after,
                        &vault_after,
                        lump_vest_s,
                        ev.buyback_cost,
                        ev.token_surplus,
                        same_wallet,
                    );
                    let bundled: Liquidate = events::of_ix(&meta, exit_index).remove(0);
                    assert_eq!(
                        borsh_bytes(&bundled),
                        borsh_bytes(&ev),
                        "the bundle's liquidation is the checked one"
                    );
                    Exit::Liquidated(Box::new(bundled), pred, meta)
                }
                Which::Expire => {
                    let ev: Expire = events::one(&alone);
                    let payout = pred.payout(which, &plan, ev.buyback_cost);
                    check_expiry(&ev, &pred, &plan, &payout, &alone);
                    check_after(
                        env,
                        mm,
                        &pred,
                        &payout,
                        &pre,
                        &post_alone,
                        &market_after,
                        &vault_after,
                        lump_vest_s,
                        ev.buyback_cost,
                        ev.token_surplus,
                        same_wallet,
                    );
                    let bundled: Expire = events::of_ix(&meta, exit_index).remove(0);
                    assert_eq!(
                        borsh_bytes(&bundled),
                        borsh_bytes(&ev),
                        "the bundle's expiry is the checked one"
                    );
                    Exit::Expired(Box::new(bundled), pred, meta)
                }
            }
        }
    }
}

fn borsh_bytes<T: anchor_lang::AnchorSerialize>(v: &T) -> Vec<u8> {
    let mut out = Vec::new();
    v.serialize(&mut out).unwrap();
    out
}

/// A checked liquidation with no bundle.
pub fn liquidate_checked(
    env: &mut Env,
    mm: &MockMarket,
    keeper: &Keypair,
    borrower: &Pubkey,
    nonce: u64,
) -> Exit {
    exit_checked(
        env,
        mm,
        Which::Liquidate,
        keeper,
        borrower,
        nonce,
        &[],
        &[],
        &[],
    )
}

/// A checked expiry with no bundle.
pub fn expire_checked(
    env: &mut Env,
    mm: &MockMarket,
    keeper: &Keypair,
    borrower: &Pubkey,
    nonce: u64,
) -> Exit {
    exit_checked(
        env,
        mm,
        Which::Expire,
        keeper,
        borrower,
        nonce,
        &[],
        &[],
        &[],
    )
}

fn check_side_events(pred: &Predicted, meta: &TransactionMetadata) {
    let swept: Vec<TraderSwept> = events::of(meta);
    if pred.view.donated > 0 {
        assert_eq!(swept.len(), 1, "one TraderSwept");
        assert_eq!(swept[0].lamports, pred.view.donated);
    } else {
        assert!(swept.is_empty(), "no TraderSwept without a donation");
    }
    let tripped: Vec<MintSafetyTripped> = events::logged(meta);
    assert!(tripped.len() <= 1);
}

fn check_liquidation(
    ev: &Liquidate,
    pred: &Predicted,
    plan: &Plan,
    payout: &Payout,
    meta: &TransactionMetadata,
) {
    let pos = &pred.view.pos;
    // The mock executes the quote exactly (its differential tests).
    assert_eq!(ev.buyback_cost, plan.b_live, "B = B̂_live on the mock");
    assert_eq!(ev.b_live, plan.b_live);
    assert_eq!(ev.b_live_risk, plan.b_live_risk);
    assert_eq!(ev.b_ref, plan.b_ref);
    assert_eq!(Some(ev.p_ref), plan.r.p_ref);
    assert_eq!(ev.max_in, plan.max_in);
    assert_eq!(ev.delta_bps, pos.delta_liq_bps);
    assert_eq!(ev.cr_spot_bps, pred.cr_spot_bps);
    assert_eq!(ev.cr_ema_bps, pred.cr_ema_bps);
    assert_eq!(ev.expired, pred.expired);
    assert_eq!(ev.token_surplus, pred.view.over_delivery);
    assert_eq!(ev.rent, payout.rent);
    assert_eq!(ev.penalty, payout.penalty);
    assert_eq!(ev.penalty_lender, payout.penalty_lenders);
    assert_eq!(ev.penalty_protocol, payout.penalty_protocol);
    assert_eq!(ev.penalty_keeper, payout.penalty_keeper);
    assert_eq!(ev.keeper_topup, payout.keeper_topup);
    assert_eq!(ev.borrower_payout, payout.borrower);
    check_side_events(pred, meta);
}

fn check_expiry(
    ev: &Expire,
    pred: &Predicted,
    plan: &Plan,
    payout: &Payout,
    meta: &TransactionMetadata,
) {
    assert_eq!(ev.buyback_cost, plan.b_live, "B = B̂_live on the mock");
    assert_eq!(ev.b_live, plan.b_live);
    assert_eq!(ev.b_live_risk, plan.b_live_risk);
    assert_eq!(ev.b_ref, plan.b_ref);
    assert_eq!(ev.pi_ref, plan.r.pi_ref);
    assert_eq!(ev.max_in, plan.max_in);
    assert_eq!(ev.token_surplus, pred.view.over_delivery);
    assert_eq!(ev.rent, payout.rent);
    assert_eq!(ev.expiry_fee, payout.expiry_fee);
    assert_eq!(ev.keeper_reward, payout.keeper);
    assert_eq!(ev.borrower_payout, payout.borrower);
    check_side_events(pred, meta);
}

/// Lamport, token and lender-pool deltas of an executed exit (the exit alone, after `front`).
fn check_after(
    env: &Env,
    mm: &MockMarket,
    pred: &Predicted,
    payout: &Payout,
    pre: &Balances,
    post: &Balances,
    market_after: &Market,
    vault_after: &Vault,
    lump_vest_s: u32,
    spent: u64,
    surplus: u64,
    same_wallet: bool,
) {
    let v = &pred.view;
    let pos = &v.pos;
    assert_eq!(payout.total().unwrap(), pos.v, "I9");
    let exposure_back = if v.exposure_q_open == pos.q {
        v.exposure_lamports
    } else {
        0
    };
    let to_borrower = v.pos_lamports - pos.v + payout.borrower + exposure_back;
    if same_wallet {
        assert_eq!(
            post.borrower - pre.borrower,
            to_borrower + payout.keeper,
            "a borrower keeping its own position gets both legs"
        );
    } else {
        assert_eq!(post.keeper - pre.keeper, payout.keeper, "the keeper's legs");
        assert_eq!(
            post.borrower - pre.borrower,
            to_borrower,
            "the borrower: its remainder, the position's rent (A18) and a closed exposure"
        );
    }
    assert_eq!(
        post.market_vault - pre.market_vault,
        payout.lenders + payout.protocol + v.donated,
        "market_vault: lenders, protocol and the swept donation"
    );
    assert_eq!(
        post.pool_vault - pre.pool_vault,
        pos.q + surplus,
        "q returned"
    );
    assert_eq!(spent, payout.buyback);
    let (p, vault) = pred.pool_after(payout, lump_vest_s);
    assert_eq!(market_after.pool(), p, "the lender pool after pool::repay");
    assert_eq!(*vault_after, vault, "the vault after pool::repay");
    // Aggregates (I6's inputs).
    assert_eq!(market_after.open_positions, v.market.open_positions - 1);
    assert_eq!(
        market_after.total_notional_open,
        v.market.total_notional_open - pos.n_open
    );
    assert_eq!(market_after.borrowed_q, v.market.borrowed_q - pos.q);
    let _ = (env, mm);
}

// ---- prices and time ---------------------------------------------------------------------

/// The pool's spot (WAD).
pub fn spot(env: &Env, mm: &MockMarket) -> u128 {
    env.spot(mm)
}

/// Moves the pool's spot to `target` with one swap by `actor` (no-op within 10 ppm).
pub fn move_to(env: &mut Env, mm: &MockMarket, actor: &Actor, target: u128) {
    let now = spot(env, mm);
    if now.abs_diff(target) * 100_000 <= now {
        return;
    }
    env.push_price(mm, actor, target);
}

/// One second of honest market: advance the clock, move the pool to `target` (if any), crank.
pub fn honest_second(env: &mut Env, mm: &MockMarket, actor: &Actor, target: Option<u128>) {
    env.warp(1);
    if let Some(t) = target {
        move_to(env, mm, actor, t);
    }
    env.crank(mm);
}

/// Ramps the pool geometrically from its spot now to `to`·spot over `secs` seconds, one honest
/// crank per second (no exit is tried).
pub fn ramp(env: &mut Env, mm: &MockMarket, actor: &Actor, to: f64, secs: u32) {
    let p0 = spot(env, mm) as f64;
    for s in 1..=secs {
        let m = to.powf(f64::from(s) / f64::from(secs));
        honest_second(env, mm, actor, Some((p0 * m) as u128));
    }
}

/// Tries `which` now and then once a second (one honest crank per second, the pool held), up
/// to `max_s` attempts, each checked with the oracle. Returns the executed exit (if any) and
/// the refusals before it.
pub fn attempts(
    env: &mut Env,
    mm: &MockMarket,
    which: Which,
    keeper: &Keypair,
    borrower: &Pubkey,
    nonce: u64,
    max_s: u32,
) -> (Option<Exit>, Vec<E>) {
    let mut refusals = Vec::new();
    for i in 0..max_s {
        if i > 0 {
            env.warp(1);
            env.crank(mm);
        }
        let ex = exit_checked(env, mm, which, keeper, borrower, nonce, &[], &[], &[]);
        if ex.executed() {
            return (Some(ex), refusals);
        }
        refusals.push(ex.refused());
    }
    (None, refusals)
}

/// A price gap now: the next second, the pool moves to `target` and the keeper cranks.
pub fn gap(env: &mut Env, mm: &MockMarket, actor: &Actor, target: u128) {
    honest_second(env, mm, actor, Some(target));
}

/// `B̂_live(q)` on the pool now (execution quote).
pub fn quote_buy(env: &Env, mm: &MockMarket, q: u64) -> Option<u64> {
    mock::quote_exact_out(&env.svm, mm.mock(), Dir::QuoteIn, q).map(|b| b.cost)
}

/// The spot price at which `B̂(q)` on the pool (moved along its constant product from its
/// current state, fee on the input) is about `cost`: bisection on the repriced quote at the
/// pool's live liquidity.
pub fn price_for_cost(env: &Env, mm: &MockMarket, q: u64, cost: u64) -> u128 {
    let r = mock::reserves(&env.svm, mm.mock());
    let fee = mock::state(&env.svm, &mm.mock().pool).fee_ppm;
    let pricing = live_pricing(fee);
    let l = quote::liquidity(u128::from(r.base), u128::from(r.quote)).unwrap();
    let (mut lo, mut hi) = (1u128, spot(env, mm) * 1_000);
    for _ in 0..256 {
        if hi - lo <= 1 {
            break;
        }
        let mid = lo + (hi - lo) / 2;
        match pricing.buy_repriced(q, l, mid) {
            Some(b) if b.cost < cost => lo = mid,
            _ => hi = mid,
        }
    }
    lo
}

/// An open by a fresh borrower that must succeed: the borrower, the nonce and the position.
pub fn open(env: &mut Env, mm: &MockMarket, q: u64) -> (Borrower, u64, ShortPosition) {
    let mut b = env.borrower();
    let (res, nonce) = env.open(mm, &mut b, q);
    ok(res);
    let pos = env.position(mm, &b.key(), nonce);
    (b, nonce, pos)
}

/// The error code a result failed with (`None` on success or a non-custom failure).
pub fn code_of(res: &TxResult) -> Option<u32> {
    custom_code(res)
}

/// `WAD`.
pub const W: u128 = WAD;

/// `⌊x·num/den⌋` for prices.
pub fn scale(x: u128, num: u128, den: u128) -> u128 {
    x * num / den
}

/// The liquidity the liquidation spot leg is judged at (FITS-2 (a); an independent spelling of
/// `shortwin_math::bounds::spot_liquidity`): `max(L_live, L_floor)`, `L_floor = min(L_ema,
/// L_mhi)` — the upper median of the bucket liquidity highs (AIR-17) — while the post-change guard
/// is set, else `L_ema`.
pub fn spot_liquidity_of(l_live: u128, obs: &shortwin_math::observe::Observation) -> u128 {
    let l_floor = if obs.ema.guard != 0 {
        let mut highs: Vec<u128> = obs.ring.iter().map(|e| e.lhi).collect();
        highs.sort_unstable();
        obs.ema.liq.min(highs[highs.len() / 2])
    } else {
        obs.ema.liq
    };
    l_live.max(l_floor)
}
