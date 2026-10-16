//! Owner: tier track. Tier assignment from USD thresholds, liquidity and pool age (ECON §6.2–§6.3; ARCH §4.7).
//!
//! Tiers apply to new opens only; an open position keeps its locked terms. Thresholds
//! are compared exactly against the cached Pyth SOL/USD price, with no rounded lamport
//! threshold in between.

use crate::error::MathError;
use crate::num::{BPS, Rounding, SQRT_WAD, U256, mul_div, overflow_to_max};
use crate::rent::RentTerms;

/// Below this age since graduation a pool tiers as C (SPEC §4: "graduated < 24 h ago").
pub const AGE_C_S: i64 = 86_400;
/// Tier A needs at least this age (SPEC §4: "age ≥ 7 days").
pub const AGE_A_S: i64 = 604_800;

/// Largest `|expo + 6|` accepted from the oracle; anything else fails safe to C.
const MAX_ORACLE_SHIFT: u32 = 30;

/// Risk tier (SPEC §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Tier {
    /// Graduated, pool liquidity ≥ θ_A, age ≥ 7 days.
    A = 0,
    /// Graduated, liquidity in [θ_B, θ_A) or younger than 7 days.
    B = 1,
    /// Graduated, liquidity < θ_B, younger than 24 h, or a stale oracle.
    C = 2,
    /// On the pump.fun bonding curve (live or migrating).
    D = 3,
}

impl Tier {
    /// All tiers, in `TierParams` table order.
    pub const ALL: [Self; 4] = [Self::A, Self::B, Self::C, Self::D];

    /// Index into a `[TierParams; 4]` table.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The tier stored as `u8` (`Market.tier`, `ShortPosition.tier`).
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::A),
            1 => Some(Self::B),
            2 => Some(Self::C),
            3 => Some(Self::D),
            _ => None,
        }
    }

    /// The SPEC §4 / §6.1 defaults of this tier (ARCH §2.3.3 `TierParams`).
    pub const fn defaults(self) -> TierParams {
        TierParams::DEFAULTS[self.index()]
    }
}

/// Per-tier parameters (ARCH §2.3.3; ECON §15). Governance may move them within their
/// compiled bounds; positions snapshot the `→P` ones at open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TierParams {
    /// Informational "initial margin" column, bps of notional.
    pub m_bps: u32,
    /// Opening CR (D4), bps.
    pub cr_open_bps: u32,
    /// Liquidation CR `L`, bps.
    pub cr_liq_bps: u32,
    /// Liquidation penalty, bps of `min(B̂_live, B̂_ref)`.
    pub penalty_bps: u32,
    /// Max duration, seconds.
    pub max_duration_s: u32,
    /// Per-position impact cap `ι_pos`, bps.
    pub impact_pos_bps: u32,
    /// Per-market impact cap `ι_mkt`, bps.
    pub impact_mkt_bps: u32,
    /// S2 slippage threshold `σ_max`, bps.
    pub max_liq_slip_bps: u32,
    /// Kinked rent curve (SPEC §6.1).
    pub rate: RentTerms,
    /// Minimum rent, seconds of rent.
    pub min_rent_s: u32,
}

