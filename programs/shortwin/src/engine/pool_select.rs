//! Raydium pool selection (ARCH §2.1 `engine/pool_select.rs`, §2.5 `None`, §2.6 as amended by
//! A60, A71 and decision 7, §3.9 `propose_pool` / `execute_pool_switch`, §4.4
//! `rewrite_liquidity`; ECON §6.2, §10.3), with the M4 security review's amendments (M4-SEC-1 to
//! M4-SEC-6, M4-SEC-8; the deviations from ARCH v0.4.1 are listed below). The arithmetic (`D`,
//! the eligibility floor, the margins, the adoption liquidity) is `shortwin_math::pool_select`;
//! the candidate's verification and `eligible()` are the adapters' (`venues::raydium_v4::verify`,
//! `venues::raydium_cpmm::verify`); this module decides what a proposal does and applies an
//! adoption to the market.
//!
//! **What a proposal does** ([`decide`], on the market after the prologue of the proposing
//! instruction, so `pool_d` and the `pool_fail` chain include this instruction's observation of
//! the incumbent):
//!
//! | Market | Condition | Outcome |
//! |---|---|---|
//! | `None` (list-only), no open position | — | adopt ([`POOL_KIND_LISTING`]) |
//! | `None` with open positions (M5: a curve that graduated elsewhere; M4-SEC-4) | a collection window running: `D_cand > fp_d`; none: — | replace the window's candidate / open a window of `pool_fast_window_s` |
//! | Raydium, no open position, inside the listing window | `D_cand ≥ (1 + pool_list_margin_bps/10⁴)·D_inc` | adopt ([`POOL_KIND_LISTING_WINDOW`]) |
//! | Raydium, no open position, after the window | `D_cand ≥ (1 + pool_switch_margin_bps/10⁴)·D_inc` | adopt ([`POOL_KIND_ZERO_POSITION`]) |
//! | Raydium, a collection window running and the `pool_fail` chain that armed it unbroken | `D_cand > fp_d` and `D_cand ≥ pool_fast_min_bps·D_inc/10⁴` | replace the window's candidate |
//! | any, a collection window run out — or, on Raydium, its chain broken (M4-SEC-2) | — | `PoolWindowClosed` (`execute_pool_switch` resolves it first) |
//! | Raydium, the fast path armed (`pool_fail` now, the chain ≥ `pool_fast_route_s` old) | `D_cand ≥ pool_fast_min_bps·D_inc/10⁴` | open a window of `pool_fast_window_s` |
//! | otherwise | | `PositionsOpen` with an open position, else `PoolNotDominant` |
//!
//! `D_inc = pool_d`, the incumbent's locked depth at its last executable observation. A non-fast
//! adoption requires `open_positions == 0` **inside the listing window too, and on a list-only
//! market** (decision 7; I15: "a non-fast adoption happened only with `open_positions == 0`"): a
//! listing window can follow a fast path taken with positions open, and those positions must
//! not see a second pool change that no failure caused; and positions on a list-only market
//! (they exist from M5, when a live curve completes with zeroed reserves and no canonical pool,
//! ARCH §2.5) must not be pinned to whichever eligible pool is proposed first — they go through a
//! collection window and `execute_pool_switch` adopts the deepest candidate seen (M4-SEC-4; the
//! market has no route at all, so no arming delay). The fast path works whether or not positions
//! are open; a candidate that also qualifies for a zero-position adoption is adopted at once.
//!
//! **Execution** ([`window_adoptable`]): `execute_pool_switch` adopts the window's candidate only
//! if, on a Raydium market, the `pool_fail` chain that armed the window is still unbroken —
//! `pool_fail` now, `pool_fail_since ≠ 0` and `pool_fail_since + pool_fast_route_s ≤ fp_t0`
//! (M4-SEC-2: a pool that recovered after the window opened and failed again for one second must
//! not be switched away from; a stale window is not a standing licence). A list-only market's
//! window needs no chain. A candidate that is no longer eligible, fails the window floor, or
//! whose program is now latched (M4V-4) is not adoptable now: the window is cancelled.
//!
//! **Eligibility** (the adapters, `raydium::check_eligible`; M4-SEC-1, M4-SEC-3): executable,
//! fee ≤ `venue_fee_cap_bps`, and the locked-depth floor `⌊⌊D·isqrt(P_ref)/10⁹⌋·10⁴/λ_min⌋ ≥
//! pool_min_real_sol` with `P_ref` = the market's `P_ema` ([`reference_price`]) — or the
//! candidate's spot on a market that never had a price. No clause reads `λ_now` or the live `R`.
//! A collection window's candidate — its adoption moves open positions — also needs the **window
//! floor** ([`window_floor_ok`], M4V-1): its locked SOL at a reference price itself,
//! `⌊D·isqrt(P)/10⁹⌋ ≥ pool_min_real_sol` (no `/λ_min`). A proposal that opens or improves the
//! window judges it at `P = min(P_ema, P_cand)` ([`window_price`]; M4R-2, **PROPOSED**, owner item
//! O18: after a dump during the incumbent's halt the carried-over `P_ema` is stale-high, and at it
//! a pool far under the floor passed), `PoolTooThin`; the execution judges it again at `P_ema`
//! (else cancelled).
//!
//! **Switch only if all fit** ([`carried_fit`]; owner decision of 2026-09-29; ARCH §2.6, §4.6, ECON
//! §6.1): an adoption that carries open positions — a Raydium market's collection window, and a
//! list-only market's window (it has positions) — is allowed only if every open position fits the
//! **candidate's normal opening caps**, evaluated as an open would be on it right after the
//! adoption: the per-position impact cap and the fee-scaled size cap on the candidate's
//! conservative locked-depth reserve `X₀ = conservative_x0(X_cand, L_slow', P, (D, λ_min))` (with
//! `L_slow' = min(L_slow, D)`, what [`adopt`] leaves), checked against `min(max_q_hwm, ΣB_Q)` — an
//! O(1) bound at or above every open `q`, `Market.max_q_hwm` (the high-water mark of `q` since
//! `open_positions` was last 0) —; the per-market impact cap and the fee-scaled OI cap on `ΣB_Q`
//! at `X₁ = X₀` (the carried tokens were sold into the old pool); the impact caps of the tier the
//! market would have on the candidate (`tier::assign` on its locked depth, real SOL, `L_slow'`,
//! age and the cached oracle); the fee-scaled caps take `φ = min(φ_inc, φ_cand,
//! CARRIED_FEE_CAP_PPM)` (`shortwin_math::caps::carried_phi`; FIT-2, ECON #100; FITS-1),
//! `φ_cand` the candidate's one-way fee, `φ_inc = Market.min_open_fee_ppm`, the lowest one-way
//! venue fee an open paid since `open_positions` last returned to 0 — the fee the open positions
//! posted their margin at — and `shortwin_math::consts::CARRIED_FEE_CAP_PPM` (**PROPOSED**,
//! 25 bps) the ceiling: a position carried onto a higher-fee pool must be proportionally smaller
//! (its own fee's size class: 0.75 % of `X₀` at 25 bps, not the impact cap's 2.47 % at 1 %), and
//! no carried position is above the 25-bps class whatever both fees are (a 1 % → 1 % switch
//! carries at most 0.75 % of `X₀`; larger positions stay on the failed pool and settle in S3 at
//! fair value); the impact caps are the candidate's tier's. `P` is the M4R-2 floor's reference `P_ref =
//! min(P_ema, P_cand)` ([`window_price`]) **and** `P_ema` ([`reference_price`]): a lower price
//! loosens `X₀`'s depth terms (a token reserve, `∝ 1/√P`), and `P_cand` is the candidate's spot
//! inside the instruction, which a dump wrapped around it lowers — the second evaluation keeps an
//! in-transaction dump from loosening the depth terms (never looser than the rule at `P_ref`
//! alone; the tier's caps come from each price's own tier). The proposal's live `X_cand` is **not**
//! protected (FIT-1, ECON #100): it binds when the candidate trades above `P_ema`, and a dump
//! wrapped around the proposal lifts it; it binds the honest flow only. The guarantee is the
//! execution's `X₀` ([`FitAt::Execution`]), up to `λ·√(P_cand/P_ema)` × a native open's cap. A
//! proposal of a candidate that does not fit is refused (`CarriedPositionsTooLarge`);
//! `execute_pool_switch` cancels a window whose candidate no longer fits, judged on the terms no
//! transaction can move ([`FitAt::Execution`]: `X₀` without the live `X_cand`, so an unlocked LP's
//! withdrawal or a buy wrapped around the execution cannot cancel an honest window — the
//! M4-SEC-1 / M4-SEC-3 principle; a genuine pump inside the 60-s window after the proposal is
//! judged by the proposal only, as M4X-3's dump). With no candidate that fits, the positions stay
//! on the failed pool and exit through the S3 chain at fair value (D3, O15). Zero-position
//! adoptions and listings are not affected. `max_q_hwm` falls only when `open_positions` returns
//! to 0: a position that has closed keeps the mark up while others stay open, so one borrower's
//! large position can keep a later switch from being adopted for everyone (FIT-3, a disclosed
//! residual: those positions then exit on the failed pool through the S3 chain at fair value, at
//! `max(deadline, no_route_since + s3_wait_s)` for one that stays healthy).
//!
//! **Every adoption** ([`adopt`]; ARCH §2.6 rule 4 as amended by M4R, **PROPOSED**, owner item
//! O18): the venue kind may change between AMM v4 and CPMM; the pool itself
//! ([`Adoption::write_pool`]: venue, pool cache, SOL side, `pool_d`); `P_ema` and the ring's prices
//! carry over; `L_ema` and every ring entry's `l`, `llo`, `lhi` become the **anchor** `cap =
//! ⌊D·10⁴/λ_min⌋` (`pool_select::depth_at_lambda`) — no sample of the new pool, which a deposit
//! or an unlocked LP's withdrawal around the adopting instruction would move (M4R-1) — each
//! entry's `(x, y)` recomputed, and `L_slow = min(L_slow, D)` (never raised); the open bucket
//! cleared (A72; `observe::rewrite_liquidity`, `VenueChange::RaydiumLocked`); then the state is
//! marked unconfirmed (`observe::mark_restart`: `ema_warm = 8`, `ema_guard = 1`), so the next
//! eight integrations re-seed `P_ema` and `L_ema` from the new pool's buckets on the side that
//! cannot force an exit, opens and keeper exits wait for them, and the exit reference reads the
//! ring's liquidity lows until they agree with `L_ema`. A list-only market that never had a price
//! starts the price state of the pool (`observe::initial_raydium`: its spot, `min(√(X·Y), cap)`,
//! `L_slow ≤ D` (AIR-13), marked) as a listing does; both route chains and the collection window
//! cleared;
//! `graduated_at` kept (age is the token's time on an eligible pool), except that a `None` market
//! takes the candidate's `age_start` as a listing would (O13, O13b); `pool_seen_at = now` only if
//! the market is not already inside a listing window.
//!
//! **Deviations from ARCH v0.4.1** (the M4 security review and its verification, M4V; owner
//! item O18 records the rules): eligibility's `R ≥ pool_min_real_sol` and `λ ≥ pool_lock_min_bps`
//! are replaced by the locked-depth floor at `P_ref` (§2.6 "Eligibility", rule 5's last
//! sentence), and a window's candidate needs its locked SOL alone at the SOL floor (M4V-1);
//! rule 4's `L_ema = min(√(X·Y)_new, current)` is the anchor and the post-change warm-up above
//! and `L_slow` falls to at most `D`; the execution also requires the arming chain unbroken (rule 3 names only
//! `pool_fail`) and cancels on a latched candidate program (M4V-4); a list-only market with open
//! positions adopts through a collection window (§2.5 "a Raydium pool can then be adopted through
//! `propose_pool`"); the latch reads the candidate program's bit only (§3.9 names none).

