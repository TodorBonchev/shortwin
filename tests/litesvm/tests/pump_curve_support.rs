//! Shared machinery of the pump-curve fork E2E (M5; ARCH §8.2 "fork E2E", §8.4, §9 M5 row;
//! ECON §11.2, §12): the **release** short.win on the **dumped** pump, pump-fees and PumpSwap
//! ELFs and the pump coins of `tests/fixtures/` (`shortwin_litesvm_tests::{fork, pump}`), driven
//! only by real instructions — short.win's own and the curve's `buy_v2` / `sell_v2` by other
//! users — with the invariant hooks after every instruction (`Env::send`).
//!
//! `tests/pump_curve_e2e.rs`, `tests/pump_curve_scenarios.rs` and `tests/pump_curve_attacks.rs`
//! include this file with `#[path]`; compiled on its own it is an empty test binary.
//!
//! **ECON's curve markets, exactly.** The fixture coin `fresh` is a `create_v2` curve at the
//! sim's `PumpCurve.at_progress(0)` state (`vT` 1.073·10¹⁵, `vQ` 30 SOL, `rT` 793.1·10¹² — pump's
//! `Global` initial reserves) with the live fee schedule of the sim (`PUMP_CURVE_FEES`: one tier,
//! 0 / 95 / 30 bps). One real `buy_v2` of `⌊793.1·10¹²·x⌋` tokens by another user takes it to
//! `D@x%` to the lamport (pump's `vQ += ⌊q·vQ/(vT − q)⌋ + 1` equals the sim's `⌈k/vT⌉` there),
//! so [`Sim::d_market`] is ECON's `D@50%` / `D@90%` market. [`Sim::run`] replays
//! `shortwin_sim.run` step for step: the same keeper cadence (every second in the event window,
//! while a threshold is near (`keeper_near`) or an exit is pending, else every 30 s), the same
//! price paths (other users trade the curve toward `P1·mult(t)` with `move_to_price`'s exact
//! `buy_v2` / `sell_v2` sizes), the keeper acting one second after the position becomes
//! actionable and retrying every second, the borrower closing at `close_at`.
//!
//! **The keeper's view** ([`Status`]) is recomputed from `shortwin-math` and the venue-state
//! readers alone (never the program's adapter): the curve's `CurvePricing` (live fee for
//! execution quotes, the position's hard cap for risk quotes; `B̂_str` above `rT`), the robust
//! spot and spot leg on `max(L_live, L_ema')`, the EMA leg on the newest `liq_persist_n` ring
//! entries' cached `(vT_i, vQ_i)`, "normal buyback allowed", `CR` as ECON's tables print it.
//!
//! **Two modelled differences** (both documented where they are asserted):
//! - `R_min`: the sim prices a position's minimum rent at its fixed 80 % utilisation; the program
//!   at the pre-open utilisation (ARCH §4.5 "pre-instruction counters"), 0 % for a market's first
//!   position (as `tests/attacks_exit_it.rs` on the mock). Where the sim's rent due is `R_min`
//!   the program's is `max(accrued, r_min)`, and the waterfall legs below the rent move by exactly
//!   that difference. The accrual itself runs at exactly 80 %: [`lend_for_80`] lends
//!   `⌈1.25·Q⌉`, so `⌈ΣB_Q·10⁴/(T + ΣB_Q)⌉ = 8,000` once `Q` is borrowed.
//! - the wallet cap: the sim has none; [`econ_params`] lifts `wallet_cap_bps` to 10⁴ (inside its
//!   compiled bounds) so the SPEC per-position cap binds, as in ECON's tables.

#![allow(
    dead_code,
    unused_imports,
    clippy::result_large_err,
    clippy::too_many_arguments
)]

pub use anchor_lang::prelude::Pubkey;
pub use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
pub use litesvm::types::TransactionMetadata;
pub use shortwin::errors::ShortwinError;
pub use shortwin::events::{
    Close, Deposit, Expire, Liquidate, Open, PriceCranked, SettleArmed, SettleDisarmed, SolSettle,
    VenueStateChanged,
};
pub use shortwin::state::{InitConfigArgs, Market, ShortPosition, VenueKind};
pub use shortwin::venues::ExtrasSel;
pub use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, expire_ix, liquidate_ix, open_short_ix,
    sol_settle_ix,
};
pub use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
pub use shortwin_litesvm_tests::fork::{self, PumpCoin, PumpCurve};
pub use shortwin_litesvm_tests::invariants::venue_view;
pub use shortwin_litesvm_tests::pamm::{self, PammMarket};
pub use shortwin_litesvm_tests::pump::{self, CurveUser, PumpMarket, PumpSpec};
pub use shortwin_litesvm_tests::tokens::TokenProgram;
pub use shortwin_litesvm_tests::{Env, TxResult, events, ix, pda, read};
pub use shortwin_math::num::WAD;
pub use shortwin_math::quote::CurveState;
pub use solana_keypair::Keypair;
pub use solana_signer::Signer;

use shortwin::constants::{PUMP_AMM_FEE_CONFIG, PUMP_AMM_GLOBAL_CONFIG, PUMP_GLOBAL};
use shortwin_math::bounds;
use shortwin_math::fees::{self, FeeBps, FeeTier, PammFeeSchedule};
use shortwin_math::num::{Rounding, mul_div, sqrt_u128};
use shortwin_math::observe;
use shortwin_math::persist;
use shortwin_math::quote::{self, BuyQuote, CurveOffsets, CurvePricing, MigrationParams};
use shortwin_math::rent;
use shortwin_math::risk::{self, Budget};
use shortwin_venue_state::{pump as vpump, pump_amm as vamm};