impl TierParams {
    /// SPEC §4 / §6.1 values for A, B, C, D (ARCH §2.3.3 table).
    pub const DEFAULTS: [Self; 4] = [
        // Tier A: max duration 30 days.
        Self {
            m_bps: 10_000,
            cr_open_bps: 20_000,
            cr_liq_bps: 12_500,
            penalty_bps: 500,
            max_duration_s: 2_592_000,
            impact_pos_bps: 500,
            impact_mkt_bps: 2_000,
            max_liq_slip_bps: 2_000,
            rate: RentTerms {
                base_bps: 1_000,
                slope1_bps: 5_000,
                slope2_bps: 100_000,
            },
            min_rent_s: 3_600,
        },
        // Tier B: max duration 14 days.
        Self {
            m_bps: 15_000,
            cr_open_bps: 25_000,
            cr_liq_bps: 13_500,
            penalty_bps: 700,
            max_duration_s: 1_209_600,
            impact_pos_bps: 500,
            impact_mkt_bps: 2_000,
            max_liq_slip_bps: 2_000,
            rate: RentTerms {
                base_bps: 2_500,
                slope1_bps: 9_375,
                slope2_bps: 150_000,
            },
            min_rent_s: 3_600,
        },
        // Tier C: max duration 7 days.
        Self {
            m_bps: 20_000,
            cr_open_bps: 30_000,
            cr_liq_bps: 15_000,
            penalty_bps: 800,
            max_duration_s: 604_800,
            impact_pos_bps: 500,
            impact_mkt_bps: 2_000,
            max_liq_slip_bps: 2_000,
            rate: RentTerms {
                base_bps: 5_000,
                slope1_bps: 18_750,
                slope2_bps: 300_000,
            },
            min_rent_s: 3_600,
        },
        // Tier D: max duration 72 hours.
        Self {
            m_bps: 30_000,
            cr_open_bps: 40_000,
            cr_liq_bps: 17_500,
            penalty_bps: 1_000,
            max_duration_s: 259_200,
            impact_pos_bps: 300,
            impact_mkt_bps: 1_000,
            max_liq_slip_bps: 3_000,
            rate: RentTerms {
                base_bps: 10_000,
                slope1_bps: 37_500,
                slope2_bps: 550_000,
            },
            min_rent_s: 14_400,
        },
    ];
}

/// The market's venue as far as tiering is concerned (ARCH §4.7). List-only (`None`)
/// markets are never tiered: they take no opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TierVenue {
    /// Pump bonding curve, live or migrating: always Tier D.
    PumpCurve,
    /// PumpSwap canonical pool, bounded by the locked depth of its burned LP (M6R-1, owner
    /// decision of 2026-09-30; ECON #105, ARCH §4.7): `D = ⌊λ_bps·isqrt(X·(Y + VQR))/10⁴⌋` with
    /// `λ_bps` from `Pool.lp_supply` and the LP mint's supply
    /// ([`pool_select::locked_depth`](crate::pool_select::locked_depth)), less the virtual reserve
    /// (`R_eff ≤ ⌊max(⌊D·isqrt(P_ema)/10⁹⌋ − max(VQR + buckets, 0), 0)·10⁴/λ_min⌋`: real SOL only,
    /// Q19).
    PumpSwap {
        /// The canonical pool's locked depth `D` (`Market.pool_d`).
        locked_depth: u128,
        /// λ_min: [`PUMPSWAP_LOCK_MIN_BPS`](crate::consts::PUMPSWAP_LOCK_MIN_BPS) (100 %) on the
        /// program's path since OWNER-M6D-1 (ECON #106; as built at #105 `pool_lock_min_bps`),
        /// so `R_eff ≤ max(⌊D·isqrt(P_ema)/10⁹⌋ − max(VQR + buckets, 0), 0)`: the burned LP's real
        /// SOL.
        lambda_min_bps: u32,
    },
    /// Adopted Raydium AMM v4 / CPMM pool, bounded by its locked depth (V2-8):
    /// `D = λ·√(X·Y)` and `pool_lock_min_bps`.
    Raydium {
        /// Locked constant-product depth `D` (`pool_select::locked_depth`).
        locked_depth: u128,
        /// `pool_lock_min_bps` (λ_min).
        lambda_min_bps: u32,
    },
    /// A constant-product pool without LP accounting, priced on `min(R_now, R_slow)` alone: the
    /// program's TEST-ONLY mock venue (a CPMM stand-in without pool selection or a locked depth;
    /// PumpSwap's tier before M6R-1).
    Unlocked,
}

