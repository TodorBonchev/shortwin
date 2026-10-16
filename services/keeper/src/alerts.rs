//! Alerts (ARCHITECTURE §7.1 "Keys and ops", duties 5, 6, 9, 10 and 12): what the keeper pages
//! about. The keeper returns them in each [`crate::keeper::TickReport`] and logs them; the
//! binary forwards them to its log (a pager integration is an ops choice).

use anchor_lang::prelude::Pubkey;
use shortwin::state::VenueKind;

use crate::monitor::AdminChange;

/// Why a market is degraded (duty 9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DegradedReason {
    /// This many consecutive seconds needed an observation and got none (budget, outage,
    /// contested blocks).
    Misses(u32),
    /// The program reported `CoverageGap{gap_s, open_positions}` (A69).
    CoverageGap { gap_s: i64, open_positions: u32 },
    /// O17, the global budget (owner decision A of 2026-10-01; ARCH §7.1 duty 8): a charge of the
    /// market, which has open positions, was refused — the budget can no longer cover it at its
    /// rank. `spent`: the day's spend of the whole budget when it was refused; `budget`: `G`
    /// (`global_daily_lamports`); `rank`: the market's rank by value at risk (`None`: below the
    /// floor, the tail spent).
    Budget {
        spent: u64,
        budget: u64,
        rank: Option<u32>,
    },
}

/// Why the keeper could not send (or had no need to hold) `set_coverage_pause` itself (O17): the
/// `Degraded` alert's page to the Emergency multisig is then the pause path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PauseUnavailable {
    /// This instance holds no `coverage_authority` key (`[coverage_pause] authority_keypair`).
    NoKey,
    /// `GlobalConfig.coverage_authority` is unset: the role is inert.
    AuthorityUnset,
    /// The key this instance holds is not the configured `coverage_authority` (rotated).
    WrongKey { configured: Pubkey, held: Pubkey },
}

