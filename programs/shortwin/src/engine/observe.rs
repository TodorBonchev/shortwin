//! Observation buckets, EMA integration and the ring (ARCH §2.1 `engine/observe.rs`, §3.0 steps
//! 5 and 7, §4.4; ECON §5.1–§5.4): thin wrappers over `shortwin_math::observe` that add the
//! program's side effects (the `CoverageGap` log event, A69) and the "record only while
//! executable" rule (A64, V2-5).
//!
//! The state lives on `Market` as `PodU128` fields ([`Market::observation`] /
//! [`Market::set_observation`] convert it); every formula — directional aggregation, the ±C
//! clamp, the EMA and slow-EMA steps, the ring push with the cached reprojection `(x, y) =
//! reprice(l, p)` and the bucket extremes `(lo, hi, llo, lhi)`, the liquidity rewrite at a venue
//! change — is the math crate's.

use anchor_lang::prelude::*;
use shortwin_math::observe::{self, EmaParams, Observation, VenueChange};

use crate::errors::MathResultExt;
use crate::events::CoverageGap;
use crate::venues::VenueState;

/// ARCH §3.0 step 5: if a bucket is open and `now > b_sec`, integrates it into `P_ema`,
/// `L_ema`, `L_slow` and pushes a ring entry (`observe::integrate`). When the market has open
/// positions and the EMA was older than `COVERAGE_GAP_S`, emits the `CoverageGap{gap_s,
/// open_positions}` log event (`emit!`, no CPI; A69). Returns the pre-roll gap if it rolled.
pub fn roll(
    market_key: &Pubkey,
    obs: &mut Observation,
    params: &EmaParams,
    open_positions: u32,
    now: i64,
    slot: u64,
) -> Result<Option<i64>> {
    let gap = obs.roll(now, params).math()?;
    if let Some(g) = gap {
        if observe::coverage_gap(g, u64::from(open_positions)) {
            emit!(CoverageGap {
                market: *market_key,
                slot,
                ts: now,
                gap_s: g,
                open_positions,
            });
        }
    }
    Ok(gap)
}

/// ARCH §3.0 step 7: adds the **pre-trade** `(P_spot, √(X·Y))` to the bucket of second `now`
/// if the venue is executable and its price is defined (ECON §5.1). A venue that cannot
/// execute is not observed (A64, V2-5); a fee above the hard cap does not stop observation.
/// Every instruction records; there is no per-slot limit (C0-5 c). Returns whether it
/// recorded.
pub fn record(obs: &mut Observation, now: i64, state: &VenueState) -> Result<bool> {
    match (state.price(), state.liquidity()) {
        (Some(p), Some(l)) => {
            obs.record(now, p, l).math()?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// A venue change's liquidity rewrite (A72; R3B-7 a): `L_ema`, `L_slow` reset (graduation) or,
/// for a Raydium adoption, `L_ema` = the new pool's anchor `⌊D·10⁴/λ_min⌋` and `L_slow ≤ D` (M4R),
/// every ring entry's `l`, `llo`, `lhi` and cached `(x, y)` rewritten, the open bucket cleared
/// (`observe::rewrite_liquidity`). Used by every Raydium adoption (`engine::pool_select`, M4, which
/// then marks the restart) and the pump transitions of M5–M6.
pub fn rewrite_liquidity(obs: &mut Observation, l_new: u128, kind: VenueChange) -> Result<()> {
    obs.rewrite_liquidity(l_new, kind).math()
}

/// The observation state of a market at its first priced venue (ARCH §3.2, §4.4
/// "Initialisation"): `P_ema = P_spot`, `L_ema = L_slow = √(X·Y)`, `ema_ts = slow_ts = now`,
/// one ring entry `(now, P, L, reprice(L, P), P, P, L, L)`, an empty bucket.
pub fn initial(now: i64, state: &VenueState) -> Result<Box<Observation>> {
    let (p, l) = spot(state)?;
    Ok(Box::new(Observation::new(now, p, l).math()?))
}

/// A Raydium price start (M4R, **PROPOSED**, owner item O18; ARCH §3.2, §4.4: a Raydium listing, a
/// list-only market's first adoption): [`initial`] with `L = min(√(X·Y), cap)`, `cap =
/// ⌊D·10⁴/λ_min⌋` the pool's anchor, and `L_slow = min(L, D)`, `D = state.locked_depth` — an
/// adoption's `L_slow ≤ D` (AIR-13: a deposit wrapped around the start seeded `L_slow` at up to
/// `cap`) — marked for the post-change warm-up (`Observation::new_raydium`: `ema_warm =
/// EMA_WARMUP_BUCKETS`, `ema_guard = 1`).
pub fn initial_raydium(now: i64, state: &VenueState, cap: u128) -> Result<Box<Observation>> {
    let (p, l) = spot(state)?;
    Ok(Box::new(
        Observation::new_raydium(now, p, l, cap, state.locked_depth).math()?,
    ))
}

/// The pool's pre-trade `(P_spot, √(X·Y))`, `PoolIneligible` if either is undefined.
fn spot(state: &VenueState) -> Result<(u128, u128)> {
    match (state.price(), state.liquidity()) {
        (Some(p), Some(l)) => Ok((p, l)),
        _ => err!(crate::errors::ShortwinError::PoolIneligible),
    }
}
