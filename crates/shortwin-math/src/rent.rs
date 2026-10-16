//! Owner: rent track. Kinked rent rate, the three per-market basis indices, rent due, lender credit and the per-position true-up, minimum rent (ECON §2.1, §7; ARCH §4.5).
//!
//! Rent is `∫ Q·P̃·r(U) dt` with the SPEC §6.1 kinked curve
//! `r(U) = base + slope1·min(U, k)/10⁴ + slope2·max(U − k, 0)/10⁴` (APR, bps) and
//! `P̃ = min(P_ema, P_ACC_MAX)`. The market integrates `P̃`, `P̃·min(U, k)` and
//! `P̃·max(U − k, 0)` into three basis indices `J0, J1, J2` (A36); a position locks its own
//! `(base, slope1, slope2)` **and its protocol share `rent_protocol_bps`** at open
//! (R3A-5, R3B-9) and owes `⌈Q·(base·ΔJ0 + slope1·ΔJ1 + slope2·ΔJ2)/(10⁴·31,536,000·WAD)⌉`.
//!
//! The lenders' part of the rent is credited continuously from the aggregate
//! `A_j = Σ_open Q·coef_j·(10⁴ − rent_protocol_bps_pos)` ([`Aggr`], [`lender_credit_carried`],
//! floor, the remainder carried to the next accrual since AIR-03) and trued up per position at
//! its exit ([`credited_for`], ceil), so both describe the same money whatever governance does
//! to `rent_protocol_bps` (ECON I-P7).
//!
//! Rounding follows the ECON §2.1 table: index increments, rent due, the true-up and
//! minimum rent up; the continuous lender credit down (its remainder carried, so the credits
//! of all accruals sum to the floor of their exact total). Products that ECON §2.2 lets exceed
//! 2¹²⁸ (`Q·coef·ΔJ`, `A_j·δJ_j`, `Q·P_ema·r·T_min`) take a 256-bit intermediate only when
//! the u128 product overflows.

use crate::error::MathError;
use crate::num::{BPS, Rounding, U256, div, mul_div, to_u64};

/// Seconds per rent year (365 days; ECON §7).
pub const YEAR_S: u64 = 31_536_000;

/// `P_ACC_MAX = 10²⁹` (WAD; a price of 100 SOL per raw unit): rent accrual clamps
/// `P_ema` here, which keeps every `J_i` below 2¹²⁸ for 100 years (ECON §2.2; ARCH §2.2
/// "Price clamps"; sim `P_ACC_MAX`). Defined in [`crate::consts`].
pub use crate::consts::P_ACC_MAX;

/// `10⁴·31,536,000·WAD`: denominator of the rent integral (ECON §7).
const RENT_DEN: u128 = 315_360_000_000_000_000_000_000_000_000;
/// `10⁴·10⁴·31,536,000·WAD`: [`RENT_DEN`] with the lender share in bps (ECON §7).
const RENT_DEN_BPS: u128 = 3_153_600_000_000_000_000_000_000_000_000_000;

/// Largest utilisation or kink, in bps (100 %).
const MAX_UTIL_BPS: u32 = 10_000;

/// A rate curve (SPEC §6.1), locked into each position at open (ADOPTED Q9; ARCH §2.3.5).
///
/// All three are APR in bps; the slopes are "per 100 % of utilisation", so the rate at
/// the kink `k` is `base + slope1·k/10⁴` (ARCH §2.3.3 `TierParams`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RentTerms {
    /// `rate_base_bps`.
    pub base_bps: u32,
    /// `rate_slope1_bps`: slope below the kink.
    pub slope1_bps: u32,
    /// `rate_slope2_bps`: slope above the kink.
    pub slope2_bps: u32,
}

impl RentTerms {
    /// The coefficients `(base, slope1, slope2)` that multiply `(ΔJ0, ΔJ1, ΔJ2)`.
    pub const fn coefficients(&self) -> [u32; 3] {
        [self.base_bps, self.slope1_bps, self.slope2_bps]
    }
}

