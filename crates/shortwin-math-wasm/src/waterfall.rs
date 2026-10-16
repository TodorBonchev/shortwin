//! Waterfall previews: who gets what out of a position's vault on close, liquidation,
//! expiry and SOL settlement (`shortwin_math::waterfall`; ECON §8; ARCH §5.2).

use shortwin_math::MathError;
use shortwin_math::risk::{ExpiryKeeperTerms, SettleKind};
use shortwin_math::waterfall::{self as mwf, ExitInputs, PenaltySplit};
use wasm_bindgen::JsError;
use wasm_bindgen::prelude::wasm_bindgen;

/// A waterfall result (`waterfall::Payout`): `buyback + keeper + protocol + lenders +
/// borrower == V`.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Payout(mwf::Payout);

#[wasm_bindgen]
impl Payout {
    /// Venue spend `B`.
    #[wasm_bindgen(getter)]
    pub fn buyback(&self) -> u64 {
        self.0.buyback
    }

    /// Rent paid `r`.
    #[wasm_bindgen(getter)]
    pub fn rent(&self) -> u64 {
        self.0.rent
    }

    /// Protocol part of the rent.
    #[wasm_bindgen(getter, js_name = rentProtocol)]
    pub fn rent_protocol(&self) -> u64 {
        self.0.rent_protocol
    }

    /// Lender part of the rent.
    #[wasm_bindgen(getter, js_name = rentLenders)]
    pub fn rent_lenders(&self) -> u64 {
        self.0.rent_lenders
    }

    /// Liquidation penalty paid.
    #[wasm_bindgen(getter)]
    pub fn penalty(&self) -> u64 {
        self.0.penalty
    }

    /// Liquidation keeper floor top-up (from the borrower remainder).
    #[wasm_bindgen(getter, js_name = keeperTopup)]
    pub fn keeper_topup(&self) -> u64 {
        self.0.keeper_topup
    }

    /// Part of the lenders' SOL that vests.
    #[wasm_bindgen(getter)]
    pub fn lump(&self) -> u64 {
        self.0.lump
    }

    /// Keeper part of the penalty.
    #[wasm_bindgen(getter, js_name = penaltyKeeper)]
    pub fn penalty_keeper(&self) -> u64 {
        self.0.penalty_keeper
    }

    /// Protocol part of the penalty.
    #[wasm_bindgen(getter, js_name = penaltyProtocol)]
    pub fn penalty_protocol(&self) -> u64 {
        self.0.penalty_protocol
    }

    /// Lender part of the penalty.
    #[wasm_bindgen(getter, js_name = penaltyLenders)]
    pub fn penalty_lenders(&self) -> u64 {
        self.0.penalty_lenders
    }

    /// Expiry fee paid (to lenders).
    #[wasm_bindgen(getter, js_name = expiryFee)]
    pub fn expiry_fee(&self) -> u64 {
        self.0.expiry_fee
    }

    /// Keeper total.
    #[wasm_bindgen(getter)]
    pub fn keeper(&self) -> u64 {
        self.0.keeper
    }

    /// Protocol total.
    #[wasm_bindgen(getter)]
    pub fn protocol(&self) -> u64 {
        self.0.protocol
    }

    /// Lender total.
    #[wasm_bindgen(getter)]
    pub fn lenders(&self) -> u64 {
        self.0.lenders
    }

    /// Borrower remainder.
    #[wasm_bindgen(getter)]
    pub fn borrower(&self) -> u64 {
        self.0.borrower
    }
}

/// Borrower close: `B` → `R_due` (protocol/lenders) → borrower (ECON §8.1).
#[wasm_bindgen(js_name = previewClose)]
pub fn preview_close(
    v: u64,
    b: u64,
    r_due: u64,
    rent_protocol_bps: u32,
) -> Result<Payout, JsError> {
    Ok(Payout(mwf::close(v, b, r_due, rent_protocol_bps)?))
}

/// Normal liquidation with the locked penalty and split (ECON §8.3).
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen(js_name = previewLiquidation)]
pub fn preview_liquidation(
    v: u64,
    b: u64,
    r_due: u64,
    b_live: u64,
    b_ref: u64,
    rent_protocol_bps: u32,
    penalty_bps: u32,
    lender_bps: u32,
    protocol_bps: u32,
    keeper_bps: u32,
    k_s: u64,
) -> Result<Payout, JsError> {
    let x = ExitInputs {
        v,
        b,
        r_due,
        b_live,
        b_ref,
        rent_protocol_bps,
    };
    let split = PenaltySplit {
        lender_bps,
        protocol_bps,
        keeper_bps,
    };
    Ok(Payout(mwf::liquidate(&x, penalty_bps, &split, k_s)?))
}

/// Expiry with the locked expiry fee and keeper terms (ECON §8.4).
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen(js_name = previewExpiry)]
pub fn preview_expiry(
    v: u64,
    b: u64,
    r_due: u64,
    b_live: u64,
    b_ref: u64,
    rent_protocol_bps: u32,
    expiry_fee_bps: u32,
    keeper_fixed: u64,
    keeper_bps: u32,
    keeper_cap: u64,
) -> Result<Payout, JsError> {
    let x = ExitInputs {
        v,
        b,
        r_due,
        b_live,
        b_ref,
        rent_protocol_bps,
    };
    let k = ExpiryKeeperTerms {
        fixed: keeper_fixed,
        bps: keeper_bps,
        cap: keeper_cap,
    };
    Ok(Payout(mwf::expire(&x, expiry_fee_bps, &k)?))
}

