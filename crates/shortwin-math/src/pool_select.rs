//! Owner: quotes track. Raydium pool selection: locked LP share `λ`, locked depth `D`, eligibility and dominance
//! (ARCH §2.6, A42; ECON §2.1, §6.2).
//!
//! Only the arithmetic lives here. Account verification (owners, discriminators, vault
//! and mint keys, the SOL side) and `venue_executable` are the program's; their outcome
//! enters [`eligible`] as plain inputs. Rounding is ECON §2.1's (the sims' `RaydiumPool`):
//! `λ` is floored to whole bps first, then `D` is floored.

use crate::error::MathError;
use crate::fees::exceeds_cap;
use crate::num::{BPS, Rounding, mul_div, mul_le, overflow_to_max, sqrt_u128};

/// Permanently locked LP share `λ_bps = ⌊(lp_counter − lp_supply)·10⁴/lp_counter⌋`
/// (ECON §2.1; ARCH §2.6; the sims' `RaydiumPool.lock_bps`). `lp_counter` is the venue's
/// internal LP count (AMM v4 `lp_amount` @720, CPMM `lp_supply` @333) and `lp_supply` the
/// LP mint's circulating supply; the difference is LP that can never be redeemed. 0 for
/// a pool with no LP counter or a circulating supply at or above it; at most 10⁴.
pub fn lambda_bps(lp_counter: u64, lp_supply: u64) -> u32 {
    match lp_counter.checked_sub(lp_supply) {
        Some(locked) if lp_counter > 0 => {
            let lam = u128::from(locked)
                .saturating_mul(u128::from(BPS))
                .checked_div(u128::from(lp_counter))
                .unwrap_or(0);
            u32::try_from(lam).unwrap_or(u32::MAX)
        }
        _ => 0,
    }
}

/// Locked constant-product depth `D = ⌊λ_bps·isqrt(X·Y)/10⁴⌋` (ECON §2.1; ARCH §2.6; the
/// sims' `RaydiumPool.locked_depth`), with [`lambda_bps`] and `X`, `Y` the pricing reserves
/// (ARCH §4.1). v0.3 floored once on the raw ratio, `⌊(counter − supply)·isqrt(X·Y)/
/// counter⌋`, which can exceed this by up to `isqrt(X·Y)/10⁴`.
///
/// Rounded down: `D` bounds how much a market may be sized on (ARCH §2.6 rule 5), so the
/// pool is never credited with depth it does not have. `X·Y < 2¹²⁸` for `u64` reserves, so
/// the square root is `u128` (ARCH §2.2).
///
/// Errors: none in practice (`Overflow` is unreachable for `u64` reserves).
pub fn locked_depth(lp_counter: u64, lp_supply: u64, x: u64, y: u64) -> Result<u128, MathError> {
    let k = u128::from(x)
        .checked_mul(u128::from(y))
        .ok_or(MathError::Overflow)?;
    mul_div(
        u128::from(lambda_bps(lp_counter, lp_supply)),
        sqrt_u128(k, Rounding::Floor),
        u128::from(BPS),
        Rounding::Floor,
    )
}

/// `λ_bps ≥ min_bps`, with [`lambda_bps`]'s floor; equivalently `(lp_counter − lp_supply)·10⁴
/// ≥ min_bps·lp_counter`. False for a pool with no LP counter or a circulating supply above it.
///
/// **Not an eligibility clause since M4-SEC-1** (ARCH §2.6 as amended; owner item O18): `λ_now`
/// is lowered by anyone who *adds* unlocked liquidity — an ordinary, fee-earning LP position
/// that can stay in the pool — so a lock test on it lets a depositor keep every alternate pool
/// ineligible (no fast path, no adoption), while the locked depth `D` it leaves unchanged is
/// what every position is sized on (rule 5). It is also raised for one instruction by the
/// pool's own LP holder (withdraw, act, re-deposit; M4-SEC-3 c). Eligibility tests the locked
/// depth instead ([`locked_floor_ok`]).
pub fn lock_share_ok(lp_counter: u64, lp_supply: u64, min_bps: u64) -> bool {
    lp_counter > 0
        && lp_supply <= lp_counter
        && u64::from(lambda_bps(lp_counter, lp_supply)) >= min_bps
}

