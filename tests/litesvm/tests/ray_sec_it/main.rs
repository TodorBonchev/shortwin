//! The M3 security review of the Raydium integration (`M3-SEC-n`) as permanent regressions, on
//! the mainnet fork: the release short.win against the dumped AMM v4 / CPMM programs. Every
//! instruction goes through `Env::send` (or `fork::raydium_admin_send` for the Raydium admin's
//! real instructions), so the invariant hooks run after each one — I17 now also on listings.
//!
//! | Test | Finding |
//! |---|---|
//! | `m3_sec_1_*` | a CPMM config whose trade fee the admin sets to 0: every buy of a pool whose creator fee is on its input reverts (`ZeroTradingTokens`); the pool is no route (S3 chain, frozen healthy positions), never an uncapped S1, and its risk view is fee-free, never `+∞` |
//! | `m3_sec_2_*` | a Raydium listing waits for the review of a flagged Raydium upgrade (`VenueUpgraded`), as opens do; the I17 listing hook |
//! | `m3_sec_3_*` | the M3 tripwire, inverted by M4: a list-only front-run is adopted onto the token's pool by `propose_pool`, and a thin eligible listing is replaced inside `pool_list_window_s` |
//!
//! M3-SEC-4 (ARCH's CPMM age formula) is a documentation finding; the code's O13 behaviour is
//! pinned by `ray_it listing::pool_age_follows_o13` and the reader's `age_start_clamps`.

#![allow(clippy::result_large_err)]

// `ray_it`'s shared helpers; this binary uses a subset of their re-exports.
#[allow(unused_imports)]
#[path = "../ray_it/common.rs"]
mod common;

mod sec;
