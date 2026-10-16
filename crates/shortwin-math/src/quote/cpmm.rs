//! Raydium CPMM fee-on-input quotes, including the creator fee on either side (ECON §3.1;
//! ARCH §2.2, §4.3; R-RAY §3.3; `raydium-cp-swap@59fb845` `curve/calculator.rs`,
//! `curve/fees.rs`, `instructions/swap_base_{input,output}.rs`).
//!
//! Reserves are `vault − (protocol + fund + creator)` counters of each side (ARCH §4.1),
//! passed as (input side, output side); rates are ppm (`D = 10⁶`). The protocol and fund
//! shares of the trade fee move into the excluded counters, as does the creator fee on
//! whichever side it is charged, so only the LP part of the trade fee stays in the
//! pricing reserves.

use super::{BuyQuote, SellQuote, muldiv, to_u64};
use crate::fees::CpmmFees;
use crate::num::{PPM, Rounding};

const D: u128 = PPM as u128;

/// `pre(a, r) = ⌈a·D/(D − r)⌉`, `pre(a, 0) = a` (`calculate_pre_fee_amount`).
fn pre(a: u128, rate: u64) -> Option<u128> {
    if rate == 0 {
        return Some(a);
    }
    muldiv(a, D, D.checked_sub(u128::from(rate))?, Rounding::Ceil)
}

/// Creator part of a combined fee, `⌊total·c/(t + c)⌋` (`split_creator_fee`); the venue
/// fails the swap when `t + c = 0`.
fn split_creator(total: u128, t: u64, c: u64) -> Option<u128> {
    let tc = t.checked_add(c)?;
    muldiv(total, u128::from(c), u128::from(tc), Rounding::Floor)
}

/// `p + f ≤ 10⁶` (`update_amm_config`): the carved shares never exceed the trade fee.
fn shares_ok(f: &CpmmFees) -> bool {
    f.protocol
        .checked_add(f.fund)
        .is_some_and(|pf| u128::from(pf) <= D)
}

/// Protocol plus fund shares of a trade fee, each `⌊trade·rate/D⌋`.
fn carved(trade: u128, f: &CpmmFees) -> Option<u128> {
    muldiv(trade, u128::from(f.protocol), D, Rounding::Floor)?.checked_add(muldiv(
        trade,
        u128::from(f.fund),
        D,
        Rounding::Floor,
    )?)
}

/// Exact-out buy of `q` tokens with SOL (`swap_base_output`):
///
/// - creator fee on the input (or off): `sw = ⌈x_sol·q/(y_tok − q)⌉`, `B̂ = pre(sw, t + c)`;
/// - creator fee on the token (output) side: `q' = pre(q, c)`, `sw = ⌈x_sol·q'/(y_tok − q')⌉`,
///   `B̂ = pre(sw, t)` (source only: no mainnet sample, R-RAY §3.4).
///
/// After the trade the SOL side holds `x_sol + B̂ − protocol − fund − creator_in` and the
/// token side `y_tok − q'`. `net` is ECON §3.1's fee-free `n(q) = ⌈x_sol·q/(y_tok − q)⌉`,
/// σ's input (ECON §5.6): `sw` when the creator fee is on the input, but below `sw(q')`
/// when it is on the token side, whose gross-up is a fee (review F-S4).
///
/// `None` iff `q = 0`, a reserve is 0, `q' ≥ y_tok`, a `pre` rate is ≥ 10⁶, the creator
/// fee is on the input with `t + c = 0` (the venue's fee split divides by zero), `B̂`
/// does not fit `u64`, or `p + f > 10⁶` (a config the venue cannot hold).
pub fn buy_exact_out(q: u64, x_sol: u128, y_tok: u128, fees: &CpmmFees) -> Option<BuyQuote> {
    if q == 0 || x_sol == 0 || y_tok == 0 || !shares_ok(fees) {
        return None;
    }
    let q128 = u128::from(q);
    let actual = if fees.creator_on_input {
        q128
    } else {
        pre(q128, fees.creator)?
    };
    if actual >= y_tok {
        return None;
    }
    let token_after = y_tok.checked_sub(actual)?;
    let sw = muldiv(x_sol, actual, token_after, Rounding::Ceil)?;
    let (cost, trade, creator_in) = if fees.creator_on_input {
        let cost = pre(sw, fees.trade.checked_add(fees.creator)?)?;
        let total = cost.checked_sub(sw)?;
        let creator = split_creator(total, fees.trade, fees.creator)?;
        (cost, total.checked_sub(creator)?, creator)
    } else {
        let cost = pre(sw, fees.trade)?;
        (cost, cost.checked_sub(sw)?, 0)
    };
    let cost64 = to_u64(cost)?;
    let kept = cost
        .checked_sub(carved(trade, fees)?)?
        .checked_sub(creator_in)?;
    let n = if actual == q128 {
        sw
    } else {
        muldiv(x_sol, q128, y_tok.checked_sub(q128)?, Rounding::Ceil)?
    };
    Some(BuyQuote {
        cost: cost64,
        net: to_u64(n)?,
        token_after,
        sol_after: x_sol.checked_add(kept)?,
    })
}

