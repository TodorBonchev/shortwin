//! Shared machinery of the graduation fork E2E (M6; ARCH §2.5 "On → PumpSwap", §5.4, §8.2
//! T-VENUE-SWITCH / T-STRADDLE, §8.4 "fork E2E", §9 M6 row; ECON §10.1, §10.2): the **release**
//! short.win on the **dumped** pump, pump-fees and PumpSwap ELFs, a curve market of
//! `pump_curve_support.rs` (ECON's `D@x` markets, [`Sim`]) taken through a **real** graduation —
//! the curve completed by other users' `buy_v2` (or an attacker's), pump's `migrate_v2` creating the
//! canonical pool, the keeper's crank moving the market `→ PumpSwap` — and then traded on the pool:
//! other users' PumpSwap `buy` / `sell`, pump's **boost** spent with the real `boost_buy_and_burn`
//! (the boost authority's signature forged, nothing else planted), the keeper's exits on the pool.
//! Every instruction goes through `Env::send` (or `fork::admin_send` for the boost), so the
//! invariant hooks run after each one.
//!
//! `tests/graduation_e2e.rs` and `tests/graduation_scenarios.rs` include this file with `#[path]`;
//! compiled on its own it is an empty test binary. It includes `pump_curve_support.rs` (the curve
//! phase: [`Sim`], [`status`], [`CurveCtx`], the scenario paths) and adds:
//!
//! - the boost: [`boost_ix`] (`boost_buy_and_burn`, the PPD IDL's 13 accounts), [`boost`] (sent with
//!   the live `GlobalConfig.boost_authority` forged as the signer) and [`BoostSchedule`] — the sim's
//!   boost (`CPPool.start_boost` / `boost_tick`: `⌈vault/40⌉` every `360/40` s from the second after
//!   the graduation), spent on the ELF;
//! - [`pool_status`]: the keeper's view of a position on the pool from `shortwin-math` and the
//!   harness's own venue view (`invariants::venue_view`; never the program's adapter) — the same
//!   [`Status`] as the curve's: the execution and risk quotes on `(base vault, WSOL vault + VQR)` at
//!   the live fee (risk: capped at the position's hard cap), the robust spot leg on
//!   `max(L_live, L_ema')`, the EMA leg on the newest ring entries' cached `(x, y)`;
//! - [`Grad`], the driver of a market through its graduation: the keeper's crank on the curve's
//!   ReadSet until the transition, on the pool's after it (the existence requirement's accounts
//!   created the second the market moves, as the keeper's post-migration duty, ARCH §7.1 / §5.4
//!   tx3), the boost ticks, other users' PumpSwap trades toward a target price
//!   ([`Grad::pool_move_to_price`], `CPPool.move_to_price`), and [`Grad::run`], the replay of
//!   `shortwin_sim.run` for ECON §10.1's `GRAD_SCENARIOS` (the completion at `TE`, `migrate_v2`
//!   `migration_delay_s` = 2 s later, the AMM ramps, the keeper's cadence and one-second latency,
//!   exits on whichever venue the market is on);
//! - [`check_pool_liquidation`]: an executed liquidation on the pool, leg by leg, against the
//!   pool's own quote.

#![allow(
    dead_code,
    unused_imports,
    clippy::result_large_err,
    clippy::too_many_arguments
)]

#[path = "pump_curve_support.rs"]
pub mod curve;

pub use curve::*;

pub use shortwin::constants::{
    PUMP_AMM_EVENT_AUTHORITY, PUMP_AMM_GLOBAL_CONFIG, PUMP_AMM_PROGRAM_ID, TOKEN_PROGRAM_ID,
    WSOL_MINT,
};
pub use shortwin_litesvm_tests::pamm::{PammPool, PammTrade, PammUser};
pub use shortwin_math::fees::FeeBps;
pub use shortwin_math::quote::Pricing;
pub use shortwin_venue_state::pump_amm as vamm;

use shortwin_math::num::{Rounding, mul_div, sqrt_u128};
use shortwin_math::{bounds, observe, persist, quote, rent, risk};