/// short.win's error codes.
pub type E = ShortwinError;
/// `WAD`.
pub const W: u128 = WAD;
/// A lamport-denominated SOL.
pub const SOL: u64 = 1_000_000_000;
/// `10⁴`.
pub const BPS: u64 = 10_000;

/// pump `Global.initial_virtual_token_reserves` (the sim's `PUMP_VT0`).
pub const PUMP_VT0: u64 = 1_073_000_000_000_000;
/// pump `Global.initial_virtual_sol_reserves` (the sim's `PUMP_VS0`).
pub const PUMP_VS0: u64 = 30_000_000_000;
/// pump `Global.initial_real_token_reserves` (the sim's `PUMP_RT0`).
pub const PUMP_RT0: u64 = 793_100_000_000_000;
/// Tokens bought from a fresh curve to reach `D@50%` (`int(PUMP_RT0·0.5)`).
pub const SOLD_D50: u64 = 396_550_000_000_000;
/// Tokens bought from a fresh curve to reach `D@90%` (`int(PUMP_RT0·0.9)`).
pub const SOLD_D90: u64 = 713_790_000_000_000;

/// The sim's event time (`shortwin_sim.TE`): 1 h after the open.
pub const TE: i64 = 3_600;

// ---- environments ---------------------------------------------------------------------------

/// ECON's sizing: the wallet cap lifted to 10⁴ bps (the sim has none; compiled bounds
/// [500, 10⁴]), everything else at the ECON §15 defaults.
pub fn econ_params(a: &mut InitConfigArgs) {
    a.params.wallet_cap_bps = 10_000;
}

/// The release short.win initialized with `f` applied to the harness's default arguments, the
/// three dumped pump programs, the pump statics, `coin`'s accounts, mainnet's Rent and the pump
/// snapshot clock (as `Env::fork_pump_coin`, with a configurable `initialize_config`).
pub fn fork_env_with(coin: PumpCoin, f: impl FnOnce(&mut InitConfigArgs)) -> (Env, PumpCurve) {
    let mut env = Env::new();
    let mut args = env.default_init_args();
    f(&mut args);
    ok(env.initialize_config(args));
    env.load_pump_programs();
    env.load_pump_statics();
    let c = env.load_pump_coin(coin);
    env.set_pump_rent();
    env.set_pump_clock();
    (env, c)
}

/// Lent tokens that make the utilisation exactly 80 % (the sim's `util_bps`) once `sum_q` is
/// borrowed: `⌈1.25·ΣQ⌉`.
pub fn lend_for_80(sum_q: u64) -> u64 {
    (u128::from(sum_q) * 5).div_ceil(4) as u64
}

// ---- small readers --------------------------------------------------------------------------

/// Lamports of `key`.
pub fn lamports(env: &Env, key: &Pubkey) -> u64 {
    read::lamports(&env.svm, key)
}

/// Token amount of `key` (0 if not a token account).
pub fn tokens(env: &Env, key: &Pubkey) -> u64 {
    read::token_amount(&env.svm, key).unwrap_or(0)
}

/// The curve's `(vT, vQ, rT, rQ, complete)`.
pub fn curve(env: &Env, pm: &PumpMarket) -> CurveState {
    pm.curve.curve_state(&env.svm)
}

/// The curve's spot `⌊vQ·WAD/vT⌋` (the sim's `spot_wad`).
pub fn spot(env: &Env, pm: &PumpMarket) -> u128 {
    let c = curve(env, pm);
    c.vq * W / c.vt
}

/// The market as stored.
pub fn market(env: &Env, pm: &PumpMarket) -> Market {
    env.market(&pm.market())
}

/// The venue kind stored on the market.
pub fn venue(env: &Env, pm: &PumpMarket) -> VenueKind {
    market(env, pm).venue_kind().expect("venue")
}

/// The single `VenueStateChanged` of `meta`, checked `from → to`.
pub fn venue_changed(meta: &TransactionMetadata, from: VenueKind, to: VenueKind) {
    let ev: VenueStateChanged = events::one(meta);
    assert_eq!((ev.from, ev.to), (from as u8, to as u8), "the transition");
}

/// No `VenueStateChanged` in `meta`.
pub fn no_venue_change(meta: &TransactionMetadata) {
    assert!(
        events::of::<VenueStateChanged>(meta).is_empty(),
        "no venue transition"
    );
}

/// `K_s` of a position (its locked settlement-keeper terms).
pub fn k_s(p: &ShortPosition) -> u64 {
    risk::k_s(p.v, &p.settle_keeper_terms()).expect("K_s")
}

/// Rent due of `pos` on the market as stored (valid right after an instruction this second).
pub fn rent_due(env: &Env, pm: &PumpMarket, pos: &ShortPosition) -> u64 {
    rent::due(&pos.pos_rent(), &market(env, pm).basis()).expect("rent")
}

// ---- the curve as short.win quotes it (shortwin-math on the readers) ------------------------

/// What the curve's quotes read (ARCH §4.2, §4.3; pump-final §6.2), read from the dumped accounts
/// with the venue-state readers: the live curve, its resolved fees, the constant offsets, the live
/// leg's `X_s = curve_base_ata − rT`, `Global.pool_migration_fee`, the post-migration pool's
/// PumpSwap schedule, the synthetic route's fee of a live curve and the post-complete quote of a
/// complete one.
#[derive(Clone, Debug)]
pub struct CurveCtx {
    pub live: CurveState,
    pub fees: FeeBps,
    pub offsets: CurveOffsets,
    pub live_pool_tokens: u128,
    pub migration_fee: u64,
    pub pool_tiers: Vec<FeeTier>,
    pub pool_creator_fee_configurable: bool,
    pub creator_is_default: bool,
    /// The fee of pump's synthetic route past `rT` (`fees::pump_curve_at_supply` on the mint's
    /// live supply): a live, non-cashback, non-mayhem curve only (pump-final §6.1.5).
    pub synthetic: Option<FeeBps>,
    /// A complete curve's pool part: `(curve_base_ata, pcQ)` when `pcB` or `pcQ` is set.
    pub post_complete: Option<(u128, u64)>,
}