/// The eligibility floor on locked depth (M4-SEC-1, M4-SEC-3; ARCH §2.6 "Eligibility" as
/// amended): `⌊⌊D·isqrt(P_ref)/10⁹⌋·10⁴/λ_min⌋ ≥ pool_min_real_sol` — the locked SOL of the
/// pool at the reference price over `λ_min`, i.e. exactly the `D` term of rule 5's `R_eff`
/// (ARCH §4.7; [`crate::tier::locked_sol`], each term floored as there). Evaluated without the
/// last division: `pool_min_real_sol·λ_min ≤ locked·10⁴`.
///
/// Both inputs resist atomic manipulation: `D` is unchanged by unlocked deposits and
/// withdrawals and by swaps (but for the fee the pool keeps), and the caller passes the
/// market's `P_ema` as `P_ref` whenever the market has a price. It guarantees what the v0.4.1
/// pair `R ≥ pool_min_real_sol ∧ λ ≥ λ_min` guaranteed for good — at least `λ_min` of the floor
/// stays locked in the pool whatever its unlocked LPs do — and every pool that pair admitted at
/// the same price passes it.
///
/// `false` for `λ_min = 0` (a misconfiguration; the compiled bound is 2,000).
pub fn locked_floor_ok(
    locked_depth: u128,
    p_ref: u128,
    lambda_min_bps: u64,
    pool_min_real_sol: u64,
) -> bool {
    if lambda_min_bps == 0 {
        return false;
    }
    let locked = crate::tier::locked_sol(locked_depth, p_ref);
    mul_le(
        u128::from(pool_min_real_sol),
        u128::from(lambda_min_bps),
        locked,
        u128::from(BPS),
    )
}

/// The floor of an adoption that carries open positions (M4V-1; ARCH §2.6 "Eligibility" as
/// amended; owner item O18): the pool's **locked SOL** at the reference price itself,
/// `⌊D·isqrt(P_ref)/10⁹⌋ ≥ pool_min_real_sol` — [`locked_floor_ok`] without the `/λ_min`.
///
/// [`locked_floor_ok`] admits a fully locked pool holding only `λ_min·pool_min_real_sol` of real
/// SOL (12.5 SOL at the defaults): harmless for a listing, a zero-position or a list-only
/// adoption, whose market has no position and whose opens stay gated by `R_eff` (rule 5), but a
/// collection window's adoption moves **open** positions, and the SOL floor exists to keep them
/// off pools where holding the price costs less (M4-SEC-3 b's harm, reached without any
/// manipulation). Every window adoption — a Raydium market's fast path, and a list-only market
/// with open positions — therefore needs the pool's locked SOL alone to reach the floor. Like
/// [`locked_floor_ok`] it reads only `D` and `P_ref`: swap-, deposit- and withdraw-invariant
/// (the program passes the market's `P_ema`).
pub fn position_floor_ok(locked_depth: u128, p_ref: u128, pool_min_real_sol: u64) -> bool {
    locked_floor_ok(locked_depth, p_ref, BPS, pool_min_real_sol)
}

/// `⌊D·10⁴/λ_bps⌋`: the constant-product depth `√(X·Y)` a pool of locked depth `D` has at locked
/// share `λ_bps` (0 for `λ_bps = 0`; saturating). With `λ_min` it is the most liquidity rule 5
/// ever credits a pool with (ARCH §2.6 rule 5), and the **anchor** every Raydium adoption writes
/// into `L_ema` and the ring (M4R, **PROPOSED**, owner item O18; ARCH §2.6 rule 4 as amended):
/// deposit-, withdraw- and swap-invariant, so the adoption reads no sample of the new pool; the
/// post-change warm-up (`observe::integrate`) replaces it with the pool's observed depth.
pub fn depth_at_lambda(locked_depth: u128, lambda_bps: u64) -> u128 {
    if lambda_bps == 0 {
        return 0;
    }
    overflow_to_max(mul_div(
        locked_depth,
        u128::from(BPS),
        u128::from(lambda_bps),
        Rounding::Floor,
    ))
    .unwrap_or(u128::MAX)
}