/// `sha256("global:boost_buy_and_burn")[..8]` (PPD `idl/pump_amm.json`).
pub const BOOST_BUY_AND_BURN: [u8; 8] = [105, 68, 6, 175, 0, 7, 35, 162];
/// The sim's boost: `BOOST_CALLS` equal chunks over `BOOST_DURATION_S` (ECON §10.1: "the boost
/// spent over 360 s"; mainnet: ~40 calls within 6 minutes, R-VEN §1.4).
pub const BOOST_CALLS: u64 = 40;
pub const BOOST_DURATION_S: i64 = 360;
/// The sim's `migration_delay_s` (ECON §10.1: "migration 2 s after completion").
pub const MIGRATION_DELAY_S: i64 = 2;
/// ARCH §6.1's PROPOSED keeper-exit CU limit on PumpSwap (the pamm harness's).
pub const POOL_KEEPER_EXIT_CU: u32 = 400_000;
/// `migrate_v2`'s CU limit (≈ 330 k on the fork).
pub const MIGRATE_CU: u32 = 600_000;

fn meta(key: Pubkey, writable: bool) -> AccountMeta {
    if writable {
        AccountMeta::new(key, false)
    } else {
        AccountMeta::new_readonly(key, false)
    }
}

// ---- the boost ------------------------------------------------------------------------------

/// The live `GlobalConfig.boost_authority` (the only signer `boost_buy_and_burn` accepts).
pub fn boost_authority(svm: &litesvm::LiteSVM) -> Pubkey {
    let acc = svm
        .get_account(&PUMP_AMM_GLOBAL_CONFIG)
        .expect("GlobalConfig");
    let g = vamm::read_global_config(&acc.owner.to_bytes(), &acc.data).expect("GlobalConfig");
    Pubkey::new_from_array(g.boost_authority())
}

/// PumpSwap `boost_buy_and_burn(quote_amount_in, min_base_amount_burned)` of `p` (PPD IDL: pool,
/// authority (W, S), `global_config`, base mint (W), quote mint, both vaults (W), the boost vault
/// authority PDA `["boost_vault", pool]`, its WSOL ATA (W), both token programs, event authority,
/// program): a zero-fee buy with the boost vault's WSOL whose tokens are burned (R-VEN §1.4, H9).
pub fn boost_ix(p: &PammPool, authority: &Pubkey, quote_in: u64, min_burn: u64) -> Instruction {
    let mut data = BOOST_BUY_AND_BURN.to_vec();
    data.extend_from_slice(&quote_in.to_le_bytes());
    data.extend_from_slice(&min_burn.to_le_bytes());
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            meta(p.pool, false),
            AccountMeta::new(*authority, true),
            meta(PUMP_AMM_GLOBAL_CONFIG, false),
            meta(p.mint, true),
            meta(WSOL_MINT, false),
            meta(p.base_vault, true),
            meta(p.quote_vault, true),
            meta(p.boost_vault, false),
            meta(p.boost_vault_ata, true),
            meta(p.base_program, false),
            meta(TOKEN_PROGRAM_ID, false),
            meta(PUMP_AMM_EVENT_AUTHORITY, false),
            meta(PUMP_AMM_PROGRAM_ID, false),
        ],
        data,
    }
}

/// The boost authority's `boost_buy_and_burn(quote_in)` on `p` (its signature forged:
/// `fork::admin_send`, the hooks included; the harness payer pays the fee).
pub fn boost(env: &mut Env, p: &PammPool, quote_in: u64) -> TxResult {
    let auth = boost_authority(&env.svm);
    fork::admin_send(env, boost_ix(p, &auth, quote_in, 0), &[auth])
}