/// An alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Alert {
    /// The keeper cannot meet duties 1–2 on `market` (O17's degraded mode): the designated
    /// pauser sends `set_coverage_pause(market, true)` itself ([`Alert::CoveragePauseSent`]);
    /// without it (or when it cannot, [`Alert::CoveragePauseUnavailable`]) page the Emergency
    /// multisig to `set_pause(market)` with reason `KeeperCoverage`. Exits continue.
    Degraded {
        market: Pubkey,
        reason: DegradedReason,
    },
    /// Coverage has been met for `recovery_s` again: the `KeeperCoverage` pause may lift.
    CoverageRestored { market: Pubkey, degraded_s: i64 },
    /// O17: the keeper's `set_coverage_pause(market, paused)` landed (opens paused, or resumed).
    CoveragePauseSent { market: Pubkey, paused: bool },
    /// O17: the keeper's `set_coverage_pause(market, paused)` was refused in simulation or failed
    /// on chain (`code`: the program's custom error); retried every second while the flag differs
    /// from the coverage state. Alerted once per market, direction and detail.
    CoveragePauseFailed {
        market: Pubkey,
        paused: bool,
        code: Option<u32>,
        detail: String,
    },
    /// O17: the designated pauser cannot send `set_coverage_pause` (`reason`): page the Emergency
    /// multisig to `set_pause(market)` (reason `KeeperCoverage`) instead. Alerted once per market
    /// and reason while the flag differs from the coverage state.
    CoveragePauseUnavailable {
        market: Pubkey,
        paused: bool,
        reason: PauseUnavailable,
    },
    /// Duty 11: the program refuses the configured feed's newer update (`reason`: the
    /// `pyth::check` refusal the keeper's view found, or the simulation's error `code`) — the
    /// cache keeps its last price and ages toward `oracle_stale_s` (every market's tier then falls
    /// to C). Alerted once per reason until a crank lands.
    OracleRefused {
        publish_time: i64,
        code: Option<u32>,
        reason: String,
    },
    /// Duty 11: the `OracleCache` is `age_s` old (above `oracle.aging_alert_s`): the feed is not
    /// updating, or its updates are refused. At `oracle_stale_s` every market's tier falls to C
    /// (USD thresholds unreadable). Once per stall (per cache `publish_time`, M7D-6), and at most
    /// hourly while it lasts.
    OracleAging { age_s: i64, publish_time: i64 },
    /// Duty 11: the configured `PriceUpdateV2` account is missing or does not decode.
    OracleUnreadable { account: Pubkey, detail: String },
    /// O17: `market`'s `paused_coverage` flag was set when the keeper first saw it (a restart,
    /// another instance's pause): it starts `Recovering`, and the flag clears after `recovery_s`
    /// of met coverage.
    CoveragePauseFound { market: Pubkey },
    /// M7-SEC-3: `market`'s `paused_coverage` flag was found **cleared** while the keeper holds
    /// the market degraded and had not cleared it itself — a clear this instance did not send:
    /// the coverage key elsewhere (another instance holding it, or the key compromised: the
    /// bracket `[clear, open_short, set]` the program's clear delay refuses) or, since
    /// OWNER-M7-3 (b), the Emergency multisig. The designated pauser sets it again in the same
    /// tick **only on a market it degraded itself and still holds degraded**; a flag it only
    /// adopted (found set at a restart, or set by someone else) stays clear and the market's
    /// adopted recovery ends. Page the Emergency multisig, which can `set_pause(market)` (a pause
    /// the coverage key cannot clear), clear the coverage flag itself and have the key rotated; to
    /// override the honest pauser's own pause, stop it first (`[coverage_pause] enabled = false`).
    CoveragePauseCleared { market: Pubkey },
    /// ★OWNER-M7-3 (b) (2026-10-01; ARCH §7.1 duty 9): `market`'s `paused_coverage` flag turned
    /// **set** while this designated pauser ran without having sent the set (seen clear, then
    /// set): a second instance holding the coverage key, or the key compromised. The market starts
    /// `Recovering` (the flag lifts only after `recovery_s` of this keeper's own coverage); page
    /// the Emergency multisig, which can clear the flag at once (`set_coverage_pause(false)`,
    /// clear only) while Governance rotates the key. A flag already set at the pauser's first
    /// sight is a restart's ([`Alert::CoveragePauseFound`]).
    CoveragePauseForeign { market: Pubkey },
    /// M7-SEC-3: an open (`position`) landed on `market` while the keeper held it degraded
    /// (degraded or recovering since `degraded_since`): with the flag set, only an open in the
    /// clear delay's gap or a stale read of the flag lets that happen — page as for
    /// [`Alert::CoveragePauseCleared`].
    OpenWhileDegraded {
        market: Pubkey,
        position: Pubkey,
        degraded_since: i64,
    },
    /// The global keeper budget (`[budget] global_daily_lamports`, owner decision A of
    /// 2026-10-01; ARCH §7.1 duty 8) refused a charge for the first time this UTC day — on
    /// `market`; `spent`: the day's spend so far, `budget`: `G`. Once a day (the per-market
    /// refusals are each market's `Degraded{Budget}`).
    BudgetExhausted {
        market: Pubkey,
        spent: u64,
        budget: u64,
    },
    /// The global budget booked a charge beyond `G` for the first time this UTC day (an
    /// exit-critical crank, or a warm-up or listing-window crank of a market without positions,
    /// which are sent anyway: OWNER-M7-6 (a), lender safety first); `overdraft`: `O` then. Once a
    /// day; `budget_overdraft_lamports` follows the rest.
    ExitOverdraft {
        market: Pubkey,
        overdraft: u64,
        budget: u64,
    },
    /// The cluster refused a v1 transaction: batches fall back to v0.
    V1Unsupported { reason: String },
    /// An exit the keeper's projection expected was refused by the program in simulation
    /// (custom error `code`), or failed on a venue with every extras set. `detail`: the error and
    /// the program's own error line (`AnchorError thrown in <source>:<line>. Error Code: …`), if
    /// any — on a curve exit refused with both TradeExtras lists on a deploy slot whose format an
    /// exit found, the known format's refusal, whose source line tells the ReadSet's
    /// (`check_read`) or VenueExtras' (`parse_extras`) parser from the TradeExtras one.
    ExitRefused {
        market: Pubkey,
        position: Pubkey,
        code: Option<u32>,
        detail: String,
    },
    /// Duty 10: venue program `venue` (a `VenueGuard` index) was upgraded — its ProgramData
    /// deploy slot is no longer the one the latch validated (or flagged), and the program would
    /// count the change as an upgrade (AIR-01: a fingerprinted venue's ProgramData that only grew
    /// with the reviewed ELF was extended, which is not alerted, nor is an extension of the
    /// deploy already reported during its review; with the bit clear the flag's simulation
    /// judges it, and when it cannot, the keeper's whole read of the account after its exits,
    /// or — that read failing too — the slot alone). The keeper flags it (`flag_venue_upgraded`);
    /// page the Emergency multisig to run the fork suite against the new binary, then
    /// `revalidate_venue(venue, deploy_slot, expected_elf_hash)` naming the reviewed trimmed
    /// ELF's sha256 (`shortwin-ops revalidate-venue`), publish an extras set or (once O6 is
    /// signed) set the route-broken flag.
    VenueUpgraded {
        venue: u8,
        program: Pubkey,
        /// The latch's validated slot (`guard_slot`), or the flagged one for a second upgrade
        /// during review.
        known_slot: u64,
        deploy_slot: u64,
    },
    /// The keeper's `flag_venue_upgraded(venue)` landed: opens on that venue's markets are
    /// refused until `revalidate_venue`.
    VenueUpgradeFlagged { venue: u8, deploy_slot: u64 },
    /// `flag_venue_upgraded(venue)` could not be sent or was refused (other than "already
    /// flagged"), or — AIR-01 — its simulation ran on a bank before the deploy (a node behind the
    /// keeper's header read); retried every second while the upgrade stands unflagged (after a
    /// lagging simulation, the next tick). Not raised when short.win itself refused it as not
    /// deployed in the slot (AIR-18: tried again the next slot; a long run is
    /// `ProgramNotDeployed`).
    VenueFlagFailed { venue: u8, detail: String },
    /// A watched upgradeable program without a latch index (Token-2022) was upgraded: alert
    /// only (ARCH §7.1 duty 10). AIR-01: judged by the program's rule against the keeper's last
    /// whole read of its ProgramData (a stranger's `ExtendProgram` is not alerted); before its
    /// first whole read, or when the read fails, any deploy-slot change is.
    ProgramUpgraded {
        program: Pubkey,
        previous_slot: u64,
        deploy_slot: u64,
    },
    /// A watched ProgramData account is missing or no longer a ProgramData account.
    ProgramDataUnreadable { program: Pubkey },
    /// AIR-18: the keeper's sends naming `program` (short.win, a venue, Token-2022) have been
    /// refused "Program is not deployed", each in a slot `program` was deployed, upgraded or
    /// extended in, since `since`, with no pause longer than `NOT_DEPLOYED_RUN_GAP_S` (5 s), for
    /// `NOT_DEPLOYED_ALERT_S` (30 s; `refusals` so far): someone deploys or extends it in
    /// nearly every slot — the loader's permissionless `ExtendProgram`, the griefing the owner
    /// accepted as bounded, running long — and every exit, crank or flag that needs it waits
    /// meanwhile (each is sent again every slot, no other alert). Once per run. Check the
    /// program's ProgramData (its growth toward the 10 MiB cap) and whether exits land.
    ProgramNotDeployed {
        program: Pubkey,
        since: i64,
        refusals: u64,
    },
    /// Duty 10: an admin action changed a market's pool (a Raydium `SetParams`,
    /// `update_pool_status` or `update_amm_config`, seen as the account change it makes;
    /// [`crate::monitor`]).
    VenueAdmin {
        market: Pubkey,
        venue: VenueKind,
        pool: Pubkey,
        change: AdminChange,
    },
    /// Duty 5 / 12 (ARCH §2.6): `market` adopted another pool — by the keeper's own
    /// `propose_pool` / `execute_pool_switch` (`kind`: its `POOL_KIND_*`), or seen on chain
    /// (`kind` `None`: someone else's transaction). A list-only market's `from` is the default
    /// key.
    PoolSwitched {
        market: Pubkey,
        from: Pubkey,
        to: Pubkey,
        venue: VenueKind,
        kind: Option<u8>,
        d_inc: u128,
        d_cand: u128,
        by_keeper: bool,
    },
    /// Duty 5: a fast-path collection window opened (or took a deeper candidate) on
    /// `candidate`; it resolves at `window_ends`.
    PoolWindow {
        market: Pubkey,
        candidate: Pubkey,
        d_cand: u128,
        window_ends: i64,
        improved: bool,
        by_keeper: bool,
    },
    /// Duty 5: a collection window closed without an adoption (the best candidate no longer
    /// eligible, or the incumbent no longer failing).
    PoolSwitchCancelled {
        market: Pubkey,
        candidate: Pubkey,
        by_keeper: bool,
    },
    /// Duty 12, R4B-10: a candidate with at least `pool_switch_margin_bps` more locked depth has
    /// been waiting since `since` while open positions pin the market to its thinner incumbent
    /// — page the Emergency multisig to pause opens on the market so it drains; the keeper
    /// adopts the candidate in the slot the last position closes.
    PoolPinned {
        market: Pubkey,
        incumbent: Pubkey,
        candidate: Pubkey,
        d_inc: u128,
        d_cand: u128,
        open_positions: u32,
        since: i64,
    },
    /// A pool-selection instruction the keeper's projection expected to pass was refused by the
    /// program (custom error `code`) or failed on chain; alerted once per pool and code.
    PoolSelectRefused {
        market: Pubkey,
        pool: Pubkey,
        code: Option<u32>,
        detail: String,
    },
    /// Duty 6: the keeper's `migrate_v2` of `market`'s completed curve failed in simulation or
    /// on chain (retried every second while the curve stays unmigrated); alerted once per
    /// market and detail. Positions pause (`VenueMigrating`) until someone migrates.
    MigrateFailed { market: Pubkey, detail: String },
    /// pump-final §7, the upgrade window: a curve exit of `position` on `market` got past
    /// short.win's account parsing with neither TradeExtras list — the v3 list (10 keys) nor
    /// shortwin 1.0.0's (20) — on the short.win deployed at ProgramData deploy slot `deploy_slot`
    /// (`None`: unreadable), at which no exit had found the format; `detail` holds both
    /// refusals with the program's error lines. Once per position, deploy slot and pair of
    /// refusal codes, again after an exit on the market parses; the refusals themselves are
    /// `ExitRefused` at each attempt. The keeper's curve exits fail until one parses (it tries
    /// both again at the next exit): check which short.win is deployed, and the live `Global`'s
    /// buyback list (T7) and the trader's pump UVA (T6) the v3 list names. The same codes come
    /// from the market's ReadSet (`check_read`) or VenueExtras (`parse_extras`): the source line
    /// in each error line tells which, and if other curve markets' exits parse
    /// (`TickReport::curve_formats`), the cause is this market's.
    CurveExtrasFormatUnknown {
        market: Pubkey,
        position: Pubkey,
        deploy_slot: Option<u64>,
        detail: String,
    },
    /// Duty 10 (pump-final §7): a covered live curve breaks pump's own invariant `vQ − rQ =
    /// initial_virtual_quote_reserves` (`BondingCurve::quote_offset_consistent`). Nothing
    /// short.win prices assumes it while the curve is live; on completion the post-complete
    /// fields would not be known (an S3 there gives the lenders the whole take,
    /// [`Alert::MigratingFvUndefined`]). Once per market while it lasts.
    CurveQuoteOffsetInconsistent {
        market: Pubkey,
        virtual_quote_reserves: u64,
        real_quote_reserves: u64,
        initial_virtual_quote_reserves: u64,
    },
    /// Duty 10 (pump-final §6.1.5 G-3): a covered **live** curve carries post-complete fields
    /// (`pcB` @150 or `pcQ` @158 non-zero), which pump writes only in the buy that completes a
    /// curve: the curve is not executable (no route: the S3 chain counts) and its risk view is
    /// A63's cap-only view on the curve's own reserves. Once per market while it lasts.
    LiveCurvePostComplete {
        market: Pubkey,
        post_complete_base_out: u64,
        post_complete_quote_in: u64,
    },
    /// Duty 10 (pump-final §6.1.5): a covered `PumpMigrating` curve's post-complete fields are not
    /// known (only one of `pcB` / `pcQ` written, or `vQ − rQ ≠ ivqr` after a pool part): its FV is
    /// undefined, so an S3 there gives the lenders the whole take and the borrower 0 (the
    /// documented exception to A63's cap-only rule). The owner may want to act before
    /// `max_migration_pause_s + s3_wait_s` (≈ 70 min) runs out. Once per market while it lasts.
    MigratingFvUndefined {
        market: Pubkey,
        post_complete_base_out: u64,
        post_complete_quote_in: u64,
    },
    /// Duty 6 (M6): the keeper's `→ PumpSwap` transition of `market` (the pool's accounts, the
    /// crank onto the pool) or the creation of the pool accounts its trades require failed; the
    /// market's exits refuse (`VenueMigrating` before the transition, `VenueAccountMismatch`
    /// without the accounts) until it lands. Retried every second; alerted once per detail.
    TransitionFailed { market: Pubkey, detail: String },
    /// Duties 6 and 13: a `sweep_trader` (with the venue cashback claims) failed. While `S = 0` —
    /// after a full wipe or, since AIR-08, after the last lender's exit — the market takes no
    /// deposit, and hence no open, until it lands (R4A-7). Alerted once per market and detail.
    SweepFailed {
        market: Pubkey,
        post_wipe: bool,
        detail: String,
    },
}

