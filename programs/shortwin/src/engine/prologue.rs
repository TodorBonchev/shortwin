//! Market prologue (ARCH §3.0; ECON §2.1 order of operations).
//!
//! Every instruction that loads a `Market` for writing runs, in order: (1) load `config` and
//! `market`, check versions (the Accounts constraints); (2) params-version sync
//! ([`sync_market_params`]: the EMA fields when `open_positions == 0` or `now ≥
//! ema_params.force_at`; the kink only when `open_positions == 0`, a forced EMA adoption never
//! touches it; A17, A59, R3A-9); (3) the mint guard ([`crate::engine::guard::mint_guard`],
//! the caller's: it needs the mint account and the instruction's path); (4) accrue
//! ([`accrue`]: the vest release (`pool::release`) then rent (`rent::accrue` on `P_ema`
//! **before** the roll, `U` from the pre-instruction counters; the lender credit to `acc` and
//! `receivable`)); (5) the observation roll ([`crate::engine::observe::roll`]: integrate a
//! completed bucket with the cached reprojection, `CoverageGap`); (6) the venue state from the
//! ReadSet (the caller parses it, `crate::venues::parse`; reader failures of correctly keyed
//! accounts are "not executable", A63) and `sync_venue` (the mock and Raydium stay; a curve
//! market moves to `PumpMigrating` when its curve completes, to `PumpSwap` when its canonical
//! pool is valid — M6: the observation state rewritten to the pool and restarted,
//! `venues::pump_amm::restart_observation`, and this instruction neither records nor observes a
//! route on the curve's ReadSet — or to `None` when it completed with zeroed reserves and no
//! canonical pool; `crate::venues::pump_curve::sync`); (7) the observation record, only while the
//! venue is executable — or migrating: the completed curve is final and still recorded (A64,
//! `VenueState::observed`); a PumpSwap market without a price starts it here, marked for the
//! warm-up, at `min(√(X·(Y + VQR)), ⌊D·10⁴/λ_min⌋)` as a Raydium start (`venues::pump_amm::
//! start_price`, M6; the cap M6R-1); (8) the route chains (the S3 chain; on Raydium markets and
//! the mock also `pool_fail`), and on Raydium and — since M6R-1 (owner decision of 2026-09-30,
//! ARCH §2.5, changelog 56) — PumpSwap the refresh of the locked depth `pool_d` while the venue
//! is executable; on PumpSwap while the post-change warm-up runs (`ema_warm > 0` after the roll)
//! also `L_slow = min(L_slow, D)` — the Raydium adoption's `L_slow ≤ D`, taken at the first
//! instruction that reads `D` after a graduation (the transition reads no LP mint), which undoes
//! a deposit between `migrate_v2` and the transition that raised the `lp_supply` anchor.
//! [`run`] does steps 2 and 4–8; then the body; (10) post-conditions; (11) one `emit_cpi!`.
//!
//! A16: `deposit` and `withdraw` run steps 1–5 and 10–11 (no venue accounts, no observation):
//! [`sync_market_params`], [`accrue`] and — since M2 — [`roll_only`]. `claim_sol`,
//! `cover_deficit`, `sync_lender` and `add_margin` do not write `Market` at all. On a
//! list-only (`None`) market there is no price, so rent is 0 and only the vest is released.
//!
//! Every step runs at [`market_now`], never before the market's last accrual.

use anchor_lang::prelude::*;
use shortwin_math::observe::Observation;
use shortwin_math::pool::{self, Pool, Vault};
use shortwin_math::rent;

use crate::engine::{observe, route};
use crate::errors::MathResultExt;
use crate::state::{GlobalConfig, Market, VenueKind};
use crate::venues::pump_curve::{self, Transition};
use crate::venues::{pump_amm, VenueState};

/// The time a `Market`-writing instruction runs at: the clock, never before the market's
/// last accrual. `Clock::unix_timestamp` is not guaranteed to be monotonic, and the vest
/// release refuses a time before `vest_last` (`pool::release`); every pool mutation runs after
/// the prologue's accrual in the same instruction, so `vest_last ≤ last_accrual_ts`, and this
/// keeps deposits and withdrawals live across a clock step back.
pub fn market_now(market: &Market, clock_now: i64) -> i64 {
    clock_now.max(market.last_accrual_ts)
}

