//! short.win program-test harness on LiteSVM 0.16 (ARCHITECTURE §8.2, §8.3).
//!
//! - [`Env`]: LiteSVM with `target/deploy/shortwin.so` (build it first with `anchor build
//!   --tools-version v1.57 --arch v3`), the admin keys (short.win's upgrade authority recorded
//!   in its ProgramData, Governance, Emergency, fee recipient), the clock under test control
//!   ([`Env::warp`], [`Env::set_clock`]) and the invariant hooks on every [`Env::send`]. The
//!   setup helpers that send around it (token builders, airdrops) go through
//!   [`env::send_direct`], which expires the blockhash after each send as [`Env::send`] does.
//! - [`ix`]: instruction builders for every M1 instruction (anchor-lang's generated types).
//! - [`tokens`]: SPL Token and Token-2022 mints, accounts and holders.
//! - [`fixtures`]: mock venue ProgramData (T-LATCH), raw account writers, an installed
//!   list-only market (as `create_market` writes it), mint fixtures.
//! - [`invariants`]: `check_all(&svm, market)` (I1, I2, I8) after every instruction, and
//!   `check_ix` (I4, I17, the lender instruction bounds) for each instruction against the
//!   state just before it; [`Env::send`] runs both on every market a transaction names, after
//!   each instruction of a multi-instruction transaction too.
//! - [`events`], [`errors`], [`read`], [`pda`]: event decoding, error assertions, account
//!   readers, PDAs.
//! - [`mock`]: the TEST-ONLY mock venue `mock_cpamm` (M2; `tests/mock_cpamm/INTERFACE.md`):
//!   [`Env::load_mock_cpamm`], pools on SPL / Token-2022 base mints, liquidity, knobs,
//!   reserves, swap builders and short.win's `quote::cpmm` view of a pool.
//! - [`engine`]: the position engine on the mock (M2): [`Env::test_venue`] (short.win's
//!   test-venue build, `scripts/build-test-venue.sh`), mock markets through real instructions,
//!   borrowers, builders of every M2 instruction.
//! - [`drive`]: time and price drivers (actors trading the mock, per-second cranks, price
//!   pushes); [`attack`]: atomic bundles and sandwiches in one transaction.
//! - [`fork`]: the Raydium mainnet fork (M3): the dumped AMM v4 / CPMM programs and pools
//!   (`tests/fixtures/`), [`Env::fork_raydium_v4`] / [`Env::fork_cpmm`], users and direct
//!   swaps, byte-level and real admin actions, `ray_log` / `SwapEvent` decoding.
//! - [`raydium`]: Raydium **markets** on the fork (M3): [`Env::ray_fork`], [`Env::ray_market`]
//!   (`create_market(RaydiumV4 | RaydiumCpmm)`, lender, listing window), [`raydium::RayMarket`]
//!   (implements [`engine::VenueMarket`], so the M2 builders trade it), pump-detection fixtures.
//! - [`pool_select`]: Raydium pool selection on the fork (M4): `propose_pool` /
//!   `execute_pool_switch` builders, list-only listings, planted candidate pools
//!   ([`pool_select::plant_v4`], [`pool_select::plant_cpmm`]) that the dumped ELFs trade.
//! - [`lp`]: a liquidity provider on a CPMM pool of the fork (the dumped program's own
//!   `deposit` / `withdraw`; M4F-1's deposit-wrapped cranks).
//! - [`pump`]: pump-curve **markets** on the fork (M5): [`Env::pump_market`]
//!   (`create_market(Auto)` on a dumped live curve, lender, warm-up), [`pump::PumpMarket`]
//!   (implements [`engine::VenueMarket`]: the curve ReadSet C0–C6 and TradeExtras T0–T19),
//!   curve users, `sweep_trader`'s token leg.
//! - [`squads`]: Squads v4 governance (M10 WP-7): the mainnet Squads ELF ([`Env::with_squads`]),
//!   [`squads::SquadsMs`] (create, stage — also through a transaction buffer —, approve,
//!   execute, cancel, config transactions; every transaction through [`Env::send`], ≤ 1,232
//!   bytes), the owner's layout ([`Env::squads_governed`]), the loader's authority and upgrade
//!   instructions.
//! - [`record`]: session recording for the indexer's replay harness (M8; INDEXER.md §11.3):
//!   [`Env::start_recording`], [`Env::execute_recorded`] (the one place a chain transaction
//!   executes), account checkpoints; `services/indexer-litesvm` renders a recording as a session.
//!
//! Agave line: LiteSVM 0.16.0 on the 4.2.2 runtime crates, pinned in the workspace lockfile
//! (C1-14; `Cargo.toml` of this crate).

// `TxResult` is LiteSVM's own `TransactionResult` (its `FailedTransactionMetadata` carries the
// logs); boxing it would only make every call site unwrap one more layer.
#![allow(clippy::result_large_err)]

pub mod attack;
pub mod drive;
pub mod engine;
pub mod env;
pub mod errors;
pub mod events;
pub mod fixtures;
pub mod fork;
pub mod invariants;
pub mod ix;
pub mod lp;
pub mod mock;
pub mod pamm;
pub mod pda;
pub mod pool_select;
pub mod pump;
pub mod raydium;
pub mod read;
pub mod record;
pub mod squads;
pub mod tokens;

pub use env::{Env, START_SLOT, START_TS, TxResult, test_venue_so_path};
