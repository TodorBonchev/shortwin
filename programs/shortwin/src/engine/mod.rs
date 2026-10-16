//! Shared instruction machinery (ARCH §2.1 `engine/`): what several instructions run in the
//! same order, applying `shortwin-math` results to accounts. Pure arithmetic stays in the math
//! crate; these modules are thin wrappers that load, call, store and move.
//!
//! | Module | Content |
//! |---|---|
//! | [`arming`] | SOL settlement on the settlement box: the inputs of `settle::evaluate` (legs, box, S3 chain), the risk quotes of the box predicates, the grief-proof `ShareEpoch` creation (ARCH §2.1's `engine/settle.rs`) |
//! | [`prologue`] | the market prologue (ARCH §3.0 steps 1–8): params and kink versions (forced EMA adoption), accrual with the vest release, the observation roll, the record (only while executable), the S3 and `pool_fail` chains |
//! | [`observe`] | roll (with `CoverageGap`; the post-change warm-up and reference guard are the math crate's `integrate`, M4R), record, liquidity rewrite, initial price state (marked for a Raydium price start) |
//! | [`route`] | `route_broken`, `s3_no_route`, `pool_fail`, the chains |
//! | [`guard`] | the mint guard (entry / exit), the open guards (pauses, coverage, route flags, venue enabled, the latch mask) |
//! | [`trader`] | trader normalisation, the ephemeral WSOL account, the leg post-conditions (I7) |
//! | [`caps`] | the SPEC §4 open caps |
//! | [`tier`] | `tier::assign` inputs from the market, the venue and the oracle |
//! | [`bounds`] | exit references and `max_in` (close; the liquidation and expiry tracks) |
//! | [`waterfall`] | position aggregates (I6), `pool::repay` booking, payout lamport moves |
//! | [`lender`] | `ShareEpoch` records, new lender positions, the post-wipe rule, SOL pay-outs |
//! | [`pool_select`] | Raydium pool selection (M4): what a proposal does (listing, listing window, zero-position, the fast path's collection window), the candidate's verification, the window floor at `min(P_ema, P_cand)` for proposals (M4R-2), the adoption (the anchor rewrite and the restart marks, M4R; chains, window, age, listing window) |

pub mod arming;
pub mod bounds;
pub mod caps;
pub mod guard;
pub mod lender;
pub mod observe;
pub mod pool_select;
pub mod prologue;
pub mod route;
pub mod tier;
pub mod trader;
pub mod waterfall;
