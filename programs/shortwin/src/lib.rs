//! short.win on-chain program (Anchor 1.2.0; ARCHITECTURE §2, §3).
//!
//! Module map (ARCH §1.2, §2.1): `constants` (compile-time venue ids, static PDAs, WSOL,
//! `T_CLAMP`, re-exported protocol constants and compiled bounds), `state` (every account of
//! ARCH §2.3, `PodU128`/`PodI128`, T-LAYOUT asserts), `instructions` (one module per
//! instruction), `engine` (shared instruction machinery), `events` (ARCH §3.10), `errors`
//! (ARCH §3.11), `lamports` (checked direct lamport moves), `mint_filter` (the allowlist
//! filter). All arithmetic is `shortwin-math`'s; the program never re-implements a formula.
//!
//! M1 (ARCH §9): admin, config, latch, markets, lenders, the oracle. M2: the position engine
//! (`engine::*`, `venues::*`), `open_short`, `close_short`, `add_margin`, `liquidate`, `expire`,
//! `sol_settle`, `crank_price` and the O6 Emergency powers `set_route_broken` /
//! `set_venue_extras`. M3: the Raydium AMM v4 and CPMM
//! adapters (`venues::raydium_v4`, `venues::raydium_cpmm`, `venues::raydium`) and the Raydium
//! listing `create_market(venue_hint = RaydiumV4 | RaydiumCpmm)` with the D1 precedence check
//! (`venues::detect`). M4: Raydium pool selection (`propose_pool`, `execute_pool_switch`;
//! `engine::pool_select`). M5–M6: the pump curve and PumpSwap venues. M7: `set_coverage_pause`,
//! the keeper's pause-opens-only `coverage_authority` for degraded mode (O17, recorded in SPEC §14
//! on 2026-09-30).
//!
//! Feature `test-venue` (TEST-ONLY, A49): `VenueKind::Mock`, the adapter of the mock venue
//! `tests/mock_cpamm`, for the LiteSVM engine tests. Release builds never enable it
//! (`scripts/build-test-venue.sh` builds it into its own target directory; CI checks the
//! release `shortwin.so`).

#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    unsafe_code
)]

use anchor_lang::prelude::*;

pub mod constants;
pub mod engine;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod lamports;
pub mod mint_filter;
pub mod state;
pub mod venues;

pub use instructions::*;
pub use state::*;

declare_id!("5yvyAddWb8RRGh3pPiFS5eV33eZYeHVmAAfwAFmqVpKK");

// security.txt (neodyme-labs/solana-security-txt =1.1.3; ARCH §8.5, M10 SECTXT-1), read by
// Solana Explorer and `query-security-txt`; `scripts/check-security-txt.py
// --forbid-placeholders` checks it in CI (the release gate). The values are the owner's (M10 Q3,
// 2026-10-02; `SECURITY.md`, the web app's `/security` page and `/.well-known/security.txt` carry
// the same). Not in
// `no-entrypoint` builds (the keeper links both programs as libraries: two `#[no_mangle]
// SECURITY_TXT` symbols would clash) nor in the TEST-ONLY `test-venue` build (A49). No `expiry`,
// `source_code` or `source_revision` while the repository is private, and no `auditors` before
// the first published audit; `source_release` is the package version (`v1.0.0` at the audit
// freeze), never a commit (the hash must not depend on it).
#[cfg(not(any(feature = "no-entrypoint", feature = "test-venue")))]
solana_security_txt::security_txt! {
    name: "short.win",
    project_url: "https://short.win",
    contacts: "email:security@short.win,link:https://short.win/security",
    policy: "https://short.win/security",
    preferred_languages: "en",
    source_release: concat!("v", env!("CARGO_PKG_VERSION"))
}

#[program]
pub mod shortwin {
    use super::*;

    // ---- initialization and admin (ARCH §3.1) ----

    /// Creates `GlobalConfig`, `VenueGuard`, the `VenueExtras` and `OracleCache` (upgrade
    /// authority, once).
    pub fn initialize_config(ctx: Context<InitializeConfig>, args: InitConfigArgs) -> Result<()> {
        instructions::initialize_config::handler(ctx, args)
    }

    /// Governance proposes a timelocked config change.
    pub fn propose_config(ctx: Context<ProposeConfig>, change: ConfigChange) -> Result<()> {
        instructions::propose_config::handler(ctx, change)
    }

    /// Anyone applies the pending change once its timelock has run.
    pub fn apply_config(ctx: Context<ApplyConfig>) -> Result<()> {
        instructions::apply_config::handler(ctx)
    }

