//! Shared helpers of the engine tests.

#![allow(dead_code)]

use anchor_lang::prelude::Pubkey;
use litesvm::types::TransactionMetadata;
use shortwin::errors::ShortwinError;
use shortwin::events::{Close, Open};

pub use shortwin_litesvm_tests::engine::{
    Borrower, MarketSpec, MockMarket, close_short_ix, crank_price_ix, open_short_ix,
};
pub use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
pub use shortwin_litesvm_tests::mock::{self, Dir};
pub use shortwin_litesvm_tests::tokens::TokenProgram;
pub use shortwin_litesvm_tests::{Env, TxResult, events, pda, read};
use shortwin_math::quote::cpmm;

/// Positions of this size fit every default cap (≈ 3 SOL of notional on the default pool).
pub const Q: u64 = 3_000_000_000_000;

/// A test-venue environment with one default mock market (SPL base).
pub fn setup() -> (Env, MockMarket) {
    setup_with(MarketSpec::default())
}

/// A test-venue environment with one mock market per `spec`.
pub fn setup_with(spec: MarketSpec) -> (Env, MockMarket) {
    let mut env = Env::test_venue();
    let mm = env.mock_market(spec);
    (env, mm)
}

/// The default spec on `program`.
pub fn spec_on(program: TokenProgram) -> MarketSpec {
    MarketSpec {
        program,
        ..MarketSpec::default()
    }
}

/// The error code of `e`.
pub fn code(e: ShortwinError) -> u32 {
    u32::from(e)
}

/// An open that must succeed: its `Open` event, nonce and metadata.
pub fn open_ok(
    env: &mut Env,
    mm: &MockMarket,
    b: &mut Borrower,
    q: u64,
) -> (Open, u64, TransactionMetadata) {
    let (res, nonce) = env.open(mm, b, q);
    let meta = ok(res);
    (events::one(&meta), nonce, meta)
}

/// A close that must succeed: its `Close` event and metadata.
pub fn close_ok(
    env: &mut Env,
    mm: &MockMarket,
    b: &Borrower,
    nonce: u64,
) -> (Close, TransactionMetadata) {
    let meta = ok(env.close(mm, b, nonce));
    (events::one(&meta), meta)
}

/// `S_exp`: the sale of `q` on the pool's live state (INTERFACE.md "short.win's view").
pub fn quote_sell(env: &Env, mm: &MockMarket, q: u64) -> u64 {
    mock::quote_exact_in(&env.svm, mm.mock(), Dir::BaseIn, q)
        .expect("sale quote")
        .proceeds
}

/// `B̂_live(q)`: the buyback of `q` on the pool's live state.
pub fn quote_buy(env: &Env, mm: &MockMarket, q: u64) -> u64 {
    mock::quote_exact_out(&env.svm, mm.mock(), Dir::QuoteIn, q)
        .expect("buy quote")
        .cost
}

/// `B̂(q; x, y)` at the pool's live fee on a hypothetical state (a ring entry's cached `(x, y)`).
pub fn quote_buy_at(env: &Env, mm: &MockMarket, q: u64, x: u128, y: u128) -> Option<u64> {
    let fee = mock::state(&env.svm, &mm.mock().pool).fee_ppm;
    if x == 0 {
        return None;
    }
    cpmm::buy_exact_out(q, y, x, &mock::cpmm_fees(fee)).map(|b| b.cost)
}

/// Instruction-trace entries of top-level instruction `ix` of a transaction: the instruction
/// itself plus every CPI at any depth (ARCH §6.2).
pub fn trace_entries(meta: &TransactionMetadata, ix: usize) -> usize {
    1 + meta.inner_instructions.get(ix).map_or(0, Vec::len)
}

/// Lamports of `key`.
pub fn lamports(env: &Env, key: &Pubkey) -> u64 {
    read::lamports(&env.svm, key)
}

/// Token amount of `key`.
pub fn tokens(env: &Env, key: &Pubkey) -> u64 {
    read::token_amount(&env.svm, key).unwrap_or(0)
}
