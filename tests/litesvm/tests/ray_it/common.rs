//! Shared helpers of the Raydium adapter tests.

#![allow(dead_code)]

pub use anchor_lang::prelude::Pubkey;
pub use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
pub use litesvm::types::TransactionMetadata;
pub use shortwin::errors::ShortwinError as E;
pub use shortwin::events::{Close, Open};
pub use shortwin::state::VenueKind;
pub use shortwin::venues::ExtrasSel;
pub use shortwin_litesvm_tests::engine::{
    Borrower, VenueMarket, close_short_ix, crank_price_ix, open_short_ix,
};
pub use shortwin_litesvm_tests::errors::{assert_err, custom_code, ok};
pub use shortwin_litesvm_tests::fork::{self, AnyPool, CpmmFixture, V4Fixture};
pub use shortwin_litesvm_tests::raydium::{
    self, RayFixture, RayMarket, RaySpec, VENUE_HINT_RAYDIUM_CPMM as VENUE_HINT_CPMM,
    VENUE_HINT_RAYDIUM_V4 as VENUE_HINT_V4,
};
pub use shortwin_litesvm_tests::tokens::TokenProgram;
pub use shortwin_litesvm_tests::{Env, TxResult, events, pda, read};
pub use solana_signer::Signer;

pub const FART: RayFixture = RayFixture::V4(V4Fixture::Fartcoin);
pub const FART_REV: RayFixture = RayFixture::V4(V4Fixture::FartcoinReversed);
pub const USELESS: RayFixture = RayFixture::Cpmm(CpmmFixture::Useless);
pub const CREATOR_FEE: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorFee);
pub const T22: RayFixture = RayFixture::Cpmm(CpmmFixture::Token2022);
pub const WSOL_T1: RayFixture = RayFixture::Cpmm(CpmmFixture::WsolToken1);
pub const CREATOR_BOTH: RayFixture = RayFixture::Cpmm(CpmmFixture::CreatorBoth);

/// The eligible dumped pools (the others fail `eligible()` as dumped: `FART_REV` is thin and
/// unlocked, `WSOL_T1` has no burned LP).
pub const ELIGIBLE: [RayFixture; 5] = [FART, USELESS, CREATOR_FEE, T22, CREATOR_BOTH];

/// The error code of `e`.
pub fn code(e: E) -> u32 {
    u32::from(e)
}

/// A position size within every default cap on `rm`'s pool: `min(X/400, 2 SOL of notional)`.
pub fn q_for(env: &Env, rm: &RayMarket) -> u64 {
    let (x, y) = rm.reserves(&env.svm).expect("reserves");
    let two_sol = u128::from(x) * 2_000_000_000 / u128::from(y);
    (u128::from(x) / 400).min(two_sol) as u64
}

/// The default position size on `pool`: `min(X/400, 2 SOL of notional)`.
pub fn q_of(env: &Env, pool: &AnyPool) -> u64 {
    let (x, y) = pool
        .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
        .expect("reserves");
    (u128::from(x) / 400).min(u128::from(x) * 2_000_000_000 / u128::from(y)) as u64
}

/// Lists `fixture` in `env` (loaded now), lends `40·Q`, waits out the listing window and
/// cranks once; returns the market and its default size `Q`.
pub fn add_market(env: &mut Env, fixture: RayFixture) -> (RayMarket, u64) {
    let pool = env.load_ray_pool(fixture);
    ok(env.list_ray(&pool, fixture.hint()));
    let q = q_of(env, &pool);
    let rm = env.ray_market_listed(RaySpec::new(fixture, q.saturating_mul(40)), pool);
    env.crank(&rm);
    (rm, q)
}

/// A fork environment with a market on `fixture` ([`add_market`]).
pub fn setup(fixture: RayFixture) -> (Env, RayMarket, u64) {
    let mut env = Env::ray_fork();
    let (rm, q) = add_market(&mut env, fixture);
    (env, rm, q)
}

/// An open that must succeed: its event, nonce and metadata.
pub fn open_ok(
    env: &mut Env,
    rm: &RayMarket,
    b: &mut Borrower,
    q: u64,
) -> (Open, u64, TransactionMetadata) {
    let (res, nonce) = env.open(rm, b, q);
    let meta = ok(res);
    (events::one(&meta), nonce, meta)
}

/// A close that must succeed: its event and metadata.
pub fn close_ok(
    env: &mut Env,
    rm: &RayMarket,
    b: &Borrower,
    nonce: u64,
) -> (Close, TransactionMetadata) {
    let meta = ok(env.close(rm, b, nonce));
    (events::one(&meta), meta)
}

/// Lamports of `key`.
pub fn lamports(env: &Env, key: &Pubkey) -> u64 {
    read::lamports(&env.svm, key)
}

/// Token amount of `key`.
pub fn tokens(env: &Env, key: &Pubkey) -> u64 {
    read::token_amount(&env.svm, key).unwrap_or(0)
}
