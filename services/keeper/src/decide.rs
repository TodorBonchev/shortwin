//! What to do with a market and each of its positions this second (ARCHITECTURE §5.1
//! decision tree, §7.1 duties 1–5; ECON §5.8).
//!
//! **Exactly the program's view.** A decision is taken on the state the next instruction
//! will see: [`project`] runs the program's own market prologue
//! (`shortwin::engine::prologue::run`: params sync, accrual, the observation roll, the
//! record, the route chains) on a copy of the market at the cluster's clock; the legs, the
//! references and the bounds are the math crate's and the program engine's
//! (`risk::{spot_leg, liquidatable, buyback_allowed}` — the spot leg at `max(L_live, L_ema')`,
//! `bounds::spot_liquidity`, FITS-2 (a) —, `persist::liq_ema_leg`,
//! `engine::bounds::{liq_reference, max_in_liq, expiry_reference, max_in_exp}`,
//! `settle::s3_due`), with the position's locked terms and the risk fee
//! `min(φ_live, venue_fee_hard_cap_bps_pos)` (V2-1) — on PumpSwap also at most φ at `P_ema`, the
//! tier a same-transaction trade cannot raise (AIR-10: `VenueState::exit_risk_cap_bps` on the
//! exits' venue read, [`crate::view::venue_state`]) —, through the venue's own quoter
//! (`VenueState::quoter`: a constant-product pool, or the pump curve, ARCH §4.3 — on a live curve
//! above `rT` pump's synthetic exit `B̂_v3` at the synthetic fee `φ_s` on the mint's live supply,
//! on a `PumpMigrating` market the post-migration pool on the post-complete fields, ρ-scaled for
//! FV: pump-final §5.2–§5.4, the program's own crates, nothing re-implemented here) — the
//! program's `engine::bounds` and `engine::arming` quote the same way. The keeper therefore acts
//! at the first second the program would accept; every send is still simulated first (duty 3),
//! and the program remains the only judge.
//!
//! **A position whose `Q` exceeds its live curve's `rT`** (pump-final §5.2, owner D3) is priced
//! at `B̂_v3` and exits like any other: its `liquidate` / `expire` buys `rT` on the curve and
//! `Q − rT` from the would-be pool in one `buy_v3`, completing the curve (the market moves to
//! `PumpMigrating` in that instruction), with `buyback_allowed` and σ judged on `B̂_v3` (C5: above
//! σ_max it goes to `sol_settle` at once). The 3-transaction straddle bundle of M5/M6 is retired
//! (owner D2). [`PositionEval::synthetic`] marks such an exit.
//!
//! **Plans** ([`Plan`]), per position:
//!
//! | Situation | Plan |
//! |---|---|
//! | venue not executable, S3 due on the market | `S3Probe` (simulate `sol_settle`; send if it settles) |
//! | venue executable, the post-change warm-up running (`ema_warm > 0` after the roll; M4R) | `Watch` (exits refuse `EmaWarmingUp`, `sol_settle` decides nothing: observe every second until the gate) |
//! | not liquidatable, not armed | `Idle` / `Watch` (near a threshold: per-second coverage) |
//! | armed and not liquidatable | `SolSettle` (it keeps the clock or disarms on box evidence; duty 4) |
//! | liquidatable, completed bucket stale | `Observe` (observe this second, act the next) |
//! | liquidatable, normal buyback allowed, expired and not CR-liquidatable | `Expire` / `ExpireWait` |
//! | liquidatable, normal buyback allowed | `Liquidate` / `LiquidateWait` (fixed δ; retry next second) |
//! | liquidatable, no normal buyback (S1/S2 legs may hold), or armed | `SolSettle` |
//! | a `*Wait` while the position's S3 is due on an executable venue (flag, a fee above its cap) | `SolSettle` |
//!
//! **The S3 chain and a position's own route (AIR-19).** The market's chain (`no_route`,
//! `start_chain`) is the prologue's: `s3_no_route` with the fee judged at the lower of the global
//! `venue_fee_hard_cap_bps` and `Market.min_open_hard_cap_bps`, the lowest cap a position open since
//! the market last emptied locked (`engine::route::s3_no_route`). A position's S3 timer takes its
//! own "no route now", as `sol_settle` does: the fee judged at the lower of the global cap and the
//! cap the position locked ([`Projection::position_no_route`], `engine::route::position_no_route`),
//! so a fee between the mark and a position's own cap runs the chain without making that position
//! S3-due.