/// The cached Pyth SOL/USD price (`OracleCache`; ADOPTED Q8): USD per SOL is
/// `price·10^expo`, its confidence interval `±conf·10^expo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OraclePrice {
    /// Pyth price mantissa.
    pub price: i64,
    /// Pyth confidence (same exponent). [`assign`] converts the USD thresholds at
    /// `price − conf`, the low end of the interval (M7-SEC-7): a lower SOL price is a larger
    /// `θ` in lamports, so the uncertainty never tiers a market more generously.
    pub conf: u64,
    /// Pyth exponent.
    pub expo: i32,
    /// Pyth publish time, unix seconds.
    pub publish_time: i64,
}

/// Inputs of [`assign`] (ARCH §4.7; ECON §6.2–§6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TierInputs {
    /// Venue kind.
    pub venue: TierVenue,
    /// `R_now`: the adopted pool's real SOL (PumpSwap: the quote vault without VQR and less the
    /// fee buckets, at most `vault + VQR`; Raydium vault minus fee counters), lamports.
    pub r_now: u64,
    /// `Y(reprice(L_slow, P_ema))`: the SOL reserve of the slow-liquidity state at the
    /// EMA price, lamports (effective, i.e. incl. VQR on PumpSwap), from
    /// [`quote::reprice`](crate::quote::reprice()) (v0.4.1: `⌈L·isqrt⁺(P)/10⁹⌉`).
    pub y_slow: u128,
    /// The live virtual part of the effective reserve (0 on Raydium), lamports: on PumpSwap
    /// `max(virtual_quote_reserves + buckets, 0)` — the boost; `max(VQR, 0)` on a pool without
    /// the 2026-10-02 redeploy's fee buckets, which come off the VQR (PUMP-2026-10-1) —,
    /// subtracted from `R_slow` and from the locked SOL (ECON §6.2, #103, #105).
    pub vqr: u64,
    /// `P_ema`, WAD (for the Raydium and PumpSwap locked-depth bounds).
    pub p_ema: u128,
    /// Current unix time.
    pub now: i64,
    /// `Market.graduated_at` (first on-chain observation of the pool, or the listing
    /// time); ≤ 0 means unknown and tiers as C.
    pub graduated_at: i64,
    /// Cached SOL/USD price.
    pub oracle: OraclePrice,
    /// `oracle_stale_s`: older caches tier graduated markets as C.
    pub oracle_stale_s: u32,
    /// `theta_A` in USD·10⁶.
    pub theta_a_usd_e6: u64,
    /// `theta_B` in USD·10⁶.
    pub theta_b_usd_e6: u64,
}

/// Locked SOL of a Raydium pool at the EMA price, `⌊D·isqrt(P_ema)/10⁹⌋` lamports
/// (ECON §2.1, §6.2; ARCH §2.6 rule 5, §4.7): the SOL side of the locked depth `D`
/// ([`pool_select::locked_depth`](crate::pool_select::locked_depth)). Saturates at
/// `u128::MAX` (a bound that large never binds).
pub fn locked_sol(locked_depth: u128, p_ema: u128) -> u128 {
    // The divisor is 10⁹, so `Overflow` is the only error.
    mul_div(locked_depth, p_ema.isqrt(), SQRT_WAD, Rounding::Floor).unwrap_or(u128::MAX)
}

