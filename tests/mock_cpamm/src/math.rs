//! The mock's own swap arithmetic, written the way a venue writes it (after
//! `raydium-cp-swap` `curve/calculator.rs` and `curve/fees.rs`), **not** by calling
//! `shortwin-math`: the differential tests (`tests` below and the LiteSVM suite) prove that
//! short.win's on-chain quote `quote::cpmm` with a zero creator fee equals this execution
//! exactly (INTERFACE.md "Pricing").
//!
//! Fee on the input, rate `fee_ppm` of [`FEE_DENOMINATOR`]; a fixed share
//! [`PROTOCOL_FEE_RATE`] of the trade fee leaves the pricing reserves into the input side's
//! protocol counter (as CPMM's protocol share).

use crate::errors::MockError;

/// Fee rates are ppm.
pub const FEE_DENOMINATOR: u64 = 1_000_000;
/// Protocol share of the trade fee, ppm of the fee (Raydium CPMM config 0 uses 12 %).
pub const PROTOCOL_FEE_RATE: u64 = 120_000;

/// One swap's amounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Swap {
    /// Units the trader pays in.
    pub amount_in: u64,
    /// Units the curve pays out (excluding any over-delivery knob).
    pub amount_out: u64,
    /// Trade fee on the input, `⌈amount_in·fee/10⁶⌉` (exact in) or `amount_in − swapped`
    /// (exact out).
    pub trade_fee: u64,
    /// `⌊trade_fee·PROTOCOL_FEE_RATE/10⁶⌋`, moved into the input side's counter.
    pub protocol_fee: u64,
}

fn to_u64(v: u128) -> Result<u64, MockError> {
    u64::try_from(v).map_err(|_| MockError::MathOverflow)
}

fn ceil_div(num: u128, den: u128) -> Result<u128, MockError> {
    if den == 0 {
        return Err(MockError::MathOverflow);
    }
    num.checked_add(den.checked_sub(1).ok_or(MockError::MathOverflow)?)
        .ok_or(MockError::MathOverflow)?
        .checked_div(den)
        .ok_or(MockError::MathOverflow)
}

fn protocol_share(trade_fee: u64) -> Result<u64, MockError> {
    let v = u128::from(trade_fee)
        .checked_mul(u128::from(PROTOCOL_FEE_RATE))
        .ok_or(MockError::MathOverflow)?
        .checked_div(u128::from(FEE_DENOMINATOR))
        .ok_or(MockError::MathOverflow)?;
    to_u64(v)
}

fn check_fee(fee_ppm: u32) -> Result<(), MockError> {
    if u64::from(fee_ppm) >= FEE_DENOMINATOR {
        return Err(MockError::BadFee);
    }
    Ok(())
}

/// Exact-in swap: `trade_fee = ⌈a·fee/10⁶⌉`, `out = ⌊R_out·(a − trade_fee)/(R_in + a −
/// trade_fee)⌋`, `out > 0`.
pub fn exact_in(
    amount_in: u64,
    reserve_in: u64,
    reserve_out: u64,
    fee_ppm: u32,
) -> Result<Swap, MockError> {
    check_fee(fee_ppm)?;
    if amount_in == 0 {
        return Err(MockError::ZeroAmount);
    }
    if reserve_in == 0 || reserve_out == 0 {
        return Err(MockError::EmptyPool);
    }
    let a = u128::from(amount_in);
    let trade_fee = ceil_div(
        a.checked_mul(u128::from(fee_ppm))
            .ok_or(MockError::MathOverflow)?,
        u128::from(FEE_DENOMINATOR),
    )?;
    let net = a.checked_sub(trade_fee).ok_or(MockError::MathOverflow)?;
    let out = u128::from(reserve_out)
        .checked_mul(net)
        .ok_or(MockError::MathOverflow)?
        .checked_div(
            u128::from(reserve_in)
                .checked_add(net)
                .ok_or(MockError::MathOverflow)?,
        )
        .ok_or(MockError::MathOverflow)?;
    if out == 0 {
        return Err(MockError::ZeroOutput);
    }
    let trade_fee = to_u64(trade_fee)?;
    Ok(Swap {
        amount_in,
        amount_out: to_u64(out)?,
        trade_fee,
        protocol_fee: protocol_share(trade_fee)?,
    })
}