/// The market's rent basis indices `J0, J1, J2` (ARCH §2.3.4 `rent_basis`; A36).
///
/// `J0 = Σ P̃·Δt`, `J1 = Σ ⌈P̃·min(U, k)·Δt/10⁴⌉`, `J2 = Σ ⌈P̃·max(U − k, 0)·Δt/10⁴⌉`
/// (WAD lamports per raw token unit × seconds), `P̃ = min(P_ema, P_ACC_MAX)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Basis {
    /// `[J0, J1, J2]`.
    pub j: [u128; 3],
}

/// `rent_aggr`: `A_j = Σ over open positions of Q·coef_j·(10⁴ − rent_protocol_bps_pos)`
/// (ARCH §2.3.4, §4.5; ECON §7). The protocol share is folded in per position (R3A-5), so
/// [`lender_credit`] needs no global share.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Aggr {
    /// `[A0, A1, A2]`.
    pub a: [u128; 3],
}

/// `q·coef_j·(10⁴ − p)` for each `j`: one position's contribution to [`Aggr`].
fn folded(q: u64, terms: &RentTerms, rent_protocol_bps: u32) -> Result<[u128; 3], MathError> {
    let share = check_share(rent_protocol_bps)?;
    let mut out = [0u128; 3];
    for (o, c) in out.iter_mut().zip(terms.coefficients()) {
        *o = u128::from(q)
            .checked_mul(u128::from(c))
            .and_then(|x| x.checked_mul(share))
            .ok_or(MathError::Overflow)?;
    }
    Ok(out)
}

impl Aggr {
    /// `A_j += q·coef_j·(10⁴ − rent_protocol_bps)` when a position opens, with the
    /// position's snapshot of `rent_protocol_bps` (ARCH §3.4 step 13).
    ///
    /// Errors: `InvalidInput` if `rent_protocol_bps > 10⁴`; `Overflow`. On error `self`
    /// is unchanged.
    pub fn add(
        &mut self,
        q: u64,
        terms: &RentTerms,
        rent_protocol_bps: u32,
    ) -> Result<(), MathError> {
        let d = folded(q, terms, rent_protocol_bps)?;
        let mut next = self.a;
        for (acc, d) in next.iter_mut().zip(d) {
            *acc = acc.checked_add(d).ok_or(MathError::Overflow)?;
        }
        self.a = next;
        Ok(())
    }

    /// `A_j −= q·coef_j·(10⁴ − rent_protocol_bps)` when a position exits, with the same
    /// snapshot it was added with (ARCH §3.5 step 7, §3.8).
    ///
    /// Errors: `InvalidInput` if `rent_protocol_bps > 10⁴`; `Underflow` if the aggregate
    /// does not contain the position; `Overflow`. On error `self` is unchanged.
    pub fn sub(
        &mut self,
        q: u64,
        terms: &RentTerms,
        rent_protocol_bps: u32,
    ) -> Result<(), MathError> {
        let d = folded(q, terms, rent_protocol_bps)?;
        let mut next = self.a;
        for (acc, d) in next.iter_mut().zip(d) {
            *acc = acc.checked_sub(d).ok_or(MathError::Underflow)?;
        }
        self.a = next;
        Ok(())
    }
}

/// What a position's rent depends on (ARCH §2.3.5: `q`, `j_open`, the locked rate curve,
/// the locked `rent_protocol_bps`, `r_min`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PosRent {
    /// Tokens owed.
    pub q: u64,
    /// The market's [`Basis`] at open.
    pub j_open: [u128; 3],
    /// The rate curve locked at open.
    pub terms: RentTerms,
    /// The protocol's share of the rent, bps, snapshot at open (R3A-5, R3B-9); used by
    /// [`credited_for`] and by the waterfalls' rent split.
    pub rent_protocol_bps: u32,
    /// Minimum rent fixed at open ([`min_rent`]), lamports.
    pub r_min: u64,
}

fn check_util(util_bps: u32, kink_bps: u32) -> Result<(u32, u32), MathError> {
    if util_bps > MAX_UTIL_BPS || kink_bps > MAX_UTIL_BPS {
        return Err(MathError::InvalidInput);
    }
    Ok((util_bps.min(kink_bps), util_bps.saturating_sub(kink_bps)))
}