use anchor_lang::prelude::Pubkey;
use anyhow::anyhow;
use shortwin::engine::{arming, bounds, prologue, route};
use shortwin::state::{GlobalConfig, Market, VenueKind};
use shortwin::venues::VenueState;
use shortwin_math::bounds as bounds_math;
use shortwin_math::observe::{self, Observation};
use shortwin_math::risk::{self, Budget};
use shortwin_math::settle;
use shortwin_math::{persist, rent};
use shortwin_services_common::ChainClock;

use crate::config::CoverageConfig;
use crate::view::{MarketView, PositionView};

/// The market after the program's prologue at the cluster clock (what the next instruction's
/// body reads).
#[derive(Clone)]
pub struct Projection {
    pub now: i64,
    pub slot: u64,
    pub market: Box<Market>,
    pub obs: Box<Observation>,
    /// `s3_no_route` now (ARCH §5.3): the market's S3-chain predicate, at the lower of the global
    /// hard cap and `min_open_hard_cap_bps` (AIR-19; the program's prologue).
    pub s3_no_route: bool,
    /// `pool_fail` now (ARCH §2.6 rule 3: the Raydium fast path's own predicate, venue-side
    /// evidence only); the projected market's `pool_fail_since` is the chain after this
    /// second's observation.
    pub pool_fail: bool,
    /// The cap on the exit reference liquidity's EMA terms (M4F-1:
    /// `engine::bounds::ref_depth_cap`, `pool_d` itself on Raydium — since AIR-14 — and PumpSwap —
    /// M6R-1, at `λ_min` = 100 % since OWNER-M6D-1 / M6E-1 —, `None` elsewhere) on the projected
    /// market.
    pub depth_cap: Option<u128>,
    /// The global config the prologue ran with (a position's route predicate reads its hard cap
    /// and route-broken bits).
    pub config: Box<GlobalConfig>,
}

impl Projection {
    /// AIR-19: `s3_no_route` of one position now — `sol_settle`'s "no route now", the input of
    /// its S3 timer (`engine::route::position_no_route` on the projected market): the market's
    /// predicate with the position's locked `pos_cap_bps` in place of the market's mark, so a
    /// live fee above that cap is no route for the position under any global cap, and a fee
    /// within it a route whatever the others locked.
    pub fn position_no_route(&self, state: &VenueState, pos_cap_bps: u16) -> bool {
        route::position_no_route(&self.config, &self.market, state, pos_cap_bps)
    }
}

/// Runs the program's market prologue on a copy of `mv` at `clock` ([module docs](self)).
pub fn project(
    config: &GlobalConfig,
    mv: &MarketView,
    clock: ChainClock,
) -> anyhow::Result<Projection> {
    let mut market = mv.market.clone();
    let mut vault = mv.vault.clone();
    let pro = prologue::run(
        config,
        &mv.key,
        &mut market,
        &mut vault,
        Some(&mv.state),
        clock.unix_timestamp,
        clock.slot,
    )
    .map_err(|e| anyhow!("prologue of {}: {e}", mv.key))?;
    Ok(Projection {
        now: pro.now,
        slot: clock.slot,
        obs: market.observation(),
        depth_cap: bounds::ref_depth_cap(config, &market, mv.kind),
        market,
        s3_no_route: pro.s3_no_route,
        pool_fail: pro.pool_fail,
        config: Box::new(*config),
    })
}

/// What the keeper does with one position this second (module table).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Plan {
    Idle,
    Watch,
    Observe,
    Liquidate,
    LiquidateWait,
    Expire,
    ExpireWait,
    SolSettle,
    S3Probe,
}

impl Plan {
    /// Whether the plan sends (or probes) an exit instruction.
    pub const fn acts(self) -> bool {
        matches!(
            self,
            Self::Liquidate | Self::Expire | Self::SolSettle | Self::S3Probe
        )
    }