    /// Governance or the Emergency vault cancels the pending change.
    pub fn cancel_config(ctx: Context<CancelConfig>) -> Result<()> {
        instructions::cancel_config::handler(ctx)
    }

    /// Pauses or resumes new opens, globally or on one market.
    pub fn set_pause(ctx: Context<SetPause>, scope: PauseScope, paused: bool) -> Result<()> {
        instructions::set_pause::handler(ctx, scope, paused)
    }

    /// The keeper's `coverage_authority` pauses or resumes new opens on one market for lost
    /// keeper coverage (degraded mode; O17); the Emergency vault may resume them too (clear
    /// only; OWNER-M7-3). Opens only; moves nothing.
    pub fn set_coverage_pause(
        ctx: Context<SetCoveragePause>,
        paused: bool,
        reason: u8,
    ) -> Result<()> {
        instructions::set_coverage_pause::handler(ctx, paused, reason)
    }

    /// The Emergency vault disables a venue for opens.
    pub fn disable_venue(ctx: Context<DisableVenue>, bit: u8) -> Result<()> {
        instructions::disable_venue::handler(ctx, bit)
    }

    /// Anyone records a venue program upgrade in the latch.
    pub fn flag_venue_upgraded(ctx: Context<FlagVenueUpgraded>, venue: u8) -> Result<()> {
        instructions::flag_venue_upgraded::handler(ctx, venue)
    }

    /// The Emergency vault re-validates an upgraded venue program at `expected_slot`, naming
    /// the sha256 of the trimmed ELF it reviewed (AIR-01; all zero: the slot alone).
    pub fn revalidate_venue(
        ctx: Context<RevalidateVenue>,
        venue: u8,
        expected_slot: u64,
        expected_elf_hash: [u8; 32],
    ) -> Result<()> {
        instructions::revalidate_venue::handler(ctx, venue, expected_slot, expected_elf_hash)
    }

    /// Anyone moves a market's protocol fees to the configured recipient.
    pub fn sweep_protocol_fees(ctx: Context<SweepProtocolFees>) -> Result<()> {
        instructions::sweep_protocol_fees::handler(ctx)
    }

    // ---- markets (ARCH §3.2, §3.9) ----