/// `10⁴ − rent_protocol_bps`, the lenders' share.
fn check_share(rent_protocol_bps: u32) -> Result<u128, MathError> {
    u128::from(BPS)
        .checked_sub(u128::from(rent_protocol_bps))
        .ok_or(MathError::InvalidInput)
}

/// Utilisation `U = ⌈ΣB_Q·10⁴/(T + ΣB_Q)⌉` in bps, rounded up (ECON §2.1, §7; ARCH §4.5).
///
/// `t_free` is the unborrowed inventory `T`, `borrowed` is `ΣB_Q`; SOL claims are not
/// counted. An empty pool has `U = 0`.
pub fn utilization_bps(t_free: u64, borrowed: u64) -> Result<u32, MathError> {
    let total = u128::from(t_free)
        .checked_add(u128::from(borrowed))
        .ok_or(MathError::Overflow)?;
    if total == 0 {
        return Ok(0);
    }
    let n = u128::from(borrowed)
        .checked_mul(u128::from(BPS))
        .ok_or(MathError::Overflow)?;
    let u = div(n, total, Rounding::Ceil)?;
    u32::try_from(u).map_err(|_| MathError::Overflow)
}

/// `r(U)·10⁴` exactly: `base·10⁴ + slope1·min(U, k) + slope2·max(U − k, 0)` (SPEC §6.1).
fn rate_x1e4(terms: &RentTerms, util_bps: u32, kink_bps: u32) -> Result<u128, MathError> {
    let (below, above) = check_util(util_bps, kink_bps)?;
    let parts = [
        u128::from(terms.base_bps).checked_mul(u128::from(BPS)),
        u128::from(terms.slope1_bps).checked_mul(u128::from(below)),
        u128::from(terms.slope2_bps).checked_mul(u128::from(above)),
    ];
    parts.into_iter().try_fold(0u128, |acc, p| {
        p.and_then(|p| acc.checked_add(p))
            .ok_or(MathError::Overflow)
    })
}

/// The kinked APR `r(U)` of SPEC §6.1 in bps, for display:
/// `base + ⌊slope1·min(U, k)/10⁴⌋ + ⌊slope2·max(U − k, 0)/10⁴⌋`, exactly the sims'
/// display-only `rate_bps` (each slope term floored). Nothing is charged at this rate:
/// rent integrates the exact kinked rate through [`Basis`] and [`min_rent`] uses the exact
/// `r(U)·10⁴`.
///
/// Errors: `InvalidInput` if `util_bps` or `kink_bps` exceeds 10⁴; `Overflow`.
pub fn rate_bps(terms: &RentTerms, util_bps: u32, kink_bps: u32) -> Result<u64, MathError> {
    let (below, above) = check_util(util_bps, kink_bps)?;
    let term = |slope: u32, u: u32| -> Result<u128, MathError> {
        let x = u128::from(slope)
            .checked_mul(u128::from(u))
            .ok_or(MathError::Overflow)?;
        div(x, u128::from(BPS), Rounding::Floor)
    };
    let r = u128::from(terms.base_bps)
        .checked_add(term(terms.slope1_bps, below)?)
        .ok_or(MathError::Overflow)?
        .checked_add(term(terms.slope2_bps, above)?)
        .ok_or(MathError::Overflow)?;
    to_u64(r)
}

/// `⌈a·u/10⁴⌉` for `u ≤ 10⁴`, exactly and without a 256-bit intermediate:
/// `a = h·10⁴ + l` gives `a·u/10⁴ = h·u + l·u/10⁴` with `h·u ≤ a`.
fn bps_part_ceil(a: u128, u: u32) -> Result<u128, MathError> {
    let bps = u128::from(BPS);
    let (h, l) = (
        a.checked_div(bps).ok_or(MathError::DivByZero)?,
        a.checked_rem(bps).ok_or(MathError::DivByZero)?,
    );
    let u = u128::from(u);
    let hi = h.checked_mul(u).ok_or(MathError::Overflow)?;
    let lo = div(
        l.checked_mul(u).ok_or(MathError::Overflow)?,
        bps,
        Rounding::Ceil,
    )?;
    hi.checked_add(lo).ok_or(MathError::Overflow)
}