    /// Order of attempts within a market: liquidations, expiries, settlements, probes.
    const fn rank(self) -> u8 {
        match self {
            Self::Liquidate => 0,
            Self::Expire => 1,
            Self::SolSettle => 2,
            Self::S3Probe => 3,
            Self::LiquidateWait | Self::ExpireWait | Self::Observe => 4,
            Self::Watch => 5,
            Self::Idle => 6,
        }
    }
}

/// One position's evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionEval {
    pub key: Pubkey,
    pub plan: Plan,
    /// Near a threshold (duty 1): live CR below `near_cr_bps`·L, deadline within the window,
    /// or armed.
    pub near: bool,
    pub expired: bool,
    pub armed: bool,
    /// CR on the live state at the risk fee (0 for an undefined quote).
    pub cr_live_bps: u64,
    pub spot_leg: bool,
    pub ema_leg: bool,
    /// A completed bucket fresh within the position's `ref_max_age_s`.
    pub fresh: bool,
    pub buyback_allowed: bool,
    /// `B̂_live` at the live fee (the execution quote).
    pub b_live: Option<u64>,
    /// The liquidation or expiry `max_in` of the plan, if computed.
    pub max_in: Option<u64>,
    /// `B̂_ref` of that bound (the reference buyback at the risk fee), if computed — with
    /// `b_live`, the U28 telemetry (`Metrics::b_ratio_*`).
    pub b_ref: Option<u64>,
    /// `R_due`, `K_s`.
    pub r_due: u64,
    pub k_s: u64,
    /// The market's S3 timer is due for this position on its own "no route now" (no route, the
    /// route-broken flag, or a live fee above the lower of the global hard cap and the cap the
    /// position locked: AIR-19, [`Projection::position_no_route`]).
    pub s3_due: bool,
    /// On a live pump curve, `Q` exceeds the curve's real token reserves `rT`: the position's exit
    /// is pump's synthetic migration (`buy_v3` of `rT` on the curve and `Q − rT` from the would-be
    /// pool, priced `B̂_v3`, pump-final §5.2) and completes the curve. Informational: the plan is
    /// the ordinary one ([module docs](self)); the keeper counts such exits
    /// (`synthetic_exits`).
    pub synthetic: bool,
    /// The position waits only for a fresh reference (M7D-2): the completed bucket is stale and
    /// the position is expired, or its EMA leg holds and its live CR at the risk fee is below L
    /// (with a fresh bucket the spot leg would follow the live price) — on a venue that executes,
    /// outside the warm-up. Below the value-at-risk floor the idle crank that brings the
    /// reference is then exit-critical (ARCH §7.1 duty 8).
    pub stale_exit: bool,
}

