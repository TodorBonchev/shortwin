//! Rent: utilisation, the kinked rate, minimum rent and rent due on the basis indices
//! (`shortwin_math::rent`; ECON §7; ARCH §4.5).

use shortwin_math::rent::{self as mrent, Aggr, Basis, PosRent, RentTerms};
use wasm_bindgen::JsError;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::bps_number;

/// Utilisation `⌈ΣB_Q·10⁴/(T + ΣB_Q)⌉` in bps.
#[wasm_bindgen(js_name = utilizationBps)]
pub fn utilization_bps(t_free: u64, borrowed: u64) -> Result<u32, JsError> {
    Ok(mrent::utilization_bps(t_free, borrowed)?)
}

/// The kinked APR in bps for display (each slope term floored, as the sims' `rate_bps`).
#[wasm_bindgen(js_name = rateBps)]
pub fn rate_bps(
    base_bps: u32,
    slope1_bps: u32,
    slope2_bps: u32,
    util_bps: u32,
    kink_bps: u32,
) -> Result<u32, JsError> {
    let terms = RentTerms {
        base_bps,
        slope1_bps,
        slope2_bps,
    };
    Ok(bps_number(mrent::rate_bps(&terms, util_bps, kink_bps)?))
}

/// Minimum rent fixed at open, `⌈q·P_ema·r(U_after)·min_rent_s/(10⁴·YEAR·WAD)⌉`.
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen(js_name = minRent)]
pub fn min_rent(
    q: u64,
    p_ema: u128,
    base_bps: u32,
    slope1_bps: u32,
    slope2_bps: u32,
    util_bps: u32,
    kink_bps: u32,
    min_rent_s: u32,
) -> Result<u64, JsError> {
    let terms = RentTerms {
        base_bps,
        slope1_bps,
        slope2_bps,
    };
    Ok(mrent::min_rent(
        q, p_ema, &terms, util_bps, kink_bps, min_rent_s,
    )?)
}

/// The market's rent basis indices `J0, J1, J2` (`rent::Basis`).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RentBasis(Basis);

#[wasm_bindgen]
impl RentBasis {
    /// `Market.rent_basis`.
    #[wasm_bindgen(constructor)]
    pub fn new(j0: u128, j1: u128, j2: u128) -> RentBasis {
        RentBasis(Basis { j: [j0, j1, j2] })
    }

    /// `J0`.
    #[wasm_bindgen(getter)]
    pub fn j0(&self) -> u128 {
        self.0.j[0]
    }

    /// `J1`.
    #[wasm_bindgen(getter)]
    pub fn j1(&self) -> u128 {
        self.0.j[1]
    }

    /// `J2`.
    #[wasm_bindgen(getter)]
    pub fn j2(&self) -> u128 {
        self.0.j[2]
    }

    /// The basis `dt` seconds later at `P_ema` and utilisation `util_bps`
    /// (`rent::accrue` on a copy): what the next instruction will accrue.
    pub fn accrue(
        &self,
        p_ema: u128,
        util_bps: u32,
        kink_bps: u32,
        dt: u64,
    ) -> Result<RentBasis, JsError> {
        let mut b = self.0;
        mrent::accrue(&mut b, p_ema, util_bps, kink_bps, dt)?;
        Ok(RentBasis(b))
    }

    /// The lenders' rent credit of that accrual: [`RentBasis::accrue`]'s increments weighed by
    /// the market's aggregate `Market.rent_aggr = [a0, a1, a2]`, plus the remainder `rem` the
    /// market carries from its previous accrual (`Market.lender_credit_rem`), floored
    /// (`rent::lender_credit_carried`, AIR-03) — what the next instruction's prologue adds to
    /// `acc` and to `receivable` (ARCH §3.0 step 4, `engine::prologue::accrue`). Throws
    /// `invalid input` if `rem` is not below the denominator `10⁴·10⁴·31,536,000·WAD`.
    #[allow(clippy::too_many_arguments)]
    #[wasm_bindgen(js_name = lenderCredit)]
    pub fn lender_credit(
        &self,
        a0: u128,
        a1: u128,
        a2: u128,
        p_ema: u128,
        util_bps: u32,
        kink_bps: u32,
        dt: u64,
        rem: u128,
    ) -> Result<u64, JsError> {
        let mut b = self.0;
        let delta = mrent::accrue(&mut b, p_ema, util_bps, kink_bps, dt)?;
        let (credit, _) = mrent::lender_credit_carried(&Aggr { a: [a0, a1, a2] }, &delta, rem)?;
        Ok(credit)
    }
}