use anchor_lang::prelude::*;
use shortwin_math::caps::{self, CarriedCaps, CarriedFit};
use shortwin_math::observe::VenueChange;
use shortwin_math::pool_select::{depth_at_lambda, dominates, fast_path_qualifies};
use shortwin_math::quote::reprice;
use shortwin_math::tier::{self as math_tier, OraclePrice, Tier, TierInputs, TierVenue};

use crate::engine::observe;
use crate::errors::{CheckedExt, MathResultExt, ShortwinError};
use crate::events::{POOL_KIND_LISTING, POOL_KIND_LISTING_WINDOW, POOL_KIND_ZERO_POSITION};
use crate::state::{GlobalConfig, Market, OracleCache, Params, TierParams, VenueKind};
use crate::venues::raydium::Adoption;
use crate::venues::{raydium, raydium_cpmm, raydium_v4};

/// The pool-selection parameters (ARCH §2.3.3; `GlobalConfig.params`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectParams {
    /// `pool_list_window_s`: opens wait this long after an adoption outside a window.
    pub list_window_s: u32,
    /// `pool_list_margin_bps`: the replacement margin inside the listing window (5 %).
    pub list_margin_bps: u32,
    /// `pool_switch_margin_bps`: the zero-position margin after the window (25 %).
    pub switch_margin_bps: u32,
    /// `pool_fast_route_s`: how long `pool_fail` must hold before the fast path (120 s).
    pub fast_route_s: u32,
    /// `pool_fast_min_bps`: a fast-path candidate's minimum depth, bps of `pool_d` (25 %).
    pub fast_min_bps: u32,
    /// `pool_fast_window_s`: the collection window (60 s).
    pub fast_window_s: u32,
}

impl SelectParams {
    /// The parameters of `params`.
    pub fn of(params: &Params) -> Self {
        Self {
            list_window_s: params.pool_list_window_s,
            list_margin_bps: params.pool_list_margin_bps,
            switch_margin_bps: params.pool_switch_margin_bps,
            fast_route_s: params.pool_fast_route_s,
            fast_min_bps: params.pool_fast_min_bps,
            fast_window_s: params.pool_fast_window_s,
        }
    }
}

/// The market as a proposal sees it, after the proposing instruction's prologue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Situation {
    /// The incumbent venue: `None` or a Raydium kind.
    pub venue: VenueKind,
    /// The market time (`prologue::market_now`).
    pub now: i64,
    pub open_positions: u32,
    pub pool_seen_at: i64,
    /// `D_inc`: the incumbent's locked depth at its last executable observation.
    pub pool_d: u128,
    /// `pool_fail` of this instruction's observation of the incumbent.
    pub pool_fail: bool,
    /// The fast-path chain after this instruction's observation.
    pub pool_fail_since: i64,
    /// The collection window (`fp_t0 = 0`: none).
    pub fp_t0: i64,
    pub fp_d: u128,
}

impl Situation {
    /// The situation of `market` (on `venue`) at `now` with this instruction's `pool_fail`.
    pub fn of(market: &Market, venue: VenueKind, now: i64, pool_fail: bool) -> Self {
        Self {
            venue,
            now,
            open_positions: market.open_positions,
            pool_seen_at: market.pool_seen_at,
            pool_d: market.pool_d.get(),
            pool_fail,
            pool_fail_since: market.pool_fail_since,
            fp_t0: market.fp_t0,
            fp_d: market.fp_d.get(),
        }
    }
}

/// What a proposal does ([`decide`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proposal {
    /// Adopt the candidate now, with its `POOL_KIND_*`.
    Adopt(u8),
    /// Open a fast-path collection window on the candidate.
    OpenWindow,
    /// Replace the running window's candidate with this deeper one.
    ImproveWindow,
}

/// `now < pool_seen_at + pool_list_window_s`: inside the listing window (ARCH §2.6 rule 1).
pub fn in_listing_window(pool_seen_at: i64, list_window_s: u32, now: i64) -> bool {
    pool_seen_at
        .checked_add(i64::from(list_window_s))
        .is_some_and(|end| now < end)
}

/// The end of the listing window that starts at `pool_seen_at`.
pub fn listing_window_ends(pool_seen_at: i64, list_window_s: u32) -> Result<i64> {
    pool_seen_at.checked_add(i64::from(list_window_s)).or_math()
}

/// The end of the collection window opened at `fp_t0`.
pub fn window_ends(fp_t0: i64, fast_window_s: u32) -> Result<i64> {
    fp_t0.checked_add(i64::from(fast_window_s)).or_math()
}

/// The fast path is armed: `pool_fail` holds now and has held continuously for
/// `pool_fast_route_s` (ARCH §2.6 rule 3). Never reads the route-broken flag (V1-7): the chain
/// is fed by `pool_fail` alone (`engine::route`).
pub fn fast_path_armed(s: &Situation, p: &SelectParams) -> bool {
    s.pool_fail
        && s.pool_fail_since != 0
        && s.now.saturating_sub(s.pool_fail_since) >= i64::from(p.fast_route_s)
}

/// The `pool_fail` chain that armed a Raydium market's collection window is unbroken
/// (M4-SEC-2): `pool_fail` now, the chain running (`pool_fail_since ≠ 0`) and started at least
/// `pool_fast_route_s` before the window opened (`pool_fail_since + pool_fast_route_s ≤ fp_t0`).
/// A window opens only on an armed fast path, so a chain that started later is one that broke
/// (a routed observation cleared it, or observations more than `S3_OBS_GAP_S` apart restarted
/// it) and began again after the window opened. `false` without a window.
pub fn window_chain_holds(s: &Situation, p: &SelectParams) -> bool {
    s.fp_t0 != 0
        && s.pool_fail
        && s.pool_fail_since != 0
        && s.pool_fail_since
            .checked_add(i64::from(p.fast_route_s))
            .is_some_and(|armed| armed <= s.fp_t0)
}

/// A collection window may still lead to an adoption (M4-SEC-2, M4-SEC-4): on a Raydium market
/// iff [`window_chain_holds`]; on a list-only market (no venue, no chain: its positions have no
/// route at all) always. `false` without a window.
pub fn window_adoptable(s: &Situation, p: &SelectParams) -> bool {
    match s.venue {
        VenueKind::None => s.fp_t0 != 0,
        VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => window_chain_holds(s, p),
        _ => false,
    }
}