/// ARCH §3.0 step 2, params-version sync (A17, A59; R3A-9): the EMA fields are adopted when
/// their version changed and the market has no open position or `now ≥ force_at`; the kink
/// only while the market has no open position (a forced EMA adoption never touches it).
pub fn sync_market_params(config: &GlobalConfig, market: &mut Market, now: i64) {
    let ema = &config.ema_params;
    if market.ema_params_version != ema.version
        && (market.open_positions == 0 || now >= ema.force_at)
    {
        market.ema_dtmax_s = ema.ema_dtmax_s;
        market.ema_clamp_bps = ema.ema_clamp_bps;
        market.liq_slow_tau_s = ema.liq_slow_tau_s;
        market.liq_slow_dtmax_s = ema.liq_slow_dtmax_s;
        market.ema_params_version = ema.version;
    }
    let kink = &config.kink;
    if market.kink_version != kink.version && market.open_positions == 0 {
        market.util_kink_bps = kink.util_kink_bps;
        market.kink_version = kink.version;
    }
}

/// ARCH §3.0 step 4, accrue (ECON §7, §9.4): the rent basis indices advance over
/// `now − last_accrual_ts` at `P̃ = min(P_ema, P_ACC_MAX)` (the EMA **before** this
/// instruction's roll) and the utilisation of the pre-instruction counters (`rent::accrue`);
/// the lenders' credit `⌊(Σ_j A_j·δJ_j + rem)/(10⁴·10⁴·31,536,000·WAD)⌋` goes to `acc` and
/// `receivable` after the vest release (`pool::accrue` releases first), and its remainder is
/// stored for the next accrual (`rent::lender_credit_carried`, `Market.lender_credit_rem`;
/// AIR-03: as built each accrual floored alone, so a market cranked every second lost up to a
/// lamport a second of lender rent). On a list-only market (no price) and with no open position
/// both are 0 and only the vest is released. Returns the lender credit.
pub fn accrue(market: &mut Market, pool: &mut Pool, vault: &mut Vault, now: i64) -> Result<u64> {
    let dt = u64::try_from(now.saturating_sub(market.last_accrual_ts)).unwrap_or(0);
    let util = rent::utilization_bps(pool.tokens_free, pool.borrowed_q).math()?;
    let mut basis = market.basis();
    let delta = rent::accrue(
        &mut basis,
        market.ema_price.get(),
        util,
        market.util_kink_bps,
        dt,
    )
    .math()?;
    let (credit, rem) =
        rent::lender_credit_carried(&market.aggr(), &delta, market.lender_credit_rem.get())
            .math()?;
    pool::accrue(pool, vault, now, credit).math()?;
    market.set_basis(&basis);
    market.lender_credit_rem.set(rem);
    market.last_accrual_ts = market.last_accrual_ts.max(now);
    Ok(credit)
}

/// What the prologue saw (ARCH §3.0 steps 1–8).
#[derive(Clone, Debug)]
pub struct Prologue {
    /// [`market_now`].
    pub now: i64,
    /// The lender pool after the accrual (the body continues from it and stores it).
    pub pool: Pool,
    /// The venue transition of step 6's `sync_venue` (ARCH §2.5), if any: the market's `venue`
    /// already holds `to` (the caller emits `VenueStateChanged`, or refuses a trade on it).
    pub transition: Option<Transition>,
    /// `s3_no_route` now (ARCH §5.3); `true` without a ReadSet.
    pub s3_no_route: bool,
    /// `pool_fail` now; `true` without a ReadSet.
    pub pool_fail: bool,
    /// The pre-roll EMA gap if a bucket was integrated.
    pub rolled: Option<i64>,
    /// This instruction recorded its pre-trade observation.
    pub recorded: bool,
    /// A route chain started or cleared.
    pub chains_changed: bool,
    /// The rent credited to lenders by the accrual.
    pub lender_credit: u64,
}

/// ARCH §3.0 steps 2 and 4–8 on a loaded market (module docs), with the venue state the caller
/// read from its key-checked ReadSet (`None`: no ReadSet, no record, no chains). The market's
/// params versions, rent basis, lender-pool fields, observation state, venue (step 6's sync)
/// and route chains are written back; the vault math state `vault` is updated in place (the
/// caller stores it, or, on an instruction without `MarketVault`, checks it did not change).
pub fn run(
    config: &GlobalConfig,
    market_key: &Pubkey,
    market: &mut Market,
    vault: &mut Vault,
    state: Option<&VenueState>,
    clock_now: i64,
    slot: u64,
) -> Result<Prologue> {
    let now = market_now(market, clock_now);
    sync_market_params(config, market, now);
    let mut pool = market.pool();
    let lender_credit = accrue(market, &mut pool, vault, now)?;
    let mut obs = market.observation();
    let rolled = observe::roll(
        market_key,
        &mut obs,
        &market.ema_params(),
        market.open_positions,
        now,
        slot,
    )?;
    let (mut recorded, mut s3, mut pf, mut changed) = (false, true, true, false);
    let mut transition = None;
    if let Some(st) = state {
        transition = pump_curve::sync(market, st, now)?;
        if transition.is_some_and(|t| t.to == VenueKind::PumpSwap) {
            // `→ PumpSwap` (M6; ARCH §2.5): the observation state is rewritten to the new pool
            // and restarted (the post-change warm-up), and this instruction's ReadSet — the
            // curve's — is neither recorded nor a route observation of the new venue: the
            // chains the transition cleared start afresh at its first observation.
            pump_amm::restart_observation(&mut obs, pump_curve::graduation_anchor(st))?;
            changed = true;
        } else {
            // A PumpSwap market without a price starts it at its first executable observation,
            // marked for the warm-up (a PumpSwap listing or re-detection, M6), its liquidity
            // capped at the pool's locked depth `D` (M6R-1; over `λ_min` = 100 %, OWNER-M6D-1).
            if market.venue_kind() == Some(VenueKind::PumpSwap) {
                pump_amm::start_price(&mut obs, st, now)?;
            }
            recorded = observe::record(&mut obs, now, st)?;
            let chains = route::observe_chains(config, market, st, now);
            (s3, pf, changed) = (chains.s3_no_route, chains.pool_fail, chains.changed);
            refresh_locked_depth(market, &mut obs, st);
        }
    }
    market.set_observation(&obs);
    market.set_pool(&pool);
    Ok(Prologue {
        now,
        pool,
        transition,
        s3_no_route: s3,
        pool_fail: pf,
        rolled,
        recorded,
        chains_changed: changed,
        lender_credit,
    })
}