/// A position's rent terms (`rent::PosRent`).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionRent(PosRent);

#[wasm_bindgen]
impl PositionRent {
    /// `q`, the basis at open, the locked rate curve, the locked `rent_protocol_bps` and
    /// `r_min`.
    #[allow(clippy::too_many_arguments)]
    #[wasm_bindgen(constructor)]
    pub fn new(
        q: u64,
        j_open: &RentBasis,
        base_bps: u32,
        slope1_bps: u32,
        slope2_bps: u32,
        rent_protocol_bps: u32,
        r_min: u64,
    ) -> PositionRent {
        PositionRent(PosRent {
            q,
            j_open: j_open.0.j,
            terms: RentTerms {
                base_bps,
                slope1_bps,
                slope2_bps,
            },
            rent_protocol_bps,
            r_min,
        })
    }

    /// Rent accrued since open, before the minimum.
    pub fn accrued(&self, basis: &RentBasis) -> Result<u64, JsError> {
        Ok(mrent::accrued(&self.0, &basis.0)?)
    }

    /// `R_due = max(accrued, r_min)`.
    pub fn due(&self, basis: &RentBasis) -> Result<u64, JsError> {
        Ok(mrent::due(&self.0, &basis.0)?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn rent_due_after_a_year() {
        // 10⁹ raw tokens at 1 lamport/raw (P = WAD), Tier A at U = 0: 10 % APR.
        let wad = shortwin_math::num::WAD;
        let open = RentBasis::new(0, 0, 0);
        let later = open.accrue(wad, 0, 8_000, 31_536_000).unwrap();
        let pos = PositionRent::new(1_000_000_000, &open, 1_000, 5_000, 100_000, 1_500, 7);
        assert_eq!(pos.due(&later).unwrap(), 100_000_000);
        assert_eq!(pos.due(&open).unwrap(), 7);
        assert_eq!(
            rate_bps(1_000, 5_000, 100_000, 8_000, 8_000).unwrap(),
            5_000
        );
        assert_eq!(utilization_bps(1, 2).unwrap(), 6_667);
    }

    #[test]
    fn lender_credit_of_a_year() {
        // The same position in the aggregate: 85 % of its 10 % APR (rent_protocol_bps 1,500).
        let wad = shortwin_math::num::WAD;
        let terms = RentTerms {
            base_bps: 1_000,
            slope1_bps: 5_000,
            slope2_bps: 100_000,
        };
        let mut aggr = Aggr::default();
        aggr.add(1_000_000_000, &terms, 1_500).unwrap();
        let [a0, a1, a2] = aggr.a;
        let open = RentBasis::new(0, 0, 0);
        let credit = open.lender_credit(a0, a1, a2, wad, 0, 8_000, 31_536_000, 0);
        assert_eq!(credit.unwrap(), 85_000_000);
        let none = open.lender_credit(a0, a1, a2, wad, 0, 8_000, 0, 0);
        assert_eq!(none.unwrap(), 0);
    }

    #[test]
    fn lender_credit_carries_the_markets_remainder() {
        // AIR-03: the prologue credits `⌊(Σ A_j·δJ_j + rem)/DEN⌋` with the market's
        // `lender_credit_rem`. A day of the position above is 8.5·10⁷/365 = 232,876 + 260/365
        // lamports, so a remainder of 105/365 of the denominator completes the lamport and one
        // unit less does not.
        let wad = shortwin_math::num::WAD;
        let den = 10_000 * 10_000 * 31_536_000 * wad;
        let terms = RentTerms {
            base_bps: 1_000,
            slope1_bps: 5_000,
            slope2_bps: 100_000,
        };
        let mut aggr = Aggr::default();
        aggr.add(1_000_000_000, &terms, 1_500).unwrap();
        let [a0, a1, a2] = aggr.a;
        let open = RentBasis::new(0, 0, 0);
        let day = |rem: u128| {
            open.lender_credit(a0, a1, a2, wad, 0, 8_000, 86_400, rem)
                .unwrap()
        };
        let edge = 105 * den / 365;
        assert_eq!(day(0), 232_876);
        assert_eq!(day(edge - 1), 232_876);
        assert_eq!(day(edge), 232_877);
        assert_eq!(day(den - 1), 232_877);
    }
}