impl CurveCtx {
    /// The context of `pm`'s curve now; `None` on a curve that has no fee tier (migrated,
    /// `vT = 0`).
    pub fn read(env: &Env, pm: &PumpMarket) -> Option<Self> {
        let svm = &env.svm;
        let live = pm.curve.curve_state(svm);
        let fees = pm.curve.fees(svm)?;
        let offsets = CurveOffsets::of(&live)?;
        let g = svm.get_account(&PUMP_GLOBAL).expect("pump Global");
        let migration_fee = vpump::read_global(&g.owner.to_bytes(), &g.data)
            .expect("Global")
            .pool_migration_fee();
        let pfc = svm
            .get_account(&PUMP_AMM_FEE_CONFIG)
            .expect("PumpSwap FeeConfig");
        let pool_tiers = vpump::read_fee_config(&pfc.owner.to_bytes(), &pfc.data)
            .expect("FeeConfig")
            .fee_tiers
            .iter()
            .map(|t| FeeTier {
                threshold: t.threshold,
                fees: FeeBps::new(t.fees.lp, t.fees.protocol, t.fees.creator),
            })
            .collect();
        let gc = svm
            .get_account(&PUMP_AMM_GLOBAL_CONFIG)
            .expect("PumpSwap GlobalConfig");
        let pool_creator_fee_configurable =
            vamm::read_global_config(&gc.owner.to_bytes(), &gc.data)
                .expect("GlobalConfig")
                .creator_fee_configurable();
        let base = u128::from(tokens(env, &pm.curve.curve_base_ata));
        let c = pm.curve.state(svm);
        let synthetic = (!live.complete && !c.is_cashback_coin && !c.is_mayhem_mode)
            .then(|| pm.curve.synthetic_fees(svm))
            .flatten();
        let post_complete = (live.complete
            && (c.post_complete_base_out != 0 || c.post_complete_quote_in != 0))
            .then_some((base, c.post_complete_quote_in));
        Some(Self {
            live,
            fees,
            offsets,
            live_pool_tokens: base.saturating_sub(live.rt),
            migration_fee,
            pool_tiers,
            pool_creator_fee_configurable,
            creator_is_default: c.creator_is_default(),
            synthetic,
            post_complete,
        })
    }

    /// `CurvePricing` at the live fee (pump-final §6.2) with the EMA-leg migration (the standard
    /// 206.9 M tokens, or the live `curve_base_ata` and `pcQ` after a synthetic pool part), the
    /// post-migration pool's schedule (`base_supply` 10¹⁵, a new pool's own creator fee 0) and
    /// the live curve's synthetic route (`B̂_v3` above `rT`), as ARCH §4.3 prices hypothetical
    /// curve states.
    pub fn pricing(&self) -> CurvePricing<'_> {
        let (pool_tokens, post_complete_quote_in) = self
            .post_complete
            .unwrap_or((shortwin::venues::STANDARD_POOL_TOKENS, 0));
        CurvePricing {
            fees: self.fees,
            offsets: self.offsets,
            migration: MigrationParams {
                migration_fee: self.migration_fee,
                pool_tokens,
                post_complete_quote_in,
            },
            pool: PammFeeSchedule {
                tiers: &self.pool_tiers,
                base_supply: 1_000_000_000_000_000,
                creator_fee_configurable: self.pool_creator_fee_configurable,
                pool_creator_fee_bps: 0,
                coin_creator_is_default: self.creator_is_default,
                cap_bps: None,
            },
            synthetic: self.synthetic,
        }
    }

    /// `B̂(q)` on the live curve (`B̂_v3` above `rT` with the live leg's `X_s`), at the live fee
    /// or capped at `cap_bps` (the risk view); `None` above `rT` without a synthetic route.
    pub fn buy_live(&self, q: u64, cap_bps: Option<u64>) -> Option<BuyQuote> {
        if !self.live.complete && self.synthetic.is_none() && u128::from(q) > self.live.rt {
            return None;
        }
        let mut p = self.pricing();
        if let Some(c) = cap_bps {
            p = p.capped(c);
        }
        p.migration.pool_tokens = self.live_pool_tokens;
        p.buy(q, &self.live)
    }

    /// `B̂(q)` on the curve state `(vT, vQ) = (x, y)` with the live offsets (a ring entry's cached
    /// reprojection), at the risk view `cap_bps`. `None` for `x = 0`.
    pub fn buy_xy(&self, q: u64, x: u128, y: u128, cap_bps: u64) -> Option<BuyQuote> {
        if x == 0 {
            return None;
        }
        let s = CurveState {
            vt: x,
            vq: y,
            rt: x.saturating_sub(self.offsets.token),
            rq: y.saturating_sub(self.offsets.quote),
            complete: false,
        };
        self.pricing().capped(cap_bps).buy(q, &s)
    }

    /// `B̂(q; curve_reprice(L, P))` at the risk view `cap_bps`.
    pub fn buy_repriced(&self, q: u64, l: u128, p: u128, cap_bps: u64) -> Option<BuyQuote> {
        self.pricing().capped(cap_bps).buy_repriced(q, l, p)
    }
}

