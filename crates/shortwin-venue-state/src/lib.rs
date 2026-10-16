//! short.win venue-state readers (ARCHITECTURE §1.2, §4.1).
//!
//! Length-tolerant, bytes-in / values-out readers of the accounts the program prices, routes
//! and guards on, shared bit for bit by the program, the keeper and the WASM frontend. Every
//! reader takes the account's owner and data as plain bytes, checks the owner, the length
//! and (where one exists) the 8-byte discriminator, and **zero-pads** reads past the end of a
//! shorter account (ARCH §4.1; R-TOOL §1.4; R-VEN §1.2). A reader failure on a correctly keyed
//! venue-state account means "not executable" to the route logic, never a revert (A63, V1-8);
//! only key mismatches revert, and key checks are the program's job, not this crate's.
//!
//! `#![no_std]`, zero dependencies (no `Pubkey` type: keys are `[u8; 32]`), and the ARCH §2.2
//! denies.
//!
//! | Module | Account | Status |
//! |---|---|---|
//! | [`bytes`] | little-endian field reads with zero padding | M1 (foundation) |
//! | [`program_data`] | BPF upgradeable loader `ProgramData` header (tag, deploy slot, authority) | M1 (foundation): the venue-upgrade latch (A7) |
//! | [`pyth`] | Pyth `PriceUpdateV2` and the `crank_sol_usd` acceptance rules | M1 (oracle track) |
//! | [`mint`] | SPL / Token-2022 mint base fields and the byte-level allowlist filter (`check`) | M1 (market track): differential-tested against the program's `check_mint_safety` |
//! | [`token_account`] | SPL / Token-2022 token accounts (mint, owner, amount, delegate, close authority) | M2: venue vaults, the pool vault's delta and delegate, `trader_wsol` |
//! | `mock_cpamm` | TEST-ONLY mock venue pool (feature `test-venue`; never in a release build, A49) | M2 |
//! | [`raydium_v4`] | Raydium AMM v4 `AmmInfo`: reserves, the route predicate, the fee in ppm, λ | M3 |
//! | [`raydium_cpmm`] | Raydium CPMM `PoolState` / `AmmConfig`: reserves, the route predicate, fees, λ, the `T_CLAMP` age | M3 |
//! | [`pump`] | pump `BondingCurve` (every layout: 49, 115, 125, 150 and 151 bytes; the lifecycle helpers), pump `Global` (a borrowed view: the fee and buyback recipient lists, `pool_migration_fee`, `creator_fee_configurable`), pump-fees `FeeConfig` (`FeeTiers::select`, differentially equal to `fees::fee_tier`; also PumpSwap's), the user volume accumulator | M3 (the detection subset of `classify_pump`); full readers M5 (the curve adapter) |
//! | [`pump_amm`] | PumpSwap `Pool` (the canonical-pool test of `classify_pump`; `reserves` with a **signed** `virtual_quote_reserves`, effective = vault + VQR in `[0, u64::MAX]`, from 2026-09-30), PumpSwap `GlobalConfig` (a borrowed view; `disable_flags` raw), the boost vault's amount | M3 (detection); M5 (`B̂_str`'s post-migration pool schedule); the PumpSwap adapter's use with M6 |
#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unwrap_used
)]

pub mod bytes;
pub mod mint;
#[cfg(feature = "test-venue")]
pub mod mock_cpamm;
pub mod program_data;
pub mod pump;
pub mod pump_amm;
pub mod pyth;
pub mod raydium_cpmm;
pub mod raydium_v4;
pub mod token_account;

/// A 32-byte account key (no dependency on a `Pubkey` type).
pub type Key = [u8; 32];

/// Why a reader refused an account. Callers map this to "not executable" (venue state, A63)
/// or to their own error (the latch, the oracle).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReadError {
    /// The account is not owned by the program that owns this kind of account.
    WrongOwner,
    /// The account is shorter than the smallest valid layout.
    TooShort,
    /// The discriminator or type tag does not match.
    WrongTag,
    /// A field holds a value the layout does not allow (e.g. an invalid `Option` tag).
    Malformed,
}
