//! `OracleCache` (Borsh, seeds `["sol_usd"]`, 45 B; ARCH §2.3.6, §4.7, A10; R-VEN §5).
//!
//! Created by `initialize_config`; written only by the permissionless `crank_sol_usd`; read by
//! `open_short` and `crank_price` for the USD tier thresholds. Closes, liquidations and
//! settlements never read it (I10).

use anchor_lang::prelude::*;

/// The cached Pyth SOL/USD price.
#[account]
#[derive(InitSpace, Debug, Default, PartialEq, Eq)]
pub struct OracleCache {
    /// Price in units of `10^expo` USD.
    pub price: i64,
    pub conf: u64,
    pub expo: i32,
    /// Pyth publish time (unix seconds); 0 = never cranked.
    pub publish_time: i64,
    pub updated_slot: u64,
    pub bump: u8,
}

impl OracleCache {
    /// PDA seed.
    pub const SEED: &'static [u8] = b"sol_usd";
    /// Account size including the discriminator.
    pub const SPACE: usize = 8 + Self::INIT_SPACE;
}