/// What a proposal of a candidate with locked depth `d_cand` does (module docs). The caller has
/// verified the candidate (`eligible()`) and refused pump-venue markets (D1).
///
/// Errors: `PoolWindowClosed` (a collection window ran out, or its arming chain broke, and
/// waits for `execute_pool_switch`), `PositionsOpen` (no failure, positions open),
/// `PoolNotDominant` (the candidate misses every margin that applies), `NoSupportedVenue` (not a
/// `None` or Raydium market).
pub fn decide(s: &Situation, d_cand: u128, p: &SelectParams) -> Result<Proposal> {
    match s.venue {
        VenueKind::None => {
            if s.open_positions == 0 {
                return Ok(Proposal::Adopt(POOL_KIND_LISTING));
            }
            // M4-SEC-4: positions on a list-only market collect the deepest candidate.
            if s.fp_t0 != 0 {
                require!(
                    s.now < window_ends(s.fp_t0, p.fast_window_s)?,
                    ShortwinError::PoolWindowClosed
                );
                require!(d_cand > s.fp_d, ShortwinError::PoolNotDominant);
                return Ok(Proposal::ImproveWindow);
            }
            return Ok(Proposal::OpenWindow);
        }
        VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => {}
        _ => return err!(ShortwinError::NoSupportedVenue),
    }
    if s.open_positions == 0 {
        let inside = in_listing_window(s.pool_seen_at, p.list_window_s, s.now);
        let (margin, kind) = if inside {
            (p.list_margin_bps, POOL_KIND_LISTING_WINDOW)
        } else {
            (p.switch_margin_bps, POOL_KIND_ZERO_POSITION)
        };
        if dominates(d_cand, s.pool_d, u64::from(margin)) {
            return Ok(Proposal::Adopt(kind));
        }
    }
    let qualifies = fast_path_qualifies(d_cand, s.pool_d, u64::from(p.fast_min_bps));
    if s.fp_t0 != 0 {
        require!(
            s.now < window_ends(s.fp_t0, p.fast_window_s)? && window_chain_holds(s, p),
            ShortwinError::PoolWindowClosed
        );
        require!(d_cand > s.fp_d && qualifies, ShortwinError::PoolNotDominant);
        return Ok(Proposal::ImproveWindow);
    }
    if fast_path_armed(s, p) {
        require!(qualifies, ShortwinError::PoolNotDominant);
        return Ok(Proposal::OpenWindow);
    }
    require!(s.open_positions == 0, ShortwinError::PositionsOpen);
    err!(ShortwinError::PoolNotDominant)
}

/// The reference price of eligibility (M4-SEC-3): the market's `P_ema` when it has a price (a
/// Raydium market; a list-only market that had one), `None` otherwise — then the candidate's
/// own spot is used (`raydium::check_eligible`).
pub fn reference_price(market: &Market) -> Option<u128> {
    let p = market.ema_price.get();
    (market.ring_count > 0 && p > 0).then_some(p)
}

/// Verifies a candidate ReadSet `read` of kind `kind` for the market's `mint` under
/// `token_program` and checks `eligible()` at `p_ref` ([`reference_price`]; ARCH §2.6; the
/// adapters' `verify`).
///
/// Errors: `NoSupportedVenue` for a kind other than the two Raydium ones; the adapters'
/// `BadRemainingAccounts`, `VenueAccountMismatch`, `PoolIneligible`, `PoolTooThin`.
pub fn verify_candidate(
    kind: VenueKind,
    read: &[AccountInfo],
    mint: &Pubkey,
    token_program: &Pubkey,
    params: &Params,
    now: i64,
    p_ref: Option<u128>,
) -> Result<Adoption> {
    match kind {
        VenueKind::RaydiumV4 => raydium_v4::verify(read, mint, token_program, params, now, p_ref),
        VenueKind::RaydiumCpmm => {
            raydium_cpmm::verify(read, mint, token_program, params, now, p_ref)
        }
        _ => err!(ShortwinError::NoSupportedVenue),
    }
}

/// The window floor of a collection window's candidate `a` (M4V-1; module docs): its locked SOL at
/// `p_ref` (the candidate's spot on a market without a price) reaches `pool_min_real_sol`
/// (`raydium::position_floor_ok`). A proposal passes [`window_price`], the execution
/// [`reference_price`].
pub fn window_floor_ok(a: &Adoption, params: &Params, p_ref: Option<u128>) -> bool {
    raydium::position_floor_ok(&a.state, params, p_ref)
}

/// The price a proposal that opens or improves a collection window judges the window floor at
/// (M4R-2, **PROPOSED**, owner item O18; module docs): `min(P_ema, P_cand)`, `P_ema` the market's
/// reference price ([`reference_price`]) and `P_cand` the candidate's pre-trade spot `⌊Y·WAD/X⌋`
/// in this instruction; `P_cand` alone on a market without a price (as before). After a dump
/// during the incumbent's halt `P_ema` is frozen high, and the candidate's locked SOL at it is
/// `floor·√(P_ema/P_live)` above the real one; the live spot closes that for an honest dump. A
/// proposer can only raise `P_cand` for its own proposal (a pump around it), never above
/// `P_ema`, so the floor is never looser than at `P_ema`.
pub fn window_price(p_ref: Option<u128>, a: &Adoption) -> Option<u128> {
    match (p_ref, a.state.price()) {
        (Some(e), Some(c)) => Some(e.min(c)),
        (e, c) => e.or(c),
    }
}

/// What the carried-position fit ([`carried_fit`]) reads besides the market and the candidate:
/// the parameters (fee-scaled caps, `λ_min`, the tier thresholds), the tier table, the cached
/// SOL/USD price and the market time.
#[derive(Clone, Copy)]
pub struct FitContext<'a> {
    pub params: &'a Params,
    pub tiers: &'a [TierParams; 4],
    pub oracle: OraclePrice,
    pub now: i64,
}

impl<'a> FitContext<'a> {
    /// The context of `config` and the oracle cache `oracle` at the market time `now`.
    pub fn of(config: &'a GlobalConfig, oracle: &OracleCache, now: i64) -> Self {
        Self {
            params: &config.params,
            tiers: &config.tiers,
            oracle: OraclePrice {
                price: oracle.price,
                conf: oracle.conf,
                expo: oracle.expo,
                publish_time: oracle.publish_time,
            },
            now,
        }
    }
}

/// `L_slow` as [`adopt`] leaves it for candidate `a`: `min(L_slow, D)` on a market with a price;
/// a market that never had one starts at the pool's `min(√(X·Y), ⌊D·10⁴/λ_min⌋, D)`
/// (`observe::initial_raydium`; AIR-13).
fn adopted_l_slow(market: &Market, a: &Adoption, lambda_min_bps: u32) -> u128 {
    let d = a.state.locked_depth;
    if market.ring_count > 0 && market.ema_price.get() > 0 {
        market.liq_slow.get().min(d)
    } else {
        let cap = depth_at_lambda(d, u64::from(lambda_min_bps));
        a.state.liquidity().unwrap_or(0).min(cap).min(d)
    }
}

/// The tier the market would have on candidate `a` at the price `p` (ARCH §4.7, as `open_short`'s
/// step 5 computes it right after the adoption, `P_ema` replaced by `p`): Raydium, bounded by the
/// candidate's locked depth over `λ_min`; `R_now` its real SOL; `R_slow` from `l_slow` (the adopted
/// `L_slow`) at `p`; the market's age (a list-only market: the candidate's `age_start`, O13); the
/// cached oracle (stale or empty: C).
pub fn candidate_tier(
    market: &Market,
    a: &Adoption,
    ctx: &FitContext,
    l_slow: u128,
    p: u128,
) -> Tier {
    let graduated_at = if market.venue_kind() == Some(VenueKind::None) {
        a.age_start
    } else {
        market.graduated_at
    };
    let params = ctx.params;
    math_tier::assign(&TierInputs {
        venue: TierVenue::Raydium {
            locked_depth: a.state.locked_depth,
            lambda_min_bps: params.pool_lock_min_bps,
        },
        r_now: a.state.r,
        y_slow: reprice(l_slow, p).map_or(0, |(_, y)| y),
        vqr: 0,
        p_ema: p,
        now: ctx.now,
        graduated_at,
        oracle: ctx.oracle,
        oracle_stale_s: params.oracle_stale_s,
        theta_a_usd_e6: params.theta_a_usd_e6,
        theta_b_usd_e6: params.theta_b_usd_e6,
    })
}