/// Accrues `dt` seconds into the basis indices (ECON §2.1, §7; ARCH §3.0 step 4, §4.5):
/// `P̃ = min(P_ema, P_ACC_MAX)`, `J0 += P̃·Δt`, `J1 += ⌈P̃·min(U, k)·Δt/10⁴⌉`,
/// `J2 += ⌈P̃·max(U − k, 0)·Δt/10⁴⌉` (sim `Market.accrue`).
///
/// `p_ema` is the EMA price **before** this instruction's EMA roll (the clamp to
/// [`P_ACC_MAX`] is applied here), `util_bps` the utilisation of the pre-instruction
/// counters ([`utilization_bps`]) and `kink_bps` the market-level `util_kink_bps`. Returns
/// the increments `δJ`, which [`lender_credit_carried`] turns into the lenders' continuous
/// rent credit. `dt = 0` changes nothing.
///
/// Errors: `InvalidInput` if `util_bps` or `kink_bps` exceeds 10⁴; `Overflow`. On error
/// `basis` is unchanged.
pub fn accrue(
    basis: &mut Basis,
    p_ema: u128,
    util_bps: u32,
    kink_bps: u32,
    dt: u64,
) -> Result<[u128; 3], MathError> {
    let (below, above) = check_util(util_bps, kink_bps)?;
    let d0 = p_ema
        .min(P_ACC_MAX)
        .checked_mul(u128::from(dt))
        .ok_or(MathError::Overflow)?;
    let delta = [d0, bps_part_ceil(d0, below)?, bps_part_ceil(d0, above)?];
    let mut next = basis.j;
    for (j, d) in next.iter_mut().zip(delta) {
        *j = j.checked_add(d).ok_or(MathError::Overflow)?;
    }
    basis.j = next;
    Ok(delta)
}

/// `n/d` rounded, as `u64`.
fn div_to_u64(n: U256, d: u128, r: Rounding) -> Result<u64, MathError> {
    let (q, rem) = n.div_rem_u128(d).ok_or(MathError::DivByZero)?;
    let q = match (r, rem != 0) {
        (Rounding::Ceil, true) => q.checked_add(U256::ONE).ok_or(MathError::Overflow)?,
        _ => q,
    };
    q.to_u64().ok_or(MathError::Overflow)
}

/// `(Σ_j a_j·b_j)·m / den`, rounded, as `u64`: the u128 path whenever every product and
/// sum fits, a 256-bit intermediate otherwise (ECON §2.2; ARCH §2.2). Identical results.
/// When only the final `·m` overflows (the lender share of a typical position's rent,
/// `credited_for`), the dot product stays in u128 and one [`mul_div`] takes the 256-bit
/// step (T-CU-MATH).
fn dot_div(a: [u128; 3], b: [u128; 3], m: u128, den: u128, r: Rounding) -> Result<u64, MathError> {
    let dot = a.iter().zip(b).try_fold(0u128, |acc, (&x, y)| {
        x.checked_mul(y).and_then(|p| acc.checked_add(p))
    });
    if let Some(s) = dot {
        return match s.checked_mul(m) {
            Some(n) => to_u64(div(n, den, r)?),
            None => to_u64(mul_div(s, m, den, r)?),
        };
    }
    let n = a
        .into_iter()
        .zip(b)
        .try_fold(U256::ZERO, |acc, (x, y)| {
            acc.checked_add(U256::from_product(x, y))
        })
        .and_then(|s| s.checked_mul(U256::from_u128(m)))
        .ok_or(MathError::Overflow)?;
    div_to_u64(n, den, r)
}

/// `ΔJ = J_now − j_open`.
fn delta_since(pos: &PosRent, basis: &Basis) -> Result<[u128; 3], MathError> {
    let mut d = [0u128; 3];
    for ((out, now), open) in d.iter_mut().zip(basis.j).zip(pos.j_open) {
        *out = now.checked_sub(open).ok_or(MathError::Underflow)?;
    }
    Ok(d)
}

/// `(Q·base, Q·slope1, Q·slope2)`: each fits `u128`.
fn q_coef(pos: &PosRent) -> [u128; 3] {
    pos.terms
        .coefficients()
        .map(|c| u128::from(pos.q).saturating_mul(u128::from(c)))
}