/// SOL settlement `kind` 1 = S1, 2 = S2, 3 = S3 with keeper reward `K_s` and fair value
/// `fv` (`undefined`: the cap does not bind) (ECON §8.6), composed as `sol_settle` composes
/// it: on every kind (S1 too since AIR-21) the part of the lenders' take above
/// `⌊q·P_ema/WAD⌋ + R_due` vests (`lump`), and on a venue that cannot execute
/// (`executable = false`: halted, layout change, list-only, migrating) the tokens count at 0,
/// so all but `R_due` vests (AIR-09: `P_ema` is the frozen pre-halt mark there).
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen(js_name = previewSettlement)]
pub fn preview_settlement(
    v: u64,
    kind: u8,
    k_s: u64,
    fv: Option<u64>,
    q: u64,
    p_ema: u128,
    r_due: u64,
    executable: bool,
) -> Result<Payout, JsError> {
    let kind = match kind {
        1 => SettleKind::S1,
        2 => SettleKind::S2,
        3 => SettleKind::S3,
        _ => return Err(MathError::InvalidInput.into()),
    };
    // `sol_settle` (AIR-09): the price the extinguished tokens count at once.
    let p_imm = if executable { p_ema } else { 0 };
    Ok(Payout(mwf::settle(v, kind, k_s, fv, q, p_imm, r_due)?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn previews_conserve_v() {
        let c = preview_close(1_000, 600, 100, 1_500).unwrap();
        assert_eq!(
            (c.rent_protocol(), c.rent_lenders(), c.borrower()),
            (15, 85, 300)
        );
        let l = preview_liquidation(
            1_000, 600, 100, 500, 550, 1_500, 1_000, 5_000, 2_000, 3_000, 20,
        )
        .unwrap();
        assert_eq!(l.penalty(), 50);
        assert_eq!((l.keeper(), l.keeper_topup(), l.lump()), (20, 5, 25));
        assert_eq!(
            l.buyback() + l.keeper() + l.protocol() + l.lenders() + l.borrower(),
            1_000
        );
        let e = preview_expiry(1_000, 600, 100, 500, 550, 1_500, 100, 1, 10, 50).unwrap();
        assert_eq!(e.expiry_fee(), 5);
        let s = preview_settlement(1_000, 2, 20, Some(700), 500, 10u128.pow(18), 50, true).unwrap();
        assert_eq!((s.keeper(), s.lenders(), s.borrower()), (20, 700, 280));
        assert_eq!(s.lump(), 150);
    }

    /// AIR-21: an S1 take above the tokens' value at `P_ema` plus `R_due` vests its excess; a
    /// genuine S1 loss (take at or below that value) vests nothing.
    #[test]
    fn an_s1_take_above_the_tokens_value_vests_the_excess() {
        let wad = 10u128.pow(18);
        // V − K_s = 980 against ⌊500·P/WAD⌋ + 50 = 550: 430 vests, the borrower gets nothing.
        let s = preview_settlement(1_000, 1, 20, None, 500, wad, 50, true).unwrap();
        assert_eq!(
            (s.keeper(), s.lenders(), s.lump(), s.borrower()),
            (20, 980, 430, 0)
        );
        // FV is ignored for S1 (never capped).
        let capped = preview_settlement(1_000, 1, 20, Some(700), 500, wad, 50, true).unwrap();
        assert_eq!((capped.lenders(), capped.lump()), (980, 430));
        // ⌊1,000·P/WAD⌋ + 50 = 1,050 ≥ 980: a loss, all of it immediate.
        let loss = preview_settlement(1_000, 1, 20, None, 1_000, wad, 50, true).unwrap();
        assert_eq!((loss.lenders(), loss.lump()), (980, 0));
    }

    /// AIR-09: on a venue that cannot execute the tokens count at 0 (`sol_settle`'s `p_imm`):
    /// only `R_due` is immediate and the rest of the take vests.
    #[test]
    fn a_settlement_on_a_venue_that_cannot_execute_prices_the_tokens_at_0() {
        let wad = 10u128.pow(18);
        let live = preview_settlement(1_000, 3, 20, Some(700), 500, wad, 50, true).unwrap();
        let halted = preview_settlement(1_000, 3, 20, Some(700), 500, wad, 50, false).unwrap();
        assert_eq!((live.lenders(), live.lump()), (700, 150));
        assert_eq!((halted.lenders(), halted.lump()), (700, 650));
        // Who gets what does not change, only what vests.
        assert_eq!(
            (halted.keeper(), halted.borrower()),
            (live.keeper(), live.borrower())
        );
        // R_due above the take: nothing vests.
        let rent = preview_settlement(1_000, 3, 20, Some(700), 500, wad, 800, false).unwrap();
        assert_eq!(rent.lump(), 0);
    }
}