/// The market-level coverage needs (duties 1, 2, 5; M4F-1's guard coverage).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarketNeeds {
    /// At least one observation this second (duty 1: a near position, or
    /// `|P_spot/P_ema − 1|` above `ema_dev_bps`; duty 5: an S3 due on an executable venue).
    pub per_second: bool,
    /// The market has no route (`s3_no_route`): observe every `no_route_interval_s`.
    pub no_route: bool,
    /// The market has open positions: observe every `idle_interval_s`.
    pub open_positions: bool,
    /// An S3 is due on an executable venue: once per second, never per slot (duty 5).
    pub s3_due_executable: bool,
    /// `|P_spot/P_ema − 1|` above the threshold.
    pub ema_deviates: bool,
    /// The route is gone and no S3 chain runs on chain yet: observe now, so the chain (and
    /// the S3 timer of every liquidatable or expired position) starts at the outage, not up to
    /// `no_route_interval_s` later.
    pub start_chain: bool,
    /// A Raydium market without open positions inside its listing window (M4V-5): observe
    /// every `listing_interval_s`, so the EMA the first open after the window reads follows the
    /// pool and not the spot a listing or an adoption started it at.
    pub listing: bool,
    /// Once per second (M4V-5, M4R): a Raydium market without open positions in the last
    /// [`LISTING_WARMUP_S`] seconds of its listing window, or after it while its post-change
    /// warm-up runs (`ema_warm > 0`; opens refuse `EmaWarmingUp` until then). M6 (a keeper
    /// choice, flagged): a PumpSwap market without open positions while its warm-up runs — after
    /// a graduation, or from the price start of a PumpSwap listing (its first observation starts
    /// the price, marked for the warm-up) — so the first open reads a `P_ema` the pool's own
    /// honest buckets seeded.
    pub warm_up: bool,
    /// The post-change warm-up runs on an executable venue (`ema_warm > 0` after the roll; M4R):
    /// with open positions the market is observed every second (duty 5) until `ema_warm == 0`,
    /// and no liquidation or expiry is sent before.
    pub warming: bool,
    /// The post-change reference guard is set on an executable venue (`ema_guard == 1` after
    /// the roll; M4F-1): with open positions the market stays on the per-second cadence after
    /// the warm-up too, until the guard clears. The guard holds the exit reference at the
    /// ring's liquidity lows and clears once `L_ema` agrees with them; an honest bucket low in
    /// every second keeps a deposit-wrapped crank in the idle seconds from lifting the lows
    /// (`L_mlo`) and clearing it. Exits are sent as usual (the guard gates nothing).
    pub guarded: bool,
    /// Liquidity-triggered coverage runs (FITS-2 (b); `coverage::MarketTrack::liq_covered`): a
    /// foreign bucket liquidity off the keeper's own `√(X·Y)` in the last `liq_cover_s` seconds
    /// on this market with open positions — an LP-wrapped crank (the withdraw drag, the deposit
    /// freeze) — keeps it on the per-second cadence (never per slot), so every second carries an
    /// honest bucket extreme. Only on an executable venue with open positions.
    pub liq_covered: bool,
}

/// Seconds at the end of a listing window observed every second (M4V-5):
/// `EMA_WARMUP_BUCKETS + 2`, so at the window's end the post-change warm-up of a listing or an
/// adoption is over (8 integrations of the last seconds' honest buckets).
pub const LISTING_WARMUP_S: i64 = shortwin_math::consts::EMA_WARMUP_BUCKETS as i64 + 2;

