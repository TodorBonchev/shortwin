//! Route predicates and chains (ARCH §2.1 `engine/route.rs`, §3.0 step 8, §5.3; A23, A50, A71;
//! C0-4, C0-10, V2-1, R3A-4, R3B-8).
//!
//! Three predicates, each with one job:
//!
//! - **`venue_executable`**: [`VenueState::executable`] (the venue predicate, and every
//!   venue-state account parses, A63). Only it drives `B̂_live`, closes, liquidations,
//!   expiries, the "no route ⇒ spot leg true" rule and observation recording.
//! - **`s3_no_route`** = `!venue_executable ∨ market.route_broken ∨ venue_route_broken[venue] ∨
//!   φ_live > min(config.venue_fee_hard_cap_bps, market.min_open_hard_cap_bps)`
//!   ([`s3_no_route`]): the S3 chain and the open gate only. `sol_settle`'s "no route now" is
//!   the position's own ([`position_no_route`]: the same with the position's locked cap in place
//!   of the market's mark).
//! - **`pool_fail`** = `!venue_executable ∨ φ_live > config.venue_fee_hard_cap_bps`
//!   ([`pool_fail`]): the Raydium fast-path chain only. It never reads the Emergency flag
//!   (T-S3-BROKEN).
//!
//! `pool_fail` uses the **global** hard cap (R3A-4 c). The S3 predicates also count a live fee
//! above the cap the position locked (AIR-19): under a global cap raised above it, a fee between
//! the two leaves the position no forced exit — its keeper exits are bounded at its own cap and
//! its S1 legs priced there — so that fee must start the chain (`Market.min_open_hard_cap_bps`,
//! the lowest cap an open position locked; 0 on existing accounts, the global cap alone) and be
//! "no route" for that position. A fee under a position's own cap stays a route for it, however
//! low the mark. The opens are unchanged: their gate also requires `φ ≤ venue_fee_cap_bps`
//! (at most 300 bps), below any locked hard cap (at least 300). The formulas are
//! `shortwin_math::settle::{s3_no_route, pool_fail, chain_step}`.

use shortwin_math::settle;

use crate::state::{GlobalConfig, Market, VenueKind, MARKET_FLAG_ROUTE_BROKEN};
use crate::venues::VenueState;

/// The Emergency route-broken flag of the market or of its venue (A40; O6). Opens and S3
/// evidence only: never an input to a quote, a leg, an exit or `pool_fail` (I10, I16).
pub fn route_broken(config: &GlobalConfig, market: &Market, kind: VenueKind) -> bool {
    market.has_flag(MARKET_FLAG_ROUTE_BROKEN)
        || (kind.venue_bit() != 0 && config.venue_route_broken & kind.venue_bit() != 0)
}

/// The hard cap the market's S3 chain judges the live fee against (AIR-19; module docs): the
/// global `venue_fee_hard_cap_bps`, or the lowest cap an open position locked
/// (`Market.min_open_hard_cap_bps`, 0 = unset) when that is lower.
pub fn chain_hard_cap_bps(config: &GlobalConfig, market: &Market) -> u64 {
    let global = u64::from(config.params.venue_fee_hard_cap_bps);
    match market.min_open_hard_cap_bps {
        0 => global,
        mark => global.min(u64::from(mark)),
    }
}

/// `s3_no_route` of the market now (module docs): the S3 chain's predicate.
pub fn s3_no_route(config: &GlobalConfig, market: &Market, state: &VenueState) -> bool {
    settle::s3_no_route(
        state.executable,
        route_broken(config, market, state.kind),
        state.fee_ppm,
        chain_hard_cap_bps(config, market),
    )
}

/// `s3_no_route` of one position now, `sol_settle`'s "no route now" (AIR-19; module docs): the
/// market's predicate with the position's locked `venue_fee_hard_cap_bps` (`pos_cap_bps`) in
/// place of the mark, so a live fee above that cap is no route for the position under any
/// global cap, and a fee within it is a route for the position whatever others locked.
pub fn position_no_route(
    config: &GlobalConfig,
    market: &Market,
    state: &VenueState,
    pos_cap_bps: u16,
) -> bool {
    let cap = u64::from(config.params.venue_fee_hard_cap_bps).min(u64::from(pos_cap_bps));
    settle::s3_no_route(
        state.executable,
        route_broken(config, market, state.kind),
        state.fee_ppm,
        cap,
    )
}