/// Rent accrued since open, before the minimum:
/// `⌈Q·(base·ΔJ0 + slope1·ΔJ1 + slope2·ΔJ2)/(10⁴·31,536,000·WAD)⌉` (ECON §2.1, §7;
/// ARCH §4.5; sim `Market.rent_accrued`).
///
/// Errors: `Underflow` if `basis` is behind `pos.j_open`; `Overflow`.
pub fn accrued(pos: &PosRent, basis: &Basis) -> Result<u64, MathError> {
    dot_div(
        q_coef(pos),
        delta_since(pos, basis)?,
        1,
        RENT_DEN,
        Rounding::Ceil,
    )
}

/// `R_due = max(rent accrued, r_min)` in lamports (ECON §3.3, §7; ARCH §4.5; sim
/// `Market.rent_due`).
///
/// Errors: as [`accrued`].
pub fn due(pos: &PosRent, basis: &Basis) -> Result<u64, MathError> {
    Ok(accrued(pos, basis)?.max(pos.r_min))
}

/// Lender rent credited on this position's behalf since open, rounded up for the
/// true-up at repay/settle, with the position's own locked share:
/// `⌈Q·Σ_j coef_j·ΔJ_j·(10⁴ − rent_protocol_bps_pos)/(10⁴·10⁴·31,536,000·WAD)⌉` (ECON §2.1,
/// §7; ARCH §4.5; `pool_model._credited_for`). The caller then applies
/// `receivable −= min(credited_for, receivable)` (the clamp ARCH §4.5 states). Rounding
/// up means the true-up never leaves lenders credited with more than was paid in, and
/// because [`lender_credit_carried`] credits the floor of the aggregate's exact total,
/// `receivable` returns to 0 whenever no position is open (ECON I-P7).
///
/// Errors: `InvalidInput` if `pos.rent_protocol_bps > 10⁴`; as [`accrued`].
pub fn credited_for(pos: &PosRent, basis: &Basis) -> Result<u64, MathError> {
    let share = check_share(pos.rent_protocol_bps)?;
    dot_div(
        q_coef(pos),
        delta_since(pos, basis)?,
        share,
        RENT_DEN_BPS,
        Rounding::Ceil,
    )
}

/// Continuous lender rent credit of one accrual from a zero remainder:
/// `⌊(A0·δJ0 + A1·δJ1 + A2·δJ2)/(10⁴·10⁴·31,536,000·WAD)⌋` with the protocol share
/// already folded into `A_j` ([`Aggr`]; ECON §2.1, §7, §9.4; ARCH §4.5), `delta` the
/// increments returned by [`accrue`]. The program credits [`lender_credit_carried`] with the
/// market's remainder; this is that credit for `rem = 0` (a preview of one accrual).
///
/// Errors: `Overflow`.
pub fn lender_credit(aggr: &Aggr, delta: &[u128; 3]) -> Result<u64, MathError> {
    lender_credit_carried(aggr, delta, 0).map(|(credit, _)| credit)
}