/// A position's liquidation inputs on the state as it is (valid right after an instruction in
/// this second: the market's accrual and roll are at `now`), from `shortwin-math` alone (ARCH
/// §4.5; ECON §5.5; the sim's `Market.status`).
#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub now: i64,
    pub executable: bool,
    pub r_due: u64,
    pub v_eff: u64,
    pub k_s: u64,
    pub fresh: bool,
    /// Execution quote `B̂_live(q)` (live fee).
    pub b_live: Option<u64>,
    /// Risk quote on the live state (the position's hard cap).
    pub b_live_risk: Option<u64>,
    pub sigma_live: u64,
    /// Robust spot, its risk quote on `max(L_live, L_ema')`.
    pub p_rs: Option<u128>,
    pub b_rs: Option<u64>,
    pub spot_leg: bool,
    pub ema_leg: bool,
    pub expired: bool,
    pub buyback_allowed: bool,
    /// Risk quote on the newest ring entry's cached `(vT_i, vQ_i)`.
    pub b_ema: Option<u64>,
    /// `CR` at the execution quote and on the newest ring entry (ECON's "CR spot/EMA").
    pub cr_s: u64,
    pub cr_e: u64,
    /// The curve's spot and the market's `P_ema`.
    pub spot: Option<u128>,
    pub ema: u128,
}

impl Status {
    /// CR-liquidatable: both legs (ECON §5.5).
    pub fn cr_liq(&self) -> bool {
        self.spot_leg && self.ema_leg
    }

    /// The sim's `st["liq"] or st["expired"]`: a keeper exit is due.
    pub fn actionable(&self) -> bool {
        self.cr_liq() || self.expired
    }
}

/// [`Status`] of `borrower`'s position `nonce` on `pm` now.
pub fn status(env: &Env, pm: &PumpMarket, borrower: &Pubkey, nonce: u64) -> Status {
    let m = market(env, pm);
    let pos = env.position(pm, borrower, nonce);
    let now = env.now();
    let q = pos.q;
    let obs = m.observation();
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).expect("rent");
    let v_eff = risk::v_eff(pos.v, r_due);
    let k_s = k_s(&pos);
    let cap = u64::from(pos.venue_fee_hard_cap_bps);
    let l_bps = pos.cr_liq_bps;
    let view = venue_view::view(&env.svm, &m).unwrap_or(venue_view::VenueView::DEAD);
    let ctx = CurveCtx::read(env, pm);
    let executable = view.executable && ctx.is_some();
    let live = curve(env, pm);
    let (b_live, b_live_risk, sigma_live, p_live, l_live) = match (&ctx, executable) {
        (Some(c), true) => {
            let b = c.buy_live(q, None);
            let br = c.buy_live(q, Some(cap)).map(|x| x.cost);
            let sigma = b.map_or(u64::MAX, |b| {
                risk::sigma_xy(Some(b.net), q, live.vt, live.vq).expect("sigma")
            });
            (
                b.map(|x| x.cost),
                br,
                sigma,
                Some(risk::price_wad(live.vt, live.vq).expect("price")),
                quote::liquidity(live.vt, live.vq).expect("L"),
            )
        }
        _ => (None, None, u64::MAX, None, obs.ema.liq),
    };
    let fresh = obs.last.is_fresh(now, pos.ref_max_age_s);
    let p_rs = p_live.and_then(|p| observe::robust_spot(p, &obs.last, now, pos.ref_max_age_s));
    let l_spot = bounds::spot_liquidity(l_live, &obs.ema, &obs.ring).expect("L_spot");
    let b_rs = match (&ctx, p_rs) {
        (Some(c), Some(p)) => c.buy_repriced(q, l_spot, p, cap).map(|b| b.cost),
        _ => None,
    };
    let spot_leg = risk::spot_leg(executable, fresh, v_eff, b_rs, l_bps).expect("spot leg");
    // Without a curve to price on (a list-only market, a migrated curve with `vT = 0`), the
    // program's risk quotes take the adapter's cap-only constant-product view (PROPOSED, M5).
    let fallback = quote::Pricing::PumpSwap(FeeBps::new(0, cap, 0));
    let quote_at = |x: u128, y: u128| match &ctx {
        Some(c) => c.buy_xy(q, x, y, cap).map(|b| b.cost),
        None if x == 0 => None,
        None => fallback.buy(q, x, y).map(|b| b.cost),
    };
    let ema_leg = persist::liq_ema_leg(
        &obs.ring,
        |x, y| risk::cr_below(v_eff, quote_at(x, y), l_bps),
        pos.liq_persist_n,
    )
    .expect("EMA leg");
    let budget = Budget {
        v: pos.v,
        k_s,
        ert: pos.ert,
    };
    let buyback_allowed =
        risk::buyback_allowed(&budget, b_live, sigma_live, u32::from(pos.max_liq_slip_bps))
            .expect("buyback allowed");
    let newest = obs.ring.newest().copied().unwrap_or_default();
    let b_ema = quote_at(newest.x, newest.y);
    Status {
        now,
        executable,
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
        b_ema,
        cr_s: risk::cr_bps(v_eff, b_live).unwrap_or(u64::MAX),
        cr_e: risk::cr_bps(v_eff, b_ema).unwrap_or(u64::MAX),
        spot: p_live,
        ema: m.ema_price.get(),
    }
}

// ---- scenarios (ECON §11.2; `shortwin_sim.SCENARIOS`) ---------------------------------------

/// A price path: the multiple of `P1` (the spot right after the open) other traders move the
/// curve to at second `t`, `None` = no push (`shortwin_sim.step_path` / `ramp_path` /
/// `spike_path`).
#[derive(Clone, Copy, Debug)]
pub enum Path {
    Flat,
    Step { te: i64, k: f64 },
    Ramp { te: i64, k: f64, dur: i64 },
    Spike { te: i64, k: f64, dur: i64 },
}