/// Raydium and PumpSwap (`VenueKind::has_locked_depth`): the locked depth `pool_d` is refreshed
/// at every executable observation of the pool (ARCH §2.3.4: "refreshed by `crank_price` and
/// `propose_pool` while the incumbent is executable"; here every instruction that reads the
/// ReadSet refreshes it, so `pool_d` is "the incumbent's locked depth at its last executable
/// observation", which the fast path's threshold names, ARCH §2.6 rule 3). The tier, the open
/// caps and the exit reference read it (§3.6, §4.6, §4.7). A pool that cannot execute keeps the
/// last value.
///
/// PumpSwap during the post-change warm-up (`ema_warm > 0` after the roll: a graduation or a
/// price start; M6R-1, owner decision of 2026-09-30, ARCH §2.5, ECON #105): `L_slow = min(L_slow,
/// D)` — the Raydium adoption's `L_slow ≤ D` at the first instruction that reads `D`. `L_slow`
/// does not integrate during the warm-up and `D` moves only by the fee the pool keeps, so the
/// first cap is the one that binds; on an honest graduation it never does (`D ≥` the `lp_supply`
/// anchor `≥ L_slow`), and it undoes a third-party deposit between `migrate_v2` and the
/// transition that raised the anchor (`Market._warmup_slow_cap` of the normative sim).
fn refresh_locked_depth(market: &mut Market, obs: &mut Observation, st: &VenueState) {
    if !st.kind.has_locked_depth() || !st.executable {
        return;
    }
    market.pool_d.set(st.locked_depth);
    if st.kind == VenueKind::PumpSwap && obs.ema.warm > 0 {
        obs.ema.liq_slow = obs.ema.liq_slow.min(st.locked_depth);
    }
}

