//! Owner: quotes track. Executable buyback cost `B̂` and sale proceeds for every venue family (ECON §2
//! `reprice`, §3.1; ARCH §2.2 API, §4.3).
//!
//! Each function reproduces its venue's own integer arithmetic bit for bit on the
//! pre-trade state (R-PUMP §4, R-VEN §2.4, R-RAY §3.3), so the on-chain quote equals what
//! the venue charges in the same instruction; the one exception is the pump curve, whose
//! quote is ECON §3.1's `⌈·⌉ + 1`, at most `1 + #fees` lamports above the program's
//! `⌊·⌋ + 1` charge ([`sol_leg::curve_charge_exact_out`]). The venues round against the
//! trader, so a quote never favours the trader (ECON §2: quotes and fees inside quotes
//! round up).
//!
//! A quote is `None` exactly when it is undefined (ARCH §4.3 "`B̂ = +∞`"): `q = 0`, `q`
//! at or above the output reserve, a zero reserve, fee parameters the venue cannot
//! execute with, a result that does not fit `u64`, or any other state in which the venue
//! would revert the trade. Each function documents its exact set. Reserves are `u128`
//! so that hypothetical states (EMA reprojections, PumpSwap effective reserves) are
//! quoted with the same exact arithmetic as live ones.
//!
//! Hypothetical states — ring entries' cached `(x, y)`, box corners, exit references,
//! FV — are quoted with the fees resolved on the **live** venue state (the live pump-fees
//! tier; ECON §3.1, ARCH §4.2, R3B-7 b), never re-resolved at the hypothetical price:
//! [`Pricing`] and [`CurvePricing`] carry resolved fees for exactly that reason. The one
//! resolution on a hypothetical state is the straddle's post-migration pool, whose
//! PumpSwap tier is read at that pool's own market cap (ARCH §4.3, as the sim). pump's
//! synthetic migration ([`curve_synthetic`]) charges both of its parts at the curve's own
//! (live) tier, so it takes resolved fees too.
//! Every quote here fits `u128` (ECON §2.2: `Q·Y < 1.1·10³⁷`), so each division takes
//! `mul_div`'s `u128` path within those bounds.

pub mod cpmm;
pub mod pricing;
pub mod ray_v4;
pub mod reprice;
pub mod sol_leg;
pub mod straddle;

pub use pricing::{CurvePricing, Pricing};
pub use reprice::{CurveOffsets, curve_reprice, liquidity, reprice, reprice_token};
pub use straddle::{
    CurveState, MIGRATION_BOOST_DIVISOR, MigrationParams, STANDARD_POOL_TOKENS, curve_straddle,
    curve_synthetic, curve_two_leg, migrated_pool, migrating_fv, migrating_fv_price,
    migration_boost,
};

use crate::num::{Rounding, mul_div};

/// An exact-out buy of `q` tokens (ECON §3.1 `B̂(q)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BuyQuote {
    /// `B̂`: lamports the venue debits, fees included.
    pub cost: u64,
    /// ECON §3.1's fee-free input `n(q)` (pump venues including ε₁; AMM v4 `pre`; CPMM
    /// `sw`, except with the creator fee on the token side, where the venue's `sw(q')`
    /// includes that fee and `net` is `⌈x_sol·q/(y_tok − q)⌉`): σ's `n` (ECON §5.6,
    /// ARCH §4.3).
    pub net: u64,
    /// Token pricing reserve after the trade.
    pub token_after: u128,
    /// SOL pricing reserve after the trade (PumpSwap: effective, i.e. including
    /// `virtual_quote_reserves`; CPMM: net of the fee counters).
    pub sol_after: u128,
}

/// An exact-in sale of `q` tokens (ECON §3.1 `S`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SellQuote {
    /// `S`: lamports the seller receives, fees deducted.
    pub proceeds: u64,
    /// Token pricing reserve after the trade.
    pub token_after: u128,
    /// SOL pricing reserve after the trade (same conventions as [`BuyQuote::sol_after`]).
    pub sol_after: u128,
}

/// `⌊a·b/d⌋` or `⌈a·b/d⌉` (`u128` fast path, 256-bit fallback only when `a·b`
/// overflows, ARCH §2.2); `None` on a zero divisor or a quotient above `u128`.
fn muldiv(a: u128, b: u128, d: u128, r: Rounding) -> Option<u128> {
    mul_div(a, b, d, r).ok()
}

/// `⌈x·rate/den⌉` on `u64` (a fee rounded up, as every venue charges it).
fn fee_up(x: u64, rate: u64, den: u64) -> Option<u64> {
    crate::num::mul_div_u64(x, rate, den, Rounding::Ceil).ok()
}

fn to_u64(x: u128) -> Option<u64> {
    u64::try_from(x).ok()
}