impl Path {
    /// The multiple at `t`, with the sim's float arithmetic (`math.log`, `math.exp`).
    pub fn at(&self, t: i64) -> Option<f64> {
        match *self {
            Self::Flat => None,
            Self::Step { te, k } => (t >= te).then_some(k),
            Self::Ramp { te, k, dur } => {
                if t < te {
                    return None;
                }
                let g = k.ln() / dur as f64;
                Some((g * (t - te).min(dur) as f64).exp())
            }
            Self::Spike { te, k, dur } => {
                if te <= t && t < te + dur {
                    Some(k)
                } else if t >= te + dur {
                    Some(1.0)
                } else {
                    None
                }
            }
        }
    }
}

/// `shortwin_sim.Scenario`.
#[derive(Clone, Copy, Debug)]
pub struct Scenario {
    pub path: Path,
    pub t_event: i64,
    pub window: i64,
    pub close_at: Option<i64>,
    pub horizon: Option<i64>,
    /// `events=((t, complete_curve),)`: at the first step at or after `t`, before the price
    /// move, another user buys all of `rT` (a real `buy_v2`; the curve completes).
    pub complete_at: Option<i64>,
}

impl Scenario {
    /// `gap kx`: a step to `k·P1` at `TE`.
    pub fn gap(k: f64) -> Self {
        Self {
            path: Path::Step { te: TE, k },
            t_event: TE,
            window: 3_600,
            close_at: None,
            horizon: None,
            complete_at: None,
        }
    }

    /// `pump kx/dur`: an exponential ramp to `k·P1` over `dur` seconds from `TE`.
    pub fn pump(k: f64, dur: i64, window: i64) -> Self {
        Self {
            path: Path::Ramp { te: TE, k, dur },
            t_event: TE,
            window,
            close_at: None,
            horizon: None,
            complete_at: None,
        }
    }

    /// `dump -50%, close`: a ramp to `0.5·P1` over 1 h from `TE`, the borrower closes at `2·TE`.
    pub fn dump_close() -> Self {
        Self {
            path: Path::Ramp {
                te: TE,
                k: 0.5,
                dur: 3_600,
            },
            t_event: TE,
            window: 3_700,
            close_at: Some(2 * TE),
            horizon: None,
            complete_at: None,
        }
    }

    /// `flat to expiry`: no push; the keeper's exit at the deadline.
    pub fn flat_to_expiry() -> Self {
        Self {
            path: Path::Flat,
            t_event: 1_000_000_000,
            window: 0,
            close_at: None,
            horizon: Some(1_000_000_000),
            complete_at: None,
        }
    }

    /// `graduation, migration stuck` (`GRAD_SCENARIOS[3]` with `migration_delay_s = 10⁹`): one
    /// buy completes the curve at `TE`, nobody migrates it; 3 h window.
    pub fn stuck_migration() -> Self {
        Self {
            path: Path::Flat,
            t_event: TE,
            window: 3 * 3_600,
            close_at: None,
            horizon: None,
            complete_at: Some(TE),
        }
    }
}

// ---- the market driver ----------------------------------------------------------------------

/// A position under test: its borrower, nonce and `Open` event.
pub struct Pos {
    pub b: Borrower,
    pub nonce: u64,
    pub open: Open,
}

impl Pos {
    pub fn key(&self) -> Pubkey {
        self.b.key()
    }
}

/// What a keeper exit (or the borrower's close) did.
pub enum Outcome {
    /// Nothing executed within the scenario's horizon.
    Open,
    Liquidated(Liquidate),
    Expired(Expire),
    Closed(Close),
    Settled(SolSettle),
}

impl Outcome {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Liquidated(_) => "liquidation",
            Self::Expired(_) => "expiry",
            Self::Closed(_) => "close",
            Self::Settled(s) => match s.condition {
                1 => "S1",
                2 => "S2",
                _ => "S3",
            },
        }
    }
}

/// The result of [`Sim::run`]: the outcome, the sim second it happened at, and the keeper's view
/// just before it (ECON's "price at exit" and "CR spot/EMA at exit").
pub struct RunResult {
    pub t: i64,
    pub outcome: Outcome,
    pub status: Status,
    /// Every sim second the keeper cranked at (the sim's `step_to` calls).
    pub steps: Vec<i64>,
    /// Moves by other traders whose sale paid 0 lamports (pump refuses them; the sim adds the
    /// dust to `vT`): `(t, tokens)`.
    pub dust_skipped: Vec<(i64, u64)>,
}

/// A curve market on the fork with its other traders (`crowd`) and a keeper; `t0` is the chain
/// time of the sim's `t = 0` (the open of the position under test).
pub struct Sim {
    pub env: Env,
    pub pm: PumpMarket,
    pub crowd: CurveUser,
    pub keeper: Keypair,
    pub t0: i64,
}

impl Sim {
    /// ECON's `D@x` market: the fresh fixture curve, one real `buy_v2` of `sold` tokens by
    /// another user (the curve lands on `PumpCurve.at_progress(x)` exactly), then
    /// `create_market(Auto)` (lists `PumpCurve`, creates the trader's base ATA and pump UVA), a
    /// lender's deposit of `lend` and 60 s of warm-up, all through real instructions, in a
    /// config with [`econ_params`].
    pub fn d_market(sold: u64, lend: u64) -> Self {
        Self::d_market_with(sold, lend, econ_params)
    }

