//! One module per instruction (ARCH §3): the `#[derive(Accounts)]` struct with every
//! constraint, and `handler`. `lib.rs` stays thin: each entrypoint calls
//! `instructions::<name>::handler`.
//!
//! M1: admin, config and latch; `create_market` and `flag_unsafe_mint`; the six lender-pool
//! instructions; `crank_sol_usd`. M2: `open_short`, `close_short`, `add_margin`, `liquidate` and
//! `expire` (which share `liquidate`'s keeper-exit machinery), `sol_settle`, `crank_price`, the
//! O6 powers `set_route_broken` and `set_venue_extras`, and `sweep_trader` (ARCH §3.9, brought
//! forward from M6 by M2-SEC-4: a full wipe is reachable from M2 on and a deposit then waits
//! for it). M4: `propose_pool` and `execute_pool_switch` (Raydium pool selection, ARCH §2.6,
//! §3.9; the rules are `crate::engine::pool_select`). M7: `set_coverage_pause` (O17, recorded in
//! SPEC §14 on 2026-09-30: the keeper's pause-opens-only `coverage_authority`).

pub mod add_margin;
pub mod apply_config;
pub mod cancel_config;
pub mod claim_sol;
pub mod close_short;
pub mod cover_deficit;
pub mod crank_price;
pub mod crank_sol_usd;
pub mod create_market;
pub mod deposit;
pub mod disable_venue;
pub mod execute_pool_switch;
pub mod expire;
pub mod flag_unsafe_mint;
pub mod flag_venue_upgraded;
pub mod initialize_config;
pub mod liquidate;
pub mod open_short;
pub mod propose_config;
pub mod propose_pool;
pub mod revalidate_venue;
pub mod set_coverage_pause;
pub mod set_pause;
pub mod set_route_broken;
pub mod set_venue_extras;
pub mod sol_settle;
pub mod sweep_protocol_fees;
pub mod sweep_trader;
pub mod sync_lender;
pub mod withdraw;

// The `#[program]` macro needs each Accounts struct (and the `__client_accounts_*` /
// `__cpi_client_accounts_*` modules its derive generates) at the crate root. Every module also
// has a `handler`; `lib.rs` always calls it by its full path, so the glob re-exports' clash on
// that one name is harmless.
#[allow(ambiguous_glob_reexports)]
pub use add_margin::*;
pub use apply_config::*;
pub use cancel_config::*;
pub use claim_sol::*;
pub use close_short::*;
pub use cover_deficit::*;
pub use crank_price::*;
pub use crank_sol_usd::*;
pub use create_market::*;
pub use deposit::*;
pub use disable_venue::*;
pub use execute_pool_switch::*;
pub use expire::*;
pub use flag_unsafe_mint::*;
pub use flag_venue_upgraded::*;
pub use initialize_config::*;
pub use liquidate::*;
pub use open_short::*;
pub use propose_config::*;
pub use propose_pool::*;
pub use revalidate_venue::*;
pub use set_coverage_pause::*;
pub use set_pause::*;
pub use set_route_broken::*;
pub use set_venue_extras::*;
pub use sol_settle::*;
pub use sweep_protocol_fees::*;
pub use sweep_trader::*;
pub use sync_lender::*;
pub use withdraw::*;
