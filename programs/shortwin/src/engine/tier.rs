//! Tier assignment at open and at crank (ARCH §2.1 `engine/tier.rs`, §3.4 step 5, §3.9
//! `crank_price`, §4.7; ECON §6.2–§6.3): assembles `shortwin_math::tier::TierInputs` from the
//! market, its venue state and the oracle cache, and calls `tier::assign`. Tiers apply to new
//! opens only; open positions keep their locked terms (A26).

use shortwin_math::quote::reprice;
use shortwin_math::tier::{self, OraclePrice, Tier, TierInputs, TierVenue};

use crate::constants::PUMPSWAP_LOCK_MIN_BPS;
use crate::state::{GlobalConfig, Market, OracleCache, VenueKind};
use crate::venues::VenueState;

/// The tier venue of a market (ARCH §4.7): the curve kinds are Tier D; Raydium adds the
/// locked-depth bound `D / λ_min` (`pool_lock_min_bps`) to `min(R_now, R_slow)` (V2-8), and
/// PumpSwap — since the owner's decision of 2026-09-30 (M6R-1; ECON #105) — a bound on its burned
/// LP's locked SOL less the virtual part `max(VQR + buckets, 0)` (`D = pool_d`; the boost,
/// PUMP-2026-10-1) over `λ_min` = `PUMPSWAP_LOCK_MIN_BPS` (100 %, OWNER-M6D-1, ECON #106, ARCH row
/// 57: `R_eff ≤ max(⌊D·isqrt(P_ema)/10⁹⌋ − max(VQR + buckets, 0), 0)`, so a
/// held third-party deposit lifts no PumpSwap tier; `VenueKind::lock_min_bps`); the mock
/// (a CPMM stand-in without pool selection or LP accounting, so without a locked depth) prices on
/// `min(R_now, R_slow)` alone.
pub fn tier_venue(config: &GlobalConfig, market: &Market, kind: VenueKind) -> Option<TierVenue> {
    match kind {
        VenueKind::PumpCurve | VenueKind::PumpMigrating => Some(TierVenue::PumpCurve),
        VenueKind::PumpSwap => Some(TierVenue::PumpSwap {
            locked_depth: market.pool_d.get(),
            lambda_min_bps: PUMPSWAP_LOCK_MIN_BPS,
        }),
        VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => Some(TierVenue::Raydium {
            locked_depth: market.pool_d.get(),
            lambda_min_bps: config.params.pool_lock_min_bps,
        }),
        #[cfg(feature = "test-venue")]
        VenueKind::Mock => Some(TierVenue::Unlocked),
        VenueKind::None => None,
    }
}

/// The `tier::assign` inputs of `market` now (ARCH §4.7): `R_now` the venue's real SOL (on
/// PumpSwap `min(WSOL vault − buckets, WSOL vault + VQR)`, the reader's `R`),
/// `Y(reprice(L_slow, P_ema))` for `R_slow` less the virtual part `max(VQR + buckets, 0)`
/// (PumpSwap, the boost: pump's 2026-10-02 redeploy takes the fee buckets off the VQR and keeps
/// them in the vault, PUMP-2026-10-1; 0 elsewhere) — under a negative VQR each input the smaller
/// of its real and effective SOL (ECON §6.2, #103: the delegated PROPOSED reading, built in M6;
/// exactly ADOPTED Q19 while `VQR ≥ 0` and no buckets) —, age from `graduated_at`, the cached
/// oracle. `None` for a list-only market (no opens).
fn inputs(
    config: &GlobalConfig,
    market: &Market,
    state: &VenueState,
    oracle: OraclePrice,
    now: i64,
) -> Option<TierInputs> {
    let venue = tier_venue(config, market, state.kind)?;
    let p_ema = market.ema_price.get();
    let y_slow = reprice(market.liq_slow.get(), p_ema).map_or(0, |(_, y)| y);
    // `max(VQR + buckets, 0)`: a positive VQR is not liquidity (Q19), and neither is the part of
    // the boost the fee buckets took off it (PUMP-2026-10-1: they are real SOL, but no sale is
    // paid out of them, so they are not in `R_now` either); a negative one is already out of the
    // effective reserve `Y(reprice(L_slow, P_ema))` prices on (ECON §6.2, #103).
    let vqr = state.pool.map_or(0, |p| p.virtual_part);
    Some(TierInputs {
        venue,
        r_now: state.r,
        y_slow,
        vqr,
        p_ema,
        now,
        graduated_at: market.graduated_at,
        oracle,
        oracle_stale_s: config.params.oracle_stale_s,
        theta_a_usd_e6: config.params.theta_a_usd_e6,
        theta_b_usd_e6: config.params.theta_b_usd_e6,
    })
}

/// `tier::assign` for `market` now (ARCH §4.7, on the inputs above); a stale or empty oracle
/// cache tiers as C (ADOPTED Q8). `None` for a list-only market (no opens).
pub fn assign(
    config: &GlobalConfig,
    market: &Market,
    state: &VenueState,
    oracle: &OracleCache,
    now: i64,
) -> Option<Tier> {
    let oracle = OraclePrice {
        price: oracle.price,
        conf: oracle.conf,
        expo: oracle.expo,
        publish_time: oracle.publish_time,
    };
    inputs(config, market, state, oracle, now).map(|i| tier::assign(&i))
}

/// `R_eff` of ARCH §4.7 in lamports: `min(R_now, R_slow)`, and on Raydium also
/// `≤ ⌊⌊D·isqrt(P_ema)/10⁹⌋·10⁴/λ_min_bps⌋` (locked SOL over `λ_min`, V2-8; on PumpSwap the
/// locked SOL less `max(VQR + buckets, 0)`, M6R-1, PUMP-2026-10-1) — half of the tier
/// liquidity `Λ = 2·R_eff` (`tier::liquidity_lamports`, which floors each term exactly as the
/// tier does). The open gate `R_eff ≥ pool_min_real_sol` of a Raydium market (ARCH §2.6 rule
/// 5, §3.4 step 3). `None` for a list-only market or the curve (no pool liquidity), or if the
/// terms cannot be computed (`λ_min = 0`, which the compiled bounds exclude).
pub fn r_eff(config: &GlobalConfig, market: &Market, state: &VenueState, now: i64) -> Option<u128> {
    let inp = inputs(config, market, state, OraclePrice::default(), now)?;
    tier::liquidity_lamports(&inp)
        .ok()
        .flatten()
        .and_then(|l| l.checked_div(2))
}
