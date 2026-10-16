//! Raydium AMM v4 fee-on-input quotes with the per-pool `num/den` fee (ECON §3.1;
//! ARCH §2.2, §4.3; R-RAY §3.3; `raydium-amm@d26944b` `processor.rs`
//! `process_swap_base_{in,out}_v2`, `math.rs` `swap_token_amount_base_{in,out}`).
//!
//! Reserves are the pricing reserves `vault − need_take_pnl` of each side (ARCH §4.1),
//! passed as (input side, output side). The whole fee stays in the pool.

use super::{BuyQuote, SellQuote, fee_up, muldiv, to_u64};
use crate::num::Rounding;

/// Exact-out buy of `q` tokens with SOL (`swap_base_out_v2`):
/// `pre = ⌈x_sol·q/(y_tok − q)⌉`, `B̂ = ⌈pre·den/(den − num)⌉`. After the trade
/// `x_sol + B̂` and `y_tok − q` remain.
///
/// `None` iff `q = 0`, `q ≥ y_tok`, `num ≥ den`, `B̂ = 0` (i.e. `x_sol = 0`; the venue
/// rejects a zero input), or `B̂` does not fit `u64`.
pub fn buy_exact_out(q: u64, x_sol: u128, y_tok: u128, num: u64, den: u64) -> Option<BuyQuote> {
    let q128 = u128::from(q);
    if q == 0 || q128 >= y_tok || num >= den {
        return None;
    }
    let token_after = y_tok.checked_sub(q128)?;
    let pre = muldiv(x_sol, q128, token_after, Rounding::Ceil)?;
    let cost = muldiv(
        pre,
        u128::from(den),
        u128::from(den.checked_sub(num)?),
        Rounding::Ceil,
    )?;
    let cost = to_u64(cost)?;
    if cost == 0 {
        return None;
    }
    Some(BuyQuote {
        cost,
        net: to_u64(pre)?,
        token_after,
        sol_after: x_sol.checked_add(u128::from(cost))?,
    })
}

/// Exact-in sale of `a` tokens for SOL (`swap_base_in_v2`): `fee = ⌈a·num/den⌉`,
/// `net = a − fee`, `S = ⌊y_sol·net/(x_tok + net)⌋`. After the trade `x_tok + a` and
/// `y_sol − S` remain.
///
/// `None` iff `a = 0`, `num ≥ den`, `S = 0` (`InvalidInput`), or `S ≥ y_sol`
/// (`InsufficientFunds`, i.e. `x_tok = 0`), or `S` does not fit `u64`.
pub fn sell_exact_in(a: u64, x_tok: u128, y_sol: u128, num: u64, den: u64) -> Option<SellQuote> {
    if a == 0 || num >= den {
        return None;
    }
    let net = u128::from(a.checked_sub(fee_up(a, num, den)?)?);
    let out = muldiv(y_sol, net, x_tok.checked_add(net)?, Rounding::Floor)?;
    if out == 0 || out >= y_sol {
        return None;
    }
    Some(SellQuote {
        proceeds: to_u64(out)?,
        token_after: x_tok.checked_add(u128::from(a))?,
        sol_after: y_sol.checked_sub(out)?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// R-RAY §3.5 worked examples (A), (B), (C) and the (F) SIM.
    #[test]
    fn worked_examples() {
        let b = buy_exact_out(
            235_932_179_000,
            17_595_123_332_536,
            37_639_277_914_829_371,
            25,
            10_000,
        )
        .unwrap();
        assert_eq!((b.net, b.cost), (110_291_219, 110_567_639));
        let s = sell_exact_in(
            1_864_750_682,
            24_152_226_230_990,
            38_145_208_954_870,
            25,
            10_000,
        )
        .unwrap();
        assert_eq!(s.proceeds, 2_937_535_090);
        let b = buy_exact_out(
            1_493_590_000,
            24_133_461_116_333,
            38_174_727_180_804,
            25,
            10_000,
        )
        .unwrap();
        assert_eq!((b.net, b.cost), (944_261_013, 946_627_582));
        let b = buy_exact_out(
            100_000_000_000,
            38_133_419_249_319,
            24_159_957_563_306,
            25,
            10_000,
        )
        .unwrap();
        assert_eq!((b.net, b.cost), (158_493_293_885, 158_890_520_186));
    }

    #[test]
    fn undefined() {
        assert_eq!(buy_exact_out(0, 10, 10, 25, 10_000), None);
        assert_eq!(buy_exact_out(10, 10, 10, 25, 10_000), None);
        assert_eq!(buy_exact_out(1, 0, 10, 25, 10_000), None);
        assert_eq!(buy_exact_out(1, 10, 10, 10_000, 10_000), None);
        assert_eq!(buy_exact_out(1, 10, 10, 0, 0), None);
        assert!(buy_exact_out(1, 10, 10, 0, 1).is_some());
        assert_eq!(sell_exact_in(1, 10, 10, 25, 10_000), None); // net 0 -> out 0
        assert_eq!(sell_exact_in(5, 0, 10, 0, 1), None); // out = y
        assert_eq!(sell_exact_in(5, 10, 0, 0, 1), None);
        assert_eq!(sell_exact_in(5, 10, 10, 1, 1), None);
        assert_eq!(sell_exact_in(5, 10, 10, 0, 1).unwrap().proceeds, 3);
    }
}