/// The sim's boost schedule (`CPPool.start_boost` / `boost_tick`): `chunk = ⌈vault/40⌉` every
/// `interval = 360/40 = 9` s, the first at `next` = the second after the graduation; a tick
/// spends every chunk due (a request above the vault's remainder spends the remainder, and the
/// call that empties the vault closes its WSOL ATA, R-VEN H9).
#[derive(Clone, Copy, Debug)]
pub struct BoostSchedule {
    pub chunk: u64,
    pub interval: i64,
    pub next: i64,
    /// Calls sent so far.
    pub calls: u32,
}

impl BoostSchedule {
    /// The schedule of `p`'s boost vault now, starting the second after chain time `t_grad`.
    pub fn start(env: &Env, p: &PammPool, t_grad: i64) -> Self {
        let vault = p.boost_amount(&env.svm);
        Self {
            chunk: vault.div_ceil(BOOST_CALLS),
            interval: (BOOST_DURATION_S / BOOST_CALLS as i64).max(1),
            next: t_grad + 1,
            calls: 0,
        }
    }

    /// Spends every chunk due at chain time `now`.
    pub fn tick(&mut self, env: &mut Env, p: &PammPool) {
        let now = env.now();
        while now >= self.next {
            let left = p.boost_amount(&env.svm);
            if left == 0 {
                return;
            }
            ok(boost(env, p, self.chunk.min(left)));
            self.calls += 1;
            self.next += self.interval;
        }
    }
}

// ---- the pool as short.win quotes it ----------------------------------------------------------

/// PumpSwap's pricing reserves of `p` now as `(X, Y_eff)`.
pub fn pool_xy(env: &Env, p: &PammPool) -> (u128, u128) {
    let r = p.reserves(&env.svm).expect("reserves");
    (u128::from(r.base), r.effective_quote)
}

/// The pool's spot `⌊Y_eff·WAD/X⌋` (the sim's `CPPool.spot_wad`).
pub fn pool_spot(env: &Env, p: &PammPool) -> u128 {
    let (x, y) = pool_xy(env, p);
    y * W / x
}

/// The base reserve at which a constant-product pool `(x, y)` trades at `target` (`CPPool.
/// move_to_price`: `isqrt(x·y·WAD // target)`).
pub fn target_base(x: u128, y: u128, target: u128) -> u128 {
    sqrt_u128(
        mul_div(x * y, W, target.max(1), Rounding::Floor).expect("k·WAD/P"),
        Rounding::Floor,
    )
}

