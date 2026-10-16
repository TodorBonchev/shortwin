//! Keeper scenarios on LiteSVM (ARCHITECTURE §7.1): v0's duties 1–4 and batching on the mock
//! venue (§9 M2), v1 on Raydium (§9 M3: the `raydium*` modules — the duties on the dumped AMM
//! v4 and CPMM programs and pools, duty 10's admin-action monitor and upgrade latch, the
//! protocol ALT built on chain), and Raydium pool selection (§9 M4: `pool_select` — duty 12's
//! pool monitor and duty 5's fast path on planted sibling pools), and `freeze` (M4F-1 and its
//! follow-ups on the fork: the keeper's per-second coverage while the post-change guard is set;
//! duty 2's liquidity-triggered **per-second** coverage on markets with open positions, FITS-2
//! (b) — the per-slot liquidity bursts M4F-1 added stay reverted, M4G-2, so duty-2 bursts are
//! price-only — against LP-wrapped cranks after a switch and in steady state; the carried fit of
//! FIT-2 / FITS-1; FITV-2 / FITS-2's withdraw contest and FITX-1's pinned residual, a genuinely
//! reached low frozen by withdraw-wrapped cranks; the keeper's cost of an LP-wrapped crank), and
//! `graduation` (M5, duty 6 on the pump-curve fork: `migrate_v2` as soon as a curve completes; a
//! curve that graduated elsewhere settled in S3 on its detection set, duty 5; a list-only market
//! re-detected on its curve and its trader accounts created; M6: the `→ PumpSwap` transition after
//! the keeper's `migrate_v2`, a graduation with a position liquidated on the pool; pump-final §7:
//! a completing liquidation and a synthetic expiry past `rT` — pump v3's synthetic migration in
//! the keeper's own exit, the market graduated in the same tick, the other positions exiting on
//! the pool at the warm-up's gate —, σ above `σ_max` going to `sol_settle`, a race with a third
//! party's synthetic completion, duty 10 on pump's post-complete fields), `upgrade_window`
//! (pump-final §7, §15: the keeper liquidates with shortwin 1.0.0's curve list before the
//! Governance upgrade and with the v3 list on the first tick after it, no failed send), `pamm`
//! (M6: the keeper on PumpSwap — a listing warmed up and a pump liquidated
//! with the keeper's PumpSwap lists, `disable_flags` reported, the existence requirement's accounts
//! created), `pamm_lp` (M6R-1, the owner's decision of 2026-09-30: duty 2's per-second
//! liquidity-anomaly coverage against a withdraw drag on a PumpSwap pool with third-party LP, and
//! none on a market without positions) and `wipe` (M6: duty 6's post-wipe transaction on PumpSwap and the curve with the venue
//! cashback claims, the market reopening; duty 13's `sweep_trader` and weekly cashback claim),
//! `coverage_pause` (M7, O17: the T-COVERAGE drill — the designated pauser's `set_coverage_pause`
//! on an outage and on an exhausted budget, exits continuing under the pause, the recovery, a
//! restart, the role unset or rotated, two instances; since 2026-10-01 the Emergency's clear of
//! a hijacked key's pause and of the pauser's own), `oracle` (M7, duty 11: `crank_sol_usd` within
//! 30 s of every feed update, refusals and aging alerted), `cascade` (M7 T-CASCADE: a 25-SOL basket
//! of `min_notional` positions liquidated under the per-account block budget), `m7_cost` (M7: the
//! keeper cost-model inputs of ECON §5.8, T-SIZE batch sizes, load), `pamm_alt` (M7 OWNER-M7-1:
//! the PumpSwap 4-extras transactions through the tables the keeper's `alt` module builds) and
//! `m7_sec` (the M7 security review's keeper findings, fixed: landing latency and slow ticks —
//! the CI latency scenario —, bursts only on reverted excursions, a coverage clear or an open
//! the keeper did not expect alerted, junk list-only markets off the per-tick reads), and
//! `budget` (the owner's decision A of 2026-10-01: the global keeper budget — a burst storm on one
//! market spends its entitlement and the free pool and nothing else, junk below the value-at-risk
//! floor gets the idle cadence only and stops at the tail, a junk market above it preempts the
//! lowest-ranked first, exhaustion degrades the lowest-ranked markets first), and `m7d` (the
//! verification of decision A, fixed: below the floor an exit lands the second after the idle
//! crank that makes it possible, a cascade whose value at risk crosses the floor is liquidated,
//! that crank is exit-critical, a burst charges one exit-critical crank a second, a keeper start
//! funds its markets at the batched estimate), and `redeploy` (AIR-18: short.win or a venue not
//! deployed for a slot — a stranger's `ExtendProgram` — and the refused exit, crank, latch flag,
//! S3 probe or sweep sent again the next slot, nothing alerted; a venue extended in every slot
//! for 30 s paged once; a venue that stays undeployable refused and alerted).
//! Every keeper transaction runs the harness's invariant hooks after every instruction
//! (`shortwin_keeper_litesvm`).
//!
//! Loads short.win's **test-venue** build (`scripts/build-test-venue.sh`, or
//! `SHORTWIN_TEST_VENUE_SO`) and the mock venue (`target/deploy/mock_cpamm.so`, or
//! `MOCK_CPAMM_SO`) for the mock scenarios, and the **release** build
//! (`target/deploy/shortwin.so`, or `SHORTWIN_SO`) with the committed mainnet fixtures for the
//! Raydium and pump ones, as the program tests do; `upgrade_window` starts from shortwin 1.0.0
//! (`tests/fixtures/shortwin/shortwin_1_0_0.so`) and upgrades it to the release build.

#![allow(clippy::result_large_err)]

mod batching;
mod budget;
mod bursts;
mod cadence;
mod cascade;
mod common;
mod coverage_pause;
mod degraded;
mod expire;
mod extras;
mod freeze;
mod graduation;
mod liquidate;
mod m7_cost;
mod m7_sec;
mod m7d;
mod oracle;
mod pamm;
mod pamm_alt;
mod pamm_lp;
mod pool_select;
mod priority;
mod raydium;
mod raydium_admin;
mod raydium_alt;
mod redeploy;
mod s3;
mod selection;
mod settle;
mod smoke;
mod upgrade_window;
mod wipe;