    /// [`Sim::d_market`] with `f` applied to the init arguments.
    pub fn d_market_with(sold: u64, lend: u64, f: impl FnOnce(&mut InitConfigArgs)) -> Self {
        let (mut env, c) = fork_env_with(PumpCoin::Fresh, f);
        let s = c.curve_state(&env.svm);
        assert_eq!(
            (s.vt, s.vq, s.rt, s.rq),
            (
                u128::from(PUMP_VT0),
                u128::from(PUMP_VS0) + 1,
                u128::from(PUMP_RT0),
                1
            ),
            "the fresh fixture: pump's initial reserves after a 1-lamport creation buy"
        );
        // The one scenario builder (ARCH §8.2 "set_account rewrites curves"): the fixture's
        // 1-lamport creation buy is undone in the curve's reserves (the lamport stays on the
        // account), so the curve is exactly the sim's `at_progress(0)`; every later change is
        // a real trade.
        fork::set_curve_reserves(&mut env.svm, &c.curve, PUMP_VT0, PUMP_VS0, PUMP_RT0, 0);
        let crowd = CurveUser::new(&mut env, &c);
        if sold > 0 {
            ok(crowd.buy(&mut env, &c, sold, u64::MAX));
        }
        ok(env.list_pump(&c));
        let pm = env.pump_market_listed(
            PumpSpec {
                coin: PumpCoin::Fresh,
                lend,
                warmup: 60,
            },
            c,
        );
        let keeper = env.funded_keypair();
        let t0 = env.now();
        Self {
            env,
            pm,
            crowd,
            keeper,
            t0,
        }
    }

    /// Chain time of sim second `t`.
    pub fn chain(&self, t: i64) -> i64 {
        self.t0 + t
    }

    /// The sim's clock at `t` (never backwards).
    pub fn at(&mut self, t: i64) {
        let d = self.t0 + t - self.env.now();
        assert!(d >= 0, "time runs forward");
        if d > 0 {
            self.env.warp(d);
        }
    }

    /// The sim second now.
    pub fn t(&self) -> i64 {
        self.env.now() - self.t0
    }

    /// `crank_price` (must succeed).
    pub fn crank(&mut self) -> TransactionMetadata {
        ok(self.env.send(&[crank_price_ix(&self.pm)], &[]))
    }

    /// `Market.step_to(t)`: the honest keeper's crank at `t`.
    pub fn step_to(&mut self, t: i64) {
        self.at(t);
        self.crank();
    }

    /// Opens `q` for a new borrower now (no bounds of its own) and makes this second the sim's
    /// `t = 0`.
    pub fn open(&mut self, q: u64) -> Pos {
        let mut b = self.env.borrower();
        let (res, nonce) = self.env.open(&self.pm, &mut b, q);
        let meta = ok(res);
        let open: Open = events::one(&meta);
        self.t0 = self.env.now();
        Pos { b, nonce, open }
    }

    /// The position as stored.
    pub fn position(&self, p: &Pos) -> ShortPosition {
        self.env.position(&self.pm, &p.key(), p.nonce)
    }

    /// [`status`] of `p` now.
    pub fn status(&self, p: &Pos) -> Status {
        status(&self.env, &self.pm, &p.key(), p.nonce)
    }

    /// [`status`] of `p` after a same-second `crank_price` (the state an instruction's own
    /// prologue would decide on), on a clone of the VM; the VM is restored.
    pub fn status_after_crank(&mut self, p: &Pos) -> Status {
        let saved = self.env.svm.clone();
        self.crank();
        let st = self.status(p);
        self.env.svm = saved;
        st
    }

    /// The curve's spot now.
    pub fn spot(&self) -> u128 {
        spot(&self.env, &self.pm)
    }

    /// `buy_v2(q)` by the crowd (unbounded cost).
    pub fn crowd_buy(&mut self, q: u64) -> TransactionMetadata {
        ok(self.crowd.buy(&mut self.env, &self.pm.curve, q, u64::MAX))
    }

    /// `sell_v2(q)` by the crowd (no minimum).
    pub fn crowd_sell(&mut self, q: u64) -> TxResult {
        self.crowd.sell(&mut self.env, &self.pm.curve, q, 0)
    }

    /// Tokens to buy (> 0) or sell (< 0) to bring the curve to `target` (`PumpCurve.
    /// move_to_price`: `target_vT = isqrt(vT·vQ·WAD // target)`, a buy capped at `rT`).
    pub fn move_size(&self, target: u128) -> i128 {
        let s = curve(&self.env, &self.pm);
        let k = s.vt * s.vq;
        let tx = sqrt_u128(
            mul_div(k, W, target.max(1), Rounding::Floor).expect("k·WAD/P"),
            Rounding::Floor,
        );
        if tx < s.vt {
            i128::try_from((s.vt - tx).min(s.rt)).expect("buy")
        } else {
            -i128::try_from(tx - s.vt).expect("sell")
        }
    }

    /// `Market.exo_move(target)`: other traders move the curve toward `target` with a real
    /// `buy_v2` / `sell_v2` of `move_to_price`'s exact size (nothing on a complete curve).
    /// Returns the tokens of a sale pump refused because it pays 0 lamports (dust), if any.
    pub fn exo_move(&mut self, target: u128) -> Option<u64> {
        if curve(&self.env, &self.pm).complete {
            return None;
        }
        match self.move_size(target) {
            0 => None,
            d if d > 0 => {
                self.crowd_buy(u64::try_from(d).expect("q"));
                None
            }
            d => {
                let q = u64::try_from(-d).expect("q");
                let pays = self
                    .pm
                    .curve
                    .quote_sell(&self.env.svm, q)
                    .is_some_and(|s| s.proceeds > 0);
                let res = self.crowd_sell(q);
                if res.is_err() {
                    assert!(!pays, "a paying sale failed: {:?}", res.err());
                    return Some(q);
                }
                None
            }
        }
    }

    /// The sim's `keeper_near` (ECON §5.8 duty): crank every second while the position is within
    /// 60 s of its deadline, the venue cannot execute, spot and EMA disagree by more than 2 %, or
    /// `CR_spot < 1.3·L` (at the execution quote).
    pub fn keeper_near(&self, p: &Pos, st: &Status) -> bool {
        let pos = self.position(p);
        if st.now >= pos.deadline - 60 {
            return true;
        }
        let Some(spot) = st.spot else {
            return true;
        };
        let dev = (spot * u128::from(BPS) / st.ema.max(1)) as i128 - i128::from(BPS);
        if dev.abs() > 200 {
            return true;
        }
        match st.b_live {
            None => true,
            Some(b) => {
                u128::from(pos.v.saturating_sub(st.r_due)) * u128::from(BPS) / u128::from(b)
                    < u128::from(pos.cr_liq_bps) * 13 / 10
            }
        }
    }

