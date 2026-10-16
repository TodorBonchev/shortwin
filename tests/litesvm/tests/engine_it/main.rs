//! The M2 position engine on the mock venue (ARCH §8.2, §9 M2): `open_short`, `close_short`,
//! `crank_price`, the prologue, trader normalisation, the O6 powers, the landed exits' gates on
//! a healthy position, sandwiches, T-TRACE and CU, the structural I14 test and the M2 hooks'
//! own tests. The other M2 instructions have their own test binaries (`add_margin_it`,
//! `liquidate_it`, `expire_it`, `settle_it`, `route_it`, `extras_it`, the attack suites).
//! Loads short.win's **test-venue** build (`scripts/build-test-venue.sh`, or
//! `SHORTWIN_TEST_VENUE_SO`) and the mock venue (`target/deploy/mock_cpamm.so`, or
//! `MOCK_CPAMM_SO`).

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

mod close;
mod common;
mod flows;
mod fuzz;
mod hooks;
mod landed;
mod o6;
mod overdelivery;
mod prologue;
mod refusals;
mod sandwich;
mod smoke;
mod structure;
mod trace_cu;
mod trader;
mod zero_lamport;