/// Exact-in sale of `a` tokens for SOL (`swap_base_input`):
///
/// - creator fee on the input (the token side, or both): `total = ⌈a·(t + c)/D⌉`,
///   `net = a − total`, `S = ⌊y_sol·net/(x_tok + net)⌋`;
/// - creator fee on the SOL (output) side: `net = a − ⌈a·t/D⌉`,
///   `sw = ⌊y_sol·net/(x_tok + net)⌋`, `S = sw − ⌈sw·c/D⌉`.
///
/// After the trade the token side holds `x_tok + a − protocol − fund − creator_in` and
/// the SOL side `y_sol − sw`.
///
/// `None` iff `a = 0`, a reserve is 0, a fee exceeds its base, the creator fee is on the
/// input with `t + c = 0`, `S = 0` (the venue requires a non-zero output), or
/// `p + f > 10⁶`.
pub fn sell_exact_in(a: u64, x_tok: u128, y_sol: u128, fees: &CpmmFees) -> Option<SellQuote> {
    if a == 0 || x_tok == 0 || y_sol == 0 || !shares_ok(fees) {
        return None;
    }
    let a128 = u128::from(a);
    let (net, trade, creator_in) = if fees.creator_on_input {
        let tc = fees.trade.checked_add(fees.creator)?;
        let total = muldiv(a128, u128::from(tc), D, Rounding::Ceil)?;
        let creator = split_creator(total, fees.trade, fees.creator)?;
        (
            a128.checked_sub(total)?,
            total.checked_sub(creator)?,
            creator,
        )
    } else {
        let trade = muldiv(a128, u128::from(fees.trade), D, Rounding::Ceil)?;
        (a128.checked_sub(trade)?, trade, 0)
    };
    let sw = muldiv(y_sol, net, x_tok.checked_add(net)?, Rounding::Floor)?;
    let out = if fees.creator_on_input {
        sw
    } else {
        sw.checked_sub(muldiv(sw, u128::from(fees.creator), D, Rounding::Ceil)?)?
    };
    if out == 0 {
        return None;
    }
    let kept = a128
        .checked_sub(carved(trade, fees)?)?
        .checked_sub(creator_in)?;
    Some(SellQuote {
        proceeds: to_u64(out)?,
        token_after: x_tok.checked_add(kept)?,
        sol_after: y_sol.checked_sub(sw)?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const CFG0: CpmmFees = CpmmFees {
        trade: 2_500,
        creator: 0,
        creator_on_input: true,
        protocol: 120_000,
        fund: 40_000,
    };

    /// R-RAY §3.5 (D) TX-CP-OUT: trade fee 2,444,356 = protocol 293,322 + fund 97,774 + LP.
    #[test]
    fn exact_out_worked_example() {
        let (x, y) = (24_271_071_490_644u128, 10_178_955_637_762u128);
        let b = buy_exact_out(409_010_196, x, y, &CFG0).unwrap();
        assert_eq!((b.net, b.cost), (975_297_955, 977_742_311));
        assert_eq!(b.sol_after, x + 977_742_311 - 293_322 - 97_774);
        assert_eq!(b.token_after, y - 409_010_196);
    }

    /// R-RAY §3.5 (E): creator fee 500 ppm on the SOL input (TX-CP-CFIN, an exact-in
    /// buy: the same arithmetic as an exact-in sale), then on the SOL output (TX-CP-CFOUT).
    #[test]
    fn creator_fee_examples() {
        let on_in = CpmmFees {
            creator: 500,
            ..CFG0
        };
        let s =
            sell_exact_in(164_842_583, 547_003_359_834, 30_996_027_477_657_999, &on_in).unwrap();
        assert_eq!(s.proceeds, 9_310_009_993_705);
        // trade 412,107, creator 82,421: protocol 49,452 and fund 16,484 leave with them.
        assert_eq!(
            s.token_after,
            547_003_359_834 + 164_842_583 - 49_452 - 16_484 - 82_421
        );
        let on_out = CpmmFees {
            creator_on_input: false,
            ..on_in
        };
        let (x, y) = (30_986_717_467_664_294u128, 547_168_054_060u128);
        let s = sell_exact_in(2_150_833_510_816, x, y, &on_out).unwrap();
        assert_eq!(s.proceeds, 37_863_222);
        // sw = 37,863,222 + creator 18,942 leaves the SOL side.
        assert_eq!(s.sol_after, y - 37_882_164);
    }

    #[test]
    fn undefined() {
        assert_eq!(buy_exact_out(0, 10, 10, &CFG0), None);
        assert_eq!(buy_exact_out(10, 10, 10, &CFG0), None);
        assert_eq!(buy_exact_out(1, 0, 10, &CFG0), None);
        let zero = CpmmFees { trade: 0, ..CFG0 };
        // Fee on input with t + c = 0: the venue's split divides by zero.
        assert_eq!(buy_exact_out(1, 10, 10, &zero), None);
        assert_eq!(sell_exact_in(5, 10, 10, &zero), None);
        let zero_out = CpmmFees {
            creator_on_input: false,
            ..zero
        };
        assert_eq!(sell_exact_in(5, 10, 10, &zero_out).unwrap().proceeds, 3);
        assert_eq!(buy_exact_out(1, 10, 10, &zero_out).unwrap().cost, 2);
        // M3-SEC-1: the execution quote stays undefined (the venue reverts), the risk view of
        // the same fee is the fee-free quote (`CpmmFees::capped`).
        let risk = zero.capped(500);
        assert_eq!(risk, zero_out);
        assert_eq!(buy_exact_out(1, 10, 10, &risk).unwrap().cost, 2);
        assert_eq!(sell_exact_in(5, 10, 10, &risk).unwrap().proceeds, 3);
        let full = CpmmFees {
            trade: 1_000_000,
            ..CFG0
        };
        assert_eq!(buy_exact_out(1, 10, 10, &full), None);
        assert_eq!(sell_exact_in(5, 10, 10, &full), None);
        // Creator fee on the output grosses q up past the reserve.
        let gross = CpmmFees {
            creator: 500_000,
            creator_on_input: false,
            ..CFG0
        };
        assert_eq!(buy_exact_out(5, 100, 10, &gross), None);
        assert!(buy_exact_out(4, 100, 10, &gross).is_some());
    }
}