/// Evaluates one position on the projection ([module docs](self)).
pub fn evaluate(
    mv: &MarketView,
    proj: &Projection,
    pv: &PositionView,
    cov: &CoverageConfig,
) -> anyhow::Result<PositionEval> {
    let pos = &pv.pos;
    let st = &mv.state;
    let m = &proj.market;
    let obs = &proj.obs;
    let now = proj.now;
    let q = pos.q;
    // AIR-10: the program's exit risk fee (`liquidate::assess`, `sol_settle`): the locked hard cap
    // and, on PumpSwap, the tier at `P_ema`'s market cap. At the live tier alone the keeper would
    // see liquidations the program refuses (`NotLiquidatable`) whenever the live market cap sits
    // in a higher-fee tier than `P_ema`'s.
    let cap = st.exit_risk_cap_bps(u64::from(pos.venue_fee_hard_cap_bps));
    let l_bps = pos.cr_liq_bps;
    let math = |e: shortwin_math::MathError| anyhow!("math: {e:?}");

    let r_due = rent::due(&pos.pos_rent(), &m.basis()).map_err(math)?;
    let v_eff = risk::v_eff(pos.v, r_due);
    let k_s = risk::k_s(pos.v, &pos.settle_keeper_terms()).map_err(math)?;
    let budget = Budget {
        v: pos.v,
        k_s,
        ert: pos.ert,
    };
    let expired = now >= pos.deadline;
    let armed = pos.s_armed != 0;
    // The risk view of the venue's own quoter (the program's `engine::bounds` / `arming`):
    // a pool, or the curve with `B̂_v3` above a live curve's `rT` (pump-final §5.2; ARCH §4.3).
    let risk_pricing = st.quoter().map(|p| p.capped(cap));

    // Live CR at the risk fee (duty 1's "live CR").
    let b_live_risk = st.buy_risk(q, cap).map(|b| b.cost);
    let cr_live_bps = risk::cr_bps(v_eff, b_live_risk).unwrap_or(0);
    let near_cr = u128::from(cr_live_bps).saturating_mul(10_000)
        < u128::from(l_bps).saturating_mul(u128::from(cov.near_cr_bps));
    let near_deadline = pos.deadline.saturating_sub(now) <= cov.deadline_window_s;

    // The legs (ARCH §4.5), on the projection. The spot leg at `max(L_live, L_ema')` (FITS-2 (a),
    // the program's `assess`: a withdrawal inside a transaction cannot make it true).
    let fresh = obs.last.is_fresh(now, pos.ref_max_age_s);
    let p_live = st.price();
    let l_live = st.liquidity().unwrap_or(obs.ema.liq);
    let l_spot = bounds_math::spot_liquidity(l_live, &obs.ema, &obs.ring).map_err(math)?;
    let p_rs = p_live.and_then(|p| observe::robust_spot(p, &obs.last, now, pos.ref_max_age_s));
    let b_rs = match (risk_pricing, p_rs) {
        (Some(pr), Some(p)) => pr.buy_repriced(q, l_spot, p).map(|b| b.cost),
        _ => None,
    };
    let spot_leg =
        risk::spot_leg(st.executable, p_rs.is_some(), v_eff, b_rs, l_bps).map_err(math)?;
    let ema_leg = persist::liq_ema_leg(
        &obs.ring,
        risk::ring_below(v_eff, l_bps, |x, y| {
            if x == 0 {
                return None;
            }
            risk_pricing
                .and_then(|pr| pr.buy_xy(q, x, y))
                .map(|b| b.cost)
        }),
        pos.liq_persist_n,
    )
    .map_err(math)?;
    let cr_liquidatable = spot_leg && ema_leg;
    let liquidatable = risk::liquidatable(spot_leg, ema_leg, now, pos.deadline);

    // Normal buyback allowed (execution quote at the live fee).
    let b_live_q = st.buy(q);
    let b_live = b_live_q.map(|b| b.cost);
    let sigma_live = match p_live {
        Some(p) => risk::sigma(b_live_q.map(|b| b.net), q, p).map_err(math)?,
        None => u64::MAX,
    };
    let buyback_allowed = st.executable
        && risk::buyback_allowed(&budget, b_live, sigma_live, u32::from(pos.max_liq_slip_bps))
            .map_err(math)?;

    // The S3 timer (the market's chain, the position's own "no route now" — AIR-19: the fee
    // judged at its locked cap, as `sol_settle` passes it — and its locked timers).
    let chain = arming::s3_chain(m, proj.position_no_route(st, pos.venue_fee_hard_cap_bps));
    let s3_timer = settle::s3_due(&chain, &pos.s3_terms(), now, true);
    let s3_due = s3_timer && (liquidatable || !st.executable);

    // pump-final §5.2: a live curve's exit past `rT` is pump's synthetic migration, priced
    // `B̂_v3` by the quotes above; it plans as any exit (owner D3) and completes the curve.
    let synthetic = mv.kind == VenueKind::PumpCurve
        && st
            .curve
            .as_ref()
            .is_some_and(|c| !c.live.complete && u128::from(q) > c.live.rt);

    let mut max_in = None;
    let mut b_ref_seen = None;
    // M4R: the post-change warm-up gates keeper exits on an executable venue.
    let warming = st.executable && m.ema_warm > 0;
    let plan = if !st.executable {
        if s3_timer {
            Plan::S3Probe
        } else if near_deadline || armed {
            Plan::Watch
        } else {
            Plan::Idle
        }
    } else if warming {
        Plan::Watch
    } else if !liquidatable && !armed {
        if near_cr || near_deadline {
            Plan::Watch
        } else {
            Plan::Idle
        }
    } else if !liquidatable {
        // Armed, not liquidatable: the call keeps the clock or disarms (box evidence).
        Plan::SolSettle
    } else if !fresh {
        if armed {
            Plan::SolSettle
        } else {
            Plan::Observe
        }
    } else if buyback_allowed {
        let b = b_live.unwrap_or(u64::MAX);
        let (bound, ok, wait) = if expired && !cr_liquidatable {
            let r = bounds::expiry_reference(pos, obs, st, now, proj.depth_cap)
                .map_err(|e| anyhow!("{e}"))?;
            b_ref_seen = r.and_then(|r| r.b_ref);
            let mi = match b_ref_seen {
                Some(b_ref) => {
                    bounds::max_in_exp(pos, b_ref, k_s, r_due).map_err(|e| anyhow!("{e}"))?
                }
                None => None,
            };
            (mi, Plan::Expire, Plan::ExpireWait)
        } else {
            let r = bounds::liq_reference(pos, obs, st, now, proj.depth_cap)
                .map_err(|e| anyhow!("{e}"))?;
            b_ref_seen = r.and_then(|r| r.b_ref);
            let mi = b_ref_seen.and_then(|b_ref| bounds::max_in_liq(pos, b_ref, k_s));
            (mi, Plan::Liquidate, Plan::LiquidateWait)
        };
        max_in = bound;
        if bound.is_some_and(|mi| b <= mi) {
            ok
        } else if s3_due {
            Plan::SolSettle
        } else {
            wait
        }
    } else {
        Plan::SolSettle
    };

    let stale_exit = st.executable
        && !warming
        && !fresh
        && (expired
            || (ema_leg && b_live_risk.is_some() && u128::from(cr_live_bps) < u128::from(l_bps)));

    Ok(PositionEval {
        key: pv.key,
        plan,
        near: near_cr || near_deadline || armed || warming,
        expired,
        armed,
        cr_live_bps,
        spot_leg,
        ema_leg,
        fresh,
        buyback_allowed,
        b_live,
        max_in,
        b_ref: b_ref_seen,
        r_due,
        k_s,
        s3_due,
        synthetic,
        stale_exit,
    })
}

