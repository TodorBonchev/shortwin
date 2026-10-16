//! Fee-on-SOL-leg quotes for the pump curve (`eps1`, ECON's ε₁ = 1) and PumpSwap
//! (ε₁ = 0) (ECON §3.1; ARCH §2.2, §4.3; R-PUMP §4, R-VEN §2.4).
//!
//! `x` is the token reserve and `y` the SOL reserve the venue prices on: `(vT, vQ)` on
//! the curve, `(base vault, quote vault + virtual_quote_reserves)` on PumpSwap. Each fee
//! component is `⌈n·bps/10⁴⌉`, charged on top of a buy and out of a sale.

use super::{BuyQuote, SellQuote, fee_up, muldiv, to_u64};
use crate::fees::FeeBps;
use crate::num::{BPS, Rounding};

/// Exact-out buy of `q` tokens, short.win's quote `B̂ = n + Σ⌈n·φᵢ/10⁴⌉` with ECON §3.1's
/// `n = ⌈q·y/(x − q)⌉ + ε₁` (`eps1`: 1 on the curve, 0 on PumpSwap). Every `B̂` of the
/// protocol uses it: CR, the S1/S2 legs, the exit bounds, FV, and close affordability.
///
/// - PumpSwap (`eps1 = false`): the venue's own charge (R-VEN §2.4).
/// - Curve (`eps1 = true`): ECON §2/§3.1/§3.2's conservative quote. The program charges
///   `⌊q·y/(x − q)⌋ + 1` ([`curve_charge_exact_out`], ARCH §4.3), so the quote exceeds the
///   charge by at most `1 + #fees` lamports (ECON §3.2: 485,764,635 against 485,764,634)
///   and a `max_in ≥ B̂` always covers it.
///
/// `net` is `n`. After the trade `x − q` tokens and `y + n + ⌈n·lp/10⁴⌉` SOL remain (the
/// LP fee stays in the pool; on the curve this is at most one lamport above the venue's
/// state). The curve's `q ≤ rT` bound is the caller's (see
/// [`curve_straddle`](super::curve_straddle)).
///
/// `None` iff `q = 0`, `q ≥ x`, `y = 0`, or an amount does not fit (`n` and `B̂` are `u64`).
pub fn buy_exact_out(q: u64, x: u128, y: u128, eps1: bool, fees: &FeeBps) -> Option<BuyQuote> {
    let n = pre_fee(q, x, y, Rounding::Ceil)?.checked_add(u128::from(eps1))?;
    with_fees(q, x, y, n, fees)
}

/// The pump curve's own exact-out charge (`buy`/`buy_v2`; R-PUMP §4, ARCH §4.3 curve row):
/// `n = ⌊q·y/(x − q)⌋ + 1`, `B̂ = n + Σ⌈n·φᵢ/10⁴⌉`, with the venue's exact post-trade
/// reserves. For reproducing executed trades and post-trade state; quotes use
/// [`buy_exact_out`] with `eps1 = true` (ECON §2, §3.1).
///
/// `None` exactly as [`buy_exact_out`].
pub fn curve_charge_exact_out(q: u64, x: u128, y: u128, fees: &FeeBps) -> Option<BuyQuote> {
    let n = pre_fee(q, x, y, Rounding::Floor)?.checked_add(1)?;
    with_fees(q, x, y, n, fees)
}

/// `q·y/(x − q)` rounded as asked; `None` iff `q = 0`, `q ≥ x` or `y = 0`.
fn pre_fee(q: u64, x: u128, y: u128, r: Rounding) -> Option<u128> {
    let q128 = u128::from(q);
    if q == 0 || q128 >= x || y == 0 {
        return None;
    }
    muldiv(y, q128, x.checked_sub(q128)?, r)
}

fn with_fees(q: u64, x: u128, y: u128, n: u128, fees: &FeeBps) -> Option<BuyQuote> {
    let n = to_u64(n)?;
    let lp = fee_up(n, fees.lp, BPS)?;
    let cost = n
        .checked_add(lp)?
        .checked_add(fee_up(n, fees.protocol, BPS)?)?
        .checked_add(fee_up(n, fees.creator, BPS)?)?;
    Some(BuyQuote {
        cost,
        net: n,
        token_after: x.checked_sub(u128::from(q))?,
        sol_after: y.checked_add(u128::from(n))?.checked_add(u128::from(lp))?,
    })
}