/// Which instruction judges the carried-position fit ([`carried_fit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitAt {
    /// `propose_pool` (and the keeper's plan of one): `X₀` as an open computes it, with the
    /// candidate's live token reserve `X_cand`. It binds every honest proposal, but it is no
    /// guarantee: a dump wrapped around the proposal lifts `X_cand` inside the transaction (FIT-1;
    /// the guarantee is [`FitAt::Execution`]'s).
    Proposal,
    /// `execute_pool_switch`: `X₀` on the terms no transaction can move — `L_slow'` and `D` over
    /// `√P` — without `X_cand`, which an unlocked LP's withdrawal or a buy wrapped around the
    /// execution would lower to cancel an honest window (the atomic griefing M4-SEC-1 / M4-SEC-3
    /// removed from eligibility).
    Execution,
}

/// The candidate's opening caps at the price `p` ([`carried_fit`]): `X₀ =
/// conservative_x0(X_cand, L_slow', p, (D, λ_min))` (at the execution without `X_cand`,
/// [`FitAt::Execution`]), the impact caps of [`candidate_tier`] at `p`, the fee-scaled caps'
/// `φ = carried_phi(min_open_fee_ppm, φ_cand)` (`min(φ_inc, φ_cand, CARRIED_FEE_CAP_PPM)`, FIT-2,
/// FITS-1) and their multipliers.
///
/// Errors: `MathOverflow` for `p = 0`; `InvalidParams` for a tier outside the table.
pub fn carried_caps(
    market: &Market,
    a: &Adoption,
    ctx: &FitContext,
    p: u128,
    at: FitAt,
) -> Result<CarriedCaps> {
    let lambda_min = ctx.params.pool_lock_min_bps;
    let l_slow = adopted_l_slow(market, a, lambda_min);
    let x_now = match at {
        FitAt::Proposal => a.state.x,
        FitAt::Execution => u64::MAX,
    };
    let x0 =
        caps::conservative_x0(x_now, l_slow, p, Some((a.state.locked_depth, lambda_min))).math()?;
    let tier = candidate_tier(market, a, ctx, l_slow, p);
    let tp = ctx
        .tiers
        .get(tier.index())
        .ok_or(ShortwinError::InvalidParams)?;
    Ok(CarriedCaps {
        x0,
        impact_pos_bps: tp.impact_pos_bps,
        impact_mkt_bps: tp.impact_mkt_bps,
        phi_ppm: caps::carried_phi(market.min_open_fee_ppm, a.state.fee_ppm),
        fee_size_cap_x100: ctx.params.fee_size_cap_x100,
        fee_oi_cap_x100: ctx.params.fee_oi_cap_x100,
    })
}

/// "Switch only if all fit" (module docs; owner decision of 2026-09-29): whether the market's open
/// positions fit candidate `a`'s normal opening caps. [`CarriedFit::Fits`] at once without an open
/// position; otherwise `shortwin_math::caps::carried_fit` of `min(max_q_hwm, ΣB_Q)` and `ΣB_Q`
/// against [`carried_caps`] (at `at`) at `P_ref = min(P_ema, P_cand)` ([`window_price`]) and at
/// `P_ema` (`p_ref`, [`reference_price`]), the first refusal returned. A market with positions but
/// no price at all shows nothing fits ([`CarriedFit::Position`]).
///
/// Errors: as [`carried_caps`] (none for an eligible candidate on a market with a price).
pub fn carried_fit(
    market: &Market,
    a: &Adoption,
    ctx: &FitContext,
    p_ref: Option<u128>,
    at: FitAt,
) -> Result<CarriedFit> {
    if market.open_positions == 0 {
        return Ok(CarriedFit::Fits);
    }
    let sum_q = market.borrowed_q;
    let q_max = caps::carried_q_bound(market.max_q_hwm, sum_q);
    let mut judged = false;
    let first = window_price(p_ref, a);
    // `P_ema` again only when it differs from `min(P_ema, P_cand)` (a candidate spot at or above
    // `P_ema`: one evaluation).
    for p in [first, p_ref.filter(|e| Some(*e) != first)]
        .into_iter()
        .flatten()
    {
        judged = true;
        let fit = caps::carried_fit(q_max, sum_q, &carried_caps(market, a, ctx, p, at)?).math()?;
        if !fit.fits() {
            return Ok(fit);
        }
    }
    Ok(if judged {
        CarriedFit::Fits
    } else {
        CarriedFit::Position
    })
}

/// Clears the fast-path collection window.
pub fn clear_window(market: &mut Market) {
    market.fp_keys = [Pubkey::default(); 6];
    market.fp_d.set(0);
    market.fp_t0 = 0;
    market.fp_venue = 0;
    market._pad1 = 0;
    market._pad1a = [0; 2];
    market._pad1b = [0; 4];
}

/// Records `a` as the collection window's candidate (`fp_keys`, `fp_d`, `fp_venue`); `open` also
/// starts the window at `now` (`fp_t0`), an improvement keeps its start. Nothing of the pool's
/// liquidity is recorded (M4R: the adoption reads no sample).
pub fn record_window(market: &mut Market, a: &Adoption, now: i64, open: bool) {
    market.fp_keys = a.window_keys();
    market.fp_d.set(a.state.locked_depth);
    market.fp_venue = a.kind as u8;
    if open {
        market.fp_t0 = now;
    }
}