/// Pool liquidity `Λ = 2·R_eff` in lamports, `R_eff = min(R_now, R_slow)` with
/// `R_slow = Y(reprice(L_slow, P_ema)) − VQR` (≥ 0), and on Raydium also
/// `R_eff ≤ ⌊locked·10⁴/λ_min_bps⌋` with `locked = ⌊D·isqrt(P_ema)/10⁹⌋` ([`locked_sol`];
/// locked SOL over `λ_min`, V2-8), each term floored separately (ECON §2.1; ARCH §4.7;
/// v0.3 floored the product once) (ADOPTED Q19; ECON §6.2). On PumpSwap (M6R-1, ECON #105) the
/// same bound on the burned LP's locked SOL less the virtual reserve,
/// `R_eff ≤ ⌊max(locked − VQR, 0)·10⁴/λ_min_bps⌋` (`inp.vqr` is `max(VQR + buckets, 0)`): only real
/// SOL counts (Q19); at the program's `λ_min` =
/// [`PUMPSWAP_LOCK_MIN_BPS`](crate::consts::PUMPSWAP_LOCK_MIN_BPS) (100 %, OWNER-M6D-1, ECON
/// #106) a deposit held to push `L_slow` credits nothing beyond `D` (at a `λ_min` of 50 % it
/// credited up to `λ/λ_min` of `D`). `None` for the curve, which is tiered by venue.
///
/// Errors: `DivByZero` if `λ_min = 0`.
pub fn liquidity_lamports(inp: &TierInputs) -> Result<Option<u128>, MathError> {
    let r_slow = inp.y_slow.saturating_sub(u128::from(inp.vqr));
    let mut r_eff = u128::from(inp.r_now).min(r_slow);
    let (locked, lambda_min_bps) = match inp.venue {
        TierVenue::PumpCurve => return Ok(None),
        TierVenue::Unlocked => return Ok(Some(r_eff.saturating_mul(2))),
        TierVenue::PumpSwap {
            locked_depth,
            lambda_min_bps,
        } => (
            locked_sol(locked_depth, inp.p_ema).saturating_sub(u128::from(inp.vqr)),
            lambda_min_bps,
        ),
        TierVenue::Raydium {
            locked_depth,
            lambda_min_bps,
        } => (locked_sol(locked_depth, inp.p_ema), lambda_min_bps),
    };
    let bound = overflow_to_max(mul_div(
        locked,
        u128::from(BPS),
        u128::from(lambda_min_bps),
        Rounding::Floor,
    ))?;
    r_eff = r_eff.min(bound);
    Ok(Some(r_eff.saturating_mul(2)))
}

/// `Λ·price·10^(expo+6)` vs `θ_usd_e6·10⁹`: is `Λ < θ` in lamports?
/// `None` if the oracle exponent is out of range, or if the scaled side exceeds even 256
/// bits (both tier as C).
///
/// u128 first: `θ_usd_e6·10⁹ < 2⁹⁴` always fits, and `Λ·price·10^(expo+6)` does for any
/// realistic pool (`Λ ≤ 10¹⁴` lamports, a Pyth price `≤ 10¹³`, `|expo + 6| ≤ 4`); ECON §2.2's
/// overflow table does not list this product. Only when a side leaves u128 does the
/// comparison fall back to [`below_threshold_wide`], which gives the same answer (M0B-S-4).
fn below_threshold(lambda: u128, price: u64, expo: i32, theta_usd_e6: u64) -> Option<bool> {
    let shift = expo.checked_add(6)?;
    let k = shift.unsigned_abs();
    if k > MAX_ORACLE_SHIFT {
        return None;
    }
    let pow = 10u128.checked_pow(k)?;
    let up = shift >= 0;
    let narrow = || {
        let lhs = lambda.checked_mul(u128::from(price))?;
        let rhs = u128::from(theta_usd_e6).checked_mul(SQRT_WAD)?;
        Some(if up {
            lhs.checked_mul(pow)? < rhs
        } else {
            lhs < rhs.checked_mul(pow)?
        })
    };
    narrow().or_else(|| below_threshold_wide(lambda, price, up, pow, theta_usd_e6))
}

/// [`below_threshold`] in 256 bits, `pow = 10^|expo + 6|` on the side `up` names; `None`
/// if the scaled side leaves 256 bits.
fn below_threshold_wide(
    lambda: u128,
    price: u64,
    up: bool,
    pow: u128,
    theta_usd_e6: u64,
) -> Option<bool> {
    let pow = U256::from_u128(pow);
    let lhs = U256::from_product(lambda, u128::from(price));
    let rhs = U256::from_product(u128::from(theta_usd_e6), SQRT_WAD);
    Some(if up {
        lhs.checked_mul(pow)? < rhs
    } else {
        lhs < rhs.checked_mul(pow)?
    })
}