/// `pool_fail` of the market now (module docs); never reads the flag.
pub fn pool_fail(config: &GlobalConfig, state: &VenueState) -> bool {
    settle::pool_fail(
        state.executable,
        state.fee_ppm,
        u64::from(config.params.venue_fee_hard_cap_bps),
    )
}

/// What [`observe_chains`] saw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Chains {
    pub s3_no_route: bool,
    pub pool_fail: bool,
    /// A chain started or cleared (`no_route_since` or `pool_fail_since` changed;
    /// `crank_price` then emits `RouteChainChanged`). A continuing chain only refreshes its
    /// `last_*_obs` and is not a change.
    pub changed: bool,
}

/// ARCH §3.0 step 8: one observation of the S3 chain (`no_route_since`, `last_no_route_obs`)
/// with `s3_no_route`, and on Raydium markets (and the mock, their stand-in) of the fast-path
/// chain (`pool_fail_since`, `last_pool_fail_obs`) with `pool_fail`
/// (`settle::chain_step`: a restart after more than `S3_OBS_GAP_S`, a clear on a routed
/// observation; C0-16).
pub fn observe_chains(
    config: &GlobalConfig,
    market: &mut Market,
    state: &VenueState,
    now: i64,
) -> Chains {
    let before = (market.no_route_since, market.pool_fail_since);
    let s3 = s3_no_route(config, market, state);
    (market.no_route_since, market.last_no_route_obs) =
        settle::chain_step(market.no_route_since, market.last_no_route_obs, now, s3);
    let pf = pool_fail(config, state);
    if state.kind.has_pool_fail_chain() {
        (market.pool_fail_since, market.last_pool_fail_obs) =
            settle::chain_step(market.pool_fail_since, market.last_pool_fail_obs, now, pf);
    }
    let after = (market.no_route_since, market.pool_fail_since);
    Chains {
        s3_no_route: s3,
        pool_fail: pf,
        changed: before != after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    fn state(fee_ppm: u32, executable: bool) -> VenueState {
        VenueState {
            kind: VenueKind::RaydiumCpmm,
            executable,
            x: 0,
            y: 0,
            r: 0,
            pricing: None,
            fee_ppm,
            locked_depth: 0,
            curve: None,
            pool: None,
        }
    }

    /// AIR-19: the S3 chain judges the live fee at the lower of the global cap and the lowest
    /// cap an open position locked, a position at the lower of the global cap and its own;
    /// `pool_fail` keeps the global cap.
    #[test]
    fn a_fee_above_a_locked_cap_is_no_route_under_a_raised_global_cap() {
        let mut config = GlobalConfig::zeroed();
        config.params.venue_fee_hard_cap_bps = 1_000;
        let mut market = Market::zeroed();
        // A live fee of 8 %: between a locked 500 and the global 1,000 bps.
        let band = state(80_000, true);
        // No mark (no position open, or an account from before AIR-19): the global cap alone.
        assert_eq!(chain_hard_cap_bps(&config, &market), 1_000);
        assert!(!s3_no_route(&config, &market, &band));
        market.min_open_hard_cap_bps = 500;
        assert_eq!(chain_hard_cap_bps(&config, &market), 500);
        assert!(s3_no_route(&config, &market, &band));
        assert!(position_no_route(&config, &market, &band, 500));
        assert!(!position_no_route(&config, &market, &band, 1_000));
        assert!(!pool_fail(&config, &band));
        // A global cap lowered below a position's cap binds it too (G-G3), and a venue that
        // cannot execute is no route for every position.
        config.params.venue_fee_hard_cap_bps = 300;
        assert_eq!(chain_hard_cap_bps(&config, &market), 300);
        assert!(position_no_route(
            &config,
            &market,
            &state(40_000, true),
            1_000
        ));
        assert!(position_no_route(&config, &market, &state(0, false), 1_000));
        assert!(!position_no_route(
            &config,
            &market,
            &state(30_000, true),
            1_000
        ));
    }
}