    /// The keeper's exit at this second (the sim's `try_exit`): `liquidate` if CR-liquidatable,
    /// else `expire` if expired; if that waits (`ExitBoundNotMet`) or a normal buyback is not
    /// allowed (`UseSolSettle`), `sol_settle` (arming, S1 / S2 / S3); a position that is neither
    /// gets `sol_settle` too (it disarms or keeps the clock). `Some` if something executed.
    pub fn keeper_exit(&mut self, p: &Pos, st: &Status) -> Option<Outcome> {
        let kp = self.keeper.insecure_clone();
        let exit = if !st.executable {
            // No route (a migrating curve, a list-only market): the sim goes straight to
            // `_settlement` (S3 only); the exits would refuse `VenueMigrating` / `NoRoute`.
            None
        } else if st.cr_liq() {
            Some(liquidate_ix(&self.pm, &kp.pubkey(), &p.key(), p.nonce))
        } else if st.expired {
            Some(expire_ix(&self.pm, &kp.pubkey(), &p.key(), p.nonce))
        } else {
            None
        };
        if let Some(ix) = exit {
            let cu = pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU);
            let res = self.env.send(&[cu, ix], &[&kp]);
            match res {
                Ok(meta) => {
                    if st.cr_liq() {
                        return Some(Outcome::Liquidated(events::one(&meta)));
                    }
                    return Some(Outcome::Expired(events::one(&meta)));
                }
                Err(_) => {
                    let code = custom_code(&res);
                    assert!(
                        code == Some(u32::from(E::ExitBoundNotMet))
                            || code == Some(u32::from(E::UseSolSettle)),
                        "keeper exit refused unexpectedly: {code:?} (status {st:?})"
                    );
                }
            }
        }
        let ix = sol_settle_ix(&self.env, &self.pm, &kp.pubkey(), &p.key(), p.nonce);
        let meta = ok(self.env.send(&[ix], &[&kp]));
        events::of::<SolSettle>(&meta).pop().map(Outcome::Settled)
    }

    /// The borrower's close (`max_buyback_cost = u64::MAX`); `None` if refused for
    /// `InsufficientVault` (the sim's `try_exit(borrower_close=True)` returning `None`).
    pub fn borrower_close(&mut self, p: &Pos) -> Option<Outcome> {
        let res = self.env.close(&self.pm, &p.b, p.nonce);
        match res {
            Ok(meta) => Some(Outcome::Closed(events::one(&meta))),
            Err(_) => {
                assert_err(&res, E::InsufficientVault);
                None
            }
        }
    }

    /// `shortwin_sim.run(market, 1.0, sc, keeper_latency=1)` from the open of `p` (module docs).
    pub fn run(&mut self, p: &Pos, sc: &Scenario) -> RunResult {
        let deadline = self.position(p).deadline - self.t0;
        let p1 = self.spot();
        let horizon = sc
            .horizon
            .unwrap_or(sc.t_event + sc.window)
            .min(deadline + 3 * 3_600);
        let mut t = 0i64;
        let mut act_at: Option<i64> = None;
        let mut near = true;
        let mut steps = Vec::new();
        let mut dust = Vec::new();
        let mut last = self.status(p);
        let mut outcome = None;
        let mut completed = false;
        while t < horizon {
            let in_window = sc.t_event - 5 <= t && t <= sc.t_event + sc.window;
            if in_window || act_at.is_some() || near {
                t += 1;
            } else {
                let mut nxt = t + 30;
                if t < sc.t_event - 5 {
                    nxt = nxt.min(sc.t_event - 5);
                }
                if t < deadline - 60 {
                    nxt = nxt.min(deadline - 60);
                }
                t = nxt.max(t + 1);
            }
            self.at(t);
            if sc.complete_at.is_some_and(|c| t >= c) && !completed {
                completed = true;
                let c = curve(&self.env, &self.pm);
                if !c.complete && c.rt > 0 {
                    self.crowd_buy(u64::try_from(c.rt).expect("rT"));
                }
            }
            if let Some(mult) = sc.path.at(t)
                && let Some(q) = self.exo_move((p1 as f64 * mult) as u128)
            {
                dust.push((t, q));
            }
            if sc.close_at.is_some_and(|c| t >= c) {
                let st = self.status_after_crank(p);
                if let Some(o) = self.borrower_close(p) {
                    last = st;
                    outcome = Some(o);
                    break;
                }
            }
            self.crank();
            steps.push(t);
            let st = self.status(p);
            near = self.keeper_near(p, &st);
            if act_at.is_none() && st.actionable() {
                act_at = Some(t + 1);
            }
            if act_at.is_some_and(|a| t >= a) {
                if let Some(o) = self.keeper_exit(p, &st) {
                    last = st;
                    outcome = Some(o);
                    break;
                }
                if !self.status(p).actionable() {
                    act_at = None;
                }
            }
            last = st;
        }
        let outcome = match outcome {
            Some(o) => o,
            None => {
                self.crank();
                last = self.status(p);
                Outcome::Open
            }
        };
        RunResult {
            t,
            outcome,
            status: last,
            steps,
            dust_skipped: dust,
        }
    }
}

/// `x` as SOL with 4 decimals (ECON's tables).
pub fn sol4(x: i128) -> String {
    format!("{:+.4}", x as f64 / 1e9)
}

// ---- list-only markets, fair value, lenders -------------------------------------------------