    /// Anyone lists a mint, paying at most `max_creation_fee`.
    pub fn create_market<'info>(
        ctx: Context<'info, CreateMarket<'info>>,
        venue_hint: u8,
        max_creation_fee: u64,
    ) -> Result<()> {
        instructions::create_market::handler(ctx, venue_hint, max_creation_fee)
    }

    /// Anyone flags a market whose mint fails the safety filter.
    pub fn flag_unsafe_mint(ctx: Context<FlagUnsafeMint>) -> Result<()> {
        instructions::flag_unsafe_mint::handler(ctx)
    }

    // ---- lenders (ARCH §3.3) ----

    /// A lender deposits tokens.
    pub fn deposit<'info>(
        ctx: Context<'info, Deposit<'info>>,
        amount: u64,
        min_shares: u128,
    ) -> Result<()> {
        instructions::deposit::handler(ctx, amount, min_shares)
    }

    /// A lender withdraws its pro-rata part (tokens and SOL), bounded below by `min_tokens`
    /// and `min_sol`.
    pub fn withdraw<'info>(
        ctx: Context<'info, Withdraw<'info>>,
        shares: u128,
        min_tokens: u64,
        min_sol: u64,
    ) -> Result<()> {
        instructions::withdraw::handler(ctx, shares, min_tokens, min_sol)
    }

    /// A lender claims its realised SOL.
    pub fn claim_sol<'info>(ctx: Context<'info, ClaimSol<'info>>) -> Result<()> {
        instructions::claim_sol::handler(ctx)
    }

    /// A lender covers a negative SOL balance.
    pub fn cover_deficit<'info>(
        ctx: Context<'info, CoverDeficit<'info>>,
        lamports: u64,
    ) -> Result<()> {
        instructions::cover_deficit::handler(ctx, lamports)
    }

    /// Anyone advances a lender through share epochs; through a rebase record only its owner,
    /// anyone else's sync stopping there with the pot-only touch (KI-66).
    pub fn sync_lender<'info>(
        ctx: Context<'info, SyncLender<'info>>,
        max_epochs: u8,
    ) -> Result<()> {
        instructions::sync_lender::handler(ctx, max_epochs)
    }

    // ---- oracle (ARCH §3.9) ----

    /// Anyone refreshes the cached SOL/USD price.
    pub fn crank_sol_usd(ctx: Context<CrankSolUsd>) -> Result<()> {
        instructions::crank_sol_usd::handler(ctx)
    }

    // ---- positions (ARCH §3.4–§3.8) ----

    /// A borrower opens a short of `q` tokens (borrow, sell, lock proceeds and D4 margin).
    #[allow(clippy::too_many_arguments)]
    pub fn open_short<'info>(
        ctx: Context<'info, OpenShort<'info>>,
        nonce: u64,
        q: u64,
        min_proceeds: u64,
        max_margin: u64,
        max_open_fee: u64,
        extras: venues::ExtrasSel,
    ) -> Result<()> {
        instructions::open_short::handler(
            ctx,
            nonce,
            q,
            min_proceeds,
            max_margin,
            max_open_fee,
            extras,
        )
    }

    /// The borrower buys back and closes its position.
    pub fn close_short<'info>(
        ctx: Context<'info, CloseShort<'info>>,
        max_buyback_cost: u64,
        extras: venues::ExtrasSel,
    ) -> Result<()> {
        instructions::close_short::handler(ctx, max_buyback_cost, extras)
    }

    /// The borrower adds SOL margin to its position (it only raises CR).
    pub fn add_margin(ctx: Context<AddMargin>, amount: u64) -> Result<()> {
        instructions::add_margin::handler(ctx, amount)
    }

    /// A keeper liquidates a CR-liquidatable position within the fixed exit bound (ARCH §3.6).
    pub fn liquidate<'info>(
        ctx: Context<'info, Liquidate<'info>>,
        extras: venues::ExtrasSel,
    ) -> Result<()> {
        instructions::liquidate::handler(ctx, extras)
    }

    /// A keeper exits an expired, not CR-liquidatable position (ARCH §3.7).
    pub fn expire<'info>(
        ctx: Context<'info, Expire<'info>>,
        extras: venues::ExtrasSel,
    ) -> Result<()> {
        instructions::expire::handler(ctx, extras)
    }

    /// A keeper settles a position in SOL on the settlement box (S1/S2/S3; ARCH §3.8).
    pub fn sol_settle<'info>(ctx: Context<'info, SolSettle<'info>>) -> Result<()> {
        instructions::sol_settle::handler(ctx)
    }

    // ---- cranks (ARCH §3.9) ----

    /// Anyone runs a market's prologue (observation, chains) and refreshes its tier.
    pub fn crank_price<'info>(ctx: Context<'info, CrankPrice<'info>>) -> Result<()> {
        instructions::crank_price::handler(ctx)
    }

    /// Anyone sweeps SOL left on a market's trader side to lenders; after a full wipe it
    /// credits the wipe record and sets `wipe_trader_swept` (ARCH §3.9; M2-SEC-4).
    pub fn sweep_trader<'info>(ctx: Context<'info, SweepTrader<'info>>) -> Result<()> {
        instructions::sweep_trader::handler(ctx)
    }

    // ---- Raydium pool selection (ARCH §2.6, §3.9; M4) ----

    /// Anyone proposes a Raydium pool of kind `candidate_kind` (4 AMM v4, 5 CPMM) for a
    /// list-only or Raydium market: an immediate adoption (a list-only market without open
    /// positions; no open position with the listing-window or zero-position margin) or a
    /// collection window (the fast path; a list-only market with open positions).
    pub fn propose_pool<'info>(
        ctx: Context<'info, ProposePool<'info>>,
        candidate_kind: u8,
    ) -> Result<()> {
        instructions::propose_pool::handler(ctx, candidate_kind)
    }

    /// Anyone resolves a collection window once it has run: adopts the best candidate if it is
    /// still eligible (with its locked SOL at the SOL floor), its program is not latched and,
    /// on a Raydium market, the `pool_fail` chain that armed the window still holds; else
    /// cancels.
    pub fn execute_pool_switch<'info>(ctx: Context<'info, ExecutePoolSwitch<'info>>) -> Result<()> {
        instructions::execute_pool_switch::handler(ctx)
    }

    // ---- O6 Emergency powers (ARCH §3.1; SPEC §11) ----

    /// The Emergency vault flags or clears a broken route (opens and S3 evidence only).
    pub fn set_route_broken(
        ctx: Context<SetRouteBroken>,
        scope: PauseScope,
        venue_bit: u8,
        broken: bool,
    ) -> Result<()> {
        instructions::set_route_broken::handler(ctx, scope, venue_bit, broken)
    }

    /// The Emergency vault publishes a venue's bounded trailing accounts.
    pub fn set_venue_extras(
        ctx: Context<SetVenueExtras>,
        venue: u8,
        ix: u8,
        entries: Vec<ExtraEntryArg>,
    ) -> Result<()> {
        instructions::set_venue_extras::handler(ctx, venue, ix, entries)
    }
}
