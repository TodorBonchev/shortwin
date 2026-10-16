//! TEST-ONLY: the M8 replay harness (docs/design/INDEXER.md §11.3, §11.4). Owner: ANALYTICS +
//! REPLAY.
//!
//! - [`scenarios`]: the recorded sessions — LiteSVM runs with the `Env` recording hook on, from
//!   before `initialize_config`: `mock_lifecycle`, `mock_admin`, `keeper_liquidation`,
//!   `keeper_coverage_pause` (test-venue build, mock venue) and `raydium_lifecycle`,
//!   `pool_switch`, `pump_graduation`, `pump_graduation_raced` (release build, mainnet fork).
//! - [`recorder`]: turns what LiteSVM executed — every transaction through
//!   `shortwin_litesvm_tests::Env::send` and the keeper's `send_with_hooks` (bundles included),
//!   via the `Env` recording hook of §11.3 (`tests/litesvm/src/record.rs`) — into a session
//!   file (`shortwin_indexer::ingest::session`): each transaction as JSON-RPC `getTransaction`
//!   would return it (base64 wire bytes, `meta.innerInstructions` with base58 data and
//!   `stackHeight`, `logMessages`, `err`, `fee`, balances, `computeUnitsConsumed`,
//!   `loadedAddresses`), plus account checkpoints of every short.win account.
//! - [`chain`]: decodes a checkpoint's accounts with `shortwin::state` into [`chain::ChainState`];
//!   decodes the session's transactions on its own (invocations, `emit_cpi!` and log events,
//!   System transfers); and [`chain::Model`] applies the contract's projection rules
//!   (INDEXER.md §5.3, §6) to them — verified against every checkpoint by
//!   [`chain::check_session`], and the independent recomputation of the analytics.
//! - [`assert`]: the DB-vs-chain assertions of §11.4 (projections against the accounts, ledger
//!   identities, analytics, rollups, dumps for idempotence / rebuild / out-of-order /
//!   rollback, catalog coverage) and [`assert::replay_session`], the whole list through the
//!   INGEST pipeline (`shortwin_indexer::ingest::ingest_session`).
//! - [`seed`]: a reference writer that stores the model's rows in the contract schema — how the
//!   harness checks its own assertions (and the analytics views and rollups) on real sessions
//!   independently of the INGEST pipeline.

pub mod assert;
pub mod chain;
pub mod recorder;
pub mod scenarios;
pub mod seed;
