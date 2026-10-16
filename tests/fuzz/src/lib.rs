//! short.win's invariant fuzz harness (ARCHITECTURE §8.3, §8.4; M7).
//!
//! A **LiteSVM 0.16 action-sequence fuzzer** on the program-test harness
//! (`shortwin_litesvm_tests`): seeded, deterministic action streams ([`action`]) against the
//! test-venue build and the mainnet-fork fixtures ([`venue`]: the mock, the pump curve and its
//! graduation, PumpSwap, Raydium AMM v4 and CPMM), every transaction through `Env::send` — so the
//! harness's invariant hooks (I1–I13, I15–I19 and A26 after every instruction and after every
//! prefix of a multi-instruction transaction) are checked after every action — plus the fuzzer's
//! own oracles ([`world`]: I10 / I17 gates, the differential exit oracles — the mock's, AMM v4's
//! and (M10's follow-ups) CPMM's for I9 / I11 / I12 / I13 / I-E11, the curve's and PumpSwap's
//! ([`pswap_exit`]) for every decision and I-E11, the curve's since pump-final §12 with its
//! synthetic exits' completion —, the `migrating_fv` oracle on every S3 of a `PumpMigrating`
//! market and every `migrate_v2` ([`migrating`]), I14 omissions and replacements, the JIT capture
//! and claim-bundle oracles (M10), the deficit tripwire ([`shortfall`]), I4's coverage clause on
//! every `set_coverage_pause` (O17; since 2026-10-01 the Emergency's clear-only power too,
//! OWNER-M7-3 (b)), the mint filter on exits, governance's timelock and cancels, the oracle's
//! refusals, the wind-down's liveness and its second claim pass). Failures are crash files with the
//! profile, seed and action list ([`runner`]); `replay` re-runs them and `tmin` minimises them by
//! delta debugging over the action list.
//!
//! Why not Crucible (`anchor fuzz`): Anchor 1.2.0's harness crates pin `litesvm =0.15.2` and the
//! Agave 4.1 runtime, which cannot share a lockfile with this repository's LiteSVM 0.16 / Agave
//! 4.2 line (C1-14), so a Crucible harness could not link the I1–I18 hooks or the sha-checked
//! fork fixtures; its stateful depth (15 actions by default) is also short for the protocol's
//! clock-driven paths (a 3,600 s S3 wait, 72 h deadlines). See README.md.

// `TxResult` is LiteSVM's own result type (as in shortwin-litesvm-tests). Most fuzzer
// transactions may be refused as an ordinary outcome: `Ctx::send` records every result, so the
// call sites that do not branch on one drop it.
#![allow(clippy::result_large_err, unused_must_use)]

pub mod action;
pub mod curve_exit;
pub mod migrating;
pub mod pswap_exit;
pub mod rng;
pub mod runner;
pub mod setup;
pub mod shortfall;
pub mod stats;
pub mod venue;
pub mod world;

/// `tests/litesvm/tests/liquidate_support.rs`, the mock's differential exit oracle (every
/// decision and amount of `liquidate` / `expire` recomputed from `shortwin-math`), included as
/// the program tests include it.
#[path = "../../litesvm/tests/liquidate_support.rs"]
pub mod liq;

/// `tests/litesvm/tests/raydium_v4_support.rs`, the AMM v4 fork E2E's differential exit oracle
/// (every decision of ARCH §3.6 / §3.7 — `ExitBoundNotMet` against `⌊B̂_ref·(10⁴ + δ)/10⁴⌋`
/// included, so I-E11 — and every lamport and token recomputed from `shortwin-math` and the
/// venue-state readers), included as the AMM v4 tests include it (M10, G-I11-real); venue-generic
/// since M10's fuzz follow-ups, so it also checks CPMM exits and decides PumpSwap's.
#[path = "../../litesvm/tests/raydium_v4_support.rs"]
pub mod v4;

/// `tests/litesvm/tests/pump_curve_support.rs`, the curve fork E2E's keeper view of a position
/// (`status`: the legs, `B̂_live`, "buyback allowed" from `shortwin-math` on the readers), on
/// which [`curve_exit`] predicts every exit decision (M10, G-I11-real).
#[path = "../../litesvm/tests/pump_curve_support.rs"]
pub mod curve_support;