/// The numeric facts of a verified candidate pool (ARCH §2.6 as amended by M4-SEC-1/-3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PoolFacts {
    /// `venue_executable(P)` (§5.3): status, open time, vaults at or above their counters.
    pub executable: bool,
    /// One-way fee φ in **ppm** ([`RayV4Fee::oneway_ppm`](crate::fees::RayV4Fee::oneway_ppm),
    /// [`CpmmFees::oneway_ppm`](crate::fees::CpmmFees::oneway_ppm); ECON §2.1).
    pub fee_ppm: u32,
    /// Locked constant-product depth `D` ([`locked_depth`]).
    pub locked_depth: u128,
    /// The reference price `P_ref` (WAD): the market's `P_ema` when it has a price, else the
    /// candidate's spot.
    pub p_ref: u128,
}

/// The pool-selection parameters `eligible` reads (ARCH §2.3.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EligibilityParams {
    /// `venue_fee_cap_bps` (150).
    pub venue_fee_cap_bps: u64,
    /// `pool_min_real_sol` (25 SOL).
    pub pool_min_real_sol: u64,
    /// `pool_lock_min_bps` λ_min (5,000).
    pub pool_lock_min_bps: u64,
}

/// `eligible(P)` (ARCH §2.6 as amended by M4-SEC-1 and M4-SEC-3): executable, `φ_ppm ≤
/// venue_fee_cap_bps·100` (no rounding, [`exceeds_cap`]) and the locked-depth floor
/// ([`locked_floor_ok`]). v0.4.1's `R ≥ pool_min_real_sol` (the live SOL reserve, raised for
/// one instruction by a buy) and `λ ≥ pool_lock_min_bps` (lowered for good by any unlocked
/// deposit, raised for one instruction by a withdrawal) are gone. The caller has already
/// verified the accounts.
pub fn eligible(p: &PoolFacts, params: &EligibilityParams) -> bool {
    p.executable
        && !exceeds_cap(p.fee_ppm, params.venue_fee_cap_bps)
        && locked_floor_ok(
            p.locked_depth,
            p.p_ref,
            params.pool_lock_min_bps,
            params.pool_min_real_sol,
        )
}

/// `eligible(P)` for an adoption that carries open positions (M4V-1; ARCH §2.6): [`eligible`]
/// and [`position_floor_ok`] — the candidate of a collection window, at its proposal and at its
/// execution.
pub fn eligible_with_positions(p: &PoolFacts, params: &EligibilityParams) -> bool {
    eligible(p, params) && position_floor_ok(p.locked_depth, p.p_ref, params.pool_min_real_sol)
}

/// `d_cand ≥ (1 + margin_bps/10⁴)·d_inc`, exactly (ARCH §2.6 rule 1 with
/// `pool_list_margin_bps`, rule 2 with `pool_switch_margin_bps`).
pub fn dominates(d_cand: u128, d_inc: u128, margin_bps: u64) -> bool {
    at_least_bps(
        d_cand,
        d_inc,
        u128::from(BPS).saturating_add(u128::from(margin_bps)),
    )
}

/// `d_cand ≥ min_bps·d_inc/10⁴`, exactly: a candidate that may open the fast-path
/// collection window (ARCH §2.6 rule 3, `pool_fast_min_bps`).
pub fn fast_path_qualifies(d_cand: u128, d_inc: u128, min_bps: u64) -> bool {
    at_least_bps(d_cand, d_inc, u128::from(min_bps))
}