/// Exact-in sale of `q` tokens: `g = ⌊q·y/(x + q)⌋`, `S = g − Σ⌈g·φᵢ/10⁴⌉` (ARCH §4.3).
///
/// `real_quote_cap` is the real SOL the venue can pay out: PumpSwap's quote vault
/// (`InsufficientRealQuoteReserves` unless `cap ≥ g − ⌈g·lp/10⁴⌉`), the curve's `rQ`.
/// After the trade `x + q` tokens and `y − (g − ⌈g·lp/10⁴⌉)` SOL remain.
///
/// `S` is monotone in `q` only up to rounding: each of the `k` non-zero fee components
/// is rounded up separately, so `S` can dip by up to `k − 1` lamports as `q` grows.
///
/// `None` iff `q = 0`, `x = 0`, `y = 0`, the cap binds, the fees exceed `g` (the venue
/// rejects a negative payout), or an amount does not fit (`g` is `u64`).
pub fn sell_exact_in(
    q: u64,
    x: u128,
    y: u128,
    fees: &FeeBps,
    real_quote_cap: u64,
) -> Option<SellQuote> {
    if q == 0 || x == 0 || y == 0 {
        return None;
    }
    let q128 = u128::from(q);
    let token_after = x.checked_add(q128)?;
    // g < y, so it fits u64 whenever y does; a larger y can still give a larger g.
    let g = to_u64(muldiv(y, q128, token_after, Rounding::Floor)?)?;
    let lp = fee_up(g, fees.lp, BPS)?;
    let paid = g.checked_sub(lp)?;
    if real_quote_cap < paid {
        return None;
    }
    let proceeds = paid
        .checked_sub(fee_up(g, fees.protocol, BPS)?)?
        .checked_sub(fee_up(g, fees.creator, BPS)?)?;
    Some(SellQuote {
        proceeds,
        token_after,
        sol_after: y.checked_sub(u128::from(paid))?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const CURVE: FeeBps = FeeBps::new(0, 95, 30);

    /// R-PUMP §4.2 worked example (slot 450,803,590) and its SIM; ECON §3.2.
    #[test]
    fn pump_worked_example() {
        let (vt, vq) = (824_130_602_106_172u128, 39_059_343_535u128);
        let q = 10_000_000_000_000;
        let b = curve_charge_exact_out(q, vt, vq, &CURVE).unwrap();
        assert_eq!((b.net, b.cost), (479_767_539, 485_764_634));
        // Review F-S2: short.win's quote rounds up (ECON §2, §3.1; ECON §3.2's 485,764,635).
        let quote = buy_exact_out(q, vt, vq, true, &CURVE).unwrap();
        assert_eq!((quote.net, quote.cost), (479_767_540, 485_764_635));
        let s = sell_exact_in(q, vt, vq, &CURVE, 9_059_343_535).unwrap();
        assert_eq!(s.proceeds, 462_410_820);
        assert_eq!(s.sol_after, vq - 468_264_123);
        // Round trip (ECON §3.2): the program's charge to buy Q back on the post-sale state.
        let back = curve_charge_exact_out(q, s.token_after, s.sol_after, &CURVE).unwrap();
        assert_eq!(back.cost, 474_117_427);
        assert_eq!((back.token_after, back.sol_after), (vt, vq + 1));
        // An exact division: ⌈⌉ + 1 = ⌊⌋ + 1, quote and charge agree.
        let (a, c) = (
            buy_exact_out(1, 3, 2, true, &CURVE).unwrap(),
            curve_charge_exact_out(1, 3, 2, &CURVE).unwrap(),
        );
        assert_eq!(a, c);
        assert_eq!(a.net, 2);
    }

    /// R-VEN §2.4 TX-BUY / TX-SELL.
    #[test]
    fn pumpswap_worked_examples() {
        let f = FeeBps::new(2, 93, 30);
        let y = 18_917_931_618u128 + 17_584_505_333;
        let b = buy_exact_out(9_155_477_586_733, 482_553_257_944_361, y, false, &f).unwrap();
        assert_eq!((b.net, b.cost), (705_954_395, 714_778_826));
        assert_eq!(b.sol_after, y + 705_954_395 + 141_191);
        let s = sell_exact_in(
            8_709_390_094_921,
            475_179_928_893_125,
            19_484_781_049 + 17_584_505_333,
            &f,
            19_484_781_049,
        )
        .unwrap();
        assert_eq!(s.proceeds, 658_859_839);
    }

    #[test]
    fn undefined() {
        assert_eq!(buy_exact_out(0, 10, 10, false, &CURVE), None);
        assert_eq!(buy_exact_out(10, 10, 10, false, &CURVE), None);
        assert_eq!(buy_exact_out(1, 10, 0, true, &CURVE), None);
        assert_eq!(buy_exact_out(1, 2, u128::MAX, false, &CURVE), None);
        assert_eq!(curve_charge_exact_out(0, 10, 10, &CURVE), None);
        assert_eq!(curve_charge_exact_out(10, 10, 10, &CURVE), None);
        assert_eq!(curve_charge_exact_out(1, 10, 0, &CURVE), None);
        assert_eq!(sell_exact_in(0, 10, 10, &CURVE, u64::MAX), None);
        // Three fees rounded up exceed a 1-lamport output.
        assert_eq!(
            sell_exact_in(1, 1, 2, &FeeBps::new(1, 1, 1), u64::MAX),
            None
        );
        // Real-reserve cap: g − lp must be payable.
        let s = sell_exact_in(100, 100, 1_000, &FeeBps::new(100, 0, 0), u64::MAX).unwrap();
        assert_eq!(s.proceeds, 495);
        assert_eq!(
            sell_exact_in(100, 100, 1_000, &FeeBps::new(100, 0, 0), 494),
            None
        );
        assert!(sell_exact_in(100, 100, 1_000, &FeeBps::new(100, 0, 0), 495).is_some());
    }
}