/// A16's step 5 for the lender instructions (`deposit`, `withdraw`): rolls a completed
/// bucket (integration, ring push, `CoverageGap`) without recording anything.
pub fn roll_only(market_key: &Pubkey, market: &mut Market, now: i64, slot: u64) -> Result<()> {
    if market.b_lo.get() == 0 {
        return Ok(());
    }
    let mut obs = market.observation();
    observe::roll(
        market_key,
        &mut obs,
        &market.ema_params(),
        market.open_positions,
        now,
        slot,
    )?;
    market.set_observation(&obs);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use shortwin_math::num::WAD;

    #[test]
    fn params_sync_follows_versions_and_positions() {
        let mut config = GlobalConfig::zeroed();
        config.ema_params.version = 2;
        config.ema_params.ema_dtmax_s = 1;
        config.ema_params.ema_clamp_bps = 5_000;
        config.ema_params.force_at = 1_000;
        config.kink.version = 3;
        config.kink.util_kink_bps = 7_000;
        let mut m = Market::zeroed();
        m.ema_params_version = 1;
        m.kink_version = 1;
        m.util_kink_bps = 8_000;
        m.open_positions = 2;

        // Positions open, before force_at: nothing is adopted.
        sync_market_params(&config, &mut m, 999);
        assert_eq!((m.ema_params_version, m.kink_version), (1, 1));
        // After force_at the EMA fields are forced; the kink is not (R3A-9).
        sync_market_params(&config, &mut m, 1_000);
        assert_eq!(
            (m.ema_params_version, m.ema_dtmax_s, m.ema_clamp_bps),
            (2, 1, 5_000)
        );
        assert_eq!((m.kink_version, m.util_kink_bps), (1, 8_000));
        // With no open position the kink follows.
        m.open_positions = 0;
        sync_market_params(&config, &mut m, 1_000);
        assert_eq!((m.kink_version, m.util_kink_bps), (3, 7_000));
    }

    #[test]
    fn accrue_on_a_list_only_market_releases_the_vest_only() {
        let mut m = Market::zeroed();
        m.last_accrual_ts = 100;
        m.util_kink_bps = 8_000;
        let mut p = Pool::new();
        p.total_shares = 1_000;
        p.tokens_free = 1;
        p.vest_bal = 600;
        p.vest_last = 100;
        p.vest_end = 700;
        let mut v = Vault::new();
        v.lender_sol = 600;
        assert_eq!(accrue(&mut m, &mut p, &mut v, 400).unwrap(), 0);
        assert_eq!(
            (p.vest_bal, p.vest_last, m.last_accrual_ts),
            (300, 400, 400)
        );
        assert_eq!(p.acc_r, 300 * WAD / 1_000);
        assert_eq!(m.rent_basis.iter().map(|j| j.get()).sum::<u128>(), 0);
        // Never backwards.
        assert_eq!(market_now(&m, 390), 400);
    }

    #[test]
    fn accrue_credits_rent_from_the_aggregate() {
        // One position: q = 10⁶ at base 5,000 bps (slopes 0), protocol share 1,500 bps, at
        // P_ema = WAD (1 lamport per raw unit), for one day: rent ⌈10⁶·0.5/365⌉ lamports, of
        // which 85 % is credited to lenders (floor).
        let mut m = Market::zeroed();
        m.last_accrual_ts = 0;
        m.util_kink_bps = 8_000;
        m.ema_price.set(WAD);
        let terms = rent::RentTerms {
            base_bps: 5_000,
            slope1_bps: 0,
            slope2_bps: 0,
        };
        let mut aggr = rent::Aggr::default();
        aggr.add(1_000_000, &terms, 1_500).unwrap();
        m.set_aggr(&aggr);
        let mut p = Pool::new();
        p.total_shares = 2_000_000_000;
        p.tokens_free = 1_000_000;
        p.borrowed_q = 1_000_000;
        let mut v = Vault::new();
        let credit = accrue(&mut m, &mut p, &mut v, 86_400).unwrap();
        let expected = 1_000_000u128 * 5_000 * 86_400 * 8_500 / (10_000 * 10_000 * 31_536_000);
        assert_eq!(u128::from(credit), expected);
        assert_eq!(p.receivable, credit);
        assert_eq!(m.basis().j[0], WAD * 86_400);
    }

    /// AIR-03: a market accrued every second (a `crank_price` each second) credits its lenders
    /// the rent of a small position. A 0.1 SOL Tier A position at 15 % APR (U = 10 %) and the
    /// 15 % protocol share credits ≈ 0.40 lamports a second: as built each accrual floored its
    /// credit to 0 and kept no remainder, so a day of one-second accruals credited nothing
    /// while the exit's true-up (`credited_for`, ceil) removes the position's whole exact share,
    /// and the difference stayed in `lender_sol` unclaimable. With the remainder carried in
    /// `Market.lender_credit_rem` the credits sum to the floor of their exact total: at most
    /// one lamport below the true-up, never above it (ECON I-P7).
    #[test]
    fn per_second_accruals_credit_the_rent_of_a_small_position() {
        let terms = rent::RentTerms {
            base_bps: 1_000,
            slope1_bps: 5_000,
            slope2_bps: 100_000,
        };
        let q = 100_000_000_000u64;
        let mut m = Market::zeroed();
        m.util_kink_bps = 8_000;
        // P_ema = 10⁻³ lamports per raw unit: q is worth 0.1 SOL.
        m.ema_price.set(WAD / 1_000);
        let mut aggr = rent::Aggr::default();
        aggr.add(q, &terms, 1_500).unwrap();
        m.set_aggr(&aggr);
        let mut p = Pool::new();
        p.total_shares = 10_000_000_000_000;
        p.tokens_free = 9 * q;
        p.borrowed_q = q;
        let mut v = Vault::new();
        let mut credits = 0u64;
        for now in 1..=86_400 {
            credits += accrue(&mut m, &mut p, &mut v, now).unwrap();
        }
        let pos = rent::PosRent {
            q,
            j_open: [0; 3],
            terms,
            rent_protocol_bps: 1_500,
            r_min: 0,
        };
        let credited = rent::credited_for(&pos, &m.basis()).unwrap();
        assert_eq!(p.receivable, credits);
        assert!(
            credited > 34_000,
            "a day of 0.1 SOL at 15 % APR: {credited}"
        );
        assert!(
            credits <= credited && credited - credits <= 1,
            "per-second accruals credited {credits} of the position's {credited} lamports"
        );
    }
}