/// Continuous lender rent credit of one accrual (the `receivable` increment) with the
/// market's carried remainder `rem` (AIR-03; `Market.lender_credit_rem`;
/// `pool_model.Pool.advance`, `credit_rem`):
/// `⌊(A0·δJ0 + A1·δJ1 + A2·δJ2 + rem)/(10⁴·10⁴·31,536,000·WAD)⌋` and the new remainder, in
/// `[0, 10⁴·10⁴·31,536,000·WAD)`. The credits of any number of accruals therefore sum to the
/// floor of their exact total: a market accrued every second no longer floors away up to a
/// lamport per accrual (as built each accrual floored alone and kept no remainder). Since
/// `Σ_k A·δJ_k = Σ_pos Q·coef·ΔJ·(10⁴ − p_pos)`, that floor never exceeds the positions'
/// ceiled true-ups ([`credited_for`]): `receivable` stays at or below them and, with the clamp,
/// returns to 0 whenever no position is open (ECON I-P7); a remainder left then credits nothing
/// (`A_j = 0`) until the next position's accruals.
///
/// Errors: `InvalidInput` if `rem` is not below the denominator; `Overflow`.
pub fn lender_credit_carried(
    aggr: &Aggr,
    delta: &[u128; 3],
    rem: u128,
) -> Result<(u64, u128), MathError> {
    if rem >= RENT_DEN_BPS {
        return Err(MathError::InvalidInput);
    }
    let dot = aggr.a.iter().zip(delta).try_fold(rem, |acc, (&x, &y)| {
        x.checked_mul(y).and_then(|p| acc.checked_add(p))
    });
    if let Some(n) = dot {
        let q = n.checked_div(RENT_DEN_BPS).ok_or(MathError::DivByZero)?;
        let r = n.checked_rem(RENT_DEN_BPS).ok_or(MathError::DivByZero)?;
        return Ok((to_u64(q)?, r));
    }
    let n = aggr
        .a
        .into_iter()
        .zip(*delta)
        .try_fold(U256::from_u128(rem), |acc, (x, y)| {
            acc.checked_add(U256::from_product(x, y))
        })
        .ok_or(MathError::Overflow)?;
    let (q, r) = n.div_rem_u128(RENT_DEN_BPS).ok_or(MathError::DivByZero)?;
    Ok((q.to_u64().ok_or(MathError::Overflow)?, r))
}

