//! Raydium CPMM against the research pass's port of `raydium-cp-swap` (R-RAY §3.3):
//! `tests/vectors/quotes/port.json` (`gen_port.py`). It covers every creator-fee branch,
//! including the exact-out buy with the creator fee on the token side, which no mainnet
//! swap in the sample exercised (R-RAY §3.4).

mod common;

use common::Dec;
use serde::Deserialize;
use shortwin_math::fees::CpmmFees;
use shortwin_math::quote::cpmm;

#[derive(Deserialize)]
struct File {
    protocol_rate: Dec<u64>,
    fund_rate: Dec<u64>,
    cpmm: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    kind: String,
    x: Dec<u128>,
    y: Dec<u128>,
    amount: Dec<u64>,
    trade: Dec<u64>,
    creator: Dec<u64>,
    on_input: bool,
    result: Dec<u64>,
    trade_fee: Dec<u64>,
    creator_fee: Dec<u64>,
}

#[test]
fn cpmm_matches_the_research_port() {
    let f: File = common::load_vectors("quotes", "port.json");
    let (p, fu) = (f.protocol_rate.0, f.fund_rate.0);
    let mut out_side_exact_out = 0;
    for (i, c) in f.cpmm.iter().enumerate() {
        let fees = CpmmFees {
            trade: c.trade.0,
            creator: c.creator.0,
            creator_on_input: c.on_input,
            protocol: p,
            fund: fu,
        };
        let tf = u128::from(c.trade_fee.0);
        let carved = tf * u128::from(p) / 1_000_000 + tf * u128::from(fu) / 1_000_000;
        let cf = u128::from(c.creator_fee.0);
        let creator_in = if c.on_input { cf } else { 0 };
        if c.kind == "exact_out" {
            let b = cpmm::buy_exact_out(c.amount.0, c.x.0, c.y.0, &fees)
                .unwrap_or_else(|| panic!("case {i}: no quote"));
            assert_eq!(b.cost, c.result.0, "case {i}");
            assert_eq!(
                b.sol_after,
                c.x.0 + u128::from(b.cost) - carved - creator_in,
                "case {i}"
            );
            let actual = u128::from(c.amount.0) + if c.on_input { 0 } else { cf };
            assert_eq!(b.token_after, c.y.0 - actual, "case {i}");
            out_side_exact_out += usize::from(!c.on_input && c.creator.0 > 0);
        } else {
            let s = cpmm::sell_exact_in(c.amount.0, c.x.0, c.y.0, &fees)
                .unwrap_or_else(|| panic!("case {i}: no quote"));
            assert_eq!(s.proceeds, c.result.0, "case {i}");
            assert_eq!(
                s.token_after,
                c.x.0 + u128::from(c.amount.0) - carved - creator_in,
                "case {i}"
            );
        }
    }
    assert!(out_side_exact_out >= 100, "{out_side_exact_out}");
}
