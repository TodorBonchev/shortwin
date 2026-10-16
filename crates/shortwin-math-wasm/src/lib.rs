//! wasm-bindgen facade over `shortwin-math` for the frontend (ARCHITECTURE §1.2, §7.3):
//! quotes on every venue family, CR and the liquidation price, the D4 margin and open
//! fee, the exit bounds, the settlement box, rent due, waterfall previews (keeper floor,
//! vesting lumps), caps, tiers, the open guards, pool selection (`λ`, the locked depth and
//! its floor), lender-pool previews (locks with `lock_until`, the vest, a withdrawal's SOL
//! part, a third party's pot-only touch with its forward chain, a claim that closes the
//! position), the parameter checks, and the create-market mint filter
//! (`shortwin_venue_state::mint::check`).
//!
//! The frontend never re-implements protocol math (ARCH §7.3); every export here is a
//! type conversion around `shortwin-math` (or `shortwin-venue-state`) calls, composed as the
//! program's handler composes them, so the UI shows exactly what the program will compute.
//!
//! JS conventions:
//! - `u64`/`i64`/`u128`/`i128` amounts, prices and indices are `bigint`; bps, ppm,
//!   seconds and small counters are `number`.
//! - An undefined result (a quote the math returns as `None`) is `undefined`.
//! - A `MathError` is thrown as a JS `Error` whose message is `MathError::as_str()`
//!   (e.g. `"above pro-rata allowance"`).

#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unwrap_used
)]

mod lender;
mod mint;
mod pool_select;
mod quotes;
mod rent;
mod risk;
mod settle;
mod waterfall;

pub use lender::{ClaimOutcome, LenderState, PoolState, PotTouch, ShareEpochs, Withdrawal};
pub use mint::MintVerdict;
pub use quotes::{
    BuyQuote, CurveSchedule, CurveState, Fees, PammSchedule, Reserves, SellQuote, Venue,
};
pub use rent::{PositionRent, RentBasis};
pub use risk::{TierInputs, TierParams};
pub use settle::{OracleState, ParamSet, SettleBox};
pub use waterfall::Payout;

use wasm_bindgen::prelude::wasm_bindgen;

/// Version of the embedded `shortwin-math` crate.
#[wasm_bindgen]
pub fn version() -> String {
    shortwin_math::VERSION.into()
}

/// `n` as a JS `number` of bps, saturating (fee rates never approach it).
fn bps_number(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The value, or a thrown JS `Error` with the `MathError` text. For results that
/// wasm-bindgen cannot wrap in `Result` (an `Option<u128>` takes three ABI words).
fn or_throw<T>(r: Result<T, shortwin_math::MathError>) -> T {
    r.unwrap_or_else(|e| wasm_bindgen::throw_str(e.as_str()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_math_crate() {
        assert_eq!(super::version(), shortwin_math::VERSION);
    }
}