/// [`Status`] of `borrower`'s position `nonce` on its PumpSwap market `pam` now: the keeper's view
/// from `shortwin-math` on the harness's venue view (module docs; ARCH §4.5; ECON §5.5).
pub fn pool_status(env: &Env, pam: &PammMarket, borrower: &Pubkey, nonce: u64) -> Status {
    let m = env.market(&pam.market());
    let pos = env.position(pam, borrower, nonce);
    let now = env.now();
    let q = pos.q;
    let obs = m.observation();
    let r_due = rent::due(&pos.pos_rent(), &m.basis()).expect("rent");
    let v_eff = risk::v_eff(pos.v, r_due);
    let k_s = k_s(&pos);
    let cap = u64::from(pos.venue_fee_hard_cap_bps);
    let l_bps = pos.cr_liq_bps;
    let view = venue_view::view(&env.svm, &m).unwrap_or(venue_view::VenueView::DEAD);
    let executable = view.executable;
    let (b_live, b_live_risk, sigma_live, p_live, l_live, risk_pricing) =
        match (executable, view.reserves, view.pricing) {
            (true, Some((x, y)), Some(pr)) => {
                let (x, y) = (u128::from(x), u128::from(y));
                let b = pr.buy(q, x, y);
                let capped = pr.capped(cap);
                let br = capped.buy(q, x, y).map(|b| b.cost);
                let sigma = b.map_or(u64::MAX, |b| {
                    risk::sigma_xy(Some(b.net), q, x, y).expect("σ")
                });
                (
                    b.map(|b| b.cost),
                    br,
                    sigma,
                    Some(risk::price_wad(x, y).expect("price")),
                    quote::liquidity(x, y).expect("L"),
                    capped,
                )
            }
            // Not executable: the adapter's cap-only constant-product risk view (as the curve's).
            _ => (
                None,
                None,
                u64::MAX,
                None,
                obs.ema.liq,
                Pricing::PumpSwap(FeeBps::new(0, cap, 0)),
            ),
        };
    let fresh = obs.last.is_fresh(now, pos.ref_max_age_s);
    let p_rs = p_live.and_then(|p| observe::robust_spot(p, &obs.last, now, pos.ref_max_age_s));
    let l_spot = bounds::spot_liquidity(l_live, &obs.ema, &obs.ring).expect("L_spot");
    let b_rs = p_rs.and_then(|p| risk_pricing.buy_repriced(q, l_spot, p).map(|b| b.cost));
    let spot_leg = risk::spot_leg(executable, fresh, v_eff, b_rs, l_bps).expect("spot leg");
    let quote_at = |x: u128, y: u128| {
        if x == 0 {
            None
        } else {
            risk_pricing.buy(q, x, y).map(|b| b.cost)
        }
    };
    let ema_leg = persist::liq_ema_leg(
        &obs.ring,
        |x, y| risk::cr_below(v_eff, quote_at(x, y), l_bps),
        pos.liq_persist_n,
    )
    .expect("EMA leg");
    let budget = risk::Budget {
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

/// An executed liquidation on the pool, checked leg by leg against the pool's own quote taken
/// just before it (`b_exec`: `quote::sol_leg` on the readers' state at the live fee) and the
/// position (ARCH §3.6; ECON §8.3): `B` = the quote, the fixed-δ bound `max_in`, the penalty on
/// `min(B̂_live,risk, B̂_ref)` and its 50/30/20 split, the keeper floor, the waterfall summing to
/// `V`, `q` back in the lending vault.
pub fn check_pool_liquidation(ev: &Liquidate, pos: &ShortPosition, b_exec: u64, vault_delta: u64) {
    assert!(!ev.expired);
    assert_eq!(ev.buyback_cost, b_exec, "B = the pool's quote");
    assert_eq!(ev.b_live, b_exec, "bounded by B̂_live");
    let k_s = k_s(pos);
    assert_eq!(
        ev.max_in,
        ((u128::from(ev.b_ref) * 10_200 / 10_000) as u64).min(pos.v - k_s - pos.ert),
        "max_in: the fixed 2 % over B̂_ref within V − K_s − ert"
    );
    assert!(ev.buyback_cost <= ev.max_in);
    let base = ev.b_live_risk.min(ev.b_ref);
    let rest = pos.v - ev.buyback_cost - ev.rent;
    let pen = (u128::from(base) * u128::from(pos.penalty_bps)).div_ceil(10_000) as u64;
    assert_eq!(
        ev.penalty,
        pen.min(rest),
        "penalty on min(B̂_live,risk, B̂_ref)"
    );
    assert_eq!(ev.penalty_keeper, ev.penalty * 3_000 / 10_000);
    assert_eq!(ev.penalty_protocol, ev.penalty * 2_000 / 10_000);
    assert_eq!(
        ev.penalty_lender,
        ev.penalty - ev.penalty_keeper - ev.penalty_protocol
    );
    if ev.borrower_payout > 0 {
        assert!(
            ev.penalty_keeper + ev.keeper_topup >= k_s,
            "the keeper floor (N22)"
        );
    }
    assert_eq!(
        ev.buyback_cost + ev.rent + ev.penalty + ev.keeper_topup + ev.borrower_payout,
        pos.v,
        "the waterfall sums to V"
    );
    assert!(ev.rent >= pos.r_min);
    assert_eq!(vault_delta, pos.q, "q back in the lending vault");
}

// ---- the graduation driver ----------------------------------------------------------------------

/// ECON §10.1's graduation scenario (`GRAD_SCENARIOS`): one buy completes the curve at `TE`,
/// `migrate_v2` `migration_delay_s` later (`None`: never — the stuck migration), then optionally
/// other users ramp the pool `×k` over `dur` seconds from `TE + 30` (`amm_ramp_events`: a
/// `move_to_price(spot·k^(1/dur))` each second), a 1,800 s window.
#[derive(Clone, Copy, Debug)]
pub struct GradScenario {
    pub complete_at: i64,
    pub migrate_delay: Option<i64>,
    pub ramp: Option<(i64, f64, i64)>,
    pub t_event: i64,
    pub window: i64,
    /// M6-SEC-1 (`attacks/pumpswap_lp.py` part 1): in the `k`-th second after the transition (`k =
    /// 1`: the first post-change bucket) an attacker sends `[sell to f_bps/10⁴ × the pool's spot,
    /// crank_price, buy back]` next to the keeper's crank, so that bucket's low is `f ×` the
    /// price: `(k, f_bps)`.
    pub low: Option<(i64, u128)>,
    /// A genuine dump: other users sell the pool to `f_bps/10⁴ ×` its spot in the first
    /// post-change second (before the keeper's crank) and it stays there.
    pub dump: Option<u128>,
}

impl GradScenario {
    /// `graduation gap (1 buy completes curve)`.
    pub fn gap() -> Self {
        Self {
            complete_at: TE,
            migrate_delay: Some(MIGRATION_DELAY_S),
            ramp: None,
            t_event: TE,
            window: 1_800,
            low: None,
            dump: None,
        }
    }

    /// `graduation, then +kx on AMM over 60s`.
    pub fn amm_ramp(k: f64) -> Self {
        Self {
            ramp: Some((TE + 30, k, 60)),
            ..Self::gap()
        }
    }
}

/// What [`Grad::run`] saw.
pub struct GradRun {
    pub r: RunResult,
    /// Sim seconds of the completion and of the transition (`None` if not reached).
    pub t_complete: Option<i64>,
    pub t_grad: Option<i64>,
    /// The first sim second the keeper's view saw the position actionable.
    pub t_actionable: Option<i64>,
    /// Keeper exit attempts refused `EmaWarmingUp`.
    pub warming_refusals: u32,
}

/// A curve market driven through its graduation (module docs).
pub struct Grad {
    pub s: Sim,
    /// The market on its pool once it moved there.
    pub pam: Option<PammMarket>,
    /// Other users trading the pool.
    pub pool_crowd: Option<PammUser>,
    pub boost: Option<BoostSchedule>,
    /// Chain times of the completion's first crank and of the transition.
    pub t_complete: Option<i64>,
    pub t_grad: Option<i64>,
    /// Whether the harness spends the boost (the sim's schedule).
    pub run_boost: bool,
}

impl Grad {
    /// ECON's `D@x` market (`Sim::d_market`).
    pub fn d_market(sold: u64, lend: u64) -> Self {
        Self::new(Sim::d_market(sold, lend))
    }

    pub fn new(s: Sim) -> Self {
        Self {
            s,
            pam: None,
            pool_crowd: None,
            boost: None,
            t_complete: None,
            t_grad: None,
            run_boost: true,
        }
    }

    pub fn env(&mut self) -> &mut Env {
        &mut self.s.env
    }

    /// The market on its pool (panics before the graduation).
    pub fn pam(&self) -> &PammMarket {
        self.pam.as_ref().expect("graduated")
    }

    pub fn graduated(&self) -> bool {
        self.pam.is_some()
    }

    /// The curve is migrated (`vT = 0` after `migrate_v2`).
    pub fn migrated(&self) -> bool {
        curve(&self.s.env, &self.s.pm).vt == 0
    }

    /// Another user's `buy_v2` of the whole `rT` (the sim's `complete_curve` event).
    pub fn complete_curve(&mut self) {
        let c = curve(&self.s.env, &self.s.pm);
        if !c.complete && c.rt > 0 {
            self.s.crowd_buy(u64::try_from(c.rt).expect("rT"));
        }
    }

    /// `migrate_v2` by a third party (anyone may call it; ARCH §5.4 tx2).
    pub fn migrate(&mut self) -> TransactionMetadata {
        let migrator = self.s.env.funded_keypair();
        let ix = pump::migrate_v2_ix(&self.s.env.svm, &migrator.pubkey(), &self.s.pm.curve);
        ok(self
            .s
            .env
            .send(&[pump::cu_limit_ix(MIGRATE_CU), ix], &[&migrator]))
    }

    /// The keeper's crank on whichever venue the market is on: the curve's ReadSet until the
    /// market moves `→ PumpSwap` (then [`Grad::on_graduated`]), the pool's after.
    pub fn crank(&mut self) -> TransactionMetadata {
        if let Some(pam) = &self.pam {
            return ok(self.s.env.send(&[crank_price_ix(pam)], &[]));
        }
        let meta = self.s.crank();
        let v = venue(&self.s.env, &self.s.pm);
        if v == VenueKind::PumpMigrating && self.t_complete.is_none() {
            self.t_complete = Some(self.s.env.now());
        }
        if v == VenueKind::PumpSwap {
            if self.t_complete.is_none() {
                self.t_complete = Some(self.s.env.now());
            }
            self.on_graduated();
        }
        meta
    }

    /// The second the market moved onto its pool: the market's pool keys, the existence
    /// requirement's accounts (the keeper's post-migration duty), a pool trader for the other
    /// users, and the boost schedule.
    pub fn on_graduated(&mut self) {
        let now = self.s.env.now();
        self.t_grad = Some(now);
        let pam = PammMarket::graduated(&self.s.env.svm, &self.s.pm);
        let payer = self.s.env.payer.pubkey();
        let ixs = pamm::venue_account_ixs(&self.s.env.svm, &payer, &pam);
        ok(self.s.env.send(&ixs, &[]));
        let crowd = PammUser::new(&mut self.s.env, &pam.pool, 50_000 * SOL);
        self.boost = Some(BoostSchedule::start(&self.s.env, &pam.pool, now));
        self.pool_crowd = Some(crowd);
        self.pam = Some(pam);
    }

    /// Spends the boost chunks due now (nothing before the graduation or with `run_boost` off).
    pub fn tick_boost(&mut self) {
        if !self.run_boost {
            return;
        }
        if let (Some(b), Some(pam)) = (self.boost.as_mut(), self.pam.as_ref()) {
            b.tick(&mut self.s.env, &pam.pool);
        }
    }

    /// Other users move the pool toward `target` (`CPPool.move_to_price`: `target_X =
    /// isqrt(X·Y·WAD // p)` on the effective reserves; a buy of `X − target_X`, or a sale of
    /// `target_X − X` capped at `99 %` of the real-reserve floor `X·vault/VQR`).
    pub fn pool_move_to_price(&mut self, target: u128) {
        let pam = self.pam.as_ref().expect("graduated");
        let p = pam.pool;
        let (x, y) = pool_xy(&self.s.env, &p);
        let tx = target_base(x, y, target);
        let crowd = self.pool_crowd.as_ref().expect("pool crowd");
        if tx < x {
            let out = u64::try_from(x - tx).expect("u64");
            ok(crowd.buy(&mut self.s.env, &p, out, u64::MAX));
        } else if tx > x {
            let r = p.reserves(&self.s.env.svm).expect("reserves");
            let vqr = p.state(&self.s.env.svm).virtual_quote_reserves;
            let mut b = tx - x;
            if vqr > 0 {
                b = b.min(x * u128::from(r.real_quote) / vqr.unsigned_abs() * 99 / 100);
            }
            if b > 0 {
                ok(crowd.sell(&mut self.s.env, &p, u64::try_from(b).expect("u64"), 0));
            }
        }
    }

    /// Other users sell the pool (`sell` only, capped as [`Grad::pool_move_to_price`] caps it) to
    /// `target`; returns the base sold.
    pub fn pool_sell_to_price(&mut self, target: u128) -> u64 {
        let pam = self.pam.as_ref().expect("graduated");
        let p = pam.pool;
        let (x, y) = pool_xy(&self.s.env, &p);
        let tx = target_base(x, y, target);
        if tx <= x {
            return 0;
        }
        let r = p.reserves(&self.s.env.svm).expect("reserves");
        let vqr = p.state(&self.s.env.svm).virtual_quote_reserves;
        let mut b = tx - x;
        if vqr > 0 {
            b = b.min(x * u128::from(r.real_quote) / vqr.unsigned_abs() * 99 / 100);
        }
        let b = u64::try_from(b).expect("u64");
        let crowd = self.pool_crowd.as_ref().expect("pool crowd");
        if b > 0 {
            ok(crowd.sell(&mut self.s.env, &p, b, 0));
        }
        b
    }

    /// M6-SEC-1's attack (`attacks/pumpswap_lp.py` part 1): `[sell to f_bps/10⁴ × the pool's spot,
    /// crank_price, buy back]` — a low recorded in this second's bucket, the pool restored (up to
    /// the round trip's fees). Returns the attacker's round-trip cost in lamports.
    pub fn recorded_low(&mut self, f_bps: u128) -> u64 {
        let (pool, crank) = (self.pam().pool, crank_price_ix(self.pam()));
        let spot = pool_spot(&self.s.env, &pool);
        let crowd = self.pool_crowd.as_ref().expect("pool crowd");
        let wsol0 = read::token_amount(&self.s.env.svm, &crowd.wsol).unwrap_or(0);
        let sold = self.pool_sell_to_price(spot * f_bps / 10_000);
        ok(self.s.env.send(&[crank], &[]));
        let crowd = self.pool_crowd.as_ref().expect("pool crowd");
        ok(crowd.buy(&mut self.s.env, &pool, sold, u64::MAX));
        wsol0.saturating_sub(read::token_amount(&self.s.env.svm, &crowd.wsol).unwrap_or(0))
    }

    /// The post-change warm-up gate (ARCH §3.6 precondition 1b; the sim's `Market.gated`): on an
    /// executable venue with `ema_warm > 0` nothing is liquidatable for a keeper exit.
    pub fn gated(&self, st: &Status) -> bool {
        st.executable && market(&self.s.env, &self.s.pm).ema_warm > 0
    }

    /// The sim's `st["liq"] or st["expired"]` (its `liq` includes the gate).
    pub fn actionable(&self, st: &Status) -> bool {
        (st.cr_liq() && !self.gated(st)) || st.expired
    }

    /// The keeper's view of `p` on whichever venue the market is on.
    pub fn status(&self, p: &Pos) -> Status {
        match &self.pam {
            Some(pam) => pool_status(&self.s.env, pam, &p.key(), p.nonce),
            None => self.s.status(p),
        }
    }

    /// The keeper's exit this second (`Sim::keeper_exit` on the market's venue; on the pool a
    /// refusal may also be the post-change warm-up's `EmaWarmingUp`, counted in `warming`).
    pub fn keeper_exit(&mut self, p: &Pos, st: &Status, warming: &mut u32) -> Option<Outcome> {
        let Some(pam) = &self.pam else {
            return self.s.keeper_exit(p, st);
        };
        let kp = self.s.keeper.insecure_clone();
        let exit = if !st.executable {
            None
        } else if st.cr_liq() && !self.gated(st) {
            Some(liquidate_ix(pam, &kp.pubkey(), &p.key(), p.nonce))
        } else if st.expired {
            Some(expire_ix(pam, &kp.pubkey(), &p.key(), p.nonce))
        } else {
            None
        };
        if let Some(ix) = exit {
            let res = self
                .s
                .env
                .send(&[pump::cu_limit_ix(POOL_KEEPER_EXIT_CU), ix], &[&kp]);
            match res {
                Ok(meta) => {
                    if events::of::<Liquidate>(&meta).is_empty() {
                        return Some(Outcome::Expired(events::one(&meta)));
                    }
                    return Some(Outcome::Liquidated(events::one(&meta)));
                }
                Err(_) => {
                    let code = custom_code(&res);
                    if code == Some(u32::from(E::EmaWarmingUp)) {
                        *warming += 1;
                    } else {
                        assert!(
                            code == Some(u32::from(E::ExitBoundNotMet))
                                || code == Some(u32::from(E::UseSolSettle)),
                            "keeper exit on the pool refused unexpectedly: {code:?} (status {st:?})"
                        );
                    }
                }
            }
        }
        let ix = sol_settle_ix(&self.s.env, pam, &kp.pubkey(), &p.key(), p.nonce);
        let meta = ok(self.s.env.send(&[ix], &[&kp]));
        events::of::<SolSettle>(&meta).pop().map(Outcome::Settled)
    }

    /// `shortwin_sim.run(market, 1.0, sc, keeper_latency=1)` for a graduation scenario, from the
    /// open of `p` (`Sim::run`'s cadence; module docs). Per second: the completion (at
    /// `complete_at`), the AMM ramp's move, `migrate_v2` once due, the boost chunks due, the
    /// keeper's crank (the transition when the curve is migrated), then the keeper's view and exit.
    pub fn run(&mut self, p: &Pos, sc: &GradScenario) -> GradRun {
        let deadline = self.s.position(p).deadline - self.s.t0;
        let horizon = (sc.t_event + sc.window).min(deadline + 3 * 3_600);
        let mut t = 0i64;
        let mut act_at: Option<i64> = None;
        let mut near = true;
        let mut steps = Vec::new();
        let mut last = self.status(p);
        let mut outcome = None;
        let mut completed = false;
        let mut ramp_step = 0i64;
        let mut t_actionable = None;
        let mut warming = 0u32;
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
            self.s.at(t);
            if !completed && t >= sc.complete_at {
                completed = true;
                self.complete_curve();
            }
            if let Some((t0, k, dur)) = sc.ramp {
                // `amm_ramp_events`: one move per event second in [t0, t0 + dur), every event due
                // by now fires (as the sim's event loop), on the pool only.
                let step = k.powf(1.0 / dur as f64);
                while ramp_step < dur && t0 + ramp_step <= t {
                    if self.graduated() {
                        let target =
                            (pool_spot(&self.s.env, &self.pam().pool) as f64 * step) as u128;
                        self.pool_move_to_price(target);
                    }
                    ramp_step += 1;
                }
            }
            if !self.graduated()
                && let (Some(d), Some(tc)) = (sc.migrate_delay, self.t_complete)
                && self.s.env.now() >= tc + d
                && !self.migrated()
            {
                self.migrate();
            }
            // The post-change second of the pool (1 = the first bucket after the transition).
            let k = self.t_grad.map(|g| self.s.env.now() - g);
            if let (Some(f), Some(1)) = (sc.dump, k) {
                let spot = pool_spot(&self.s.env, &self.pam().pool);
                self.pool_sell_to_price(spot * f / 10_000);
            }
            self.tick_boost();
            self.crank();
            if let Some((sec, f)) = sc.low
                && k == Some(sec)
            {
                self.recorded_low(f);
            }
            steps.push(t);
            let st = self.status(p);
            near = self.s.keeper_near(p, &st);
            let due = self.actionable(&st);
            if due && t_actionable.is_none() {
                t_actionable = Some(t);
            }
            if act_at.is_none() && due {
                act_at = Some(t + 1);
            }
            if act_at.is_some_and(|a| t >= a) {
                if let Some(o) = self.keeper_exit(p, &st, &mut warming) {
                    last = st;
                    outcome = Some(o);
                    break;
                }
                let again = self.status(p);
                if !self.actionable(&again) {
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
        GradRun {
            r: RunResult {
                t,
                outcome,
                status: last,
                steps,
                dust_skipped: Vec::new(),
            },
            t_complete: self.t_complete.map(|c| c - self.s.t0),
            t_grad: self.t_grad.map(|g| g - self.s.t0),
            t_actionable,
            warming_refusals: warming,
        }
    }
}
