//! Account structs, sizes and PDA seeds (ARCH §2.3; A67).
//!
//! | Account | Kind | Seeds | Size |
//! |---|---|---|---|
//! | [`GlobalConfig`] | zero-copy | `["config"]` | 1,448 |
//! | [`VenueGuard`] | zero-copy | `["venue_guard"]` | 224 |
//! | [`VenueExtras`] | zero-copy | `["venue_extras", venue u8]` | 4,352 |
//! | [`OracleCache`] | Borsh | `["sol_usd"]` | 45 |
//! | [`Market`] | zero-copy | `["market", mint]` | 3,104 |
//! | [`MarketVault`] | zero-copy | `["market_vault", market]` | 2,200 |
//! | pool token vault | SPL/T22 token account | `["pool_vault", market]`, authority `market` | 165/170 |
//! | `trader` | system-owned, zero data, never allocated | `["trader", market]` | 0 |
//! | `trader_wsol` | ephemeral WSOL account | `["trader_wsol", market]` | 165 |
//! | [`ShortPosition`] | Borsh | `["position", market, borrower, nonce u64 LE]` | 384 |
//! | [`LenderPosition`] | Borsh | `["lender", market, owner]` | 241 |
//! | [`ShareEpoch`] | Borsh | `["share_epoch", market, epoch u32 LE]` | 143 |
//! | [`WalletExposure`] | Borsh | `["exposure", market, borrower]` | 81 |
//!
//! Bumps are stored and re-used with `create_program_address`; `find_program_address` is never
//! called on a hot path. Rent-exempt minimum = `Rent::minimum_balance(len)` (never hard-coded).

pub mod config;
pub mod epoch;
pub mod exposure;
pub mod layout_asserts;
pub mod lender;
pub mod market;
pub mod oracle;
pub mod pod;
pub mod position;
pub mod vault;
pub mod venue_extras;
pub mod venue_guard;

pub use config::*;
pub use epoch::*;
pub use exposure::*;
pub use lender::*;
pub use market::*;
pub use oracle::*;
pub use pod::*;
pub use position::*;
pub use vault::*;
pub use venue_extras::*;
pub use venue_guard::*;

/// Seed of the pool token vault (`["pool_vault", market]`, authority = `market`).
pub const POOL_VAULT_SEED: &[u8] = b"pool_vault";
/// Seed of the per-market `trader` PDA (`["trader", market]`, system-owned, zero data).
pub const TRADER_SEED: &[u8] = b"trader";
/// Seed of the ephemeral WSOL account (`["trader_wsol", market]`).
pub const TRADER_WSOL_SEED: &[u8] = b"trader_wsol";
/// Seed of the v1.1 Jupiter per-position signer (`["swap_auth", position]`).
pub const SWAP_AUTH_SEED: &[u8] = b"swap_auth";