/// Minimum rent fixed at open:
/// `r_min = ⌈q·P_ema·r(U)·min_rent_s/(10⁴·31,536,000·WAD)⌉` (SPEC §6.1; ECON §2.1, §4;
/// ARCH §3.4 step 12; sim `Market.open`), with `r(U)` the exact kinked rate of `terms`
/// and `P_ema` the post-roll EMA price (opens are refused above `P_OPEN_MAX`, so no clamp).
///
/// Errors: `InvalidInput` if `util_bps` or `kink_bps` exceeds 10⁴; `Overflow`.
pub fn min_rent(
    q: u64,
    p_ema: u128,
    terms: &RentTerms,
    util_bps: u32,
    kink_bps: u32,
    min_rent_s: u32,
) -> Result<u64, MathError> {
    let rate = rate_x1e4(terms, util_bps, kink_bps)?;
    let n = U256::from_product(u128::from(q), p_ema)
        .checked_mul(U256::from_u128(rate))
        .and_then(|n| n.checked_mul(U256::from_u64(u64::from(min_rent_s))))
        .ok_or(MathError::Overflow)?;
    div_to_u64(n, RENT_DEN_BPS, Rounding::Ceil)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::num::WAD;

    const A: RentTerms = RentTerms {
        base_bps: 1_000,
        slope1_bps: 5_000,
        slope2_bps: 100_000,
    };

    #[test]
    fn constants() {
        assert_eq!(
            RENT_DEN,
            u128::from(BPS) * u128::from(YEAR_S) * crate::num::WAD
        );
        assert_eq!(RENT_DEN_BPS, RENT_DEN * u128::from(BPS));
        assert_eq!(P_ACC_MAX, 10u128.pow(29));
        // ECON §2.2: 100 years at P_ACC_MAX keep J below 2¹²⁸.
        assert!(P_ACC_MAX.checked_mul(100 * u128::from(YEAR_S)).is_some());
    }

    #[test]
    fn spec_rate_table() {
        // SPEC §6.1: Tier A 10 % base, 50 % at 80 %, 250 % at 100 %.
        assert_eq!(rate_bps(&A, 0, 8_000), Ok(1_000));
        assert_eq!(rate_bps(&A, 8_000, 8_000), Ok(5_000));
        assert_eq!(rate_bps(&A, 10_000, 8_000), Ok(25_000));
        assert_eq!(rate_bps(&A, 8_001, 8_000), Ok(5_010));
        // Display form: each slope term floored (the sims' `rate_bps`).
        assert_eq!(rate_bps(&A, 1, 8_000), Ok(1_000));
        assert_eq!(rate_bps(&A, 8_003, 8_000), Ok(5_030));
        assert_eq!(rate_bps(&A, 10_001, 8_000), Err(MathError::InvalidInput));
    }

    #[test]
    fn utilization_rounds_up() {
        assert_eq!(utilization_bps(0, 0), Ok(0));
        assert_eq!(utilization_bps(2, 1), Ok(3_334));
        assert_eq!(utilization_bps(0, 5), Ok(10_000));
        assert_eq!(utilization_bps(u64::MAX, u64::MAX), Ok(5_000));
    }

    #[test]
    fn one_year_of_rent() {
        // 10⁹ raw at 1 lamport each for a year at 80 % utilisation (50 % APR).
        let mut b = Basis::default();
        let d = accrue(&mut b, WAD, 8_000, 8_000, YEAR_S).unwrap();
        assert_eq!(d, b.j);
        let pos = PosRent {
            q: 1_000_000_000,
            j_open: [0; 3],
            terms: A,
            rent_protocol_bps: 1_500,
            r_min: 0,
        };
        assert_eq!(accrued(&pos, &b), Ok(500_000_000));
        assert_eq!(credited_for(&pos, &b), Ok(425_000_000));
        let mut ag = Aggr::default();
        ag.add(pos.q, &A, 1_500).unwrap();
        assert_eq!(lender_credit(&ag, &d), Ok(425_000_000));
        assert_eq!(
            due(
                &PosRent {
                    r_min: 600_000_000,
                    ..pos
                },
                &b
            ),
            Ok(600_000_000)
        );
        // A second position with its own share: the aggregate folds each one's.
        ag.add(pos.q, &A, 3_000).unwrap();
        assert_eq!(lender_credit(&ag, &d), Ok(425_000_000 + 350_000_000));
        ag.sub(pos.q, &A, 1_500).unwrap();
        assert_eq!(lender_credit(&ag, &d), Ok(350_000_000));
        assert_eq!(ag.sub(pos.q, &A, 1_500), Err(MathError::Underflow));
        ag.sub(pos.q, &A, 3_000).unwrap();
        assert_eq!(ag, Aggr::default());
        assert_eq!(ag.sub(1, &A, 0), Err(MathError::Underflow));
        assert_eq!(ag.add(1, &A, 10_001), Err(MathError::InvalidInput));
    }

    /// AIR-03: the carried credit of `n` accruals sums to the floor of their exact total (each
    /// accrual floored alone loses its fraction), the remainder stays below the denominator, a
    /// remainder alone credits nothing, and the 256-bit path carries the same way.
    #[test]
    fn carried_credit_sums_to_the_floor_of_the_exact_total() {
        let terms = RentTerms {
            base_bps: 1_000,
            slope1_bps: 0,
            slope2_bps: 0,
        };
        let mut ag = Aggr::default();
        // 0.4 lamports a second: q·1,000·8,500·10¹⁵/RENT_DEN_BPS with q = 1.5·10¹¹.
        ag.add(150_000_000_000, &terms, 1_500).unwrap();
        let d = [WAD / 1_000, 0, 0];
        let one = ag.a[0] * d[0];
        let (mut sum, mut rem) = (0u64, 0u128);
        for _ in 0..1_000 {
            let (c, r) = lender_credit_carried(&ag, &d, rem).unwrap();
            assert!(r < RENT_DEN_BPS);
            sum += c;
            rem = r;
        }
        assert_eq!(lender_credit(&ag, &d), Ok(0), "one accrual floored alone");
        assert_eq!(u128::from(sum), one * 1_000 / RENT_DEN_BPS);
        assert_eq!(rem, one * 1_000 % RENT_DEN_BPS);
        assert!(sum > 400);
        // No position: the remainder credits nothing and stays.
        assert_eq!(
            lender_credit_carried(&Aggr::default(), &d, rem),
            Ok((0, rem))
        );
        assert_eq!(
            lender_credit_carried(&ag, &d, RENT_DEN_BPS),
            Err(MathError::InvalidInput)
        );
        // The 256-bit path (A·δJ > 2¹²⁸) carries exactly as the u128 one.
        let big = Aggr {
            a: [u128::MAX / 3, 0, 0],
        };
        let dj = [7, 0, 0];
        let r0 = RENT_DEN_BPS - 1;
        let n = U256::from_product(big.a[0], 7)
            .checked_add(U256::from_u128(r0))
            .unwrap();
        let (q, r) = n.div_rem_u128(RENT_DEN_BPS).unwrap();
        assert_eq!(
            lender_credit_carried(&big, &dj, r0),
            Ok((q.to_u64().unwrap(), r))
        );
    }

    #[test]
    fn accrual_clamps_at_p_acc_max() {
        let mut hi = Basis::default();
        let d = accrue(&mut hi, u128::MAX, 9_000, 8_000, 7).unwrap();
        let mut at = Basis::default();
        assert_eq!(accrue(&mut at, P_ACC_MAX, 9_000, 8_000, 7), Ok(d));
        assert_eq!(
            d,
            [7 * P_ACC_MAX, 7 * P_ACC_MAX * 8 / 10, 7 * P_ACC_MAX / 10]
        );
        // Any Δt a J index can hold for 100 years, without a 256-bit intermediate.
        let mut b = Basis::default();
        let dt = 100 * YEAR_S;
        let d = accrue(&mut b, P_ACC_MAX, 10_000, 3_333, dt).unwrap();
        let p = P_ACC_MAX * u128::from(dt);
        assert_eq!(d, [p, p / 10_000 * 3_333, p / 10_000 * 6_667]);
    }

    #[test]
    fn index_increment_is_the_exact_ceiling() {
        for (p, u, dt) in [
            (1u128, 1u32, 1u64),
            (9_999, 1, 1),
            (10_001, 9_999, 3),
            (u128::from(u64::MAX), 7_777, 86_400),
            (P_ACC_MAX - 1, 10_000, 1),
        ] {
            let mut b = Basis::default();
            let d = accrue(&mut b, p, u, 0, dt).unwrap();
            let want = (p * u128::from(dt) * u128::from(u)).div_ceil(10_000);
            assert_eq!(d[2], want, "{p} {u} {dt}");
        }
    }

    #[test]
    fn min_rent_one_hour() {
        // 1 hour at 50 % APR on 10⁹ lamports of notional = 10⁹·0.5/8760.
        let r = min_rent(1_000_000_000, WAD, &A, 8_000, 8_000, 3_600).unwrap();
        assert_eq!(r, 57_078);
    }

    #[test]
    fn wide_products_take_the_256_bit_path() {
        // Q·coef·ΔJ > 2¹²⁸: the result must equal the exact value.
        let pos = PosRent {
            q: u64::MAX,
            j_open: [0; 3],
            terms: RentTerms {
                base_bps: 10,
                slope1_bps: 0,
                slope2_bps: 0,
            },
            rent_protocol_bps: 0,
            r_min: 0,
        };
        let j0 = P_ACC_MAX / 1_000;
        let b = Basis { j: [j0, 0, 0] };
        assert!((u128::from(u64::MAX) * 10).checked_mul(j0).is_none());
        let n = U256::from_product(u128::from(u64::MAX) * 10, j0);
        assert_eq!(accrued(&pos, &b), div_to_u64(n, RENT_DEN, Rounding::Ceil));
        assert_eq!(accrued(&pos, &b).map(|r| r > 0), Ok(true));
    }

    #[test]
    fn errors_leave_state_unchanged() {
        let mut b = Basis {
            j: [u128::MAX, 0, 0],
        };
        assert_eq!(accrue(&mut b, 1, 0, 0, 1), Err(MathError::Overflow));
        assert_eq!(b.j, [u128::MAX, 0, 0]);
        let pos = PosRent {
            q: 1,
            j_open: [1, 0, 0],
            ..PosRent::default()
        };
        assert_eq!(accrued(&pos, &Basis::default()), Err(MathError::Underflow));
        assert_eq!(
            credited_for(
                &PosRent {
                    rent_protocol_bps: 10_001,
                    ..pos
                },
                &Basis { j: [1, 0, 0] }
            ),
            Err(MathError::InvalidInput)
        );
        let mut ag = Aggr { a: [5, 0, 0] };
        assert_eq!(ag.sub(1, &A, 0), Err(MathError::Underflow));
        assert_eq!(ag.a, [5, 0, 0]);
    }
}