/// A pump market seen as list-only (`None`): its ReadSet is the detection set `[bonding_curve,
/// canonical_pool]` (the adapter's `None` row, ARCH §3.12), no TradeExtras.
pub struct AsNone<'a>(pub &'a PumpMarket);

impl VenueMarket for AsNone<'_> {
    fn keys(&self) -> &shortwin_litesvm_tests::fixtures::MarketKeys {
        &self.0.keys
    }
    fn read_set(&self, _writable: bool) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(self.0.curve.curve, false),
            AccountMeta::new_readonly(pda::canonical_pool(&self.0.keys.mint), false),
        ]
    }
    fn trade_extras(&self) -> Vec<AccountMeta> {
        Vec::new()
    }
}

/// `p'` at an evaluated state (ARCH §4.5; R4A-3): the locked expiry fee if the position is
/// expired and `CR(V_eff, B̂_FV) ≥ L` there, else the locked penalty.
pub fn pprime(pos: &ShortPosition, now: i64, v_eff: u64, b_fv: u64) -> u32 {
    let below = risk::cr_below(v_eff, Some(b_fv), pos.cr_liq_bps).expect("CR");
    risk::fv_rate_bps(
        !(now >= pos.deadline && !below),
        u32::from(pos.penalty_bps),
        u32::from(pos.expiry_fee_bps),
    )
}

/// `FV(P, L)` (ARCH §4.5, D3) of `pos` given its risk quote `b_fv` at that state and its rent
/// due: `B̂_FV + ⌈p'·B̂_FV/10⁴⌉ + R_due`, with `p'` decided at the same state.
pub fn fair_value(pos: &ShortPosition, now: i64, r_due: u64, b_fv: u64) -> u64 {
    let v_eff = risk::v_eff(pos.v, r_due);
    let rate = pprime(pos, now, v_eff, b_fv);
    risk::fair_value(Some(b_fv), rate, r_due).expect("FV")
}

/// `B̂_FV(q; P)` of a stuck migration (a `PumpMigrating` market; ARCH §4.5 "stuck migration";
/// pump-final §5.4): the pool `migrate_v2` will build from the complete curve (the standard
/// 206.9 M tokens and `rQ − pool_migration_fee`, or after a synthetic pool part the live
/// `curve_base_ata` and `+ pcQ`, `P` then ρ-scaled), repriced at `P` on its own liquidity, at its
/// PumpSwap tier with the position's hard cap (the risk view) — `quote::migrating_fv` on the
/// readers.
pub fn b_fv_migrating(env: &Env, pm: &PumpMarket, pos: &ShortPosition, p: u128) -> u64 {
    let ctx = CurveCtx::read(env, pm).expect("a complete curve with vT > 0");
    assert!(ctx.live.complete && ctx.live.vt > 0, "a stuck migration");
    let pr = ctx.pricing().capped(u64::from(pos.venue_fee_hard_cap_bps));
    quote::migrating_fv(pos.q, p, &ctx.live, &pr.migration, &pr.pool)
        .expect("B̂_FV")
        .cost
}

/// `B̂_FV(q; reprice(L, P))` of a list-only market (`None`): the adapter's cap-only
/// constant-product risk view (one fee at the position's hard cap), the view the program prices
/// a market without a venue at (M5 adapter, PROPOSED; ARCH §3.8 names no view for `None`).
pub fn b_fv_none(pos: &ShortPosition, l: u128, p: u128) -> u64 {
    quote::Pricing::PumpSwap(FeeBps::new(0, u64::from(pos.venue_fee_hard_cap_bps), 0))
        .buy_repriced(pos.q, l, p)
        .expect("B̂_FV")
        .cost
}

/// A lender's full exit once everything vested: `withdraw(all shares)` predicted with
/// `pool::withdraw` + `pool::claim` on the stored state (ARCH §3.3), sent, and checked: the
/// tokens and the SOL paid are the prediction, the lender record is closed. Returns `(tokens,
/// sol)`.
pub fn withdraw_all(
    env: &mut Env,
    market: &Pubkey,
    keys: &ix::LenderKeys,
    owner: &Keypair,
) -> (u64, u64) {
    use shortwin::state::LenderPosition;
    use shortwin_math::pool as mpool;
    let lock_grace_s = env.config().params.lock_grace_s;
    let lp_key = pda::lender(market, &owner.pubkey()).0;
    let lp: LenderPosition = read::borsh(&env.svm, &lp_key).expect("lender");
    let now = env.now();
    let (mut p, mut v, mut l) = (
        env.market(market).pool(),
        *env.market_vault(market).vault(),
        lp.lender(),
    );
    let want_tokens = mpool::withdraw(
        &mut p,
        &mut v,
        &mut l,
        &mut [],
        now,
        lock_grace_s,
        lp.shares,
    )
    .expect("withdraw");
    let want_sol = mpool::claim(&p, &mut v, &mut l, &mut [], now).expect("claim");
    let lp_rent = lamports(env, &lp_key);
    let (acc0, l0) = (
        tokens(env, &keys.owner_token_account),
        lamports(env, &owner.pubkey()),
    );
    let meta = ok(env.send(&[ix::withdraw(keys, lp.shares, 0)], &[owner]));
    let w: shortwin::events::Withdraw = events::one(&meta);
    assert_eq!(
        (w.amount, w.sol_paid),
        (want_tokens, want_sol),
        "pool::withdraw, pool::claim"
    );
    assert_eq!(tokens(env, &keys.owner_token_account) - acc0, want_tokens);
    assert!(
        read::account(&env.svm, &lp_key).is_none(),
        "the lender record is closed"
    );
    assert_eq!(lamports(env, &owner.pubkey()) - l0, want_sol + lp_rent);
    assert_eq!(env.market(market).pool(), p);
    assert_eq!(*env.market_vault(market).vault(), v);
    (want_tokens, want_sol)
}