/// Evaluates every open position of `mv`, most urgent first (liquidations, expiries,
/// settlements, probes; lower live CR first).
pub fn evaluate_all(
    mv: &MarketView,
    proj: &Projection,
    cov: &CoverageConfig,
) -> Vec<Result<PositionEval, (Pubkey, String)>> {
    let mut out: Vec<Result<PositionEval, (Pubkey, String)>> = mv
        .positions
        .iter()
        .map(|pv| evaluate(mv, proj, pv, cov).map_err(|e| (pv.key, e.to_string())))
        .collect();
    out.sort_by_key(|r| match r {
        Ok(e) => (e.plan.rank(), e.cr_live_bps),
        Err(_) => (u8::MAX, u64::MAX),
    });
    out
}

/// The market's coverage needs (duties 1 and 5; the listing window of a Raydium market without
/// positions, M4V-5; the post-change warm-up and reference guard of a market with positions,
/// M4R / M4F-1; duty 2's liquidity-triggered coverage, FITS-2 (b)) from its evaluations;
/// `list_window_s` is `pool_list_window_s`, `liq_covered` whether the market's liquidity-triggered
/// coverage window runs now (`coverage::MarketTrack::liq_covered`).
pub fn market_needs(
    mv: &MarketView,
    proj: &Projection,
    evals: &[PositionEval],
    cov: &CoverageConfig,
    list_window_s: u32,
    liq_covered: bool,
) -> MarketNeeds {
    let open = !mv.positions.is_empty() || proj.market.open_positions > 0;
    let ema_deviates = match (mv.state.price(), proj.obs.ema.price) {
        (Some(spot), ema) if ema > 0 => {
            let diff = spot.abs_diff(ema);
            diff.saturating_mul(10_000) > ema.saturating_mul(u128::from(cov.ema_dev_bps))
        }
        _ => false,
    };
    let s3_due_executable = mv.state.executable && evals.iter().any(|e| e.s3_due);
    let near = evals.iter().any(|e| e.near || e.plan != Plan::Idle);
    // Only a venue that can execute records observations (A64): a halted one is kept on the
    // no-route cadence (duty 5), never per second.
    let executable = mv.state.executable;
    // M4R: the post-change warm-up (a listing, an adoption) on an executable venue; M4F-1: the
    // reference guard it sets, until it clears.
    let warming = executable && proj.market.ema_warm > 0;
    let guarded = executable && proj.market.ema_guard != 0;
    // FITS-2 (b): the liquidity-triggered window, on an executable venue with open positions.
    let liq_covered = executable && open && liq_covered;
    // M4V-5: a Raydium market without positions, in its listing window or not yet warmed up.
    // M6: a PumpSwap market without positions while its warm-up runs (the projection has started
    // the price of a PumpSwap market that had none).
    let (listing, warm_up) = if mv.kind.is_raydium() && !open && executable {
        let m = &proj.market;
        let end = m.pool_seen_at.saturating_add(i64::from(list_window_s));
        let warm = m.ema_warm == 0;
        let inside = proj.now < end;
        (
            inside,
            proj.now >= end.saturating_sub(LISTING_WARMUP_S) && (inside || !warm),
        )
    } else if mv.kind == VenueKind::PumpSwap && !open && executable {
        (false, proj.market.ema_warm > 0)
    } else {
        (false, false)
    };
    MarketNeeds {
        per_second: executable
            && (open && (near || ema_deviates || warming || guarded || liq_covered)
                || s3_due_executable),
        no_route: proj.s3_no_route && open,
        start_chain: proj.s3_no_route && open && mv.market.no_route_since == 0,
        open_positions: open,
        s3_due_executable,
        ema_deviates,
        listing,
        warm_up,
        warming,
        guarded,
        liq_covered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::AnchorDeserialize;
    use bytemuck::Zeroable;
    use shortwin::state::ShortPosition;
    use shortwin::venues::pump_amm::PoolVenue;
    use shortwin_math::fees::FeeBps;
    use shortwin_math::quote::Pricing;

    /// The projection of a market as read (no prologue: the test sets what it reads), under a
    /// global hard cap of 1,000 bps.
    fn projection(market: Market) -> Projection {
        let mut config = GlobalConfig::zeroed();
        config.params.venue_fee_hard_cap_bps = 1_000;
        Projection {
            now: 1_000,
            slot: 0,
            obs: market.observation(),
            market: Box::new(market),
            s3_no_route: false,
            pool_fail: false,
            depth_cap: None,
            config: Box::new(config),
        }
    }

    /// AIR-10: on PumpSwap the exits' risk fee is the position's locked hard cap and at most the
    /// tier at `P_ema`'s market cap (`VenueState::exit_risk_cap_bps`), as `liquidate::assess` and
    /// `sol_settle` price it. A pool whose live tier is 50 bps and whose tier at `P_ema` is 30: the
    /// keeper's live CR at the risk fee is the 30-bps quote's and the position, 0.2 % over the
    /// near-threshold CR there, is `Idle`; at the live 50 bps it would be near (`Watch`). With the
    /// fee at `P_ema` unresolved (`u32::MAX`), or above the live tier, the hard cap alone applies:
    /// the live 50 bps. At the hard cap alone (the keeper before AIR-10) the first case is `Watch`.
    #[test]
    fn air10_pumpswap_exits_are_judged_at_most_at_the_tier_at_p_ema() {
        let fees = FeeBps::new(20, 5, 25);
        let mut st = VenueState::not_executable(VenueKind::PumpSwap);
        st.executable = true;
        st.x = 1_000_000_000_000_000;
        st.y = 100_000_000_000;
        st.r = st.y;
        st.pricing = Some(Pricing::PumpSwap(fees));
        st.fee_ppm = 5_000;
        st.pool = Some(PoolVenue {
            disable_flags: 0,
            boost_amount: 0,
            virtual_part: 0,
            is_cashback: false,
            fees: Some(fees),
            fee_ppm_at_ema: 3_000,
        });
        let hard_cap = 1_000u16;
        assert_eq!(st.exit_risk_cap_bps(u64::from(hard_cap)), 30);
        let q = st.x / 1_000;
        let cov = CoverageConfig::default();
        let cost = |st: &VenueState, cap: u64| st.buy_risk(q, cap).expect("a quote").cost;
        let (b30, b50) = (cost(&st, 30), cost(&st, 50));
        assert!(b30 < b50 && b50 == cost(&st, u64::from(hard_cap)));
        // `L` 100 %: near below a live CR of 13,000 bps (`near_cr_bps` 130 %); `v` the smallest
        // that holds 13,000 bps at the 30-bps quote.
        let l_bps = 10_000u32;
        let near = u128::from(l_bps) * u128::from(cov.near_cr_bps) / 10_000;
        let v = u64::try_from((near * u128::from(b30)).div_ceil(10_000)).unwrap();
        let mut pos = ShortPosition::deserialize(&mut &[0u8; 1_024][..]).unwrap();
        pos.q = q;
        pos.v = v;
        pos.cr_liq_bps = l_bps;
        pos.venue_fee_hard_cap_bps = hard_cap;
        pos.deadline = 1_000_000;
        pos.liq_persist_n = 3;
        pos.ref_max_age_s = 2;
        pos.max_liq_slip_bps = 1_000;
        let pv = PositionView {
            key: Pubkey::new_unique(),
            pos,
            lamports: 0,
        };
        let proj = projection(Market::zeroed());
        let eval = |st: &VenueState| {
            let mv = MarketView {
                key: Pubkey::new_unique(),
                market: proj.market.clone(),
                vault: Box::new(shortwin_math::pool::Vault::new()),
                kind: VenueKind::PumpSwap,
                read_keys: Vec::new(),
                read_accounts: Vec::new(),
                state: st.clone(),
                positions: vec![pv.clone()],
                extras: None,
                supply: None,
            };
            evaluate(&mv, &proj, &pv, &cov).expect("an evaluation")
        };
        let cr = |b: u64| risk::cr_bps(v, Some(b)).unwrap();
        assert!(u128::from(cr(b30)) >= near && u128::from(cr(b50)) < near);

        let e = eval(&st);
        assert_eq!(e.cr_live_bps, cr(b30), "the risk fee is the tier at P_ema");
        assert_eq!(e.plan, Plan::Idle);
        assert!(!e.near && !e.spot_leg && !e.ema_leg);

        // Unresolved: the hard cap; a tier at `P_ema` above the live one caps nothing.
        for (at_ema, risk_cap) in [(u32::MAX, 1_000), (8_000, 80)] {
            let mut st = st.clone();
            if let Some(p) = st.pool.as_mut() {
                p.fee_ppm_at_ema = at_ema;
            }
            assert_eq!(st.exit_risk_cap_bps(u64::from(hard_cap)), risk_cap);
            let e = eval(&st);
            assert_eq!(e.cr_live_bps, cr(b50), "{at_ema}: the live tier");
            assert_eq!((e.plan, e.near), (Plan::Watch, true), "{at_ema}");
        }
    }

    /// AIR-19: the market's S3 chain judges the live fee at the lower of the global hard cap and
    /// the lowest cap an open position locked (the prologue's `s3_no_route`); a position's own
    /// "no route now" at the lower of the global cap and its own locked cap, whatever the mark —
    /// `sol_settle`'s input of the S3 timer, which the keeper's projection now takes.
    #[test]
    fn a_positions_own_route_takes_its_locked_cap() {
        let mut config = GlobalConfig::zeroed();
        config.params.venue_fee_hard_cap_bps = 1_000;
        let mut market = Market::zeroed();
        market.min_open_hard_cap_bps = 500;
        let proj = Projection {
            now: 0,
            slot: 0,
            obs: market.observation(),
            market: Box::new(market),
            s3_no_route: true,
            pool_fail: false,
            depth_cap: None,
            config: Box::new(config),
        };
        let mut st = VenueState::not_executable(VenueKind::RaydiumCpmm);
        st.executable = true;
        // 8 %: above the mark and a 500-bps position's cap, under the global 1,000 bps.
        st.fee_ppm = 80_000;
        assert!(
            route::s3_no_route(&proj.config, &proj.market, &st),
            "the market's chain runs at the mark"
        );
        assert!(proj.position_no_route(&st, 500));
        assert!(
            !proj.position_no_route(&st, 1_000),
            "a route for the position that locked 1,000 bps"
        );
        st.fee_ppm = 120_000;
        assert!(proj.position_no_route(&st, 1_000), "above the global cap");
        st.fee_ppm = 40_000;
        assert!(!proj.position_no_route(&st, 500), "within both caps");
        st.executable = false;
        assert!(
            proj.position_no_route(&st, 1_000),
            "a venue that cannot execute is no route for every position"
        );
    }
}