impl Alert {
    /// Logs the alert.
    pub fn log(&self) {
        match self {
            Self::Degraded { .. }
            | Self::BudgetExhausted { .. }
            | Self::CoveragePauseFailed { .. }
            | Self::CoveragePauseUnavailable { .. }
            | Self::CoveragePauseCleared { .. }
            | Self::CoveragePauseForeign { .. }
            | Self::ExitOverdraft { .. }
            | Self::OpenWhileDegraded { .. } => {
                tracing::error!(alert = ?self, "keeper coverage alert");
            }
            Self::VenueUpgraded { .. }
            | Self::VenueFlagFailed { .. }
            | Self::ProgramUpgraded { .. }
            | Self::ProgramDataUnreadable { .. }
            | Self::ProgramNotDeployed { .. } => {
                tracing::error!(alert = ?self, "venue program alert");
            }
            Self::VenueAdmin { change, .. } if change.is_severe() => {
                tracing::error!(alert = ?self, "venue admin action");
            }
            Self::PoolPinned { .. } => tracing::error!(alert = ?self, "pool selection alert"),
            Self::OracleRefused { .. }
            | Self::OracleAging { .. }
            | Self::OracleUnreadable { .. } => {
                tracing::error!(alert = ?self, "oracle alert");
            }
            Self::MigrateFailed { .. } | Self::TransitionFailed { .. } => {
                tracing::error!(alert = ?self, "graduation alert");
            }
            Self::CurveExtrasFormatUnknown { .. }
            | Self::CurveQuoteOffsetInconsistent { .. }
            | Self::LiveCurvePostComplete { .. }
            | Self::MigratingFvUndefined { .. } => {
                tracing::error!(alert = ?self, "pump curve alert");
            }
            Self::SweepFailed {
                post_wipe: true, ..
            } => {
                tracing::error!(alert = ?self, "post-wipe sweep alert");
            }
            Self::PoolSwitched { .. }
            | Self::PoolWindow { .. }
            | Self::PoolSwitchCancelled { .. }
            | Self::PoolSelectRefused { .. } => {
                tracing::warn!(alert = ?self, "pool selection");
            }
            _ => tracing::warn!(alert = ?self, "keeper alert"),
        }
    }
}