/// Adopts `a` on `market` at `now` (module docs, ARCH §2.6 rule 4 as amended by M4R): the pool;
/// on a market with a price the liquidity rewrite to the anchor `cap = ⌊D·10⁴/λ_min⌋` with `L_slow
/// ≤ D`, then the restart marks (`ema_warm`, `ema_guard`); on a list-only market that never had a
/// price a fresh, marked price state at the pool's spot with `L = min(√(X·Y), cap)` and `L_slow =
/// min(L, D)` (AIR-13); both route
/// chains and the collection window cleared, `graduated_at` kept (a `None` market: the pool's
/// `age_start`), `pool_seen_at = now` unless the market is inside a listing window.
/// `lambda_min_bps` is `pool_lock_min_bps`. The priced path reads no liquidity of the pool.
///
/// Errors: `PoolIneligible` for a candidate without a defined price or liquidity on a market
/// without a price (an eligible pool always has both), `NoSupportedVenue` for a corrupt venue
/// byte, `MathOverflow` for a zero locked depth (an eligible pool has `D > 0`).
pub fn adopt(
    market: &mut Market,
    a: &Adoption,
    now: i64,
    list_window_s: u32,
    lambda_min_bps: u32,
) -> Result<()> {
    let from = market.venue_kind().ok_or(ShortwinError::NoSupportedVenue)?;
    let inside = from.is_raydium() && in_listing_window(market.pool_seen_at, list_window_s, now);
    let d = a.state.locked_depth;
    let cap = depth_at_lambda(d, u64::from(lambda_min_bps));
    let priced = market.ring_count > 0 && market.ema_price.get() > 0;
    a.write_pool(market)?;
    if from == VenueKind::None {
        market.graduated_at = a.age_start;
    }
    if !inside {
        market.pool_seen_at = now;
    }
    if priced {
        let mut obs = market.observation();
        observe::rewrite_liquidity(&mut obs, cap, VenueChange::RaydiumLocked { slow_cap: d })?;
        obs.mark_restart();
        market.set_observation(&obs);
    } else {
        let obs = observe::initial_raydium(now, &a.state, cap)?;
        market.set_observation(&obs);
    }
    market.no_route_since = 0;
    market.last_no_route_obs = 0;
    market.pool_fail_since = 0;
    market.last_pool_fail_obs = 0;
    clear_window(market);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::state::TierParams;
    use crate::venues::VenueState;
    use bytemuck::Zeroable;
    use shortwin_math::num::WAD;
    use shortwin_math::quote::reprice;

    fn code(r: Result<Proposal>) -> Option<u32> {
        match r {
            Err(anchor_lang::error::Error::AnchorError(e)) => Some(e.error_code_number),
            _ => None,
        }
    }

    const P: SelectParams = SelectParams {
        list_window_s: 3_600,
        list_margin_bps: 500,
        switch_margin_bps: 2_500,
        fast_route_s: 120,
        fast_min_bps: 2_500,
        fast_window_s: 60,
    };

    /// A healthy Raydium market listed at 1,000, now 10,000, `pool_d` 10,000, no window.
    fn healthy() -> Situation {
        Situation {
            venue: VenueKind::RaydiumV4,
            now: 10_000,
            open_positions: 0,
            pool_seen_at: 1_000,
            pool_d: 10_000,
            pool_fail: false,
            pool_fail_since: 0,
            fp_t0: 0,
            fp_d: 0,
        }
    }

    #[test]
    fn a_list_only_market_adopts_any_eligible_candidate() {
        let s = Situation {
            venue: VenueKind::None,
            pool_d: 0,
            ..healthy()
        };
        assert_eq!(
            decide(&s, 1, &P).unwrap(),
            Proposal::Adopt(POOL_KIND_LISTING)
        );
    }

    /// M4-SEC-4: a list-only market with open positions (M5's curve that graduated elsewhere)
    /// is never pinned to the first eligible proposal: it collects candidates in a window of
    /// `pool_fast_window_s` (no arming — it has no route at all), keeps the deepest, and its
    /// execution needs no chain; once its positions are gone it adopts at once again.
    #[test]
    fn a_list_only_market_with_positions_collects_the_deepest_candidate() {
        let none = Situation {
            venue: VenueKind::None,
            pool_d: 0,
            open_positions: 3,
            ..healthy()
        };
        assert_eq!(decide(&none, 1, &P).unwrap(), Proposal::OpenWindow);
        let open = Situation {
            fp_t0: 10_000,
            fp_d: 1,
            ..none
        };
        assert!(
            window_adoptable(&open, &P),
            "no chain on a list-only market"
        );
        let later = Situation {
            now: 10_059,
            ..open
        };
        assert_eq!(
            decide(&later, 1_000_000, &P).unwrap(),
            Proposal::ImproveWindow
        );
        assert_eq!(
            code(decide(&later, 1, &P)),
            Some(u32::from(ShortwinError::PoolNotDominant))
        );
        let closed = Situation {
            now: 10_060,
            ..open
        };
        assert_eq!(
            code(decide(&closed, u128::MAX, &P)),
            Some(u32::from(ShortwinError::PoolWindowClosed))
        );
        let drained = Situation {
            open_positions: 0,
            ..closed
        };
        assert_eq!(
            decide(&drained, 1, &P).unwrap(),
            Proposal::Adopt(POOL_KIND_LISTING)
        );
        // The Raydium market it becomes refuses a much deeper pool while positions are open
        // (decision 7) — which is why the first adoption must already be the deepest.
        let after = Situation {
            venue: VenueKind::RaydiumCpmm,
            now: 10_100,
            pool_seen_at: 10_060,
            pool_d: 1_000_000,
            fp_t0: 0,
            fp_d: 0,
            ..none
        };
        assert_eq!(
            code(decide(&after, u128::MAX, &P)),
            Some(u32::from(ShortwinError::PositionsOpen))
        );
    }

    #[test]
    fn zero_position_margins_inside_and_after_the_listing_window() {
        let after = healthy();
        assert_eq!(
            decide(&after, 12_500, &P).unwrap(),
            Proposal::Adopt(POOL_KIND_ZERO_POSITION)
        );
        assert_eq!(
            code(decide(&after, 12_499, &P)),
            Some(u32::from(ShortwinError::PoolNotDominant))
        );
        let inside = Situation {
            pool_seen_at: 9_000,
            ..healthy()
        };
        assert!(in_listing_window(9_000, 3_600, 10_000));
        assert!(!in_listing_window(9_000, 3_600, 12_600));
        assert_eq!(
            decide(&inside, 10_500, &P).unwrap(),
            Proposal::Adopt(POOL_KIND_LISTING_WINDOW)
        );
        assert_eq!(
            code(decide(&inside, 10_499, &P)),
            Some(u32::from(ShortwinError::PoolNotDominant))
        );
        // The window's last second is still inside; the next is not.
        let edge = Situation {
            now: 9_000 + 3_599,
            ..inside
        };
        assert_eq!(
            decide(&edge, 10_500, &P).unwrap(),
            Proposal::Adopt(POOL_KIND_LISTING_WINDOW)
        );
        let out = Situation {
            now: 9_000 + 3_600,
            ..inside
        };
        assert_eq!(
            code(decide(&out, 10_500, &P)),
            Some(u32::from(ShortwinError::PoolNotDominant))
        );
    }

    #[test]
    fn open_positions_block_every_non_fast_adoption() {
        for seen in [1_000, 9_500] {
            let s = Situation {
                open_positions: 1,
                pool_seen_at: seen,
                ..healthy()
            };
            assert_eq!(
                code(decide(&s, u128::MAX, &P)),
                Some(u32::from(ShortwinError::PositionsOpen))
            );
        }
    }

    #[test]
    fn the_fast_path_arms_after_pool_fast_route_s_of_pool_fail() {
        let failing = |since: i64, positions: u32| Situation {
            open_positions: positions,
            pool_fail: true,
            pool_fail_since: since,
            ..healthy()
        };
        // 119 s: not armed; 120 s: armed, whatever the positions.
        for positions in [0, 5] {
            let not_yet = failing(10_000 - 119, positions);
            assert!(!fast_path_armed(&not_yet, &P));
            let armed = failing(10_000 - 120, positions);
            assert!(fast_path_armed(&armed, &P));
            assert_eq!(decide(&armed, 2_500, &P).unwrap(), Proposal::OpenWindow);
            assert_eq!(
                code(decide(&armed, 2_499, &P)),
                Some(u32::from(ShortwinError::PoolNotDominant))
            );
        }
        // Not armed without this instruction's pool_fail (the chain alone is not enough).
        let stale = Situation {
            pool_fail: false,
            ..failing(1, 1)
        };
        assert!(!fast_path_armed(&stale, &P));
        assert_eq!(
            code(decide(&stale, u128::MAX, &P)),
            Some(u32::from(ShortwinError::PositionsOpen))
        );
        // With no position a dominant candidate is adopted at once, armed or not.
        assert_eq!(
            decide(&failing(1, 0), 12_500, &P).unwrap(),
            Proposal::Adopt(POOL_KIND_ZERO_POSITION)
        );
    }

    #[test]
    fn the_collection_window_keeps_the_deepest_candidate() {
        let open = Situation {
            open_positions: 2,
            pool_fail: true,
            pool_fail_since: 9_000,
            fp_t0: 9_990,
            fp_d: 5_000,
            ..healthy()
        };
        assert!(window_chain_holds(&open, &P) && window_adoptable(&open, &P));
        assert_eq!(decide(&open, 5_001, &P).unwrap(), Proposal::ImproveWindow);
        for weaker in [5_000, 4_000, 2_499] {
            assert_eq!(
                code(decide(&open, weaker, &P)),
                Some(u32::from(ShortwinError::PoolNotDominant))
            );
        }
        // M4-SEC-2: once the chain that armed the window broke — the incumbent recovered (this
        // second's observation cleared it), or it recovered and failed again (the chain
        // restarted after the window opened), or it restarted after a gap — the window takes no
        // candidate and can only be cancelled.
        let recovered = Situation {
            pool_fail: false,
            pool_fail_since: 0,
            ..open
        };
        let restarted = Situation {
            pool_fail_since: 10_000 - 40,
            ..open
        };
        for broken in [recovered, restarted] {
            assert!(!window_chain_holds(&broken, &P) && !window_adoptable(&broken, &P));
            assert_eq!(
                code(decide(&broken, 6_000, &P)),
                Some(u32::from(ShortwinError::PoolWindowClosed))
            );
        }
        // The chain exactly `pool_fast_route_s` older than the window still holds.
        let edge = Situation {
            pool_fail_since: 9_990 - 120,
            ..open
        };
        assert!(window_chain_holds(&edge, &P));
        let late = Situation {
            pool_fail_since: 9_990 - 119,
            ..open
        };
        assert!(!window_chain_holds(&late, &P));
        assert!(!window_chain_holds(&Situation { fp_t0: 0, ..open }, &P));
        // The window's last second takes candidates; afterwards it waits for the execution.
        let last = Situation {
            now: 9_990 + 59,
            ..open
        };
        assert_eq!(decide(&last, 6_000, &P).unwrap(), Proposal::ImproveWindow);
        let closed = Situation {
            now: 9_990 + 60,
            ..open
        };
        assert_eq!(
            code(decide(&closed, u128::MAX, &P)),
            Some(u32::from(ShortwinError::PoolWindowClosed))
        );
        // With no position a dominant candidate still wins at once.
        let empty = Situation {
            open_positions: 0,
            ..closed
        };
        assert_eq!(
            decide(&empty, 12_500, &P).unwrap(),
            Proposal::Adopt(POOL_KIND_ZERO_POSITION)
        );
    }

    #[test]
    fn pump_venues_and_the_mock_never_select() {
        for venue in [
            VenueKind::PumpCurve,
            VenueKind::PumpMigrating,
            VenueKind::PumpSwap,
        ] {
            let s = Situation { venue, ..healthy() };
            assert_eq!(
                code(decide(&s, u128::MAX, &P)),
                Some(u32::from(ShortwinError::NoSupportedVenue))
            );
        }
    }

    /// An eligible-looking adoption of kind `kind` with reserves `(x, y)` and depth `d`.
    fn adoption(kind: VenueKind, x: u64, y: u64, d: u128, age_start: i64) -> Adoption {
        Adoption {
            kind,
            pool: Pubkey::new_unique(),
            vault_sol: Pubkey::new_unique(),
            vault_tok: Pubkey::new_unique(),
            amm_config: if kind == VenueKind::RaydiumCpmm {
                Pubkey::new_unique()
            } else {
                Pubkey::default()
            },
            observation: if kind == VenueKind::RaydiumCpmm {
                Pubkey::new_unique()
            } else {
                Pubkey::default()
            },
            lp_mint: Pubkey::new_unique(),
            tok_program: crate::constants::TOKEN_PROGRAM_ID,
            sol_first: false,
            state: VenueState {
                kind,
                executable: true,
                x,
                y,
                r: y,
                pricing: None,
                fee_ppm: 2_500,
                locked_depth: d,
                curve: None,
                pool: None,
            },
            age_start,
        }
    }

    /// M4R: a list-only market's first adoption starts the pool's price state at its spot with
    /// `L = min(√(X·Y), ⌊D·10⁴/λ_min⌋)` and `L_slow = min(L, D)` (AIR-13), marked for the
    /// post-change warm-up.
    #[test]
    fn a_list_only_market_starts_the_pools_price_state() {
        for (d, want_l, want_slow) in [
            (1_900_000u128, 2_000_000u128, 1_900_000u128),
            (900_000, 1_800_000, 900_000),
        ] {
            let mut m = Market::zeroed();
            m.venue = VenueKind::None as u8;
            m.no_route_since = 50;
            m.last_no_route_obs = 90;
            let a = adoption(VenueKind::RaydiumCpmm, 4_000_000, 1_000_000, d, 77);
            adopt(&mut m, &a, 100, 3_600, 5_000).unwrap();
            assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumCpmm));
            assert_eq!((m.graduated_at, m.pool_seen_at), (77, 100));
            assert_eq!(m.pool_d.get(), d);
            assert_eq!(m.ema_price.get(), WAD / 4);
            assert_eq!((m.ema_liq.get(), m.liq_slow.get()), (want_l, want_slow));
            assert_eq!((m.ring_count, m.ema_ts), (1, 100));
            assert_eq!((m.ema_warm, m.ema_guard), (8, 1), "marked");
            assert_eq!(
                (m.no_route_since, m.last_no_route_obs, m.pool_fail_since),
                (0, 0, 0)
            );
            assert_eq!(m.fp_t0, 0);
        }
    }

    /// AIR-13: a deposit wrapped around a Raydium price start — a list-only market's first
    /// adoption here, a listing through `create_market` (`observe::initial_raydium` too) — lifts
    /// the candidate's `√(X·Y)` to twice its locked depth on a fully locked pool; `L_ema` may start
    /// at that sample up to the anchor `⌊D·10⁴/λ_min⌋` (the warm-up re-seeds it), but `L_slow`,
    /// which sizing, the tier and the exit reference read and nothing re-seeds, starts at no more
    /// than `D`, as a priced market's adoption leaves it.
    #[test]
    fn a_price_start_seeds_l_slow_at_most_the_locked_depth() {
        let d = 1_000_000u128;
        // √(4·10⁶·10⁶) = 2·10⁶ = 2D: the pool's own depth D doubled by the wrapped deposit.
        let a = adoption(VenueKind::RaydiumCpmm, 4_000_000, 1_000_000, d, 77);
        let mut m = Market::zeroed();
        m.venue = VenueKind::None as u8;
        adopt(&mut m, &a, 100, 3_600, 5_000).unwrap();
        assert_eq!(m.ema_liq.get(), 2 * d, "L_ema: min(√(X·Y), 2D)");
        assert_eq!(m.liq_slow.get(), d, "L_slow: at most D");
        let obs = observe::initial_raydium(100, &a.state, 2 * d).unwrap();
        assert_eq!((obs.ema.liq, obs.ema.liq_slow), (2 * d, d));
        // The fit check of a list-only market's adoption reads the same L_slow.
        assert_eq!(adopted_l_slow(&Market::zeroed(), &a, 5_000), d);
    }

    /// M4R: every Raydium adoption of a priced market writes the anchor `⌊D·10⁴/λ_min⌋` into
    /// `L_ema` and the ring, lowers `L_slow` to at most `D`, keeps the prices and marks the state;
    /// it reads no liquidity of the pool, so a deposit or a withdrawal around the adopting
    /// instruction changes nothing.
    #[test]
    fn a_switch_rewrites_the_ring_and_keeps_the_age() {
        let mut m = Market::zeroed();
        let first = adoption(VenueKind::RaydiumV4, 4_000_000, 1_000_000, 1_990_000, 10);
        first.apply(&mut m, 10).unwrap();
        let mut obs = observe::initial(10, &first.state).unwrap();
        // Two more integrated seconds at a higher price.
        obs.record(11, WAD / 2, 2_000_000).unwrap();
        obs.roll(12, &shortwin_math::observe::EmaParams::DEFAULT)
            .unwrap();
        m.set_observation(&obs);
        m.pool_fail_since = 5_000;
        m.last_pool_fail_obs = 5_100;
        m.fp_t0 = 5_050;
        m.fp_d.set(400_000);
        m.fp_venue = VenueKind::RaydiumCpmm as u8;
        m._pad1a = [7, 7];
        let p_ema = m.ema_price.get();
        let (count, ts) = (m.ema_count, m.ema_ts);
        let ring_p: Vec<_> = m
            .ring
            .iter()
            .map(|e| (e.ts, e.p.get(), e.lo.get()))
            .collect();
        // A thinner CPMM pool (√(X·Y) = 500,000, D = 450,000) after the listing window: L_ema
        // the anchor ⌊D·10⁴/λ_min⌋ = 900,000, L_slow its locked depth.
        let thin = adoption(VenueKind::RaydiumCpmm, 1_000_000, 250_000, 450_000, 99_999);
        adopt(&mut m, &thin, 5_110, 3_600, 5_000).unwrap();
        assert_eq!(m.venue_kind(), Some(VenueKind::RaydiumCpmm));
        assert_eq!(m.graduated_at, 10, "age kept");
        assert_eq!(m.pool_seen_at, 5_110, "outside a window: opens wait again");
        assert_eq!(m.ema_price.get(), p_ema, "P_ema carries over");
        assert_eq!((m.ema_count, m.ema_ts), (count, ts), "no integration");
        assert_eq!((m.ema_liq.get(), m.liq_slow.get()), (900_000, 450_000));
        assert_eq!((m.ema_warm, m.ema_guard), (8, 1), "marked");
        let n = usize::from(m.ring_count);
        for e in m.ring.iter().take(n) {
            assert_eq!(
                (e.l.get(), e.llo.get(), e.lhi.get()),
                (900_000, 900_000, 900_000)
            );
            let (x, y) = reprice(900_000, e.p.get()).unwrap();
            assert_eq!((e.x.get(), e.y.get()), (x, y));
        }
        let after: Vec<_> = m
            .ring
            .iter()
            .map(|e| (e.ts, e.p.get(), e.lo.get()))
            .collect();
        assert_eq!(after, ring_p, "prices and bucket price extremes kept");
        assert_eq!((m.b_sec, m.b_lo.get()), (0, 0), "bucket cleared");
        assert_eq!((m.pool_fail_since, m.last_pool_fail_obs), (0, 0));
        assert_eq!(
            (m.fp_t0, m.fp_d.get(), m.fp_venue, m._pad1a),
            (0, 0, 0, [0, 0])
        );
        assert_eq!(m.fp_keys, [Pubkey::default(); 6]);
        assert_eq!(m.pool_d.get(), 450_000);
        // Inside the new listing window a further adoption does not extend it; a deeper pool
        // gets its own anchor (deeper or thinner than before alike), L_slow is never raised.
        let deep = adoption(VenueKind::RaydiumV4, 16_000_000, 4_000_000, 7_900_000, 1);
        let mut m0 = m;
        m0.ema_warm = 3;
        m0.ema_guard = 0;
        adopt(&mut m0, &deep, 5_200, 3_600, 5_000).unwrap();
        assert_eq!(
            m0.pool_seen_at, 5_110,
            "a chain of proposals cannot extend a window"
        );
        assert_eq!(m0.graduated_at, 10);
        assert_eq!(
            (m0.ema_liq.get(), m0.liq_slow.get()),
            (15_800_000, 450_000),
            "L_ema the new anchor, L_slow never raised"
        );
        assert_eq!((m0.ema_warm, m0.ema_guard), (8, 1), "re-marked");
        let n = usize::from(m0.ring_count);
        assert!(
            m0.ring
                .iter()
                .take(n)
                .all(|e| (e.l.get(), e.llo.get(), e.lhi.get())
                    == (15_800_000, 15_800_000, 15_800_000))
        );
        // No dependence on the pool's √(X·Y) now: a deposit of 100× (√(X·Y) 50,000,000) and an
        // unlocked LP's withdrawal (√(X·Y) = D) around the adopting instruction write the same.
        let honest = adoption(VenueKind::RaydiumCpmm, 2_400_000, 150_000, 450_000, 1);
        let with_reserves = |x: u64, y: u64| Adoption {
            state: VenueState {
                x,
                y,
                r: y,
                ..honest.state.clone()
            },
            ..honest.clone()
        };
        let withdrawn = with_reserves(900_000, 225_000);
        let deposit = with_reserves(100_000_000, 25_000_000);
        let mut outs = Vec::new();
        for a in [&withdrawn, &deposit, &honest] {
            let mut m2 = m;
            adopt(&mut m2, a, 5_300, 3_600, 5_000).unwrap();
            outs.push(m2);
        }
        for m2 in &outs {
            assert_eq!(m2.ema_liq.get(), 900_000);
            assert_eq!(bytemuck::bytes_of(m2), bytemuck::bytes_of(&outs[0]));
        }
        assert_eq!(
            m0.pool_cache[3],
            Pubkey::default(),
            "CPMM config slot cleared"
        );
        assert_eq!(
            m0.pool_cache[4],
            Pubkey::default(),
            "CPMM observation slot cleared"
        );
        // A zero locked depth cannot be adopted.
        let mut m3 = m;
        let zero = adoption(VenueKind::RaydiumCpmm, 1_000_000, 250_000, 0, 1);
        assert!(adopt(&mut m3, &zero, 5_300, 3_600, 5_000).is_err());
    }

    /// M4R-2: a proposal judges the window floor at `min(P_ema, P_cand)`; the candidate's spot
    /// alone without a price.
    #[test]
    fn the_proposal_window_floor_reads_min_of_ema_and_candidate() {
        // x = 4,000,000 tokens, y = 1,000,000 lamports: P_cand = WAD/4.
        let a = adoption(VenueKind::RaydiumCpmm, 4_000_000, 1_000_000, 1_000, 1);
        assert_eq!(window_price(Some(WAD), &a), Some(WAD / 4));
        assert_eq!(window_price(Some(WAD / 8), &a), Some(WAD / 8));
        assert_eq!(window_price(None, &a), Some(WAD / 4));
        let mut params = Params::defaults();
        // D = 10¹⁰: locked SOL ⌊D·isqrt(P)/10⁹⌋ = 10 SOL at P = WAD, 5 SOL at WAD/4.
        let a = adoption(
            VenueKind::RaydiumCpmm,
            4_000_000,
            1_000_000,
            10_000_000_000,
            1,
        );
        params.pool_min_real_sol = 8_000_000_000;
        assert!(window_floor_ok(&a, &params, Some(WAD)));
        assert!(!window_floor_ok(&a, &params, window_price(Some(WAD), &a)));
        params.pool_min_real_sol = 5_000_000_000;
        assert!(window_floor_ok(&a, &params, window_price(Some(WAD), &a)));
    }

    /// A priced Raydium market with `n` open positions, `ΣB_Q = sum`, the mark `hwm`, `P_ema =
    /// p_ema` and a deep `L_slow`.
    fn carrying(n: u32, sum: u64, hwm: u64, p_ema: u128) -> Market {
        let mut m = Market::zeroed();
        m.venue = VenueKind::RaydiumV4 as u8;
        m.ring_count = 1;
        m.ema_price.set(p_ema);
        m.liq_slow.set(u128::MAX / 4);
        m.graduated_at = 1;
        m.open_positions = n;
        m.borrowed_q = sum;
        m.max_q_hwm = hwm;
        m
    }

    /// Switch only if all fit (owner decision of 2026-09-29): the per-position caps on
    /// `min(max_q_hwm, ΣB_Q)` and the OI caps on `ΣB_Q`, on the candidate's conservative
    /// locked-depth `X₀` at `min(P_ema, P_cand)` and at `P_ema`, with the tier it would have.
    #[test]
    fn carried_positions_must_fit_the_candidates_opening_caps() {
        let params = Params::defaults();
        let tiers = TierParams::defaults();
        let ctx = FitContext {
            params: &params,
            tiers: &tiers,
            oracle: OraclePrice::default(),
            now: 1_000_000,
        };
        // A 1,000-SOL CPMM candidate at P = WAD/4 (isqrt 5·10⁸), λ 99.78 %, fee 2,500 ppm.
        let (x, y) = (4_000_000_000_000u64, 1_000_000_000_000u64);
        let d = 9_978 * 2_000_000_000_000u128 / 10_000;
        let a = adoption(VenueKind::RaydiumCpmm, x, y, d, 1);
        // X₀ = ⌊L_slow'·10⁹/isqrt(P)⌋ with L_slow' = D: 2·D; the fee-scaled caps bind at 25 bps.
        let x0 = u64::try_from(2 * d).unwrap();
        let caps_at = carried_caps(
            &carrying(1, 1, 1, WAD / 4),
            &a,
            &ctx,
            WAD / 4,
            FitAt::Proposal,
        )
        .unwrap();
        assert_eq!(caps_at.x0, x0);
        assert_eq!(caps_at.phi_ppm, 2_500);
        let pos = caps::position_cap(x0, 500, 2_500, params.fee_size_cap_x100).unwrap();
        let oi = caps::oi_cap(x0, 2_000, 2_500, params.fee_oi_cap_x100).unwrap();
        assert_eq!(pos, x0 * 75 / 10_000);
        assert_eq!(oi, x0 * 1_875 / 100_000);
        let fit = |m: &Market, p_ref: Option<u128>| {
            carried_fit(m, &a, &ctx, p_ref, FitAt::Proposal).unwrap()
        };
        let e = Some(WAD / 4);
        // Per position, exactly at the cap.
        assert_eq!(fit(&carrying(1, pos, pos, WAD / 4), e), CarriedFit::Fits);
        assert_eq!(
            fit(&carrying(1, pos + 1, pos + 1, WAD / 4), e),
            CarriedFit::Position
        );
        // The mark is conservative: a closed large position still counts (while ΣQ allows).
        assert_eq!(
            fit(&carrying(2, pos + 10, pos + 1, WAD / 4), e),
            CarriedFit::Position
        );
        // ... and ΣQ caps the mark.
        assert_eq!(
            fit(&carrying(1, pos, pos * 3, WAD / 4), e),
            CarriedFit::Fits
        );
        // A market written before the mark existed (0 with positions open): ΣQ bounds every q.
        assert_eq!(
            fit(&carrying(3, pos + 1, 0, WAD / 4), e),
            CarriedFit::Position
        );
        // Per market: three positions within the per-position cap whose ΣQ passes the OI cap.
        assert_eq!(fit(&carrying(3, oi, pos, WAD / 4), e), CarriedFit::Fits);
        assert_eq!(
            fit(&carrying(3, oi + 1, pos, WAD / 4), e),
            CarriedFit::Market
        );
        // No open position: nothing to carry.
        assert_eq!(fit(&carrying(0, 0, 0, WAD / 4), e), CarriedFit::Fits);
        // P_ema 4× the candidate's spot (a dump during the halt, or a dump wrapped around the
        // instruction): X₀ at P_ema is half of X₀ at min(P_ema, P_cand) — the fit is judged at
        // both, so a position that fits only at the candidate's spot is refused.
        let half = pos / 2 + 1;
        let stale = carrying(1, half, half, WAD);
        assert_eq!(
            caps::carried_fit(
                half,
                half,
                &carried_caps(&stale, &a, &ctx, WAD / 4, FitAt::Proposal).unwrap()
            ),
            Ok(CarriedFit::Fits)
        );
        assert_eq!(fit(&stale, Some(WAD)), CarriedFit::Position);
        assert_eq!(
            fit(&carrying(1, half - 1, half - 1, WAD), Some(WAD)),
            CarriedFit::Fits
        );
        // P_ema below the spot: min(P_ema, P_cand) = P_ema, one price; there the candidate's live
        // token reserve X_now = 4·10¹² binds X₀ (the L terms give 4·D and 8·D).
        let low = carrying(1, 1, 1, WAD / 16);
        assert_eq!(
            carried_caps(&low, &a, &ctx, WAD / 16, FitAt::Proposal)
                .unwrap()
                .x0,
            x
        );
        let at_x = x / 10_000 * 75;
        assert_eq!(
            fit(&carrying(1, at_x, at_x, WAD / 16), Some(WAD / 16)),
            CarriedFit::Fits
        );
        assert_eq!(
            fit(&carrying(1, at_x + 1, at_x + 1, WAD / 16), Some(WAD / 16)),
            CarriedFit::Position
        );
        // No price at all shows nothing fits.
        let mut blind = a.clone();
        blind.state.executable = false;
        assert_eq!(
            carried_fit(
                &carrying(1, 1, 1, WAD / 4),
                &blind,
                &ctx,
                None,
                FitAt::Proposal
            )
            .unwrap(),
            CarriedFit::Position
        );
        // L_slow' = min(L_slow, D): a shallow L_slow bounds X₀.
        let mut shallow = carrying(1, 1, 1, WAD / 4);
        shallow.liq_slow.set(d / 2);
        assert_eq!(
            carried_caps(&shallow, &a, &ctx, WAD / 4, FitAt::Proposal)
                .unwrap()
                .x0,
            u64::try_from(d).unwrap()
        );
        // The execution judges without the live X_cand (FitAt::Execution): there X₀ is the
        // L terms alone — 4·D at WAD/16 — so a withdrawal or a buy wrapped around the execution
        // (which only lowers X_cand) cannot cancel the window; a position that fits only
        // because X_cand is not read at the execution is refused by the proposal.
        let exec = carried_caps(&low, &a, &ctx, WAD / 16, FitAt::Execution).unwrap();
        assert_eq!(exec.x0, u64::try_from(4 * d).unwrap());
        let big = carrying(1, at_x + 1, at_x + 1, WAD / 16);
        assert_eq!(
            carried_fit(&big, &a, &ctx, Some(WAD / 16), FitAt::Execution).unwrap(),
            CarriedFit::Fits
        );
        let mut withdrawn = a;
        withdrawn.state.x = x / 2;
        withdrawn.state.y = y / 2;
        let ok_q = carrying(1, pos, pos, WAD / 4);
        assert_eq!(
            carried_fit(&ok_q, &withdrawn, &ctx, e, FitAt::Proposal).unwrap(),
            CarriedFit::Position,
            "the proposer's own withdrawal only refuses its own proposal"
        );
        assert_eq!(
            carried_fit(&ok_q, &withdrawn, &ctx, e, FitAt::Execution).unwrap(),
            CarriedFit::Fits,
            "a withdrawal wrapped around the execution cancels nothing"
        );
    }

    /// FIT-2 / FITS-1: the fee-scaled caps take `min(min_open_fee_ppm, φ_cand,
    /// CARRIED_FEE_CAP_PPM)`. Positions opened on a 25 bps pool and carried onto a 1 % candidate
    /// keep the 25 bps size class (0.75 % of `X₀`), not the candidate's own (its impact cap,
    /// 2.47 %, binds at 1 %); so do positions margined at 1 % (FITS-1: the ceiling); a lower-fee
    /// candidate's own fee binds; a zero mark (an account written before the field) is the
    /// candidate's fee under the ceiling.
    #[test]
    fn the_fee_scaled_caps_take_the_lower_of_the_open_fee_and_the_candidates() {
        let params = Params::defaults();
        let tiers = TierParams::defaults();
        let ctx = FitContext {
            params: &params,
            tiers: &tiers,
            oracle: OraclePrice::default(),
            now: 1_000_000,
        };
        let (x, y) = (4_000_000_000_000u64, 1_000_000_000_000u64);
        let d = 9_978 * 2_000_000_000_000u128 / 10_000;
        let mut a = adoption(VenueKind::RaydiumCpmm, x, y, d, 1);
        a.state.fee_ppm = 10_000;
        let x0 = u64::try_from(2 * d).unwrap();
        let impact = caps::max_q_position(x0, 500).unwrap();
        let at_25 = x0 * 75 / 10_000;
        assert!(at_25 < impact);
        let e = Some(WAD / 4);
        let with = |fee: u32, q: u64| {
            let mut m = carrying(1, q, q, WAD / 4);
            m.min_open_fee_ppm = fee;
            m
        };
        let phi = |m: &Market, a: &Adoption| {
            carried_caps(m, a, &ctx, WAD / 4, FitAt::Proposal)
                .unwrap()
                .phi_ppm
        };
        assert_eq!(phi(&with(2_500, 1), &a), 2_500);
        assert_eq!(
            phi(&with(0, 1), &a),
            2_500,
            "no mark: the candidate's, under the ceiling"
        );
        assert_eq!(
            phi(&with(10_000, 1), &a),
            2_500,
            "1 % → 1 %: the ceiling (FITS-1)"
        );
        assert_eq!(phi(&with(1_000, 1), &a), 1_000);
        let fit = |m: &Market, a: &Adoption, at: FitAt| carried_fit(m, a, &ctx, e, at).unwrap();
        for at in [FitAt::Proposal, FitAt::Execution] {
            // Margined at 25 bps: the 25 bps class on the 1 % candidate.
            assert_eq!(fit(&with(2_500, at_25), &a, at), CarriedFit::Fits);
            assert_eq!(fit(&with(2_500, at_25 + 1), &a, at), CarriedFit::Position);
            assert_eq!(fit(&with(2_500, impact), &a, at), CarriedFit::Position);
            // Margined at 1 % (FITS-1): the 25 bps class too, not the candidate's impact cap.
            assert_eq!(fit(&with(10_000, at_25), &a, at), CarriedFit::Fits);
            assert_eq!(fit(&with(10_000, at_25 + 1), &a, at), CarriedFit::Position);
            assert_eq!(fit(&with(10_000, impact), &a, at), CarriedFit::Position);
        }
        // A 25 bps candidate for positions margined at 1 %: its own fee binds.
        let mut low = a.clone();
        low.state.fee_ppm = 2_500;
        assert_eq!(phi(&with(10_000, 1), &low), 2_500);
        assert_eq!(
            fit(&with(10_000, at_25 + 1), &low, FitAt::Proposal),
            CarriedFit::Position
        );
        // The per-market fee-scaled cap follows: 1.875 % of X₀ at 25 bps, not 7.5 % at 1 %.
        let foi = x0 * 1_875 / 100_000;
        let mut m = carrying(3, foi, at_25, WAD / 4);
        m.min_open_fee_ppm = 2_500;
        assert_eq!(fit(&m, &a, FitAt::Proposal), CarriedFit::Fits);
        m.borrowed_q = foi + 1;
        assert_eq!(fit(&m, &a, FitAt::Proposal), CarriedFit::Market);
    }

    /// The caps are those of the tier the market would have on the candidate: a stale oracle
    /// tiers C; a fresh one with the candidate's liquidity and the market's age another tier,
    /// whose caps then apply; a list-only market takes the candidate's age.
    #[test]
    fn the_fit_uses_the_tier_the_market_would_have_on_the_candidate() {
        let params = Params::defaults();
        let mut tiers = TierParams::defaults();
        // Tier C with a 0.1 % per-position impact cap; A and B keep 5 %.
        tiers[Tier::C.index()].impact_pos_bps = 10;
        let now = 2_000_000_000i64;
        let (x, y) = (4_000_000_000_000u64, 1_000_000_000_000u64);
        let d = 9_978 * 2_000_000_000_000u128 / 10_000;
        let a = adoption(VenueKind::RaydiumCpmm, x, y, d, now - 1);
        let mut m = carrying(1, 0, 0, WAD / 4);
        m.graduated_at = now - 30 * 86_400;
        let x0 = u64::try_from(2 * d).unwrap();
        let q = caps::max_q_position(x0, 10).unwrap() + 1;
        m.borrowed_q = q;
        m.max_q_hwm = q;
        let stale = FitContext {
            params: &params,
            tiers: &tiers,
            oracle: OraclePrice::default(),
            now,
        };
        let l_slow = d;
        assert_eq!(candidate_tier(&m, &a, &stale, l_slow, WAD / 4), Tier::C);
        assert_eq!(
            carried_fit(&m, &a, &stale, Some(WAD / 4), FitAt::Proposal).unwrap(),
            CarriedFit::Position
        );
        // $1,000 per SOL, fresh: Λ ≈ 1,996 SOL ≈ $2 M on a 30-day-old market is not C.
        let fresh = FitContext {
            oracle: OraclePrice {
                price: 100_000_000_000,
                conf: 0,
                expo: -8,
                publish_time: now,
            },
            ..stale
        };
        let t = candidate_tier(&m, &a, &fresh, l_slow, WAD / 4);
        assert_ne!(t, Tier::C);
        assert_eq!(
            carried_fit(&m, &a, &fresh, Some(WAD / 4), FitAt::Proposal).unwrap(),
            CarriedFit::Fits
        );
        // A list-only market takes the candidate's age (1 s old: C, the tier with the 0.1 % cap).
        let mut none = m;
        none.venue = VenueKind::None as u8;
        assert_eq!(candidate_tier(&none, &a, &fresh, l_slow, WAD / 4), Tier::C);
        assert_eq!(
            carried_fit(&none, &a, &fresh, Some(WAD / 4), FitAt::Proposal).unwrap(),
            CarriedFit::Position
        );
    }

    #[test]
    fn window_keys_follow_the_adapters_read_sets() {
        let mut v4 = adoption(VenueKind::RaydiumV4, 1, 1, 1, 0);
        v4.sol_first = true;
        let k = v4.window_keys();
        assert_eq!(
            k,
            [
                v4.pool,
                v4.vault_sol,
                v4.vault_tok,
                v4.lp_mint,
                Pubkey::default(),
                Pubkey::default()
            ]
        );
        let cpmm = adoption(VenueKind::RaydiumCpmm, 1, 1, 1, 0);
        assert_eq!(
            cpmm.window_keys(),
            [
                cpmm.pool,
                cpmm.amm_config,
                cpmm.vault_tok,
                cpmm.vault_sol,
                cpmm.lp_mint,
                cpmm.observation
            ]
        );
        let mut m = Market::zeroed();
        record_window(&mut m, &cpmm, 700, true);
        assert_eq!(
            (m.fp_t0, m.fp_d.get(), m.fp_venue, m._pad1a),
            (700, 1, VenueKind::RaydiumCpmm as u8, [0, 0])
        );
        record_window(&mut m, &v4, 750, false);
        assert_eq!(m.fp_t0, 700, "an improvement keeps the window's start");
        assert_eq!(m.fp_keys, v4.window_keys());
        m._pad1a = [1, 2];
        clear_window(&mut m);
        assert_eq!((m.fp_t0, m.fp_venue, m._pad1a), (0, 0, [0, 0]));
    }
}