/// Exact-out swap: `swapped = ⌈R_in·out/(R_out − out)⌉`, `amount_in = swapped` if the fee
/// is 0, else `⌈swapped·10⁶/(10⁶ − fee)⌉` (CPMM's `calculate_pre_fee_amount`);
/// `0 < out < R_out`.
pub fn exact_out(
    amount_out: u64,
    reserve_in: u64,
    reserve_out: u64,
    fee_ppm: u32,
) -> Result<Swap, MockError> {
    check_fee(fee_ppm)?;
    if amount_out == 0 {
        return Err(MockError::ZeroAmount);
    }
    if reserve_in == 0 || reserve_out == 0 {
        return Err(MockError::EmptyPool);
    }
    if amount_out >= reserve_out {
        return Err(MockError::InsufficientLiquidity);
    }
    let out = u128::from(amount_out);
    let swapped = ceil_div(
        u128::from(reserve_in)
            .checked_mul(out)
            .ok_or(MockError::MathOverflow)?,
        u128::from(reserve_out)
            .checked_sub(out)
            .ok_or(MockError::MathOverflow)?,
    )?;
    let amount_in = if fee_ppm == 0 {
        swapped
    } else {
        ceil_div(
            swapped
                .checked_mul(u128::from(FEE_DENOMINATOR))
                .ok_or(MockError::MathOverflow)?,
            u128::from(FEE_DENOMINATOR)
                .checked_sub(u128::from(fee_ppm))
                .ok_or(MockError::MathOverflow)?,
        )?
    };
    let amount_in = to_u64(amount_in)?;
    let trade_fee = to_u64(
        u128::from(amount_in)
            .checked_sub(swapped)
            .ok_or(MockError::MathOverflow)?,
    )?;
    Ok(Swap {
        amount_in,
        amount_out,
        trade_fee,
        protocol_fee: protocol_share(trade_fee)?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use shortwin_math::fees::CpmmFees;
    use shortwin_math::quote::cpmm;

    /// short.win's view of the mock: CPMM fees with a zero creator fee (INTERFACE.md).
    fn fees(fee_ppm: u32) -> CpmmFees {
        CpmmFees {
            trade: u64::from(fee_ppm),
            creator: 0,
            creator_on_input: false,
            protocol: PROTOCOL_FEE_RATE,
            fund: 0,
        }
    }

    /// Hand-checked: 1,000 in at 0.25 % on (10⁶, 10⁶): fee 3, out ⌊10⁶·997/1,000,997⌋ = 996.
    #[test]
    fn worked_example() {
        let s = exact_in(1_000, 1_000_000, 1_000_000, 2_500).unwrap();
        assert_eq!((s.trade_fee, s.amount_out, s.protocol_fee), (3, 996, 0));
        let b = exact_out(996, 1_000_000, 1_000_000, 2_500).unwrap();
        // swapped ⌈10⁶·996/999,004⌉ = 997; in ⌈997·10⁶/997,500⌉ = 1,000
        assert_eq!((b.amount_in, b.trade_fee), (1_000, 3));
    }

    #[test]
    fn edge_errors() {
        assert_eq!(exact_in(0, 1, 1, 0), Err(MockError::ZeroAmount));
        assert_eq!(exact_in(1, 0, 1, 0), Err(MockError::EmptyPool));
        assert_eq!(exact_in(1, 10, 1, 0), Err(MockError::ZeroOutput));
        assert_eq!(exact_in(1, 1, 1, 1_000_000), Err(MockError::BadFee));
        assert_eq!(
            exact_out(5, 10, 5, 0),
            Err(MockError::InsufficientLiquidity)
        );
        assert_eq!(exact_out(0, 10, 5, 0), Err(MockError::ZeroAmount));
        assert_eq!(
            exact_out(u64::MAX - 1, u64::MAX, u64::MAX, 0),
            Err(MockError::MathOverflow)
        );
    }

    fn reserve() -> impl Strategy<Value = u64> {
        prop_oneof![
            1u64..1_000,
            1u64..=u64::MAX,
            1_000_000u64..1_000_000_000_000_000
        ]
    }

    fn fee() -> impl Strategy<Value = u32> {
        prop_oneof![
            Just(0u32),
            Just(2_500u32),
            0u32..1_000_000,
            999_000u32..1_000_000
        ]
    }

    proptest! {
        /// Exact in: executes iff the quote is defined, with the quote's output and
        /// post-trade reserves.
        #[test]
        fn exact_in_equals_cpmm_quote(a in reserve(), x in reserve(), y in reserve(), f in fee()) {
            let q = cpmm::sell_exact_in(a, u128::from(x), u128::from(y), &fees(f));
            match exact_in(a, x, y, f) {
                Ok(s) => {
                    let q = q.expect("mock executed an undefined quote");
                    prop_assert_eq!(s.amount_out, q.proceeds);
                    prop_assert_eq!(u128::from(x) + u128::from(a) - u128::from(s.protocol_fee), q.token_after);
                    prop_assert_eq!(u128::from(y) - u128::from(s.amount_out), q.sol_after);
                }
                Err(_) => prop_assert!(q.is_none(), "quote defined but mock failed"),
            }
            // a zero creator fee on either side is the same quote (except fee 0 on the input,
            // which the CPMM split leaves undefined)
            if f > 0 {
                let on_in = CpmmFees { creator_on_input: true, ..fees(f) };
                prop_assert_eq!(cpmm::sell_exact_in(a, u128::from(x), u128::from(y), &on_in), q);
            }
        }

        /// Exact out: executes iff the quote is defined, with the quote's cost and post-trade
        /// reserves.
        #[test]
        fn exact_out_equals_cpmm_quote(q in reserve(), x in reserve(), y in reserve(), f in fee()) {
            let quote = cpmm::buy_exact_out(q, u128::from(x), u128::from(y), &fees(f));
            match exact_out(q, x, y, f) {
                Ok(s) => {
                    let quote = quote.expect("mock executed an undefined quote");
                    prop_assert_eq!(s.amount_in, quote.cost);
                    prop_assert_eq!(u128::from(x) + u128::from(s.amount_in) - u128::from(s.protocol_fee), quote.sol_after);
                    prop_assert_eq!(u128::from(y) - u128::from(q), quote.token_after);
                }
                Err(_) => prop_assert!(quote.is_none(), "quote defined but mock failed"),
            }
            if f > 0 {
                let on_in = CpmmFees { creator_on_input: true, ..fees(f) };
                prop_assert_eq!(cpmm::buy_exact_out(q, u128::from(x), u128::from(y), &on_in), quote);
            }
        }
    }
}