/// Tier of a market for new opens (ARCH §4.7; ECON §6.2–§6.3; SPEC §4):
///
/// ```text
/// PumpCurve                              -> D
/// oracle older than oracle_stale_s       -> C   (fail safe; also a non-positive price)
/// age < 24 h or Λ < θ_B                  -> C
/// Λ ≥ θ_A and age ≥ 7 d                  -> A
/// otherwise                              -> B
/// ```
///
/// with `Λ` from [`liquidity_lamports`], `age = now − graduated_at` and
/// `θ_lamports = θ_usd_e6·10⁹/(price·10^(expo+6))` compared exactly, `price` the conservative
/// `oracle.price − oracle.conf` (M7-SEC-7; a non-positive one tiers C). Anything that makes
/// the inputs unusable (unknown graduation time, bad oracle, `λ_min = 0`) tiers as C,
/// the tier with the largest cushion.
pub fn assign(inp: &TierInputs) -> Tier {
    let lambda = match liquidity_lamports(inp) {
        Ok(Some(l)) => l,
        Ok(None) => return Tier::D,
        Err(_) => return Tier::C,
    };
    let stale = inp.now.saturating_sub(inp.oracle.publish_time) > i64::from(inp.oracle_stale_s);
    // M7-SEC-7: the conservative end of the confidence interval (≤ `oracle_max_conf_bps` of the
    // price, which `crank_sol_usd` enforces).
    let price = match u64::try_from(inp.oracle.price).map(|p| p.saturating_sub(inp.oracle.conf)) {
        Ok(p) if p > 0 && !stale => p,
        _ => return Tier::C,
    };
    if inp.graduated_at <= 0 {
        return Tier::C;
    }
    let age = inp.now.saturating_sub(inp.graduated_at);
    let (Some(below_b), Some(below_a)) = (
        below_threshold(lambda, price, inp.oracle.expo, inp.theta_b_usd_e6),
        below_threshold(lambda, price, inp.oracle.expo, inp.theta_a_usd_e6),
    ) else {
        return Tier::C;
    };
    if age < AGE_C_S || below_b {
        Tier::C
    } else if !below_a && age >= AGE_A_S {
        Tier::A
    } else {
        Tier::B
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    const SOL: u64 = 1_000_000_000;

    fn inputs(real_sol: u64, age: i64) -> TierInputs {
        let now = 1_800_000_000;
        TierInputs {
            venue: TierVenue::Unlocked,
            r_now: real_sol * SOL,
            y_slow: u128::from(real_sol * SOL),
            vqr: 0,
            p_ema: crate::num::WAD,
            now,
            graduated_at: now - age,
            // $121.20 per SOL, Pyth style.
            oracle: OraclePrice {
                price: 12_120_000_000,
                conf: 0,
                expo: -8,
                publish_time: now - 30,
            },
            oracle_stale_s: 600,
            theta_a_usd_e6: 500_000_000_000,
            theta_b_usd_e6: 100_000_000_000,
        }
    }

    /// M7-SEC-7: the thresholds convert at `price − conf`, the low end of the confidence
    /// interval: a market exactly at θ_A on the mid price tiers B once the interval is counted, and
    /// a confidence as wide as the price fails safe to C.
    #[test]
    fn the_confidence_interval_never_tiers_more_generously() {
        let mut at = inputs(2_063, AGE_A_S);
        assert_eq!(assign(&at), Tier::A);
        at.oracle.conf = 5_000_000; // $0.05 on $121.20 (4 bp)
        assert_eq!(assign(&at), Tier::B, "Λ just above θ_A at the mid price");
        let mut b = inputs(2_100, AGE_A_S);
        b.oracle.conf = 5_000_000;
        assert_eq!(assign(&b), Tier::A, "comfortably above θ_A");
        b.oracle.conf = u64::try_from(b.oracle.price).unwrap();
        assert_eq!(assign(&b), Tier::C, "price − conf ≤ 0");
    }

    #[test]
    fn arch_examples() {
        // ARCH §4.7: at $121.2, $500 k ≈ 4,125 SOL of Λ and $100 k ≈ 825 SOL.
        assert_eq!(assign(&inputs(2_063, AGE_A_S)), Tier::A);
        assert_eq!(assign(&inputs(2_062, AGE_A_S)), Tier::B);
        assert_eq!(assign(&inputs(413, AGE_A_S)), Tier::B);
        assert_eq!(assign(&inputs(412, AGE_A_S)), Tier::C);
        assert_eq!(assign(&inputs(38_100, AGE_C_S - 1)), Tier::C);
        assert_eq!(assign(&inputs(38_100, AGE_A_S - 1)), Tier::B);
        // A fresh pump graduate (≈ 67 SOL real) is C.
        assert_eq!(assign(&inputs(67, AGE_A_S)), Tier::C);
    }

    #[test]
    fn fail_safe_and_curve() {
        let mut i = inputs(38_100, AGE_A_S);
        i.venue = TierVenue::PumpCurve;
        assert_eq!(assign(&i), Tier::D);
        let mut i = inputs(38_100, AGE_A_S);
        i.oracle.publish_time = i.now - 3_601;
        assert_eq!(assign(&i), Tier::C);
        let mut i = inputs(38_100, AGE_A_S);
        i.oracle.price = 0;
        assert_eq!(assign(&i), Tier::C);
        let mut i = inputs(38_100, AGE_A_S);
        i.graduated_at = 0;
        assert_eq!(assign(&i), Tier::C);
        let mut i = inputs(38_100, AGE_A_S);
        i.oracle.expo = 40;
        assert_eq!(assign(&i), Tier::C);
        // VQR is not liquidity.
        let mut i = inputs(4_000, AGE_A_S);
        i.vqr = 3_900 * SOL;
        assert_eq!(assign(&i), Tier::C);
    }

    #[test]
    fn raydium_locked_depth_bounds_liquidity() {
        let mut i = inputs(38_100, AGE_A_S);
        // Locked depth worth 1,000 SOL at P = 1 WAD, λ_min 50 % → R_eff 2,000 SOL.
        i.venue = TierVenue::Raydium {
            locked_depth: u128::from(1_000 * SOL),
            lambda_min_bps: 5_000,
        };
        assert_eq!(liquidity_lamports(&i), Ok(Some(u128::from(4_000 * SOL))));
        assert_eq!(assign(&i), Tier::B);
        i.venue = TierVenue::Raydium {
            locked_depth: 1,
            lambda_min_bps: 0,
        };
        assert_eq!(assign(&i), Tier::C);
        // ECON §2.1: locked = ⌊D·isqrt(P)/10⁹⌋ first, then ⌊locked·10⁴/λ_min⌋.
        // D = 1,999,999,999, isqrt(P) = 10⁹ + 1: D·isqrt(P)/10⁹ = 2,000,000,000.99…,
        // locked = 2·10⁹; over λ_min 3,000: 6,666,666,666 (v0.3's single floor of the
        // product gave 6,666,666,669).
        let p = 1_000_000_001u128 * 1_000_000_001;
        assert_eq!(locked_sol(1_999_999_999, p), 2_000_000_000);
        i.p_ema = p;
        i.venue = TierVenue::Raydium {
            locked_depth: 1_999_999_999,
            lambda_min_bps: 3_000,
        };
        assert_eq!(liquidity_lamports(&i), Ok(Some(2 * 6_666_666_666)));
        // isqrt, not isqrt⁺: one below the square gives isqrt 10⁹.
        assert_eq!(locked_sol(1_999_999_999, p - 1), 1_999_999_999);
        assert_eq!(locked_sol(u128::MAX, u128::MAX), u128::MAX);
        assert_eq!(locked_sol(5, 0), 0);
    }

    /// M6R-1 (ECON #105): a PumpSwap pool's tier liquidity is bounded by its burned LP's locked
    /// SOL less `max(VQR + buckets, 0)`, over `λ_min`, each term floored first; a bound past u128
    /// saturates and never binds; `λ_min = 0` tiers as C.
    #[test]
    fn pumpswap_locked_depth_bounds_liquidity() {
        let mut i = inputs(38_100, AGE_A_S);
        i.vqr = 100 * SOL;
        i.y_slow = u128::from(38_200 * SOL);
        // Locked depth worth 1,100 SOL at P = 1 WAD, less VQR 100 SOL, λ_min 50 % → 2,000 SOL.
        i.venue = TierVenue::PumpSwap {
            locked_depth: u128::from(1_100 * SOL),
            lambda_min_bps: 5_000,
        };
        assert_eq!(liquidity_lamports(&i), Ok(Some(u128::from(4_000 * SOL))));
        assert_eq!(assign(&i), Tier::B);
        // The same depth on Raydium ignores VQR (0 there anyway): 2,200 SOL.
        i.venue = TierVenue::Raydium {
            locked_depth: u128::from(1_100 * SOL),
            lambda_min_bps: 5_000,
        };
        assert_eq!(liquidity_lamports(&i), Ok(Some(u128::from(4_400 * SOL))));
        // Locked SOL below VQR: no real SOL is locked, Λ = 0 (C).
        i.venue = TierVenue::PumpSwap {
            locked_depth: u128::from(99 * SOL),
            lambda_min_bps: 5_000,
        };
        assert_eq!(liquidity_lamports(&i), Ok(Some(0)));
        assert_eq!(assign(&i), Tier::C);
        // Idle: a depth far above R_now leaves min(R_now, R_slow).
        i.venue = TierVenue::PumpSwap {
            locked_depth: u128::MAX,
            lambda_min_bps: 2_000,
        };
        assert_eq!(
            liquidity_lamports(&i),
            Ok(Some(u128::from(2 * 38_100 * SOL)))
        );
        i.venue = TierVenue::Unlocked;
        assert_eq!(
            liquidity_lamports(&i),
            Ok(Some(u128::from(2 * 38_100 * SOL)))
        );
        i.venue = TierVenue::PumpSwap {
            locked_depth: 1,
            lambda_min_bps: 0,
        };
        assert_eq!(assign(&i), Tier::C);
        // OWNER-M6D-1 (ECON #106): at the program's λ_min = PUMPSWAP_LOCK_MIN_BPS the bound is the
        // burned LP's real SOL itself — 1,100 − 100 = 1,000 SOL, Λ 2,000 SOL (Tier B's floor at
        // $121 is ≈ 826 SOL of Λ); with a held deposit L_slow and R_now above it change nothing.
        i.p_ema = crate::num::WAD;
        i.vqr = 100 * SOL;
        i.venue = TierVenue::PumpSwap {
            locked_depth: u128::from(1_100 * SOL),
            lambda_min_bps: crate::consts::PUMPSWAP_LOCK_MIN_BPS,
        };
        assert_eq!(liquidity_lamports(&i), Ok(Some(u128::from(2_000 * SOL))));
        i.y_slow = u128::from(3 * 38_200 * SOL);
        assert_eq!(liquidity_lamports(&i), Ok(Some(u128::from(2_000 * SOL))));
        i.y_slow = u128::from(38_200 * SOL);
        // Floors: ⌊D·isqrt(P)/10⁹⌋ = 2·10⁹, less VQR 10⁹, then ⌊10⁹·10⁴/3,000⌋.
        let p = 1_000_000_001u128 * 1_000_000_001;
        i.p_ema = p;
        i.vqr = SOL;
        i.venue = TierVenue::PumpSwap {
            locked_depth: 1_999_999_999,
            lambda_min_bps: 3_000,
        };
        assert_eq!(liquidity_lamports(&i), Ok(Some(2 * 3_333_333_333)));
    }

    /// M0B-S-4 (review): the u128 fast path of the threshold comparison is bit-identical
    /// to the 256-bit comparison it replaced, on both sides of every u128 edge.
    #[test]
    fn threshold_fast_path_equals_256_bits() {
        let wide = |lambda: u128, price: u64, expo: i32, theta: u64| {
            let shift = expo.checked_add(6)?;
            let k = shift.unsigned_abs();
            if k > MAX_ORACLE_SHIFT {
                return None;
            }
            below_threshold_wide(lambda, price, shift >= 0, 10u128.pow(k), theta)
        };
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut narrow, mut fell_back, mut out_of_range) = (0, 0, 0);
        for i in 0..200_000u32 {
            // Half the exponents are Pyth-like (−12..=4), half anywhere in −46..=34.
            let expo = if i % 2 == 0 {
                i32::try_from(next() % 17).unwrap() - 12
            } else {
                i32::try_from(next() % 81).unwrap() - 46
            };
            let price = match i % 4 {
                0 => next() % 100_000_000_000_000,
                1 => next(),
                2 => u64::MAX - next() % 3,
                _ => 12_120_000_000,
            };
            let theta = match i % 3 {
                0 => 500_000_000_000,
                1 => next(),
                _ => next() % 100_000_000_000_000,
            };
            let shift = expo + 6;
            let k = shift.unsigned_abs().min(30);
            let lambda = match i % 5 {
                0 => u128::from(next()) * u128::from(next() % 1_000_000),
                1 => (u128::from(next()) << 64) | u128::from(next()),
                2 => u128::MAX >> (next() % 128),
                // The threshold itself in lamports, ± 1: the comparison flips here.
                3 => {
                    let t = u128::from(theta) * SQRT_WAD;
                    let p = u128::from(price.max(1));
                    let base = if shift >= 0 {
                        t / p / 10u128.pow(k)
                    } else {
                        t.saturating_mul(10u128.pow(k)) / p
                    };
                    (base + u128::from(next() % 3)).saturating_sub(1)
                }
                _ => u128::from(next() % 1_000_000) * 1_000_000_000,
            };
            let got = below_threshold(lambda, price, expo, theta);
            assert_eq!(
                got,
                wide(lambda, price, expo, theta),
                "{lambda} {price} {expo} {theta}"
            );
            // Which path answered (the fast path's own overflow conditions).
            let lhs = lambda.checked_mul(u128::from(price));
            let rhs = u128::from(theta) * SQRT_WAD;
            let fits = if shift >= 0 {
                lhs.and_then(|l| l.checked_mul(10u128.pow(k))).is_some()
            } else {
                lhs.is_some() && rhs.checked_mul(10u128.pow(k)).is_some()
            };
            match (shift.unsigned_abs() > MAX_ORACLE_SHIFT, fits) {
                (true, _) => out_of_range += 1,
                (false, true) => narrow += 1,
                (false, false) => fell_back += 1,
            }
        }
        assert!(
            narrow > 60_000 && fell_back > 30_000 && out_of_range > 5_000,
            "{narrow} {fell_back} {out_of_range}"
        );
        // The ARCH §4.7 example stays on the u128 path and flips at 4,125 SOL of Λ.
        let t = 500_000_000_000u64;
        assert_eq!(
            below_threshold(4_125 * u128::from(SOL), 12_120_000_000, -8, t),
            Some(true)
        );
        assert_eq!(
            below_threshold(4_126 * u128::from(SOL), 12_120_000_000, -8, t),
            Some(false)
        );
        assert_eq!(below_threshold(u128::MAX, u64::MAX, 24, t), None);
        assert_eq!(below_threshold(u128::MAX, u64::MAX, -36, t), Some(false));
        assert_eq!(below_threshold(1, 1, 31, t), None);
    }

    #[test]
    fn defaults_table() {
        assert_eq!(Tier::A.defaults().cr_liq_bps, 12_500);
        assert_eq!(Tier::D.defaults().min_rent_s, 14_400);
        assert_eq!(Tier::B.defaults().rate.slope1_bps, 9_375);
        for t in Tier::ALL {
            assert_eq!(Tier::from_u8(t as u8), Some(t));
            let p = t.defaults();
            assert!(p.cr_open_bps >= p.cr_liq_bps + 2_000);
        }
        assert_eq!(Tier::from_u8(4), None);
    }
}