fn at_least_bps(d_cand: u128, d_inc: u128, bps: u128) -> bool {
    mul_le(d_inc, bps, d_cand, u128::from(BPS))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn depth() {
        // Fartcoin AMM v4 (R-RAY §2.1): lp_amount 4,051,966,758,784, LP supply
        // 8,906,225,417 -> λ = 99.78 %.
        let (c, s) = (4_051_966_758_784u64, 8_906_225_417u64);
        let (x, y) = (24_145_801_124_347u64, 38_155_515_042_581u64);
        let d = locked_depth(c, s, x, y).unwrap();
        let root = sqrt_u128(u128::from(x) * u128::from(y), Rounding::Floor);
        // λ floored to whole bps first (ECON §2.1): 9,978 bps, then D floored.
        assert_eq!(lambda_bps(c, s), 9_978);
        assert_eq!(d, 9_978 * root / 10_000);
        // v0.3's single floor on the raw ratio is larger here.
        assert!((u128::from(c - s) * root) / u128::from(c) > d);
        assert!(lock_share_ok(c, s, 9_978) && !lock_share_ok(c, s, 9_979));
        assert_eq!(locked_depth(0, 0, x, y), Ok(0));
        assert_eq!(locked_depth(5, 7, x, y), Ok(0));
        assert_eq!(locked_depth(5, 0, x, y), Ok(root));
        assert_eq!(
            locked_depth(u64::MAX, 0, u64::MAX, u64::MAX),
            Ok(u128::from(u64::MAX))
        );
        assert_eq!(
            (lambda_bps(0, 0), lambda_bps(5, 7), lambda_bps(5, 5)),
            (0, 0, 0)
        );
        assert_eq!((lambda_bps(3, 1), lambda_bps(u64::MAX, 0)), (6_666, 10_000));
        assert!(!lock_share_ok(0, 0, 0));
        assert!(lock_share_ok(5, 5, 0) && !lock_share_ok(5, 6, 0));
    }

    #[test]
    fn rules() {
        let params = EligibilityParams {
            venue_fee_cap_bps: 150,
            pool_min_real_sol: 25_000_000_000,
            pool_lock_min_bps: 5_000,
        };
        // At P_ref = 1 lamport per raw unit (isqrt = 10⁹) the locked SOL is `D` itself, and the
        // floor is `D·10⁴/λ_min ≥ 25 SOL`: D ≥ 12.5 SOL.
        let wad = crate::num::WAD;
        let p = PoolFacts {
            executable: true,
            fee_ppm: 2_500,
            locked_depth: 12_500_000_000,
            p_ref: wad,
        };
        assert!(eligible(&p, &params));
        assert!(!eligible(
            &PoolFacts {
                locked_depth: 12_499_999_999,
                ..p
            },
            &params
        ));
        // At 4 lamports per raw unit (isqrt 2·10⁹) half the depth carries the same SOL.
        assert!(eligible(
            &PoolFacts {
                locked_depth: 6_250_000_000,
                p_ref: 4 * wad,
                ..p
            },
            &params
        ));
        assert!(!eligible(
            &PoolFacts {
                locked_depth: 6_249_999_999,
                p_ref: 4 * wad,
                ..p
            },
            &params
        ));
        // φ is compared in ppm with cap·100: 1.5 % passes, 1.5001 % does not.
        assert!(eligible(
            &PoolFacts {
                fee_ppm: 15_000,
                ..p
            },
            &params
        ));
        assert!(!eligible(
            &PoolFacts {
                fee_ppm: 15_001,
                ..p
            },
            &params
        ));
        assert!(!eligible(
            &PoolFacts {
                executable: false,
                ..p
            },
            &params
        ));
        // No price, no depth, or λ_min = 0: never eligible.
        assert!(!eligible(&PoolFacts { p_ref: 0, ..p }, &params));
        assert!(!eligible(
            &PoolFacts {
                locked_depth: 0,
                ..p
            },
            &params
        ));
        assert!(!locked_floor_ok(u128::MAX, u128::MAX, 0, 1));
        assert!(locked_floor_ok(u128::MAX, u128::MAX, 10_000, u64::MAX));

        // M4-SEC-1: an unlocked deposit as large as the pool halves λ (99.78 % → 49.89 %,
        // under λ_min) and leaves D, hence eligibility, as it was; the v0.4.1 λ clause failed.
        let (c, s) = (4_051_966_758_784u64, 8_906_225_417u64);
        let (x, y) = (24_145_801_124_347u64, 38_155_515_042_581u64);
        let d0 = locked_depth(c, s, x, y).unwrap();
        let d1 = locked_depth(2 * c, s + c, 2 * x, 2 * y).unwrap();
        assert!(lambda_bps(2 * c, s + c) < 5_000 && !lock_share_ok(2 * c, s + c, 5_000));
        assert!(d1.abs_diff(d0) * 10_000 <= d0 * 3, "D {d0} -> {d1}");
        let p_spot = u128::from(y) * wad / u128::from(x);
        for d in [d0, d1] {
            assert!(eligible(
                &PoolFacts {
                    locked_depth: d,
                    p_ref: p_spot,
                    ..p
                },
                &params
            ));
        }
    }

    #[test]
    fn the_adoption_anchor_is_the_depth_at_lambda_min() {
        // D = 1,000 at λ_min = 50 %: at most 2,000 is credited, and that is the anchor.
        assert_eq!(depth_at_lambda(1_000, 5_000), 2_000);
        assert_eq!(depth_at_lambda(1_000, 10_000), 1_000);
        assert_eq!(depth_at_lambda(1_000, 8_000), 1_250);
        assert_eq!(depth_at_lambda(1_000, 0), 0);
        assert_eq!(depth_at_lambda(u128::MAX, 2_000), u128::MAX);
    }

    #[test]
    fn position_floor_is_the_locked_sol_itself() {
        // M4V-1: at 1 lamport per raw unit the locked SOL is D. The listing floor admits D ≥
        // 12.5 SOL (over λ_min = 50 %); an adoption carrying positions needs D ≥ 25 SOL.
        let wad = crate::num::WAD;
        let floor = 25_000_000_000u64;
        assert!(locked_floor_ok(12_500_000_000, wad, 5_000, floor));
        assert!(!position_floor_ok(12_500_000_000, wad, floor));
        assert!(!position_floor_ok(24_999_999_999, wad, floor));
        assert!(position_floor_ok(25_000_000_000, wad, floor));
        // At 4 lamports per raw unit half the depth carries the same SOL.
        assert!(position_floor_ok(12_500_000_000, 4 * wad, floor));
        assert!(!position_floor_ok(12_499_999_999, 4 * wad, floor));
        assert!(!position_floor_ok(u128::MAX, 0, 1));
        let params = EligibilityParams {
            venue_fee_cap_bps: 150,
            pool_min_real_sol: floor,
            pool_lock_min_bps: 5_000,
        };
        let p = PoolFacts {
            executable: true,
            fee_ppm: 2_500,
            locked_depth: 20_000_000_000,
            p_ref: wad,
        };
        assert!(eligible(&p, &params) && !eligible_with_positions(&p, &params));
        let deep = PoolFacts {
            locked_depth: 25_000_000_000,
            ..p
        };
        assert!(eligible_with_positions(&deep, &params));
        assert!(!eligible_with_positions(
            &PoolFacts {
                executable: false,
                ..deep
            },
            &params
        ));
    }

    #[test]
    fn margins() {
        assert!(dominates(10_500, 10_000, 500) && !dominates(10_499, 10_000, 500));
        assert!(dominates(12_500, 10_000, 2_500) && !dominates(12_499, 10_000, 2_500));
        assert!(dominates(0, 0, 0) && dominates(u128::MAX, u128::MAX, 0));
        assert!(!dominates(u128::MAX, u128::MAX, 1));
        assert!(fast_path_qualifies(2_500, 10_000, 2_500));
        assert!(!fast_path_qualifies(2_499, 10_000, 2_500));
    }
}
